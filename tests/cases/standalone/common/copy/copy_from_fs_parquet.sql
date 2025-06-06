CREATE TABLE demo(host string, cpu double, memory double, jsons JSON, ts TIMESTAMP time index);

CREATE TABLE demo_2(host string, cpu double, memory double, ts TIMESTAMP time index);

insert into
    demo(host, cpu, memory, jsons, ts)
values
    ('host1', 66.6, 1024, '{"foo":"bar"}', 1655276557000),
    ('host2', 88.8, 333.3, '{"a":null,"foo":"bar"}', 1655276558000);

insert into
    demo(host, cpu, memory, ts)
values
    ('host3', 111.1, 444.4, 1722077263000);

insert into
    demo_2(host, cpu, memory, ts)
values
    ('host4', 77.7, 1111, 1655276555000),
    ('host5', 99.9, 444.4, 1655276556000),
    ('host6', 222.2, 555.5, 1722077264000);

Copy demo TO '${SQLNESS_HOME}/demo/export/parquet_files/demo.parquet';

Copy demo_2 TO '${SQLNESS_HOME}/demo/export/parquet_files/demo_2.parquet';

CREATE TABLE with_filename(host string, cpu double, memory double, ts timestamp time index);

Copy with_filename FROM '${SQLNESS_HOME}/demo/export/parquet_files/demo.parquet' with (start_time='2022-06-15 07:02:37', end_time='2022-06-15 07:02:39');

select * from with_filename order by ts;

CREATE TABLE with_path(host string, cpu double, memory double, ts timestamp time index);

Copy with_path FROM '${SQLNESS_HOME}/demo/export/parquet_files/';

select * from with_path order by ts;

CREATE TABLE with_json(host string, cpu double, memory double, jsons JSON, ts timestamp time index);

Copy with_json FROM '${SQLNESS_HOME}/demo/export/parquet_files/demo.parquet';

select host, cpu, memory, json_to_string(jsons), ts from with_json order by ts;

-- SQLNESS PROTOCOL MYSQL
select host, cpu, memory, jsons, ts from demo where host != 'host3';

-- SQLNESS PROTOCOL POSTGRES
select host, cpu, memory, jsons, ts from demo where host != 'host3';

CREATE TABLE with_pattern(host string, cpu double, memory double, ts timestamp time index);

Copy with_pattern FROM '${SQLNESS_HOME}/demo/export/parquet_files/' WITH (PATTERN = 'demo.*', start_time='2022-06-15 07:02:39');

select * from with_pattern order by ts;

CREATE TABLE without_limit_rows(host string, cpu double, memory double, ts timestamp time index);

Copy without_limit_rows FROM '${SQLNESS_HOME}/demo/export/parquet_files/';

select count(*) from without_limit_rows;

CREATE TABLE with_limit_rows_segment(host string, cpu double, memory double, ts timestamp time index);

Copy with_limit_rows_segment FROM '${SQLNESS_HOME}/demo/export/parquet_files/' LIMIT 3;

select count(*) from with_limit_rows_segment;

Copy with_limit_rows_segment FROM '${SQLNESS_HOME}/demo/export/parquet_files/' LIMIT hello;

CREATE TABLE demo_with_external_column(host string, cpu double, memory double, ts timestamp time index, external_column string default 'default_value');

Copy demo_with_external_column FROM '${SQLNESS_HOME}/demo/export/parquet_files/demo.parquet';

select * from demo_with_external_column order by ts;

CREATE TABLE demo_with_less_columns(host string, memory double, ts timestamp time index);

Copy demo_with_less_columns FROM '${SQLNESS_HOME}/demo/export/parquet_files/demo.parquet';

select * from demo_with_less_columns order by ts;

drop table demo;

drop table demo_2;

drop table with_filename;

drop table with_json;

drop table with_path;

drop table with_pattern;

drop table without_limit_rows;

drop table with_limit_rows_segment;

drop table demo_with_external_column;

drop table demo_with_less_columns;
