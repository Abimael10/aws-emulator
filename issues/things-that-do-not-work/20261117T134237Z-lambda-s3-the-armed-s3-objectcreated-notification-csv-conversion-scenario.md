Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix
Issue-Key: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix|_watch_absent#26|Timeout|return found
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# The armed S3 ObjectCreated notification CSV-conversion scenario never completes within the 60 s budget

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Cause 1 of 1: the end-to-end S3-notification-to-Lambda conversion scenario never reaches a verdict — the whole check is abandoned at the 60 s ceiling.

AWS: recorded behavior only for the end-to-end timing (the suite reports no response to compare). The shape AWS serves is modelled and documented: s3 PutBucketNotificationConfiguration with a LambdaFunctionConfigurations entry for `s3:ObjectCreated:*` and a suffix filter arms the bucket, and a PutObject matching that filter invokes the function promptly (seconds), while an object whose suffix differs only in case does NOT match — the suffix filter is case-sensitive, so an uppercase `.CSV` is suppressed while `.csv` fires.

Emulator: the check is abandoned at the 60 s ceiling. The scenario — arm the notification, put a matching CSV, observe the converted output, put an uppercase-suffixed twin and observe it suppressed — produces no verdict at all inside the budget, so either the notification never fires or some leg of the scenario blocks indefinitely.

Why: S3-event-driven Lambda is the most common serverless trigger there is. A client cannot observe whether an object arriving in a bucket invokes its function, nor whether the case-sensitive suffix filter suppresses the twin; the run hangs rather than answering, so there is nothing for a caller to time out on or retry against.

Fix: the S3 event-notification dispatch path — the ObjectCreated matcher (including case-sensitive suffix/prefix filtering) and the Lambda invocation it issues — and whichever leg of the arm/put/observe sequence is not converging.

Traces: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix

## Evidence
Expected: the `.csv` PutObject invokes the function within seconds and the converted object appears; the `.CSV` twin is suppressed by the case-sensitive suffix filter.
Actual, verbatim:
  Error: Check timed out after 60.0s
No assertion was ever reached — the check produced no status, body or error from the emulator inside the budget.

## Steps to Reproduce
1. CreateFunction with a handler that reads the notified object and writes a converted copy.
2. lambda AddPermission for principal s3.amazonaws.com with the bucket as SourceArn.
3. s3 PutBucketNotificationConfiguration with a LambdaFunctionConfigurations entry on `s3:ObjectCreated:*` filtered to suffix `.csv`.
4. PutObject `<key>.csv`, then PutObject `<key>.CSV`.
5. Observe: the check never reaches a verdict within 60 s. Expected the `.csv` put to invoke the function and produce the converted object, and the `.CSV` put to be suppressed by the case-sensitive suffix filter.
