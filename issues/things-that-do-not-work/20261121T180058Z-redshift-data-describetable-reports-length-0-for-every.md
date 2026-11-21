Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive
Issue-Shape-Id: com.amazonaws.redshiftdata#RedshiftData
Issue-Key: redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive|run#18|AssertionError|assert [(c["name"], c["typeName"], c["precision"], c["scale"], c["length"])
Issue-Commit: 0969d3134c46b721ee0a8be60387d306d35083b0

# redshift-data DescribeTable reports length 0 for every column instead of the column's declared width

> _Note: this issue was identified against the code base at commit `0969d3134c46b721ee0a8be60387d306d35083b0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTable

## Description
Cause 1 of 1: DescribeTable's ColumnList omits the per-column `length`, reporting the structure default 0 for every column, while the same answer's `precision` and `scale` are correct.

AWS: Smithy (com.amazonaws.redshiftdata) ColumnMetadata.length is `Integer` with `default: 0`, documented "The length of the column."; ColumnMetadata.precision is "The precision value of a decimal number column, or the column length for a non-numeric column." The model states no value rule, so this is recorded behavior: on the catalogue surface (DescribeTable) AWS POPULATES `length` with the column's declared width, which for the types in the recording equals the reported `precision` — an `int` column reports typeName `int4`, precision 10, scale 0, length 10; a `varchar(n)` reports precision n, scale 0, length n; a `decimal(p,s)` reports typeName `numeric`, precision p, scale s, length p. The check also records the deliberate asymmetry of the two surfaces: GetStatementResult's ColumnMetadata for the SAME columns reports length 0, so `length` is populated only on the catalogue read, not on the data-path read. Both surfaces agree on `typeName`. Only `length` diverges here; DescribeTable's HTTP status (200), TableName composition (`<database>.<schema>.<table>`, including the double-dot form when Schema is omitted) and the empty-ColumnList answer for an absent table or schema all matched.

Emulator: every ColumnMetadata entry DescribeTable returns carries `length: 0`, i.e. the member is never populated and falls through to its modeled default, even though `precision`/`scale` for the same column are computed correctly. Replay output for `create table <t>(id int, name varchar(32), amt decimal(9,3))`: `[{... 'name': 'id', 'typeName': 'int4', 'precision': 10, 'scale': 0, 'length': 0}, {... 'name': 'name', 'typeName': 'varchar', 'precision': 32, 'scale': 0, 'length': 0}, {... 'name': 'amt', 'typeName': 'numeric', 'precision': 9, 'scale': 3, 'length': 0}]` where AWS reports lengths 10, 32 and 9 respectively.

Why: a client that introspects a table through the Data API catalogue surface — JDBC/ODBC-style metadata bridges, schema-diff and codegen tools, and anything that sizes a buffer or a target column from ColumnMetadata.length — reads 0 for every column and cannot tell a varchar(32) from a varchar(65535), so it either truncates, over-allocates, or treats the column as zero-width. Because the emulator reports 0 on BOTH surfaces, it also erases the documented distinction between the catalogue read (length populated) and the data-path read (length 0), so a client that uses DescribeTable precisely to recover widths GetStatementResult does not carry gets nothing.

Fix: the catalogue column-metadata projection that DescribeTable's handler consumes — the handler at services/service-com-amazonaws-redshiftdata-redshiftdata/src/ops/describe_table.rs builds its ColumnList by cloning `described.columns` straight out of `crate::state::catalog_on` (see lines 339-402), so the `length` member must be populated where those ColumnMetadata values are constructed in the service's state/catalog layer, for the catalogue path only, leaving the GetStatementResult projection at 0.

Traces: behavioral check redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive.

## Evidence
Check: redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive

Setup SQL: `create table <table>(id int, name varchar(32), amt decimal(9,3))` in database `devdb`, schema `public`, on a Redshift Serverless workgroup.

Expected (AWS, as recorded) from DescribeTable(Database=devdb, Schema=public, Table=<table>) — (name, typeName, precision, scale, length):
  ("id",   "int4",    10, 0, 10)
  ("name", "varchar", 32, 0, 32)
  ("amt",  "numeric",  9, 3,  9)

Actual (emulator), HTTP 200:
  AssertionError: DescribeTable must report each column's type, precision, scale AND a populated length; got
  [{'isCaseSensitive': False, 'isCurrency': False, 'isSigned': True, 'label': 'id', 'name': 'id', 'nullable': 1, 'precision': 10, 'scale': 0, 'schemaName': 'public', 'tableName': '<table>', 'typeName': 'int4', 'length': 0},
   {'isCaseSensitive': True, 'isCurrency': False, 'isSigned': False, 'label': 'name', 'name': 'name', 'nullable': 1, 'precision': 32, 'scale': 0, 'schemaName': 'public', 'tableName': '<table>', 'typeName': 'varchar', 'length': 0},
   {'isCaseSensitive': False, 'isCurrency': False, 'isSigned': True, 'label': 'amt', 'name': 'amt', 'nullable': 1, 'precision': 9, 'scale': 3, 'schemaName': 'public', 'tableName': '<table>', 'typeName': 'numeric', 'length': 0}]
(the table name is a per-run generated identifier and is not part of the requirement; the diverging field is `length` on every column.)

The companion assertion in the same check — GetStatementResult's ColumnMetadata reporting length [0, 0, 0] for the same three columns — is the contrast that shows `length` is a catalogue-surface value, and it is NOT what failed.

## Steps to Reproduce
1. Create a Redshift Serverless namespace (dbName=devdb) and workgroup, wait for AVAILABLE.
2. redshift-data ExecuteStatement on that workgroup/database: `create table t1(id int, name varchar(32), amt decimal(9,3))`; poll DescribeStatement to FINISHED.
3. redshift-data DescribeTable(WorkgroupName=<wg>, Database="devdb", Schema="public", Table="t1").
4. Inspect ColumnList: each entry's `length` is 0, where AWS reports 10 for the int4 column, 32 for the varchar(32) column and 9 for the numeric(9,3) column (precision/scale are already correct).
5. For contrast, run `select id, name, amt from t1` and read GetStatementResult's ColumnMetadata: there `length` is legitimately 0 on AWS too.
