Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock.delete-model-invocation-logging-withholds-further-records
Issue-Key: bedrock.delete-model-invocation-logging-withholds-further-records|run#29|Timeout|time.sleep(_POLL_S)
Issue-Commit: df6c59e1d392df4e843198fdd80c29d823f1c724

# bedrock model invocation logging still delivers no record: the stop-after-delete scenario now consumes its entire 60 s budget in leg A's positive-control delivery wait

> _Note: this issue was identified against the code base at commit `df6c59e1d392df4e843198fdd80c29d823f1c724`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutModelInvocationLoggingConfiguration

## Description
This issue carries 1 root cause grounded in 1 failing check under the suite's `bedrock` service heading.

The open issue 20261109T011501Z-bedrock-model-invocation-logging-delivers-no-record-to.md reports the same underlying defect and even names this check in its Issue-Trace-Ids, but the replay key it carries for it is `bedrock.delete-model-invocation-logging-withholds-further-records|_await_record#19|AssertionError|raise AssertionError(` — an assertion raised inside `_await_record`. On this replay the check does not reach that assertion at all: it is killed by the harness budget (`Check timed out after 60.0s`), so this failure is NOT carried by that issue's key and the replay report did not mark it OWNED. This issue exists to add that key, not to describe a second defect. Named here so a fixer can close both together.

The emulator is up and answering on this surface: CreateBucket/PutBucketPolicy, PutModelInvocationLoggingConfiguration, GetModelInvocationLoggingConfiguration and Converse all return 200 in this check (the check's own early assertions on the echoed configuration are not what fails), so this is a fidelity divergence, not an unbootstrapped service.

---
Cause 1 of 1: an accepted model-invocation-logging configuration is stored and echoed back, but no invocation ever produces a log record at the configured S3 destination, so the delete-stops-delivery guarantee can never be observed — the check's leg-A positive control polls an empty prefix until the budget is exhausted.

AWS: Smithy (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json): PutModelInvocationLoggingConfiguration PUT /logging/modelinvocations -> 200, input member `loggingConfig` (LoggingConfig, required), errors AccessDeniedException 403 / ValidationException 400 / ThrottlingException 429 / InternalServerException 500; GetModelInvocationLoggingConfiguration GET /logging/modelinvocations -> 200 with output member `loggingConfig`; DeleteModelInvocationLoggingConfiguration DELETE /logging/modelinvocations -> 200. Record DELIVERY is not modeled, so for delivery this is recorded behavior only: with `loggingConfig.s3Config = {bucketName, keyPrefix}` and `textDataDeliveryEnabled: true`, every subsequent inference writes a JSON-lines object under that prefix in that bucket within the delivery window, and each line carries that invocation's own `requestId`, its `operation` (here `Converse`), its `modelId`, and — with text delivery enabled — the prompt and completion bodies. After DeleteModelInvocationLoggingConfiguration, Get reports no configuration and further invocations produce NO new record, and re-Putting the configuration resumes delivery for later invocations WITHOUT retroactively flushing the ones made while it was deleted. Bucket name, key prefix, requestId, model id and the prompt nonce are all per-request/per-run values, never literals.

Emulator: Put is accepted, Get echoes the stored configuration (so the check's `bucketName` assertion passes), and Converse answers 200 with a RequestId — but zero objects ever appear under the configured prefix. The check's leg-A positive control (`_await_record(..., 'leg-A-positive-control', 110.0)`) polls ListObjectsV2/GetObject under `s3://<bucket>/sim2real` on a 2 s interval and never finds a record, so the check consumes the whole harness budget and is reported as `Error: Check timed out after 60.0s`. Because the positive control never lands, legs B (no new record while logging is deleted) and C (delivery resumes after re-Put, and leg B's record is still absent) are never reached.

Why: a client cannot distinguish 'logging is configured' from 'logging works'. Audit and prompt/completion-review pipelines that enable invocation logging see the configuration echoed back and conclude records are being written, then find an empty bucket; the compliance-relevant guarantee that Delete STOPS delivery (and that a re-enable does not retroactively flush withheld invocations) is unobservable, so a consumer cannot verify that invocations made with logging off were never recorded.

Fix: the invocation-logging delivery path — a post-inference hook in the bedrock-runtime inference handlers (Converse/InvokeModel and their streaming forms) that emits a record, plus the bedrock control plane's model-invocation-logging component that resolves the stored `s3Config` (bucketName/keyPrefix), writes the JSON-lines object with the invocation's requestId/operation/modelId, honours textDataDeliveryEnabled / imageDataDeliveryEnabled / embeddingDataDeliveryEnabled, and emits nothing at all once the configuration has been deleted.

Traces: bedrock.delete-model-invocation-logging-withholds-further-records.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 100/156 passed (64.1%); service heading `bedrock` 46/57 (81%), 11 failed. Every other bedrock / bedrock-runtime failure in this replay was marked OWNED by an open issue; this is the only surviving one.

bedrock.delete-model-invocation-logging-withholds-further-records [mismatch]
  Setup that SUCCEEDED (emulator answered 200): s3.create_bucket + put_bucket_tagging + put_bucket_policy; bedrock.put_model_invocation_logging_configuration(loggingConfig={'s3Config': {'bucketName': <per-run bucket>, 'keyPrefix': 'sim2real'}, 'textDataDeliveryEnabled': True, 'imageDataDeliveryEnabled': False, 'embeddingDataDeliveryEnabled': False}); bedrock.get_model_invocation_logging_configuration() echoed the same bucketName (assertion passed); bedrock-runtime.converse(modelId='amazon.nova-lite-v1:0', ...) returned 200 with a RequestId.
  Expected (AWS): within the delivery window a JSON-lines object appears under s3://<bucket>/sim2real whose record has requestId == the Converse response's RequestId, operation == 'Converse', and contains the leg-A prompt nonce.
  Actual: nothing is ever delivered. `_await_record(..., 'leg-A-positive-control', timeout_s=110.0)` polls every 2 s and never finds a record, so the check never reaches an assertion and the harness reports:
    Error: Check timed out after 60.0s
  Note on the timeout: the 60 s is the harness budget, not an AWS requirement. The ONLY unbounded-looking wait reached before it is leg A's delivery poll (legs B and C are gated behind leg A's success), and the previous replay of this same check — recorded in 20261109T011501Z-bedrock-model-invocation-logging-delivers-no-record-to.md under a 180 s budget — failed at exactly that poll with `AssertionError: leg-A-positive-control: no invocation log record carrying requestId '<per-run id>' appeared under s3://<bucket>/sim2real within 110.0s; 0 record(s) present, requestIds []`. The emulator's absent delivery is what consumes the budget.

Model evidence: PutModelInvocationLoggingConfiguration PUT /logging/modelinvocations -> 200 (loggingConfig required); GetModelInvocationLoggingConfiguration GET -> 200 with loggingConfig; DeleteModelInvocationLoggingConfiguration DELETE -> 200; on services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json. Delivery of the records themselves is not modeled (recorded behavior only).

Bootstrap gate: get_bootstrap_status() returns an empty `pending` list, and `bootstrapped` contains service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice and service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice, so bedrock is stood up and this is a fidelity finding.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite so the emulator is built and booted.
2. Re-replay exactly the check in trace_ids: bedrock.delete-model-invocation-logging-withholds-further-records.
3. Manually: create an S3 bucket in the suite's own region and attach the Bedrock log-write bucket policy (Principal Service bedrock.amazonaws.com, s3:PutObject on arn:aws:s3:::<bucket>/*).
4. bedrock.put_model_invocation_logging_configuration(loggingConfig={'s3Config': {'bucketName': <bucket>, 'keyPrefix': 'sim2real'}, 'textDataDeliveryEnabled': True, 'imageDataDeliveryEnabled': False, 'embeddingDataDeliveryEnabled': False}) and confirm get_model_invocation_logging_configuration() echoes it (this already works).
5. bedrock-runtime.converse(modelId='amazon.nova-lite-v1:0', messages=[{'role':'user','content':[{'text':'Repeat exactly this token and nothing else: <nonce>'}]}], inferenceConfig={'maxTokens':40,'temperature':0.0}) and note ResponseMetadata.RequestId.
6. Poll s3.list_objects_v2(Bucket=<bucket>, Prefix='sim2real') / get_object on each key. Expected (AWS): a JSON-lines record carrying that RequestId, operation 'Converse' and the nonce. Observed: zero objects, forever — this is the wait that exhausts the check's budget.
7. Once delivery exists, verify the rest of the scenario the timeout hides: bedrock.delete_model_invocation_logging_configuration(); confirm get_model_invocation_logging_configuration() reports no configuration; issue a second Converse and confirm NO new requestId appears under the prefix for at least 3x the latency measured in step 6; then re-Put the same configuration, issue a third Converse and confirm its record DOES arrive while the invocation made in the deleted window is still absent (i.e. withheld, not buffered).
