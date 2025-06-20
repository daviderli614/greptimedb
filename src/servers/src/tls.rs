// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fs::File;
use std::io::{BufReader, Error as IoError, ErrorKind};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, RwLock};

use common_telemetry::{error, info};
use notify::{EventKind, RecursiveMode, Watcher};
use rustls::ServerConfig;
use rustls_pemfile::{certs, read_one, Item};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use snafu::ResultExt;
use strum::EnumString;

use crate::error::{FileWatchSnafu, InternalIoSnafu, Result};

/// TlsMode is used for Mysql and Postgres server start up.
#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq, Eq, EnumString)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    #[default]
    #[strum(to_string = "disable")]
    Disable,

    #[strum(to_string = "prefer")]
    Prefer,

    #[strum(to_string = "require")]
    Require,

    // TODO(SSebo): Implement the following 2 TSL mode described in
    // ["34.19.3. Protection Provided in Different Modes"](https://www.postgresql.org/docs/current/libpq-ssl.html)
    #[strum(to_string = "verify-ca")]
    VerifyCa,

    #[strum(to_string = "verify-full")]
    VerifyFull,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TlsOption {
    pub mode: TlsMode,
    #[serde(default)]
    pub cert_path: String,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub watch: bool,
}

impl TlsOption {
    pub fn new(mode: Option<TlsMode>, cert_path: Option<String>, key_path: Option<String>) -> Self {
        let mut tls_option = TlsOption::default();

        if let Some(mode) = mode {
            tls_option.mode = mode
        };

        if let Some(cert_path) = cert_path {
            tls_option.cert_path = cert_path
        };

        if let Some(key_path) = key_path {
            tls_option.key_path = key_path
        };

        tls_option
    }

    pub fn setup(&self) -> Result<Option<ServerConfig>> {
        if let TlsMode::Disable = self.mode {
            return Ok(None);
        }
        let cert = certs(&mut BufReader::new(
            File::open(&self.cert_path)
                .inspect_err(|e| error!(e; "Failed to open {}", self.cert_path))
                .context(InternalIoSnafu)?,
        ))
        .collect::<std::result::Result<Vec<CertificateDer>, IoError>>()
        .context(InternalIoSnafu)?;

        let mut key_reader = BufReader::new(
            File::open(&self.key_path)
                .inspect_err(|e| error!(e; "Failed to open {}", self.key_path))
                .context(InternalIoSnafu)?,
        );
        let key = match read_one(&mut key_reader)
            .inspect_err(|e| error!(e; "Failed to read {}", self.key_path))
            .context(InternalIoSnafu)?
        {
            Some(Item::Pkcs1Key(key)) => PrivateKeyDer::from(key),
            Some(Item::Pkcs8Key(key)) => PrivateKeyDer::from(key),
            Some(Item::Sec1Key(key)) => PrivateKeyDer::from(key),
            _ => {
                return Err(IoError::new(ErrorKind::InvalidInput, "invalid key"))
                    .context(InternalIoSnafu);
            }
        };

        // TODO(SSebo): with_client_cert_verifier if TlsMode is Required.
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert, key)
            .map_err(|err| std::io::Error::new(ErrorKind::InvalidInput, err))?;

        Ok(Some(config))
    }

    pub fn should_force_tls(&self) -> bool {
        !matches!(self.mode, TlsMode::Disable | TlsMode::Prefer)
    }

    pub fn cert_path(&self) -> &Path {
        Path::new(&self.cert_path)
    }

    pub fn key_path(&self) -> &Path {
        Path::new(&self.key_path)
    }

    pub fn watch_enabled(&self) -> bool {
        self.mode != TlsMode::Disable && self.watch
    }
}

/// A mutable container for TLS server config
///
/// This struct allows dynamic reloading of server certificates and keys
pub struct ReloadableTlsServerConfig {
    tls_option: TlsOption,
    config: RwLock<Option<Arc<ServerConfig>>>,
    version: AtomicUsize,
}

impl ReloadableTlsServerConfig {
    /// Create server config by loading configuration from `TlsOption`
    pub fn try_new(tls_option: TlsOption) -> Result<ReloadableTlsServerConfig> {
        let server_config = tls_option.setup()?;
        Ok(Self {
            tls_option,
            config: RwLock::new(server_config.map(Arc::new)),
            version: AtomicUsize::new(0),
        })
    }

    /// Reread server certificates and keys from file system.
    pub fn reload(&self) -> Result<()> {
        let server_config = self.tls_option.setup()?;
        *self.config.write().unwrap() = server_config.map(Arc::new);
        self.version.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Get the server config hold by this container
    pub fn get_server_config(&self) -> Option<Arc<ServerConfig>> {
        self.config.read().unwrap().clone()
    }

    /// Get associated `TlsOption`
    pub fn get_tls_option(&self) -> &TlsOption {
        &self.tls_option
    }

    /// Get version of current config
    ///
    /// this version will auto increase when server config get reloaded.
    pub fn get_version(&self) -> usize {
        self.version.load(Ordering::Relaxed)
    }
}

pub fn maybe_watch_tls_config(tls_server_config: Arc<ReloadableTlsServerConfig>) -> Result<()> {
    if !tls_server_config.get_tls_option().watch_enabled() {
        return Ok(());
    }

    let tls_server_config_for_watcher = tls_server_config.clone();

    let (tx, rx) = channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(tx).context(FileWatchSnafu { path: "<none>" })?;

    let cert_path = tls_server_config.get_tls_option().cert_path();
    watcher
        .watch(cert_path, RecursiveMode::NonRecursive)
        .with_context(|_| FileWatchSnafu {
            path: cert_path.display().to_string(),
        })?;

    let key_path = tls_server_config.get_tls_option().key_path();
    watcher
        .watch(key_path, RecursiveMode::NonRecursive)
        .with_context(|_| FileWatchSnafu {
            path: key_path.display().to_string(),
        })?;

    std::thread::spawn(move || {
        let _watcher = watcher;
        while let Ok(res) = rx.recv() {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Modify(_) | EventKind::Create(_) => {
                        info!("Detected TLS cert/key file change: {:?}", event);
                        if let Err(err) = tls_server_config_for_watcher.reload() {
                            error!(err; "Failed to reload TLS server config");
                        } else {
                            info!("Reloaded TLS cert/key file successfully.");
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_ring_crypto_provider;
    use crate::tls::TlsMode::Disable;

    #[test]
    fn test_new_tls_option() {
        assert_eq!(TlsOption::default(), TlsOption::new(None, None, None));
        assert_eq!(
            TlsOption {
                mode: Disable,
                ..Default::default()
            },
            TlsOption::new(Some(Disable), None, None)
        );
        assert_eq!(
            TlsOption {
                mode: Disable,
                cert_path: "/path/to/cert_path".to_string(),
                key_path: "/path/to/key_path".to_string(),
                watch: false
            },
            TlsOption::new(
                Some(Disable),
                Some("/path/to/cert_path".to_string()),
                Some("/path/to/key_path".to_string())
            )
        );
    }

    #[test]
    fn test_tls_option_disable() {
        let s = r#"
        {
            "mode": "disable"
        }
        "#;

        let t: TlsOption = serde_json::from_str(s).unwrap();

        assert!(!t.should_force_tls());

        assert!(matches!(t.mode, TlsMode::Disable));
        assert!(t.key_path.is_empty());
        assert!(t.cert_path.is_empty());
        assert!(!t.watch_enabled());

        let setup = t.setup();
        let setup = setup.unwrap();
        assert!(setup.is_none());
    }

    #[test]
    fn test_tls_option_prefer() {
        let s = r#"
        {
            "mode": "prefer",
            "cert_path": "/some_dir/some.crt",
            "key_path": "/some_dir/some.key"
        }
        "#;

        let t: TlsOption = serde_json::from_str(s).unwrap();

        assert!(!t.should_force_tls());

        assert!(matches!(t.mode, TlsMode::Prefer));
        assert!(!t.key_path.is_empty());
        assert!(!t.cert_path.is_empty());
        assert!(!t.watch_enabled());
    }

    #[test]
    fn test_tls_option_require() {
        let s = r#"
        {
            "mode": "require",
            "cert_path": "/some_dir/some.crt",
            "key_path": "/some_dir/some.key"
        }
        "#;

        let t: TlsOption = serde_json::from_str(s).unwrap();

        assert!(t.should_force_tls());

        assert!(matches!(t.mode, TlsMode::Require));
        assert!(!t.key_path.is_empty());
        assert!(!t.cert_path.is_empty());
        assert!(!t.watch_enabled());
    }

    #[test]
    fn test_tls_option_verify_ca() {
        let s = r#"
        {
            "mode": "verify_ca",
            "cert_path": "/some_dir/some.crt",
            "key_path": "/some_dir/some.key"
        }
        "#;

        let t: TlsOption = serde_json::from_str(s).unwrap();

        assert!(t.should_force_tls());

        assert!(matches!(t.mode, TlsMode::VerifyCa));
        assert!(!t.key_path.is_empty());
        assert!(!t.cert_path.is_empty());
        assert!(!t.watch_enabled());
    }

    #[test]
    fn test_tls_option_verify_full() {
        let s = r#"
        {
            "mode": "verify_full",
            "cert_path": "/some_dir/some.crt",
            "key_path": "/some_dir/some.key"
        }
        "#;

        let t: TlsOption = serde_json::from_str(s).unwrap();

        assert!(t.should_force_tls());

        assert!(matches!(t.mode, TlsMode::VerifyFull));
        assert!(!t.key_path.is_empty());
        assert!(!t.cert_path.is_empty());
        assert!(!t.watch_enabled());
    }

    #[test]
    fn test_tls_option_watch_enabled() {
        let s = r#"
        {
            "mode": "verify_full",
            "cert_path": "/some_dir/some.crt",
            "key_path": "/some_dir/some.key",
            "watch": true
        }
        "#;

        let t: TlsOption = serde_json::from_str(s).unwrap();

        assert!(t.should_force_tls());

        assert!(matches!(t.mode, TlsMode::VerifyFull));
        assert!(!t.key_path.is_empty());
        assert!(!t.cert_path.is_empty());
        assert!(t.watch_enabled());
    }

    #[test]
    fn test_tls_file_change_watch() {
        common_telemetry::init_default_ut_logging();
        let _ = install_ring_crypto_provider();

        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("server.crt");
        let key_path = dir.path().join("server.key");

        std::fs::copy("tests/ssl/server.crt", &cert_path).expect("failed to copy cert to tmpdir");
        std::fs::copy("tests/ssl/server-rsa.key", &key_path).expect("failed to copy key to tmpdir");

        assert!(std::fs::exists(&cert_path).unwrap());
        assert!(std::fs::exists(&key_path).unwrap());

        let server_tls = TlsOption {
            mode: TlsMode::Require,
            cert_path: cert_path
                .clone()
                .into_os_string()
                .into_string()
                .expect("failed to convert path to string"),
            key_path: key_path
                .clone()
                .into_os_string()
                .into_string()
                .expect("failed to convert path to string"),
            watch: true,
        };

        let server_config = Arc::new(
            ReloadableTlsServerConfig::try_new(server_tls).expect("failed to create server config"),
        );
        maybe_watch_tls_config(server_config.clone()).expect("failed to watch server config");

        assert_eq!(0, server_config.get_version());
        assert!(server_config.get_server_config().is_some());

        let tmp_file = key_path.with_extension("tmp");
        std::fs::copy("tests/ssl/server-pkcs8.key", &tmp_file)
            .expect("Failed to copy temp key file");
        std::fs::rename(&tmp_file, &key_path).expect("Failed to rename temp key file");

        const MAX_RETRIES: usize = 30;
        let mut retries = 0;
        let mut version_updated = false;

        while retries < MAX_RETRIES {
            if server_config.get_version() > 0 {
                version_updated = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            retries += 1;
        }

        assert!(version_updated, "TLS config did not reload in time");
        assert!(server_config.get_version() > 0);
        assert!(server_config.get_server_config().is_some());
    }
}
