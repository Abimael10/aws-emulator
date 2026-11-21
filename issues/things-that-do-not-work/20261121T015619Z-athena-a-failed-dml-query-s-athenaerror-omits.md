Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.query-execution-external-table-s3-roundtrip
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena.query-execution-external-table-s3-roundtrip|run#39|AssertionError|assert isinstance(err.get("ErrorType"), int), f"expected an AthenaError ErrorType for {sql!r}, got {err!r}"
Issue-Commit: a277ee47ed04075090b028f1683b0172a7857865

# Athena: a FAILED DML query's AthenaError omits ErrorType, so the failure carries no Athena error code

> _Note: this issue was identified against the code base at commit `a277ee47ed04075090b028f1683b0172a7857865`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueryExecution

## Description
Cause 1 of 1: QueryExecutionStatus.AthenaError is served without its ErrorType member on the DML (write) failure path of a query that fails because the table does not exist.

AWS: Smithy (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json) — QueryExecutionStatus.AthenaError targets AthenaError, whose members are ErrorCategory (integer @range 1..3: 1 System, 2 User, 3 Other), ErrorType (integer @range 0..9999, documented as "An integer value that provides specific information about an Athena query error. For the meaning of specific values, see the Error Type Reference in the Amazon Athena User Guide"), Retryable (boolean, default false) and ErrorMessage (string). AWS populates ErrorType together with ErrorCategory on every FAILED query execution: the pair (category, type) is the standardized classification a client uses to tell one failure class from another, and the Error Type Reference enumerates the per-category type codes (the 1xxx band for the ErrorCategory 2 user errors a missing table falls into). Hypothesis: the specific numeric code for a table-not-found user error — the model fixes only that the member is present and in 0..9999, so the requirement graded here is its PRESENCE as an integer, not one particular value.

Emulator: for a query that fails against a table that was dropped, GetQueryExecution returns Status.State=FAILED with AthenaError = {'ErrorCategory': 2, 'Retryable': False, 'ErrorMessage': 'no such table: <database>.<table>'} — ErrorType is absent entirely. The same emulator DOES emit an integer ErrorType on the SELECT (read) failure of the identical dropped table in the same check (that assertion passed one loop iteration earlier), so the omission is specific to the DML/INSERT failure path, not to AthenaError as a whole. ErrorCategory (2 = user) and Retryable (false) are correct.

Why: a client that classifies a failed query by the (ErrorCategory, ErrorType) pair — retry policies, dashboards and error-routing code generated from the Athena model — reads ErrorType as unset/None for every failed write and cannot distinguish a missing table from any other user error, or crashes on the absent member where AWS always supplies an integer. The failure looks less specific than AWS reports it, and the asymmetry between the read and write paths means the same underlying condition is classified two different ways by the same emulator.

Fix: the query-execution record's failure classifier in the athena crate's state module — the code that builds QueryExecutionStatus.AthenaError when it projects a stored execution outcome into the modelled view (`to_query_execution`, reached from services/service-com-amazonaws-athena-amazonathena/src/ops/get_query_execution.rs:174). The DML failure branch must set ErrorType for the same condition the SELECT branch already sets it for, so every FAILED execution's AthenaError carries both ErrorCategory and ErrorType.

Note: the ErrorMessage text itself ('no such table: ...') is SQLite engine wording rather than Athena's, which is already covered by the open issue 20261104T163600Z-athena-statements-are-executed-by-a-sqlite-engine.md; this issue is only about the missing ErrorType member.

Traces: athena.query-execution-external-table-s3-roundtrip (the INSERT iteration of its dropped-table read/write loop).

## Evidence
Failing check athena.query-execution-external-table-s3-roundtrip:

  AssertionError: expected an AthenaError ErrorType for "INSERT INTO <database>.<table> VALUES (40, 'qux')", got {'ErrorCategory': 2, 'Retryable': False, 'ErrorMessage': 'no such table: <database>.<table>'}

Expected (AWS): GetQueryExecution for the failed INSERT returns QueryExecution.Status = {State: FAILED, StateChangeReason: <non-empty, names the missing table>, AthenaError: {ErrorCategory: 2, ErrorType: <integer in 0..9999 from the Athena Error Type Reference>, Retryable: false, ErrorMessage: <short description>}}.

Actual (emulator): the same read returns AthenaError with ErrorCategory=2, Retryable=false and ErrorMessage present, but NO ErrorType key at all. HTTP status is 200 in both cases; no error code is involved — the divergence is a dropped member inside a 200 body. The immediately preceding iteration of the same loop (a SELECT against the same dropped table) produced an AthenaError whose ErrorType WAS an integer, so the two failure paths disagree.

## Steps to Reproduce
1. Create an S3 results bucket and upload a CSV object under a data prefix.
2. StartQueryExecution "CREATE DATABASE <db>" and poll GetQueryExecution to SUCCEEDED.
3. StartQueryExecution "CREATE EXTERNAL TABLE <db>.<tbl> (id INT, name STRING) ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' LOCATION 's3://<bucket>/<prefix>/'" and poll to SUCCEEDED.
4. StartQueryExecution "DROP TABLE IF EXISTS <db>.<tbl>" and poll to SUCCEEDED.
5. StartQueryExecution "INSERT INTO <db>.<tbl> VALUES (40, 'qux')" and poll GetQueryExecution until Status.State is terminal.
6. Inspect QueryExecution.Status.AthenaError: ErrorCategory is 2 but ErrorType is missing. Repeat with "SELECT * FROM <db>.<tbl>" to see ErrorType present on the read path.
