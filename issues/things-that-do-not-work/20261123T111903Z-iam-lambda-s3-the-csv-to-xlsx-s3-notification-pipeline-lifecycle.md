Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__s3.e2e-csv-to-xlsx-pipeline-lifecycle
Issue-Key: iam__lambda__s3.e2e-csv-to-xlsx-pipeline-lifecycle|_watch_absent#9|Timeout|return found
Issue-Commit: f3fcdc08bdb09b2741d5bd13ee7a493b1992f806

# The CSV-to-XLSX S3 notification pipeline lifecycle never reaches a verdict: no ObjectCreated delivery is ever observed

> _Note: this issue was identified against the code base at commit `f3fcdc08bdb09b2741d5bd13ee7a493b1992f806`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
One net-new root cause.

Cause 1 of 1: the deploy/serve/destroy lifecycle driven by a bucket's ObjectCreated notification into a Lambda converter never progresses — the replay produced no phase verdict at all before it was cut off. Failing check: iam__lambda__s3.e2e-csv-to-xlsx-pipeline-lifecycle.

AWS: recorded behavior only. The recording shows PutBucketNotificationConfiguration returning and the configuration then ARMING (measured on real AWS: on probe round 11, about 330 s later, consistent with the S3 User Guide's "usually takes about five minutes to take effect"); from then on every ObjectCreated:Put of a .csv key invokes the configured function, which writes the converted workbook object stamped with content type application/vnd.openxmlformats-officedocument.spreadsheetml.sheet. Events that fire before the configuration arms are dropped and never redelivered, so the check re-uploads a NEW key each round rather than waiting on one object.

Emulator: nothing in the lifecycle is observed to happen. The replay ends with "Check timed out after 60.0s" and no phase (deploy, setup, pre_invoke, solve, post_invoke, cleanup, destroy) recorded a result, so not a single ObjectCreated delivery into the function was seen. Hypothesis: the emulator accepts PutBucketNotificationConfiguration but never delivers the ObjectCreated event to the Lambda target, so the arming probe can never succeed and the pipeline cannot advance; open issue 20261029T180026Z-dynamodb-lambda-s3-s3-notifications-never-invoke-lambda-are-accepted-without.md reports that behavior for a DIFFERENT check and does not carry this check's replay key, so this lifecycle is not covered by it.

Why: a client that wires a bucket to a function — the canonical S3-triggered ETL shape — gets a 200 from PutBucketNotificationConfiguration and then silence: objects land, no invocation happens, no converted output is ever written, and no error is surfaced anywhere for the client to act on.

Fix: S3's bucket-notification delivery path — the component that evaluates a bucket's LambdaFunctionConfigurations against an ObjectCreated event and issues the lambda invoke (including the arming/propagation of a freshly put configuration) — and the Lambda invoke it drives.

Traces: iam__lambda__s3.e2e-csv-to-xlsx-pipeline-lifecycle (verdict: mismatch; detail "Check timed out after 60.0s", no phase graded).

## Evidence
Expected (real AWS, measured): after PutBucketNotificationConfiguration on the ETL bucket, a .csv key uploaded in a later probe round triggers the converter function, which writes the .xlsx object with Content-Type application/vnd.openxmlformats-officedocument.spreadsheetml.sheet; the configuration armed about 330 s after the call returned, well inside the check's 540 s propagation budget of 15 s rounds.

Actual (emulator): no phase of the lifecycle completed and no converted object or invocation was observed; the replay reports only "Error: Check timed out after 60.0s".

## Steps to Reproduce
1. Deploy the ETL estate (bucket, execution role, converter function) and seed the three fixed CSV objects.
2. Call PutBucketNotificationConfiguration on the bucket with a LambdaFunctionConfigurations entry for s3:ObjectCreated:* targeting the converter function.
3. Poll by uploading a NEW .csv key every 15 s and reading back the expected .xlsx output object; on AWS the configuration arms and the output appears, while on the emulator no output object is ever produced and the lifecycle never advances past its first phase.
