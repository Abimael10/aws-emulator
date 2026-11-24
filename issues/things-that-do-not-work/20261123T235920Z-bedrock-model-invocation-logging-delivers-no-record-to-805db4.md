Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.delete-model-invocation-logging-withholds-further-records
Issue-Key: bedrock.delete-model-invocation-logging-withholds-further-records|run#29|Timeout|time.sleep(_POLL_S)
Issue-Commit: dc6d632c08aad82c0d642cbd411db86477f8bbd7

# bedrock: model invocation logging delivers no record to the configured s3Config destination, so the delete-withholds-further-records scenario never reaches a verdict and is killed at the 60 s cap

> _Note: this issue was identified against the code base at commit `dc6d632c08aad82c0d642cbd411db86477f8bbd7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutModelInvocationLoggingConfiguration

## Description
This issue carries 1 root cause grounded in 1 failing check under the suite's `bedrock` service heading. It was the only failing check in this replay that the suite did not mark "ALREADY FILED" or "OWNED by" — every other bedrock / bedrock-runtime failure was owned by an existing open issue and was skipped.

The open issue 20261109T011501Z-bedrock-model-invocation-logging-delivers-no-record-to.md describes this same underlying defect and even names this check in its Issue-Trace-Ids, but it carries this check under the replay key `bedrock.delete-model-invocation-logging-withholds-further-records|_await_record#19|AssertionError|raise AssertionError(`. In this replay the check no longer reaches that assertion at all: the harness budget is 60 s while leg A's delivery wait is 110 s, so the check is killed by the cap and reports a bare `Check timed out after 60.0s` with no assertion text. That key is therefore NOT carried by any open issue — this issue exists to add it, not to describe a second defect. The emulator answered the control-plane calls here (Put/Get/Delete of the logging configuration all succeed), so bedrock is up and answering wrongly, not unbootstrapped.

---
Cause 1 of 1: an accepted model-invocation-logging configuration is stored and echoed back, but no invocation ever produces a log record at the configured S3 destination, so the "delivery stops after DeleteModelInvocationLoggingConfiguration" guarantee has no positive control to contrast against and the whole scenario hangs out its delivery wait.

AWS: Smithy (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json) models PutModelInvocationLoggingConfiguration PUT /logging/modelinvocations -> 200 with required input member `loggingConfig` (LoggingConfig), GetModelInvocationLoggingConfiguration GET /logging/modelinvocations -> 200 with output member `loggingConfig` (LoggingConfig), and DeleteModelInvocationLoggingConfiguration DELETE /logging/modelinvocations -> 200; all three declare AccessDeniedException (403), ThrottlingException (429), InternalServerException (500), and Put additionally ValidationException (400). Record delivery itself is NOT modeled, so recorded behavior only: with `loggingConfig.s3Config = {bucketName, keyPrefix}` and `textDataDeliveryEnabled = true`, every subsequent bedrock-runtime inference writes a JSON-lines object under that prefix in that bucket within the delivery window, and each record carries the invocation's own `requestId` (the RequestId of the Converse/InvokeModel response), its `modelId`, its `operation` (e.g. the value `Converse` for a Converse call), and — when text delivery is enabled — the prompt and completion bodies. After DeleteModelInvocationLoggingConfiguration, GetModelInvocationLoggingConfiguration reports no configuration and further invocations produce NO new record, and re-Putting the configuration resumes delivery for later invocations WITHOUT retroactively flushing the invocation made while logging was deleted. Bucket name, key prefix, requestId, nonce and model id are all per-request values taken from the request and the emulator's state, never literals.

Emulator: PutModelInvocationLoggingConfiguration is accepted and GetModelInvocationLoggingConfiguration echoes the stored configuration (those assertions pass, and the Converse calls themselves return 200), but zero objects ever appear under the configured prefix. The check's leg-A positive control polls ListObjectsV2/GetObject under s3://<bucket>/<keyPrefix> for up to 110 s waiting for a record carrying the Converse response's requestId; nothing is ever delivered, so the poll consumes the entire harness budget and the check is killed with `Check timed out after 60.0s`. Because delivery never happens, neither the post-delete absence leg nor the leg-C resume leg is ever reached.

Why: a client cannot distinguish "invocation logging is configured" from "invocation logging works" against the emulator. An operator or audit pipeline that sees the configuration echoed back by GetModelInvocationLoggingConfiguration concludes records are being written to its bucket; nothing is. The stop-after-delete guarantee (the security-relevant half: no prompt/completion data is written once logging is switched off) and the resume-on-re-put behaviour are both unobservable, and prompt/completion review and token accounting built on these records receive nothing.

Fix: an invocation-logging delivery path — a post-inference hook in the bedrock-runtime inference handlers that emits the record, plus the bedrock control plane's model-invocation-logging component that resolves the stored s3Config (bucketName/keyPrefix), writes the JSON-lines object carrying requestId/modelId/operation, honours textDataDeliveryEnabled / imageDataDeliveryEnabled / embeddingDataDeliveryEnabled, stops emitting as soon as the configuration is deleted, and does not buffer-and-flush withheld records when the configuration is re-put.

Traces: bedrock.delete-model-invocation-logging-withholds-further-records.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 101/156 passed (64.7%); service heading `bedrock` 47/57 (82%), 10 failed. bedrock is bootstrapped (get_bootstrap_status().bootstrapped contains service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice and `pending` is empty).

bedrock.delete-model-invocation-logging-withholds-further-records [mismatch]
  Setup that succeeded: s3.create_bucket + put_bucket_policy granting bedrock.amazonaws.com s3:PutObject on the bucket; bedrock.put_model_invocation_logging_configuration(loggingConfig={'s3Config': {'bucketName': <bucket>, 'keyPrefix': 'sim2real'}, 'textDataDeliveryEnabled': True, 'imageDataDeliveryEnabled': False, 'embeddingDataDeliveryEnabled': False}) -> 200; bedrock.get_model_invocation_logging_configuration() -> 200 echoing that bucketName.
  Then: bedrock-runtime Converse on amazon.nova-lite-v1:0 -> 200 with a RequestId, and the check polled s3://<bucket>/sim2real every 2 s for a record whose `requestId` equals that RequestId (leg-A positive control, 110 s budget).
  Expected (AWS): a JSON-lines object appears under the configured prefix within the delivery window, carrying that invocation's own requestId, `operation` == 'Converse', and the prompt nonce; the check then deletes the configuration, holds the absence, and re-enables to observe redelivery.
  Actual: no object is ever delivered — the leg-A poll never terminates and the check is killed by the harness: `Error: Check timed out after 60.0s`. No assertion text is produced because the harness cap (60 s) is shorter than the check's own leg-A delivery timeout (110 s).
  Note: the previously filed open issue 20261109T011501Z-bedrock-model-invocation-logging-delivers-no-record-to.md recorded this check under an AssertionError key at _await_record#19 (`leg-A-positive-control: no invocation log record carrying requestId ... appeared under s3://<bucket>/sim2real within 110.0s; 0 record(s) present, requestIds []`). Under the current budget the same absent delivery surfaces as a bare timeout instead, so the suite no longer matches it to that issue.

Model evidence: PutModelInvocationLoggingConfiguration PUT /logging/modelinvocations -> 200 (required `loggingConfig`: LoggingConfig; errors AccessDeniedException 403 / ValidationException 400 / ThrottlingException 429 / InternalServerException 500); GetModelInvocationLoggingConfiguration GET -> 200 with `loggingConfig`; DeleteModelInvocationLoggingConfiguration DELETE -> 200. Record delivery and record contents are not modeled (recorded behavior only).

## Steps to Reproduce
1. git_clone, then run_behavioral_suite so the emulator is built and booted.
2. Re-replay exactly the check id in trace_ids: bedrock.delete-model-invocation-logging-withholds-further-records.
3. Create an S3 bucket in the suite's own region and attach a bucket policy allowing Principal {"Service": "bedrock.amazonaws.com"} s3:PutObject on arn:aws:s3:::<bucket>/*.
4. Call bedrock.put_model_invocation_logging_configuration(loggingConfig={'s3Config': {'bucketName': <bucket>, 'keyPrefix': 'sim2real'}, 'textDataDeliveryEnabled': True, 'imageDataDeliveryEnabled': False, 'embeddingDataDeliveryEnabled': False}) and confirm bedrock.get_model_invocation_logging_configuration() echoes it.
5. Issue a bedrock-runtime Converse call and note its response ResponseMetadata.RequestId; poll ListObjectsV2/GetObject under s3://<bucket>/sim2real. Expected: a JSON-lines record whose `requestId` equals that RequestId, with `operation` == 'Converse' and the prompt text present. Observed: zero objects, indefinitely — this is where the check's whole budget goes.
6. Once step 5 delivers: call bedrock.delete_model_invocation_logging_configuration(), confirm get_model_invocation_logging_configuration now reports no configuration (ResourceNotFoundException / absent loggingConfig), issue another Converse, and confirm NO new requestId appears under the prefix for the hold period.
7. Re-put the same configuration, issue a third Converse, and confirm its record is delivered while the invocation made in step 6 is still absent (withheld, not buffered-and-flushed).
