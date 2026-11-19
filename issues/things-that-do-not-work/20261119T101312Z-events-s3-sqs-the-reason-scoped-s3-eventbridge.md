Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule
Issue-Key: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule|_drain#36|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: a46c7b329d14c33881a40e10b99591f6626f6ae1

# events__s3__sqs: the reason-scoped S3 EventBridge routing check (PutObject vs POST Object) reaches no verdict — the whole 60 s budget is exhausted inside its bounded delivery polls

> _Note: this issue was identified against the code base at commit `a46c7b329d14c33881a40e10b99591f6626f6ae1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: an `Object Created` event whose `detail.reason` names the creating API is never delivered to a reason-scoped EventBridge rule, so the whole check stalls and is killed at the harness budget.

AWS: a bucket with `NotificationConfiguration.EventBridgeConfiguration` enabled publishes every object creation to the account's default event bus as `source: "aws.s3"`, `detail-type: "Object Created"`, with `detail.bucket.name` holding the bucket and `detail.reason` holding the name of the API that created the object — the documented literals are `PutObject`, `POST Object`, `CopyObject` and `CompleteMultipartUpload` (Amazon S3 User Guide, "Using EventBridge"). Two rules on the same bucket that differ only in `detail.reason` are therefore disjoint: a plain `PutObject` matches the rule pinning `["PutObject"]` and is withheld from the rule pinning `["POST Object"]`, and a browser form upload (POST Object, which answers HTTP 201 when `success_action_status=201` is in the signed policy) matches only the `["POST Object"]` rule. Neither S3's EventBridge notification payload nor POST Object is expressed in the Smithy model (`aws.protocols#restXml` service `com.amazonaws.s3#AmazonS3` models neither the notification event shape nor a POST-form operation), so this is documented/recorded behavior only; `PutBucketNotificationConfiguration` / `GetBucketNotificationConfiguration` and their `EventBridgeConfiguration` member ARE modeled and are what arms the path.

Emulator: the check never reaches a verdict. It is killed by the harness with `Check timed out after 60.0s`, i.e. it spent its entire budget inside its own bounded polls (each declared with `timeout_s=180`, and the check declares a 900 s worst case) without any of them being satisfied, and without ever raising one of its own assertions. Because the failure is a budget exhaustion rather than an assertion, the suite output does not name the stalling poll; the only polls that can consume the budget are the three delivery polls, so at least one of (a) the positive control — the `PutObject`-scoped rule receiving the `Object Created` event for a plain `put_object`, (b) the `POST Object`-scoped rule receiving the form upload's event with `detail.reason == "POST Object"`, or (c) the encore `PutObject` delivery after the form upload — never produces a message on its queue. Two already-open issues bear on the same path but neither carries this check: 20261028T105230Z-s3-8-net-new-root-causes-in-the.md records that S3 POST Object is not routed at all (a conforming multipart form POST to the bucket root is answered HTTP 404 with a non-AWS JSON body `{"__type":"UnknownService",...}` instead of 201), and 20261113T151530Z-events-s3-sqs-per-event-type-s3-queue.md records that per-event-type S3 *queue* notification routing never settles. A 404 at the POST step would surface as this check's own `assert status == 201`, not as a timeout, so the budget is being burned in a reason-scoped delivery poll — the `detail.reason` discriminator on the EventBridge path is the behavior that is missing or wrong.

Why: a client that relies on `detail.reason` to separate server-side PUT/copy/multipart writes from browser direct uploads gets no verdict at all against the emulator: the reason-scoped rule it deploys either never fires or fires for the wrong creations, and the symptom a developer sees is a scenario that hangs rather than an AWS-shaped answer. Every negative-delivery guarantee built on `detail.reason` (fan-out isolation, per-upload-path processing, abuse controls on form uploads) is unverifiable.

Fix: the s3 crate's EventBridge notification emitter — the component that builds the `Object Created` detail when a bucket carries `EventBridgeConfiguration` — must populate `detail.reason` from the creating API for every creation path (`PutObject`, `POST Object`, `CopyObject`, `CompleteMultipartUpload`) and publish it to the default bus so EventBridge's pattern matcher can discriminate on it; the request router's service attribution must additionally dispatch a bucket-root `multipart/form-data` POST to a POST Object handler so that creation path exists at all and emits its own event. The runner-side symptom (budget exhausted instead of an assertion) is secondary to that missing discriminator.

Traces: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule.

## Evidence
Expected (AWS): the `PutObject`-scoped rule (pattern `{"source":["aws.s3"],"detail-type":["Object Created"],"detail":{"bucket":{"name":[<bucket>]},"reason":["PutObject"]}}`) receives the event for a plain `put_object` with `detail.reason == "PutObject"`; the signed browser form POST answers HTTP 201 and `head_object` reports the uploaded `ContentLength`; the `POST Object`-scoped rule receives that creation with `detail.reason == "POST Object"`; and the `PutObject` queue stays empty (and reports depth 0) for the form upload while a later `put_object` still reaches it.

Actual (emulator): no leg is ever observed. The check is terminated by the harness with `Error: Check timed out after 60.0s` — every bounded poll (`timeout_s=180`, interval 0, draining with `WaitTimeSeconds=5`) is still unsatisfied when the budget runs out, so the check raises none of its own assertions and no status/error code is produced anywhere. The emulator's related recorded behavior on this path, from issue 20261028T105230Z-s3-8-net-new-root-causes-in-the.md, is that a conforming POST Object form is answered HTTP 404 with body `{"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"}` where AWS answers 201.

## Steps to Reproduce
1. Replay the behavioral suite and re-run the check id in trace_ids.
2. Manually: create a bucket and arm it with `put_bucket_notification_configuration(Bucket=b, NotificationConfiguration={"EventBridgeConfiguration": {}})`; confirm `get_bucket_notification_configuration` reports `EventBridgeConfiguration`.
3. Create two SQS queues, each with a queue policy allowing `sqs:SendMessage` to `events.amazonaws.com` for `arn:*:events:<region>:<account>:rule/*`.
4. `put_rule` twice on the default bus with patterns identical except `detail.reason`: `["PutObject"]` -> queue A, `["POST Object"]` -> queue B; `put_targets` each to its queue and assert `FailedEntryCount == 0`.
5. `put_object` a key under the bucket and poll queue A: observe whether any `Object Created` message arrives and what `detail.reason` it carries.
6. Build a SigV4-signed POST Object policy for the same bucket and POST `multipart/form-data` (fields `x-amz-algorithm`, `x-amz-credential`, `x-amz-date`, `success_action_status=201`, `policy`, `x-amz-signature`, then `key`, then `file`) to the bucket root; observe the HTTP status, then poll queue B for a message with `detail.reason == "POST Object"` and poll queue A to confirm it stays empty.
