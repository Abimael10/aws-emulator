Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__sns__sqs.refarch-s3ev-e2e-diagnose-s3-event-notification-break-lifecycle
Issue-Key: lambda__s3__sns__sqs.refarch-s3ev-e2e-diagnose-s3-event-notification-break-lifecycle|_delivered_keys#9|Timeout|return keys
Issue-Commit: 48d9d0ee5625876130172571538589ce856f53ef

# lambda__s3__sns__sqs: the S3 event-notification break/diagnose lifecycle still reaches no verdict — no step of the chain settles inside the check deadline (check times out at 60s)

> _Note: this issue was identified against the code base at commit `48d9d0ee5625876130172571538589ce856f53ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Cause 1 of 1: the deploy → solve → cleanup → destroy lifecycle over an S3 bucket whose ObjectCreated events are destined for an SQS queue and an SNS topic never reaches an observable state. The check is killed by the harness deadline with no phase recorded, so neither the positive control (a configuration installed directly must deliver an s3:TestEvent probe and the matching ObjectCreated record) nor the negative (an unconfigured bucket delivers nothing) can be graded. Failing check: lambda__s3__sns__sqs.refarch-s3ev-e2e-diagnose-s3-event-notification-break-lifecycle.

NOTE: this is the same check already described by the open issue 20261106T062901Z-lambda-s3-sns-sqs-s3-event-notification-break-diagnose-lifecycle-never-completes.md; the replay suite did not report this failure as owned by it, so it is re-filed here with the current replay evidence rather than assumed covered. If that issue is still being worked, close them together.

AWS: every call in this lifecycle settles promptly with a response. Smithy (s3): PutBucketNotificationConfiguration is PUT /{Bucket}?notification with required NotificationConfiguration payload and HTTP 200 success and no modelled error members; unless x-amz-skip-destination-validation is set, S3 validates each destination as it accepts the configuration by publishing an s3:TestEvent probe to it, and refuses an unusable destination with 400 InvalidArgument rather than hanging. CreateStack/DescribeStacks reach a terminal StackStatus (CREATE_COMPLETE or a *_FAILED/ROLLBACK status), GetQueueAttributes/GetTopicAttributes return the deployed Policy, PutObject returns, and an armed ObjectCreated:* configuration delivers the record to the SQS queue (and the SNS topic) within the polling window — or is provably absent because no configuration exists.

Emulator: recorded behavior only — the replay reports exactly 'Error: Check timed out after 60.0s' and nothing else; no phase marker, no HTTP status and no error code were produced, so at least one call in the chain neither answers nor is refused inside the deadline. The check's own polls are bounded far above the harness deadline (420s for a terminal stack status, 600s for notification arming, 180s default poll), so a single non-settling step consumes the whole budget and the run ends with no verdict.

Why: a client cannot use this estate at all. A deploy that never reaches a terminal status leaves an automation waiting forever; an armed notification that never produces the s3:TestEvent probe or the ObjectCreated record makes every S3-event-driven consumer (queue worker, SNS fan-out) silently dead, and makes the absence assertions a diagnosing operator relies on meaningless because absence cannot be distinguished from a stalled path.

Fix: the S3 bucket-notification path — PutBucketNotificationConfiguration's destination validation/probe emission and the ObjectCreated delivery to SQS/SNS destinations — plus whichever step of the CloudFormation create/delete of the bucket+queue+queue-policy+topic+topic-policy stack does not reach a terminal status. Every operation on this path must answer with a status (success or a modelled refusal) within its normal latency instead of leaving the caller polling.

Traces: lambda__s3__sns__sqs.refarch-s3ev-e2e-diagnose-s3-event-notification-break-lifecycle.

## Evidence
Expected (AWS): the stack of bucket + SQS queue + queue policy + SNS topic + topic policy reaches a terminal StackStatus; GetQueueAttributes/GetTopicAttributes return the deployed Policy documents; PutBucketNotificationConfiguration answers HTTP 200 (or refuses 400 InvalidArgument when a destination cannot be validated) and emits an s3:TestEvent probe to the accepted destination; an upload under the configured prefix then delivers one ObjectCreated record to the queue; with no configuration installed, nothing at all arrives.

Actual (emulator): 'Error: Check timed out after 60.0s'. No phase marker, HTTP status, error code or message was emitted for any step — the check produced no verdict, so the delivery (and its documented absence) could not be observed.

## Steps to Reproduce
1. Re-replay lambda__s3__sns__sqs.refarch-s3ev-e2e-diagnose-s3-event-notification-break-lifecycle against the emulator.
2. Create a stack holding an S3 bucket, an SQS queue with a policy allowing s3.amazonaws.com sqs:SendMessage scoped to the bucket ARN, and an SNS topic with the matching sns:Publish policy; time DescribeStacks until a terminal StackStatus.
3. Call PutBucketNotificationConfiguration on the bucket with a QueueConfiguration for s3:ObjectCreated:* under prefix CategoryA/ and time the response; check the queue for the s3:TestEvent probe.
4. PutObject under CategoryA/ and poll the queue for the ObjectCreated record; repeat with no notification configuration installed and confirm nothing arrives.
5. Observe that one of these steps never settles, so the whole check is killed at the harness deadline.
