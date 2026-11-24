Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-configuration-set-publishes-delivery-event-to-sns
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-configuration-set-publishes-delivery-event-to-sns
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sqs|GetQueueAttributes|2xx->2xx|$.Attributes.ApproximateNumberOfMessages
Issue-Key: sqs|ReceiveMessage|2xx->2xx|$
Issue-Commit: a28aa3cbb761848919217b218a8a95a512704746

# sqs: a queue subscribed to an SES configuration-set SNS event destination never receives the notification, so it stays empty

> _Note: this issue was identified against the code base at commit `a28aa3cbb761848919217b218a8a95a512704746`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ReceiveMessage

## Description
Two net-new causes on service token sqs, both in fixture sesv2-configuration-set-publishes-delivery-event-to-sns: a queue that is subscribed to an SNS topic used as an SES configuration-set event destination never acquires the notification message, so both the attribute read and the receive see an empty queue.

Cause 1 of 2 - GetQueueAttributes reports ApproximateNumberOfMessages 0 forever where AWS reports 1 (step task#9, operation GetQueueAttributes).
AWS: Smithy (sqs): GetQueueAttributes takes QueueUrl (required) and AttributeNames (AttributeNameList, xmlName AttributeName, flattened) and returns Attributes (QueueAttributeMap, xmlName Attribute, flattened); its modeled refusals are InvalidAddress (404), InvalidAttributeName (400), InvalidSecurity (403), QueueDoesNotExist (400, AWS.SimpleQueueService.NonExistentQueue), RequestThrottled (403) and UnsupportedOperation (400). The delivery itself is recorded behavior only: once a message has been published to the subscribed topic, HTTP 200 carries Attributes.ApproximateNumberOfMessages counting the messages currently available on the queue - the recording settles on "1" (one notification delivered). The requirement is that the counter reflects the messages actually enqueued, i.e. a message delivered through the SNS subscription makes the count non-zero; the literal "1" is simply the number of notifications this scenario produces.
Emulator: answers HTTP 200 with ApproximateNumberOfMessages "0", and keeps answering "0" for the whole poll window - replay: "$.Attributes.ApproximateNumberOfMessages: value_mismatch (expected '1', got '0')" and "poll: not in ['1'] after 40 attempt(s)". Nothing is ever enqueued.
Why: a client that polls the attribute to learn whether an SES event landed waits forever and concludes no event was produced. Every SES event-publication workflow that fans out through SNS to SQS is unobservable, and so is any backlog/queue-depth logic built on the counter.
Fix: the delivery path that must enqueue the SNS notification onto the subscribed queue plus the queue's message state and its attribute computation (services/service-com-amazonaws-sqs-amazonsqs/src/ops/get_queue_attributes.rs reading the queue state the SNS sqs-protocol delivery writes) - the attribute must be derived from the queue's actual available-message set.
Traces: sesv2-configuration-set-publishes-delivery-event-to-sns#task#9.

Cause 2 of 2 - ReceiveMessage returns an empty body where AWS returns the delivered notification (step task#10, operation ReceiveMessage).
AWS: Smithy (sqs): ReceiveMessage takes QueueUrl (required) with MaxNumberOfMessages/VisibilityTimeout/WaitTimeSeconds (NullableInteger) and returns Messages (MessageList, xmlName Message, flattened); modeled refusals include QueueDoesNotExist (AWS.SimpleQueueService.NonExistentQueue, 400), OverLimit (403), InvalidAddress (404) and the KMS.* family. Recorded behavior: HTTP 200 with one Messages entry carrying Body (the SNS notification envelope for the SES event), MD5OfBody, MessageId and ReceiptHandle. MessageId and ReceiptHandle are per-run values the recording asserts by shape only ('<shaped-not-asserted>') and so are not requirements; the requirement is that the receive returns a message whose Body is the notification delivered through the subscription (MD5OfBody being the digest of that body, not a fixed constant).
Emulator: answers HTTP 200 with an empty body - replay: "$: empty_body (expected {'Messages': [{'Body': '<shaped-not-asserted>', 'MD5OfBody': '0af1f8afaff0628470385feaf85d34d0', 'MessageId': '<shaped-not-asserted>', 'ReceiptHandle': '<shaped-not-asserted>'}]}, got {})". No Messages member and no message at all.
Why: the consumer of the queue receives nothing, so no SES delivery/bounce/complaint notification can ever be read, parsed or acknowledged. A consumer loop that expects at least one message after the send blocks indefinitely rather than processing the event.
Fix: same component as Cause 1 - the SNS-to-SQS delivery that must enqueue the notification, and ReceiveMessage's read of the queue's message set (services/service-com-amazonaws-sqs-amazonsqs/src/ops/receive_message.rs): a message present on the queue must be returned with Body/MD5OfBody/MessageId/ReceiptHandle.
Traces: sesv2-configuration-set-publishes-delivery-event-to-sns#task#10.

Relationship to other issues: the same fixture's step task#8 (sesv2 SendEmail through the configuration set) is refused HTTP 400 by the emulator and is OWNED by the open issue 20261121T085411Z-sesv2-sendemail-through-a-configuration-set-with-an-sns.md, so the send that should trigger the notification currently fails; Hypothesis: these two sqs causes are downstream of that refusal and of the missing SES event publication (open issue 20261026T065823Z-sesv2-sns-sqs-ses-publishes-no-events-to.md), but the replay reports both of these steps as UNOWNED - no open issue carries their replay keys, so neither is closed by those issues and they must be re-replayed in their own right. The previously RESOLVED issues on this leg (20261113T161204Z-sqs-an-ses-configuration-set-delivery-event-never.md and 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md, plus 20261102T110414Z-lambda-sesv2-a-sesv2-delivery-event-never-reaches.md) are marked fixed yet the queue is still empty on replay, so this is a reopening of that behaviour and not covered by them.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200; 384 steps, 332 passed, 52 mismatched). Two unowned sqs clusters (stub_path empty, owned_by empty), both in fixture sesv2-configuration-set-publishes-delivery-event-to-sns:

1) step task#9, GetQueueAttributes, verdict mismatch, expected_status 200, actual_status 200, affected_count 1:
   $.Attributes.ApproximateNumberOfMessages: value_mismatch (expected '1', got '0')
   $.Attributes.ApproximateNumberOfMessages: poll: not in ['1'] after 40 attempt(s) (expected '1', got '0')

2) step task#10, ReceiveMessage, verdict mismatch, expected_status 200, actual_status 200, affected_count 1:
   $: empty_body (expected {'Messages': [{'Body': '<shaped-not-asserted>', 'MD5OfBody': '0af1f8afaff0628470385feaf85d34d0', 'MessageId': '<shaped-not-asserted>', 'ReceiptHandle': '<shaped-not-asserted>'}]}, got {})

Context in the same fixture: the setup steps (sns CreateTopic, sqs CreateQueue, GetQueueAttributes for QueueArn, SetQueueAttributes with the policy allowing sns.amazonaws.com to sqs:SendMessage, Subscribe with Protocol 'sqs', sesv2 CreateConfigurationSet and CreateConfigurationSetEventDestination) replay as passing, so topic, queue, policy and subscription all exist; the send at task#8 is refused 400 (owned by 20261121T085411Z-sesv2-sendemail-through-a-configuration-set-with-an-sns.md).

Model evidence (smithy_query, services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json): GetQueueAttributes output Attributes (QueueAttributeMap, xmlName Attribute, flattened); ReceiveMessage output Messages (MessageList, xmlName Message, flattened). MessageId and ReceiptHandle are per-run values (shape-asserted only) and are not part of the requirement.

## Steps to Reproduce
1. git clone the repository and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
3. Replay fixture sesv2-configuration-set-publishes-delivery-event-to-sns and let its setup run: sns CreateTopic; sqs CreateQueue; sqs GetQueueAttributes(AttributeNames=['QueueArn']); sqs SetQueueAttributes with a Policy allowing sns.amazonaws.com to sqs:SendMessage from that topic; sns Subscribe(Protocol='sqs', Endpoint=<queue arn>); sesv2 CreateConfigurationSet + CreateConfigurationSetEventDestination with an enabled SnsDestination on that topic; then the send at task#8.
4. Step task#9: call sqs GetQueueAttributes with AttributeNames=['ApproximateNumberOfMessages'] on that queue, polling. Observe '0' on every one of the 40 attempts where the recording settles on '1'.
5. Step task#10: call sqs ReceiveMessage on the same queue. Observe HTTP 200 with an empty body (no Messages) where the recording returns one message with Body/MD5OfBody/MessageId/ReceiptHandle.
6. Sanity check the upstream leg while fixing: the SES send at task#8 is currently refused 400 (its own open issue), and SES event publication to an SNS event destination is itself reported missing, so verify a message is actually published to the topic and then that SNS's sqs-protocol delivery enqueues it.
7. After the fix, re-replay sesv2-configuration-set-publishes-delivery-event-to-sns#task#9 and #task#10 and confirm the queue reports a non-zero ApproximateNumberOfMessages and ReceiveMessage returns the notification.
