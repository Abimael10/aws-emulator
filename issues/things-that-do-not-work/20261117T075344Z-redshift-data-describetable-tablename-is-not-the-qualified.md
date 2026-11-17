Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift-data.list-databases-schemas-tables-roundtrip redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive
Issue-Shape-Id: com.amazonaws.redshiftdata#RedshiftData
Issue-Key: redshift-data.list-databases-schemas-tables-roundtrip|run#22|AssertionError|assert described["TableName"] == expected_table_name, (
Issue-Key: redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive|run#12|KeyError|assert absent["TableName"] == f"{_DATABASE}.public.{table}", (
Issue-Commit: 7f5dd6cdee8fc233932a8e3b3b251e9cc2539379

# redshift-data DescribeTable.TableName is not the qualified <database>.<schema>.<table> path: bare table name when the table exists, omitted entirely when it does not (2 causes)

> _Note: this issue was identified against the code base at commit `7f5dd6cdee8fc233932a8e3b3b251e9cc2539379`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTable

## Description
Replay of the behavioral suite against the emulator found that BOTH surviving redshift-data failures are the `TableName` member of the `DescribeTable` response. The columns and the HTTP status are right in both cases (200, no error) — only the response's table-identity member diverges. Both causes sit in ONE expression, the `table_name` projection of the DescribeTable handler, but they are two distinct behavioral requirements and each must be closed separately: one is a wrong VALUE for a table that exists, the other is an ABSENT member for a name that matches nothing.

This bundle carries TWO causes (Cause 1 of 2 .. Cause 2 of 2); both must be closed before submitting.

Related but NOT covering: the open issue 20261109T193330Z-redshift-data-api-serverless-workgroup-statement-responses-diverge.md (Cause 5) covers the shared `ColumnMetadata` projection used by GetStatementResult and DescribeTable — it says nothing about `TableName` and carries no key for either check below. No other open redshift-data issue names this member.

=== Cause 1 of 2: DescribeTable reports the BARE table name where AWS reports the fully qualified <database>.<schema>.<table> path ===
AWS: Smithy (services/service-com-amazonaws-redshiftdata-redshiftdata/smithy-model/com-amazonaws-redshiftdata-redshiftdata.json): `DescribeTableResponse.TableName` is a String documented only as 'The table name.', so the model does not state the composition — this is recorded behavior only. The recording shows that for a DescribeTable naming a Database, a Schema and a Table, the Data API answers HTTP 200 with `TableName` set to the THREE-PART path of what was asked for, joined with dots: the request's `Database`, then its `Schema`, then its `Table`. The same check also records the degenerate form for a request that omits `Schema` — a DOUBLE-DOT `<database>..<table>` — i.e. the path is composed from the REQUEST's own selectors, with an empty component where a selector was not given, rather than from whatever the catalogue happens to hold. (That omitted-Schema assertion is in the same check but was not reached, because Cause 2 aborts the check earlier; it is named here because it is the same projection and the same rule.)
Emulator: `TableName` carries only the LAST component. For a table created in a named schema, the emulator answered `'tbl1'` where AWS answers `'<database>.<schema>.tbl1'` — in the failing check, expected `'dev.sim2real_schema_a_2e38cf093fc5.tbl1'`, got `'tbl1'` (the database `dev` and the schema name are this run's values; the requirement is the three-part dotted composition, not these literals). The handler takes the first matching catalogue relation and deliberately strips the qualification: `described.first().map(|described| crate::state::split_relation(&described.table).1.to_string())` keeps only element `.1` of the split relation name, discarding both the schema part and the database entirely.
Why: `TableName` is the only place a DescribeTable response states WHAT it described. A client that uses it to key a catalogue cache, to render a qualified identifier, or to confirm that the response matches the table it asked about reads an unqualified name that collides across schemas — two tables of the same name in different schemas become indistinguishable in the response, and any consumer comparing `TableName` against the path it requested mismatches on every call.
Fix: the `table_name` projection in the DescribeTable handler must compose the member from the request's own selectors (`Database`, `Schema`, `Table`) as a dotted path, with an empty component for a selector the request omitted, instead of deriving a single bare component from the first matching catalogue relation.
Traces: behavioral check redshift-data.list-databases-schemas-tables-roundtrip (AssertionError: expected TableName 'dev.sim2real_schema_a_2e38cf093fc5.tbl1', got 'tbl1').

=== Cause 2 of 2: DescribeTable OMITS TableName entirely when the named table or schema matches nothing, instead of echoing the requested path ===
AWS: Smithy: `DescribeTableResponse.TableName` (String), 'The table name.'; the operation models `ResourceNotFoundException` (HTTP 404) and `ValidationException` (HTTP 400), neither of which AWS uses here. Recorded behavior: a DescribeTable for a table that does not exist (never created, or already DROPped), and likewise for a Schema that does not exist, is NOT an error — it answers HTTP 200 with an EMPTY `ColumnList` AND with `TableName` still present, echoing the path that was requested. The member is reported from the request, so it is there whether or not anything was found; that is precisely what lets a caller prove a table is gone while still seeing which path the answer refers to.
Emulator: the member is MISSING from the response body. The emulator answers 200 with the correct empty `ColumnList`, but no `TableName` at all, so a client reading the field raises `KeyError: 'TableName'`. The cause is that the projection derives the member from the catalogue rather than the request: `table.is_some().then(|| described.first().map(...)).flatten()` — when nothing matched, `described` is empty, `described.first()` is `None`, and `.flatten()` collapses the whole member to `None`. The same `None` results for any request that omits `Table`, where AWS reports the degenerate dotted path.
Why: AWS's contract here is that the absent-table read is a SUCCESS that still identifies its subject, and clients depend on both halves. A consumer that reads `response['TableName']` to confirm which path the empty answer refers to does not get a different value — it gets no key at all and crashes (KeyError / absent-field) on exactly the not-found probe the 200-with-empty-list behavior exists to support. Code that drives a 'does this table still exist?' check through DescribeTable therefore fails on the negative branch rather than observing the empty ColumnList.
Fix: the same `table_name` projection in the DescribeTable handler — report the member unconditionally from the request's selectors, independent of whether the catalogue read matched any relation, so an empty `ColumnList` is still accompanied by the requested path. Fixing Cause 1 by composing from the request rather than from `described` is what makes this cause closable in the same place, but it is a separate requirement: the member must survive a zero-match read.
Traces: behavioral check redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive (KeyError: 'TableName' on the pre-create DescribeTable probe).

## Evidence
Behavioral suite replay: 44/64 passed (68.8%); redshift-data 22/24 (92%), 2 failed. Both redshift-data failures are the two causes above; every other failing check in the run is already OWNED by an open issue. Neither failure is a wrong status or a wrong error — both responses are HTTP 200 with a correct ColumnList.

Cause 1 — check redshift-data.list-databases-schemas-tables-roundtrip, verdict mismatch:
  The check creates two schemas via SQL over a Redshift Serverless workgroup, creates `<schema_a>.tbl1 (id INT, name VARCHAR(20))`, confirms ListSchemas/ListTables narrow correctly (those pass), then calls:
    describe_table(WorkgroupName=<wg>, Database='dev', Schema=<schema_a>, Table='tbl1')
  Response: HTTP 200. ColumnList is correct — the follow-on assertion `col_names == {'id','name'}` is not what failed.
    $.TableName: value_mismatch — expected 'dev.sim2real_schema_a_2e38cf093fc5.tbl1', got 'tbl1'
  Failure:
    File "check_redshift_data_list_databases_schemas_tables_roundtrip.py", line 58, in run
      assert described["TableName"] == expected_table_name
    AssertionError: expected TableName 'dev.sim2real_schema_a_2e38cf093fc5.tbl1', got 'tbl1'
  The expected value is built by the check as f"{wg['database']}.{schema_a}.tbl1" — the database name 'dev' and the generated schema name are this run's values; the requirement asserted is the three-part dotted composition.

Cause 2 — check redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive, verdict error:
  The check provisions a Redshift Serverless namespace + workgroup, confirms ListDatabases reports 'devdb' and ListSchemas reports 'public' (both pass), confirms ListTables for the not-yet-created table is empty (passes), then probes the absent table:
    describe_table(WorkgroupName=<wg>, Database='devdb', Schema='public', Table=<table>)
  Response: HTTP 200 with ColumnList == [] — the preceding assertion on the empty ColumnList PASSED, so the over-permissive 200 is right. The next assertion crashed:
    File "check_redshift_data_catalogue_reflects_created_table_and_absent_table_over_permissive.py", line 194, in run
      assert absent["TableName"] == f"{_DATABASE}.public.{table}"
    KeyError: 'TableName'
  i.e. the member is absent from the body, not merely wrong. Because the check aborts here, its later assertions on the same operation were never exercised and are NOT claimed by this issue: the post-CREATE DescribeTable column precision/scale/length, the GetStatementResult length-0 comparison, the wrong-schema echo, and the omitted-Schema double-dot `devdb..<table>` form.

Emulator source (services/service-com-amazonaws-redshiftdata-redshiftdata/src/ops/describe_table.rs:411-422), the single projection behind both causes:
    Ok(DescribeTableOutput {
        table_name: table
            .is_some()
            .then(|| {
                described
                    .first()
                    .map(|described| crate::state::split_relation(&described.table).1.to_string())
            })
            .flatten(),
        column_list: Some(columns[offset..end].to_vec()),
        ...
  - `split_relation(...).1` keeps only the trailing component, dropping the schema and never adding the database => Cause 1.
  - `described.first()` is None for a zero-match read and `.flatten()` turns the member into None => Cause 2 (also None whenever `Table` is omitted).
The handler's own doc comment at describe_table.rs:386-396 already states the correct 200-with-empty-ColumnList rule for a name the catalogue holds nothing for, so only the TableName member is at odds with it.

Model corroboration (services/service-com-amazonaws-redshiftdata-redshiftdata/smithy-model/com-amazonaws-redshiftdata-redshiftdata.json): DescribeTable output members are `TableName` (String, 'The table name.'), `ColumnList` (ColumnList), `NextToken` (String); modeled errors DatabaseConnectionException(500), InternalServerException(500), QueryTimeoutException(400), ResourceNotFoundException(404), ValidationException(400). The model does not state TableName's composition, so the dotted-path rule is grounded in the recorded behavior asserted by both checks.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Create a redshift-serverless namespace (dbName 'dev') and a workgroup on it, and wait for the workgroup to reach AVAILABLE.
3. Cause 1 (wrong value):
   a. Through redshift-data against that workgroup, execute `CREATE SCHEMA <schema_a>` and then `CREATE TABLE <schema_a>.tbl1 (id INT, name VARCHAR(20))`, polling DescribeStatement until FINISHED.
   b. Call describe_table(WorkgroupName=<wg>, Database='dev', Schema=<schema_a>, Table='tbl1').
   c. Expect HTTP 200 with TableName == '<database>.<schema_a>.tbl1' (the dotted three-part path of the request's own selectors). Observe TableName == 'tbl1' — only the trailing component.
4. Cause 2 (member absent):
   a. Pick a table name that was never created (or DROP the table from step 3).
   b. Call describe_table(WorkgroupName=<wg>, Database=<db>, Schema='public', Table=<that name>).
   c. Expect HTTP 200, ColumnList == [], AND a TableName member echoing '<db>.public.<that name>'. Observe HTTP 200 and ColumnList == [] (correct) but NO TableName member at all — reading response['TableName'] raises KeyError.
   d. Repeat with a Schema that does not exist, and with Schema omitted entirely (AWS reports the double-dot '<db>..<table>' form); both go through the same projection and likewise report no TableName.
5. Re-replay the two behavioral checks named in trace_ids — redshift-data.list-databases-schemas-tables-roundtrip and redshift-data.catalogue-reflects-created-table-and-absent-table-over-permissive — to confirm both causes are closed. Note that the second check continues past its line-194 probe once Cause 2 is fixed and will then exercise further DescribeTable/GetStatementResult assertions that this issue does not claim.
