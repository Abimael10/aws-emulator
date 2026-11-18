Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected|_put_giant_object#9|Timeout|part_number, etag = fut.result()
Issue-Commit: 03a0508b3aa0858110fb1a346ba54180a1d13415

# stepfunctions: a DISTRIBUTED Map with an S3 CSV ItemReader never settles, so the 10 GB ItemReader input-file quota (States.ItemReaderFailed) is never observed

> _Note: this issue was identified against the code base at commit `03a0508b3aa0858110fb1a346ba54180a1d13415`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
One net-new root cause behind 1 failing check.

Cause 1 of 1: an execution of a STANDARD state machine whose single state is a Map with ProcessorConfig.Mode=DISTRIBUTED / ExecutionType=EXPRESS and an ItemReader of arn:aws:states:::s3:getObject (InputType=CSV, CSVHeaderLocation=GIVEN, CSVHeaders) makes no observable terminal progress: the check that exercises it is killed at the suite's budget ('Check timed out after 60.0s') with no response captured, so neither the small-CSV positive control nor the oversized-file rejection is ever reached. Failing check: stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected.

AWS: a Distributed Map whose ItemReader points at a small CSV object reads every data row, runs one child execution per row and ends the parent execution SUCCEEDED; DescribeMapRun for the run's single Map Run then reports itemCounts.total == itemCounts.succeeded == the number of data rows and itemCounts.failed == 0 (Smithy, com-amazonaws-sfn-awsstepfunctions: DescribeMapRun -> {mapRunArn, executionArn, status: MapRunStatus, maxConcurrency, toleratedFailurePercentage, toleratedFailureCount, itemCounts: MapRunItemCounts, executionCounts} — all required; DescribeExecution -> {status: ExecutionStatus, error: SensitiveError (max 256), cause: SensitiveCause (max 32768)}). When the SAME machine is pointed at an S3 object past the documented 10 GB 'Maximum input file size for ItemReader' quota (https://docs.aws.amazon.com/step-functions/latest/dg/service-quotas.html), AWS refuses to read it: DescribeExecution reports status='FAILED' with error='States.ItemReaderFailed' — the fixed ASL error name is the requirement; the accompanying cause is prose naming the reader/bucket/key, whose bucket and key are values taken from the request — and DescribeMapRun reports itemCounts total=succeeded=failed=0, because no item was ever read. The ItemReader ASL members themselves are recorded behavior only (they are not Smithy-modelled); the quota and the States.ItemReaderFailed name are AWS-documented.

Emulator: nothing is returned at all. The replay reports '[mismatch] ... Error: Check timed out after 60.0s' — no DescribeExecution status, no error code, no cause, and no DescribeMapRun result was produced. The run neither succeeds on the in-quota control nor fails with States.ItemReaderFailed on the oversized object; it simply does not settle inside the per-check budget, the same non-terminal symptom already recorded for Distributed Map runs in open issue 20261106T214711Z-stepfunctions-4-net-new-causes-resultwriter-refused-as.md (its Cause 2 — a 1000-state-transition emulator cap applied to DISTRIBUTED parents — and Cause 3 — a zero-tolerance map run that never terminates). That issue does NOT carry this check's replay key and names no ItemReader quota, so it does not cover this failure. No issue matching 'distributed-map' or 'itemreader' is open.

Why: a client relying on AWS's behavior cannot use Distributed Map over an S3 ItemReader at all against the emulator. DescribeExecution never leaves RUNNING, so any poll loop waiting for a terminal ExecutionStatus hangs until its own deadline; DescribeMapRun's required itemCounts are never observable; and the 10 GB input-file guard rail is invisible — code that expects States.ItemReaderFailed for an oversized source (and expects zero items counted for it) sees a run that appears to still be working, so an operator reading the emulator would conclude an oversized source is accepted.

Fix: the sfn service crate's Map-state handler and its map-run state — drive a DISTRIBUTED Map Run with an arn:aws:states:::s3:getObject ItemReader to a terminal state (reading the CSV with CSVHeaderLocation=GIVEN/CSVHeaders and recording itemCounts), and add the ItemReader input-file-size check that fails the execution with error States.ItemReaderFailed and itemCounts all zero when the named object exceeds the 10 GB quota, instead of leaving the execution non-terminal.

Traces: stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected (the only failing check in this sweep not already owned by an open issue).

## Evidence
Expected (AWS): (1) positive control — a 3-data-row CSV read by the ItemReader, DescribeExecution status='SUCCEEDED', DescribeMapRun itemCounts (total, succeeded, failed) == (3, 3, 0); (2) the one changed input — the same machine over an S3 object larger than 10 GB, DescribeExecution status='FAILED', error='States.ItemReaderFailed' (fixed ASL error name; the cause's bucket/key are request values), DescribeMapRun itemCounts (total, succeeded, failed) == (0, 0, 0).

Actual (emulator): no response of any kind — replay verdict for stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected is 'Error: Check timed out after 60.0s'. The execution never reaches any of SUCCEEDED/FAILED/TIMED_OUT/ABORTED within the budget, so the status, the error code and the cause are all unobservable. ListMapRuns/DescribeMapRun are consequently never answered for the run.

Note on values: the bucket name, object keys, role name, state-machine name and execution name in the recording are per-run values and are not part of the requirement; the requirements are the terminal ExecutionStatus values, the error code 'States.ItemReaderFailed', and the all-zero itemCounts for the rejected run.

## Steps to Reproduce
1. CreateBucket; PutObject a 3-row CSV (control.csv) and assemble a second object (giant.csv) larger than 10 GB (e.g. PutObject one 5 MiB CSV chunk, then CreateMultipartUpload + ~2200 UploadPartCopy of that chunk + CompleteMultipartUpload; HeadObject to confirm ContentLength > 10 GiB).
2. CreateRole trusted by states.amazonaws.com with s3:GetObject on the bucket and states:StartExecution/DescribeExecution/StopExecution.
3. CreateStateMachine (type=STANDARD) with one state: {"Type":"Map","MaxConcurrency":4,"ItemReader":{"Resource":"arn:aws:states:::s3:getObject","ReaderConfig":{"InputType":"CSV","CSVHeaderLocation":"GIVEN","CSVHeaders":["a","b","c","d"]},"Parameters":{"Bucket.$":"$.bucket","Key.$":"$.key"}},"ItemProcessor":{"ProcessorConfig":{"Mode":"DISTRIBUTED","ExecutionType":"EXPRESS"},"StartAt":"P","States":{"P":{"Type":"Pass","End":true}}},"End":true}.
4. StartExecution with input {"bucket":<bucket>,"key":"control.csv"} and poll DescribeExecution. Expected SUCCEEDED, then DescribeMapRun on the single ListMapRuns entry showing itemCounts (3,3,0). Observed: the execution never becomes terminal.
5. StartExecution with input {"bucket":<bucket>,"key":"giant.csv"} and poll DescribeExecution. Expected FAILED with error='States.ItemReaderFailed' and DescribeMapRun itemCounts (0,0,0). Observed: the execution never becomes terminal; the check is killed at the 60 s budget.
