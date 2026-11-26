Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__glue__s3.root-located-table-reads-athena-result-objects-back-as-rows athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle
Issue-Key: athena__glue__s3.root-located-table-reads-athena-result-objects-back-as-rows|run#17|AssertionError|assert echoed, (
Issue-Key: athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle|_poll#14|Timeout|return last
Issue-Commit: 2901dc6a3b1a6d3c36a8ae908870ebb8c494c947

# athena__glue__s3: 2 net-new causes — a table located at the bucket root never reads the objects Athena itself wrote under a sub-prefix of that location, and the CSV-estate deploy/lifecycle never settles

> _Note: this issue was identified against the code base at commit `2901dc6a3b1a6d3c36a8ae908870ebb8c494c947`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartQueryExecution

## Description
Two net-new root causes under the suite heading `### athena__glue__s3`. Neither failing check is marked ALREADY FILED or OWNED, so no filed issue carries their replay keys. The open issues 20261104T082656Z-athena-glue-s3-the-athena-data-plane-never.md and 20261108T073030Z-athena-glue-s3-athena-queries-bypass-glue-authorization.md are about the catalog read path and authorization, not about which objects a location enumerates; they do not cover these.

This bundle carries 2 causes; BOTH must be closed before submitting.

Cause 1 of 2: a table whose Glue location is the bucket root enumerates only the objects at the root itself — the query-result objects Athena wrote into a sub-prefix of the same bucket are never read back as rows.
  AWS: recorded behavior only (the Smithy model describes no scan semantics; the behavior is Athena's documented 'all files under the table's LOCATION are scanned, recursively' rule, and the hidden `$path` column names the object each row came from). Recorded: with a Glue EXTERNAL_TABLE whose StorageDescriptor.Location is 's3://<bucket>/' (LazySimpleSerDe, TextInputFormat, skip.header.line.count=2) and a workgroup whose ResultConfiguration.OutputLocation is a prefix INSIDE that same bucket, a first `SELECT count(*)` over the table reads only the single log object and returns its row count; that query writes '<OutputLocation-prefix>/<QueryExecutionId>.csv' and its '.csv.metadata' sibling into the bucket. A second query, `SELECT "$path", count(*) FROM <db>.<table> GROUP BY 1 ORDER BY 1`, therefore returns MORE THAN ONE $path group: the original log object with its own row count, plus at least one group whose $path is one of the first query's own result objects — the feedback loop a root-located table creates. Every group's $path is an s3:// URI of an object that exists in the bucket under the table's location.
  Emulator: the second query returns exactly one $path group — the log object with the correct count — and no group for anything the first query wrote. Either the result objects are not present in the bucket, or the scan enumerates only keys directly at the location prefix and does not descend into sub-prefixes of it. Hypothesis: the scan is a non-recursive listing of the location prefix (the log object sits at the bucket root, the result objects one prefix deeper), since the first query's row count and the $path value it does report are both correct.
  Why: a client gets silently incomplete results. Any table over a bucket root or over a prefix with nested layout (date/hour partitions written as sub-prefixes, Athena results written beside the data, tools that write under a sub-prefix) under-reports rows with no error at all — counts, aggregates and downstream dashboards are wrong rather than failed, and the self-amplifying growth a root-located table produces in production is invisible here.
  Fix: the query engine's table-location scan (the Athena data-plane S3 enumeration behind StartQueryExecution/GetQueryResults) — enumerate every object under the table's location prefix recursively, including objects Athena itself wrote into a sub-prefix of that location, and report each row's source object in `$path`.
  Traces: athena__glue__s3.root-located-table-reads-athena-result-objects-back-as-rows.

Cause 2 of 2: the CSV-estate deploy/lifecycle never reaches a verdict — the check is cut off at its budget with no result.
  AWS: recorded behavior only. The recording completes all seven phases (deploy, setup, pre_invoke, solve, post_invoke, cleanup, destroy) of a CloudFormation-deployed Athena/Glue/S3 CSV estate, so each call in that walk answers in bounded time and each polled resource settles.
  Emulator: the replay is cut off — 'Check timed out after 60.0s' — so some call or polled state in that walk never settles within the budget the recording fits in. Hypothesis: a stack operation or a query/crawl poll in the deploy or solve phase never reaches a terminal state, leaving the harness polling until the deadline. No exception and no response mismatch is reported, so the emulator produced no refusal — it produced no settled answer.
  Why: a client driving this estate hangs instead of progressing: an SDK waiter or a CI deploy step blocks until its own timeout and the lifecycle can never be exercised end to end, which also hides every assertion the later phases would make.
  Fix: whichever Athena/Glue/S3 operation in this seven-phase walk does not settle — identify it by replaying the check with per-call logging and make it reach a terminal state in bounded time (the stack-deploy path and the query/crawl state machines it polls are the candidates).
  Traces: athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 312/443 passed; athena__glue__s3 40/56 (71%). Both failures below are listed under the heading `### athena__glue__s3` with verdict [mismatch] and neither carries an ALREADY FILED or OWNED marker.

Cause 1 — athena__glue__s3.root-located-table-reads-athena-result-objects-back-as-rows [mismatch]:
  AssertionError at check line 47: "the second query read only the log object ({'s3://<bucket>/<log-key>': 4}); a root-located table reads the first query's result objects too"
  Sequence: CreateBucket; PutObject <log-key> (a gzipped 33-column CloudFront log with 2 header lines and 4 data rows); CreateWorkGroup with ResultConfiguration.OutputLocation = 's3://<bucket>/athena-results/'; Glue CreateDatabase + CreateTable with StorageDescriptor.Location = 's3://<bucket>/' ; query 1 `SELECT count(*) FROM <db>.<table>` -> State SUCCEEDED, rows [['4']] (both correct); query 2 `SELECT "$path", count(*) FROM <db>.<table> GROUP BY 1 ORDER BY 1` ->
    expected: at least two $path groups — 's3://<bucket>/<log-key>' with 4 rows, plus one or more groups drawn from {'s3://<bucket>/athena-results/<query1-id>.csv', 's3://<bucket>/athena-results/<query1-id>.csv.metadata'}
         got: exactly one group {'s3://<bucket>/<log-key>': 4}
  The bucket name, log object name, database/table/workgroup names and the first query's QueryExecutionId are per-run values; the requirement is that the result objects of the earlier query, which live under a sub-prefix of the table's location, are scanned by the later query.

Cause 2 — athena__glue__s3.e2e-create-athena-tables-from-s3-lifecycle [mismatch]:
  Error: Check timed out after 60.0s — no exception from the emulator and no value mismatch; the walk (PHASES = deploy, setup, pre_invoke, solve, post_invoke, cleanup, destroy over the CloudFormation template deployments/athena-csv-estate/template.json, SERVICES = athena, glue, s3) never reached a verdict. The 60s budget is the harness's; the recorded run of the same walk completes inside it.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator, replay simparity-behavioral-aws-v1 and look at the two check ids in trace_ids.

Cause 1, by hand: create bucket <b>. PutObject <b>/log.gz with a gzipped tab-separated CloudFront log (2 '#'-prefixed header lines + 4 data rows). CreateWorkGroup <wg> with Configuration.ResultConfiguration.OutputLocation='s3://<b>/athena-results/'. Glue CreateDatabase <db>; Glue CreateTable with TableType EXTERNAL_TABLE, Parameters {'skip.header.line.count':'2','EXTERNAL':'TRUE'}, SerdeInfo LazySimpleSerDe field.delim='\t', InputFormat org.apache.hadoop.mapred.TextInputFormat and StorageDescriptor.Location='s3://<b>/'. Run `SELECT count(*) FROM <db>.<table>` on <wg> and read its results (4). Confirm with ListObjectsV2 that 'athena-results/<QueryExecutionId>.csv' now exists in <b>. Then run `SELECT "$path", count(*) FROM <db>.<table> GROUP BY 1 ORDER BY 1`: the emulator returns only the 's3://<b>/log.gz' group; AWS also returns a group whose $path is the first query's result object under 's3://<b>/athena-results/'.

Cause 2, by hand: deploy the CloudFormation template at the check's deployments/athena-csv-estate/template.json against the emulator and walk the seven phases, logging each call with a timestamp; observe which Athena/Glue/S3/CloudFormation call or polled state does not settle, which is what exhausts the 60s budget.
