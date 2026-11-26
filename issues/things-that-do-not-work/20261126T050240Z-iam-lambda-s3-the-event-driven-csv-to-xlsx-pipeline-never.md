Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam__lambda__s3.e2e-csv-to-xlsx-pipeline-lifecycle
Issue-Key: iam__lambda__s3.e2e-csv-to-xlsx-pipeline-lifecycle|_watch_absent#9|Timeout|return found
Issue-Commit: 559be37fa4dedada83c481a89b0cf045655cee0c

# The event-driven CSV-to-XLSX pipeline never completes: an s3:ObjectCreated:* notification on a deployed bucket produces no Lambda invocation, so the check never reaches an assertion

> _Note: this issue was identified against the code base at commit `559be37fa4dedada83c481a89b0cf045655cee0c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
1 cause in this issue.

Cause 1 of 1: an S3 bucket notification whose destination is a Lambda function is accepted and reads back, but an object created under the configured prefix/suffix never results in an invocation of that function, so the end-to-end pipeline never produces its output objects and the check runs to the harness budget without ever reaching an assertion.

AWS: S3 bucket notifications are modelled by PutBucketNotificationConfiguration (Smithy: PUT /{Bucket}?notification, required members Bucket and NotificationConfiguration httpPayload, optional x-amz-skip-destination-validation; success is HTTP 200 with an empty body) and read back by GetBucketNotificationConfiguration (GET /{Bucket}?notification, output member LambdaFunctionConfigurations, XML name CloudFunctionConfiguration, carrying LambdaFunctionArn, Events and the Filter prefix/suffix rules). On real AWS, once such a configuration is armed, every PutObject whose key satisfies the configured filter rules causes S3 to invoke the named Lambda function once with an event whose Records[].s3.bucket.name and Records[].s3.object.key name the created object; a key that fails the filter rules causes no invocation. The S3 User Guide states a notification configuration change "usually takes about five minutes to take effect", and events fired before it arms are dropped and not redelivered — that is why the recording's propagation prober re-uploads a NEW key each round (budget 540 s, round 15 s) instead of waiting on a single probe. Recorded behavior: against real AWS this check's configuration armed on round 11 (~330 s after PutBucketNotificationConfiguration returned) and the pipeline then produced one .xlsx per seeded raw/*.csv, each with ContentType application/vnd.openxmlformats-officedocument.spreadsheetml.sheet, with no further API call by the caller.

Emulator: the check does not complete. The replay reports only `Error: Check timed out after 60.0s` — no assertion in run() was reached and no verdict about any single response was produced. The only unbounded wait in the whole scenario is _propagate(), the loop that uploads a fresh raw/<token>.csv and polls for the handler's converted/<stem>.xlsx; every other step (the CloudFormation deploy, _seed, CreateRole, CreateFunction, AddPermission, PutBucketNotificationConfiguration, GetBucketNotificationConfiguration, GetPolicy) is a bounded sequence of control-plane calls that an emulator serves without any real propagation delay. Hypothesis: the emulator accepts and reads back the LambdaFunctionConfigurations entry (LambdaFunctionArn, Events=["s3:ObjectCreated:*"], Filter prefix raw/ suffix .csv) but never delivers an ObjectCreated event to the Lambda target, so the prober polls until the harness cuts the check off. This is the same class of gap as the already-open issue 20261029T180026Z-dynamodb-lambda-s3-s3-notifications-never-invoke-lambda-are-accepted-without.md, which does NOT carry this check's replay key and therefore does not cover it; if a single fix covers both, this check must still be re-replayed to confirm it.

Why: a client that wires S3 -> Lambda, the canonical event-driven ingest pattern, sees its notification configuration accepted and reads it back intact, and therefore believes the pipeline is live — but no object it uploads is ever processed. Nothing in the API surface reports the failure: there is no error, no 4xx, no empty read-back, just silence. Every downstream expectation (the derived output key, the object's ContentType, its bytes) never materialises, and a deploy-time integration test built on this pattern hangs rather than failing with a diagnosable error.

Fix: the S3 notification dispatch path — the component that, after a successful PutObject, evaluates the bucket's stored NotificationConfiguration (event-type match plus the Filter prefix/suffix rules) and invokes each matching LambdaFunctionConfiguration's target asynchronously with the S3 event record; and the state that PutBucketNotificationConfiguration writes, if the configuration is stored in a form the PutObject path never consults. The read-back serializer for GetBucketNotificationConfiguration is NOT implicated: the check's assertions on it were never reached, but nothing in the replay suggests it is wrong.

Traces: iam__lambda__s3.e2e-csv-to-xlsx-pipeline-lifecycle (verdict mismatch; `Error: Check timed out after 60.0s`). Note for the fixer: this check's own propagation budget is 540 s, so re-replay it with a budget that allows the propagation loop to run to completion — a 60 s cut cannot distinguish "no delivery" from "not yet delivered".

## Evidence
Expected (real AWS, recorded): after PutBucketNotificationConfiguration returns 200 for a LambdaFunctionConfigurations entry with LambdaFunctionArn=<converter fn>, Events=["s3:ObjectCreated:*"], Filter rules {prefix: "raw/", suffix: ".csv"}, the configuration arms (observed ~330 s / round 11 of the prober) and each PutObject of raw/<stem>.csv causes exactly one asynchronous invocation of that function; the handler then writes converted/<stem>.xlsx with ContentType application/vnd.openxmlformats-officedocument.spreadsheetml.sheet whose sheet rows equal the source CSV rows, and a key failing the filter yields no object. The check then asserts the read-back (one LambdaFunctionConfigurations entry, Events == ["s3:ObjectCreated:*"], rules == {prefix: raw/, suffix: .csv}), the function policy (one statement, Principal.Service == s3.amazonaws.com, Action == lambda:InvokeFunction, SourceArn == arn:aws:s3:::<bucket>), and teardown.

Actual (emulator): the check produced no response-level verdict at all — the replay reports `Error: Check timed out after 60.0s`. No assertion in run() was reached, no output object was observed, and the only wait in the scenario that is not bounded by a control-plane round trip is the propagation loop that polls for the handler's converted/*.xlsx output after each fresh raw/*.csv upload.

## Steps to Reproduce
1. Deploy the check's cfn-etl-bucket stack and read its ETLBucketName and output-prefix outputs.
2. Seed three objects raw/<name>.csv into the bucket.
3. CreateRole for the converter, CreateFunction for the CSV->XLSX handler, and AddPermission granting lambda:InvokeFunction to Principal Service s3.amazonaws.com with SourceArn arn:aws:s3:::<bucket>.
4. Call PutBucketNotificationConfiguration on the bucket with a single LambdaFunctionConfigurations entry: LambdaFunctionArn = the converter's ARN, Events = ["s3:ObjectCreated:*"], Filter rules prefix "raw/" and suffix ".csv". Observe 200.
5. Call GetBucketNotificationConfiguration and observe the entry reads back.
6. PutObject a NEW key raw/<token>.csv and poll ListObjectsV2/GetObject for converted/<token>.xlsx, re-uploading a fresh key every 15 s (the check's propagation prober), for up to 540 s.
7. Observe that no converted/*.xlsx object ever appears and the converter function is never invoked, so the check consumes its entire budget and is cut off (reported here at 60.0 s) without reaching any assertion. Re-run with a budget >= 540 s to see the propagation loop exhaust its own window.
