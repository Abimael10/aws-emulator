Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule
Issue-Key: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule|_drain#36|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: f94f81a4e4acb5f06aec5c20cf2f41781bd39454

# events__s3__sqs: no aws.s3 'Object Created' event reaches an EventBridge rule on an EventBridge-enabled bucket, so the POST Object vs PutObject reason routing reaches no verdict

> _Note: this issue was identified against the code base at commit `f94f81a4e4acb5f06aec5c20cf2f41781bd39454`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutObject

## Description
Cause 1 of 1: an EventBridge-enabled bucket's Object Created events never reach a matching EventBridge rule's SQS target, so the reason-scoped routing contract cannot be graded.

AWS: with PutBucketNotificationConfiguration {"EventBridgeConfiguration": {}} set, every object creation in the bucket is published to the default EventBridge bus as source "aws.s3", detail-type "Object Created", with detail.bucket.name = the bucket, detail.object.key = the key, and detail.reason naming the API that created the object — "PutObject" for a PUT and "POST Object" for a browser form POST (API_PostObject). A rule whose pattern pins detail.reason therefore receives only its own creation kind: the PutObject-scoped rule gets the PUT and never the form POST, and vice versa. Smithy: S3's model carries no EventBridge notification shape, so the detail-type and reason literals are recorded behavior only, taken from the recording.

Emulator: the check is cut off at the harness cap (Error: Check timed out after 60.0s) at its FIRST awaited observable — the PutObject-scoped rule's queue receiving the Object Created event for a plain PutObject on that bucket (the check's own budget for that poll is 180s). GetBucketNotificationConfiguration does report the EventBridgeConfiguration back, so the configuration is stored but no event is ever delivered. Nothing downstream — the POST Object upload's reason literal, the disjointness of the two reason-scoped rules, the withheld-but-not-broken encore — can be observed.

Why: a client that wires S3 object events through EventBridge gets a bucket that acknowledges the configuration and then publishes nothing, so no rule ever fires; and because no event carries detail.reason, a consumer cannot distinguish a PUT upload from a browser form POST, which is exactly what reason-scoped rules are for.

Fix: S3's EventBridge notification emission for object creation (the publish path that turns a PutObject / POST Object into an aws.s3 "Object Created" event on the default bus, including the detail.reason literal), and the EventBridge default-bus rule dispatch to an SQS target that it feeds.

Traces: events__s3__sqs.a-post-object-upload-is-withheld-from-a-putobject-scoped-rule.

Related but not covering this: 20261123T132533Z-events-s3-sqs-a-bucket-notification-configuration-with.md reports non-delivery for a two-queue bucket notification configuration (the native S3 queue destination), not the EventBridge path with reason-scoped rules.

## Evidence
Expected: after PutBucketNotificationConfiguration{EventBridgeConfiguration:{}}, a PutObject on the bucket produces an aws.s3 / "Object Created" event with detail.reason == "PutObject" delivered to the PutObject-scoped rule's queue; a signed POST Object upload (HTTP 201) produces one with detail.reason == "POST Object" delivered only to the POST-scoped rule's queue, and the PutObject-scoped queue stays empty (ApproximateNumberOfMessages 0).

Actual (replay): Error: Check timed out after 60.0s. The first poll — "the PutObject-scoped rule <rule> delivered the event for <key> — the positive control that makes the absence below evidence rather than an artefact" — never satisfies its predicate, so no Object Created event is delivered at all. GetBucketNotificationConfiguration does return EventBridgeConfiguration, so the absence is in the emission/dispatch path, not the configuration store.

## Steps to Reproduce
1. CreateBucket (LocationConstraint outside us-east-1) and PutBucketNotificationConfiguration with NotificationConfiguration={"EventBridgeConfiguration": {}}; confirm GetBucketNotificationConfiguration reports it.
2. Create two SQS queues and two EventBridge rules on the default bus, patterns {"source":["aws.s3"],"detail-type":["Object Created"],"detail":{"bucket":{"name":[<bucket>]},"reason":["PutObject"]}} and the same with reason ["POST Object"], each with its queue as target.
3. PutObject an object under the prefix and poll the PutObject-scoped queue: no message arrives.
4. (Blocked by step 3) Upload a second object with a SigV4-signed multipart POST Object form (success_action_status=201) and poll the POST-scoped queue for detail.reason == "POST Object" while holding the PutObject queue empty.
