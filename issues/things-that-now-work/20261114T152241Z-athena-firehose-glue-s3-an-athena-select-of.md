Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__firehose__glue__s3.firehose-interval-ceiling-900-echoed-and-size-hint-flushes-through-athena
Issue-Key: athena__firehose__glue__s3.firehose-interval-ceiling-900-echoed-and-size-hint-flushes-through-athena|run#32|AssertionError|assert state == "SUCCEEDED", f"Athena query {qid} ended {state!r}: {reason!r}"
Issue-Commit: d86ba179c5d8f834b6ff97eae2a4be90662aba63

# athena__firehose__glue__s3: an Athena SELECT of the "$path" hidden column now settles FAILED with COLUMN_NOT_FOUND, so a Firehose-delivered object's rows can never be attributed to it

> _Note: this issue was identified against the code base at commit `d86ba179c5d8f834b6ff97eae2a4be90662aba63`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartQueryExecution

## Description
Cause 1 of 1: an Athena `SELECT "$path", count(*) FROM "<db>"."<table>" GROUP BY 1` over a Glue EXTERNAL_TABLE whose StorageDescriptor.Location is a Firehose delivery prefix no longer completes at all — the query execution reaches terminal state FAILED and reports that the `$path` column cannot be resolved. Failing check: athena__firehose__glue__s3.firehose-interval-ceiling-900-echoed-and-size-hint-flushes-through-athena.

AWS: recorded behavior only. `$path` is a hidden per-row metadata column the Athena/Trino engine exposes on Hive-table scans; it is engine behavior and is not described by the Athena Smithy model (the model describes only the envelope — QueryExecutionStatus.State/StateChangeReason/AthenaError on GetQueryExecution and the ResultSet on GetQueryResults; Smithy: com.amazonaws.athena#QueryExecutionStatus members State, StateChangeReason, AthenaError; com.amazonaws.athena#AthenaError members ErrorCategory, ErrorType, Retryable, ErrorMessage). The recording shows that a SELECT projecting `"$path"` over such a table RESOLVES that column and the query settles SUCCEEDED (State=SUCCEEDED, no StateChangeReason, no AthenaError), returning one row per scanned source object: the first column is that object's full `s3://<bucket>/<key>` URI — the same bucket+key ListObjectsV2 reports, with no query string and no variant spelling — and the second column is the number of records Athena read from it (for newline-delimited JSON, one row per non-empty line). The requirement is that template: for every object present under the table location there is a row whose `$path` is that object's s3:// URI and whose count is that object's line count. Object names, keys and the particular counts seen in any one run are per-run values that fill the template; they are not part of it.

Emulator: the query settles FAILED and reports StateChangeReason "COLUMN_NOT_FOUND: line 1:8: Column '$path' cannot be resolved". The fixed text of that diagnostic is Trino's `COLUMN_NOT_FOUND: line <line>:<col>: Column '<name>' cannot be resolved`; the variable parts are the source position and the column name taken from the submitted statement — i.e. the emulator's engine emits a correctly-shaped unresolved-column diagnostic because it does not implement the hidden metadata columns at all, rather than resolving `$path` against the scanned object. Everything earlier in the same check passes, so this is not the Firehose or S3 side: CreateDeliveryStream refuses IntervalInSeconds=901 with ValidationException and leaves no stream behind, the 900 s stream reaches ACTIVE, DescribeDeliveryStream echoes BufferingHints {SizeInMBs: 1, IntervalInSeconds: 900}, and the size-hint-triggered object is delivered to S3 under the stream prefix with the expected ids and readable JSON lines.

This is NOT covered by the already-open issue 20261111T025824Z-athena-firehose-glue-s3-athena-s-path-per.md. That issue records the emulator answering SUCCEEDED with a result set that merely omits a row for the delivered object (per-path lookup yields None); here the projection is rejected outright and the whole query ends FAILED, so the observable contract that breaks is different (terminal state and StateChangeReason, not result-set contents) and that issue's replay key does not carry this failure.

Why: a client relying on AWS's behavior cannot audit a Firehose -> S3 -> Glue -> Athena ingest pipeline. `$path` is the standard way to reconcile delivered objects against queried rows, locate the object behind a bad row, and de-duplicate a re-delivered object. Against the emulator any statement that projects, groups by or filters on `$path` (and, by the same gap, `$file_size` / `$file_modified_time`) fails instead of returning data, so an application or dashboard built on that column is wholly inoperable rather than merely incomplete — and because the failure arrives as a terminal FAILED execution, retry logic treats a permanently unsupported query as a transient query failure.

Fix: the Athena query-execution data plane for Glue-catalog tables — the analyzer/planner that resolves column references for a Hive table scan must recognise the hidden metadata columns and bind `$path` to the scanned object's `s3://<bucket>/<key>` URI, and the scan must enumerate objects under the table location's sub-prefixes so that one group per source object is produced.

Traces: athena__firehose__glue__s3.firehose-interval-ceiling-900-echoed-and-size-hint-flushes-through-athena.

## Evidence
Check athena__firehose__glue__s3.firehose-interval-ceiling-900-echoed-and-size-hint-flushes-through-athena, verdict: mismatch.

Expected (AWS, recorded): `SELECT "$path", count(*) FROM "<db>"."<table>" GROUP BY 1` settles with QueryExecution.Status.State = SUCCEEDED and GetQueryResults returns one row per object under the table location, first column = that object's `s3://<bucket>/<key>` URI, second column = its number of JSON lines.

Actual (emulator): the execution settles FAILED and the status carries the unresolved-column diagnostic:

  AssertionError: Athena query <query-execution-id> ended 'FAILED': "COLUMN_NOT_FOUND: line 1:8: Column '$path' cannot be resolved"

(raised at check line 409: `assert state == "SUCCEEDED", f"Athena query {qid} ended {state!r}: {reason!r}"`). Fixed text of the diverging message: `COLUMN_NOT_FOUND: line <line>:<column>: Column '<name>' cannot be resolved`; variable parts `<line>`/`<column>`/`<name>` come from the submitted statement (here 1:8 and `$path`). The query-execution id is a per-run value.

All preceding assertions in the same check pass, establishing that the data is in S3 and readable and that the Firehose side is correct: IntervalInSeconds=901 refused with ValidationException ("1 validation error detected: Value at 'extendedS3DestinationConfiguration.bufferingHints.intervalInSeconds' failed to satisfy constraint: Member must have value less than or equal to 900") and no stream created, the 900 s stream ACTIVE, DescribeDeliveryStream echoing BufferingHints {SizeInMBs: 1, IntervalInSeconds: 900}, and the delivered object holding only ids that were put.

## Steps to Reproduce
1. Create an S3 bucket and an IAM role trusted by firehose.amazonaws.com with s3:PutObject/GetObject/ListBucket/GetBucketLocation/ListBucketMultipartUploads/AbortMultipartUpload on it.
2. Create an Athena workgroup with ResultConfiguration.OutputLocation = s3://<bucket>/athena-results/.
3. Create a Glue database and an EXTERNAL_TABLE with Parameters {classification: json, EXTERNAL: TRUE}, one int column `id`, StorageDescriptor.Location = s3://<bucket>/<stream-prefix>/, InputFormat org.apache.hadoop.mapred.TextInputFormat, OutputFormat org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat, SerdeInfo org.openx.data.jsonserde.JsonSerDe.
4. CreateDeliveryStream (DirectPut, ExtendedS3DestinationConfiguration) with Prefix=<stream-prefix>/, RoleARN of the role, BufferingHints {SizeInMBs: 1, IntervalInSeconds: 900}; poll DescribeDeliveryStream until ACTIVE.
5. PutRecordBatch enough newline-terminated JSON records ({"id": n, "pad": "x"*8000}) to cross the 1 MiB size hint, then poll ListObjectsV2 under <stream-prefix>/ until an object appears.
6. StartQueryExecution with QueryString `SELECT "$path", count(*) FROM "<db>"."<table>" GROUP BY 1`, QueryExecutionContext.Database=<db>, WorkGroup=<workgroup>; poll GetQueryExecution to a terminal state.
7. Observe Status.State = FAILED with StateChangeReason "COLUMN_NOT_FOUND: line 1:8: Column '$path' cannot be resolved", instead of SUCCEEDED with one result row per delivered object carrying that object's s3:// URI and line count.
