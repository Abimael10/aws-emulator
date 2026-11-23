Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__s3__sqs.s3-multi-event-type-routing-sqs
Issue-Key: events__s3__sqs.s3-multi-event-type-routing-sqs|_drain#18|Timeout|resp = sqs.receive_message(
Issue-Commit: 81f864176a353ca0b12e45138ef5482587aa1344

# events__s3__sqs: a bucket notification configuration with two SQS queue destinations delivers no object event at all, so per-event-type routing isolation reaches no verdict

> _Note: this issue was identified against the code base at commit `81f864176a353ca0b12e45138ef5482587aa1344`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
One net-new root cause, grounded in one failing behavioral check.

Cause 1 of 1: S3 object-event notifications configured with `QueueConfigurations` are never published to the destination SQS queues, so neither the positive leg (ObjectCreated -> its own queue, ObjectRemoved -> its own queue) nor the isolation leg (nothing leaks to the other queue) can be observed.

AWS: `PutBucketNotificationConfiguration` takes `NotificationConfiguration.QueueConfigurations`, a list of `QueueConfiguration` whose `QueueArn` and `Events` members are both @required (Smithy: com.amazonaws.s3#QueueConfiguration.QueueArn xmlName=Queue required, .Events -> EventList xmlFlattened required; the event tokens come from the enum com.amazonaws.s3#Event, which includes `s3:ObjectCreated:*`, `s3:ObjectCreated:Put`, `s3:ObjectRemoved:*`, `s3:ObjectRemoved:Delete`, `s3:ObjectRemoved:DeleteMarkerCreated`). Once such a configuration is accepted, AWS delivers, per recorded behavior: (a) a one-off destination-validation handshake message `{"Service":"Amazon S3","Event":"s3:TestEvent",...}` to each newly attached queue, and (b) for every object event, a JSON document `{"Records":[{"eventName":"ObjectCreated:Put", ...}]}` to EXACTLY the queues whose configuration's `Events` list matches that event type — an ObjectCreated event reaches only the ObjectCreated-scoped configuration's queue and an ObjectRemoved event only the ObjectRemoved-scoped one. The queue policies in this check already grant `sqs:SendMessage` to the `s3.amazonaws.com` service principal with an `ArnEquals` condition on `aws:SourceArn`, so delivery is authorized.

Emulator: after `PutBucketNotificationConfiguration` is accepted with two `QueueConfigurations` (`s3:ObjectCreated:*` -> queue A, `s3:ObjectRemoved:*` -> queue B), a `PutObject` on the bucket produces nothing on either queue: repeated `ReceiveMessage` (MaxNumberOfMessages=10, WaitTimeSeconds=5) on queue A returns no message, no `Records` document and not even the `s3:TestEvent` handshake, and the check is killed with `Check timed out after 60.0s` while still waiting for its FIRST expected ObjectCreated record (the check's own wait budget is 90 s). The subsequent `DeleteObject` leg and both no-leak assertions are therefore never reached. The accept of the configuration is not accompanied by any delivery path.

Why: a client that wires a bucket to SQS and consumes object events — the standard S3 -> SQS fan-out — sees an accepted notification configuration and then an eternally empty queue. Workers block on long polls forever, nothing is processed, and the failure is silent: there is no error to catch, and `GetBucketNotificationConfiguration` reports the wiring as present. Code that asserts routing isolation (per-event-type destinations) cannot be exercised at all.

Fix: S3's bucket-notification dispatch — the object-mutation path (PutObject / DeleteObject / CompleteMultipartUpload / delete-marker creation) must evaluate the bucket's stored `QueueConfigurations`, match the concrete event type against each configuration's `Events` list (including the `:*` wildcards), and SendMessage the `Records` envelope to each matching queue, plus the one-shot `s3:TestEvent` handshake emitted when a queue destination is first attached in PutBucketNotificationConfiguration's handler/state.

Related but NOT covering this check (the replay marked it neither ALREADY FILED nor OWNED): 20261028T220212Z-s3-sqs-putbucketnotificationconfiguration-accepts-an-sqs-destination-it.md, 20261112T091340Z-s3-sqs-a-versioned-bucket-s-objectremoved-deletemarkercreated.md and 20261119T101312Z-events-s3-sqs-the-reason-scoped-s3-eventbridge.md.

Traces: events__s3__sqs.s3-multi-event-type-routing-sqs (verdict: mismatch, `Check timed out after 60.0s`).

## Evidence
Expected (AWS): after PutBucketNotificationConfiguration with QueueConfigurations [{Id:"created", Events:["s3:ObjectCreated:*"], QueueArn:A}, {Id:"removed", Events:["s3:ObjectRemoved:*"], QueueArn:B}], a PutObject yields on queue A one SQS message whose body is {"Records":[{"eventName":"ObjectCreated:Put",...}]} (preceded once by the {"Service":"Amazon S3","Event":"s3:TestEvent"} handshake) and nothing on queue B; a DeleteObject then yields one {"Records":[{"eventName":"ObjectRemoved:Delete",...}]} on queue B and nothing on queue A.

Actual (emulator): both queues stay empty. Looping ReceiveMessage(MaxNumberOfMessages=10, WaitTimeSeconds=5, MessageAttributeNames=["All"]) on queue A after PutObject returns zero messages for the whole run; the check never gets past its first positive control and is terminated with `Error: Check timed out after 60.0s` (its own assertion would have been "expected 1 S3 notification(s), got 0 within 90s"). No s3:TestEvent is observed either, so no part of the destination wiring is exercised.

## Steps to Reproduce
1. create_bucket(Bucket=B).
2. create_queue twice (A and B_q); set on each a queue policy allowing Principal {"Service":"s3.amazonaws.com"} Action sqs:SendMessage on its own ARN, Condition ArnEquals aws:SourceArn = arn:aws:s3:::B.
3. s3.put_bucket_notification_configuration(Bucket=B, NotificationConfiguration={"QueueConfigurations":[{"Id":"created","QueueArn":<A arn>,"Events":["s3:ObjectCreated:*"]},{"Id":"removed","QueueArn":<B_q arn>,"Events":["s3:ObjectRemoved:*"]}]}).
4. s3.put_object(Bucket=B, Key="route-me.txt", Body=b"x").
5. Poll sqs.receive_message on queue A (WaitTimeSeconds=5) for up to 90 s -> expected one body with Records[0].eventName starting "ObjectCreated"; emulator returns nothing.
6. s3.delete_object(Bucket=B, Key="route-me.txt") and poll queue B_q -> expected one Records[0].eventName starting "ObjectRemoved"; emulator returns nothing.
