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
Issue-Commit: fc05151cc5015eb457b31a79871539af51fb9fdd

# sqs: the SES configuration-set SNS delivery event never lands on the subscribed queue, so the queue reports zero messages and ReceiveMessage returns nothing

> _Note: this issue was identified against the code base at commit `fc05151cc5015eb457b31a79871539af51fb9fdd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ReceiveMessage

## Description
Two causes, both on service token sqs, in fixture sesv2-configuration-set-publishes-delivery-event-to-sns. They share one fault: a message published to an SNS topic that an SQS queue is subscribed to is never enqueued on that queue, so neither the queue's message counters nor a receive ever observe it. Both steps are reported unowned by the replay (owned_by empty, stub_path empty). The two issues that previously described this leg — 20261113T161204Z-sqs-an-ses-configuration-set-delivery-event-never.md and 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md — are both in the RESOLVED folder, so neither closes these steps; the still-open 20261121T085411Z-sesv2-sendemail-through-a-configuration-set-with-an-sns.md covers the UPSTREAM sesv2 SendEmail refusal (task#8) and carries that step's key, not these.

Cause 1 of 2 — GetQueueAttributes reports ApproximateNumberOfMessages 0 for a queue that has been delivered a notification (fixture sesv2-configuration-set-publishes-delivery-event-to-sns, step task#9).
AWS: Smithy (sqs): GetQueueAttributes is input {QueueUrl @required, AttributeNames} -> output {Attributes: QueueAttributeMap}; its modeled refusals are InvalidAddress (404), InvalidAttributeName (400), InvalidSecurity (403), QueueDoesNotExist/AWS.SimpleQueueService.NonExistentQueue (400), RequestThrottled (403), UnsupportedOperation (400) — none applies here, and the call does answer 200. The attribute ApproximateNumberOfMessages is the approximate count of messages available for retrieval from the queue, so once a message has been delivered into the queue it must be counted there. Recorded behavior: after the preceding publish leg, the step polls this attribute and it reaches the count of delivered messages (the recording settles on one delivered notification) within the step's poll budget.
Emulator: answers 200 but ApproximateNumberOfMessages stays at its empty value for the whole poll window — replay: "$.Attributes.ApproximateNumberOfMessages: value_mismatch (expected '1', got '0')" and "poll: not in ['1'] after 40 attempt(s)". The literal 1 is this recording's delivered-message count, not a constant: the requirement is that the counter equals the number of messages delivered to and not yet consumed from the queue, i.e. it becomes non-zero once the subscription has been delivered a notification.
Why: a client polls ApproximateNumberOfMessages to decide whether a fan-out actually delivered. With the counter pinned at zero, any wait-for-delivery loop (the standard SNS->SQS and SES-event-destination integration test) times out and reports a healthy pipeline as broken, and nothing downstream of the queue is ever triggered.
Fix: the queue's message state and the SNS-to-SQS delivery path that feeds it (the sqs service's queue state / message store, and the subscription-delivery component that must enqueue an sqs:SendMessage for an sqs-protocol subscription), not the GetQueueAttributes serializer: the attribute is computed correctly from an empty queue, the queue is wrongly empty.
Traces: sesv2-configuration-set-publishes-delivery-event-to-sns#task#9.

Cause 2 of 2 — ReceiveMessage on that same queue returns an empty body instead of the delivered notification (fixture sesv2-configuration-set-publishes-delivery-event-to-sns, step task#10).
AWS: Smithy (sqs): ReceiveMessage is input {QueueUrl @required, MessageSystemAttributeNames, MessageAttributeNames, MaxNumberOfMessages, VisibilityTimeout, WaitTimeSeconds, ReceiveRequestAttemptId} -> output {Messages: MessageList (xmlName Message, flattened)}; modeled refusals are InvalidAddress (404), InvalidSecurity (403), the KMS.* family (400/403), OverLimit (403), QueueDoesNotExist/AWS.SimpleQueueService.NonExistentQueue (400), RequestThrottled (403), UnsupportedOperation (400) — again none applies, and the call answers 200. A received message carries MessageId, ReceiptHandle, Body and MD5OfBody, where MD5OfBody is the MD5 digest of the message body as delivered. Recorded behavior: the receive returns one Message whose Body is the SNS notification envelope wrapping the published event; MessageId and ReceiptHandle are per-run values (the recording asserts their shape only, '<shaped-not-asserted>') and the recorded MD5OfBody digest is the digest of that one run's body, so neither the ids nor that digest is a requirement — the requirement is a non-empty Messages list with one entry per undelivered message, each carrying a MessageId, a ReceiptHandle, the delivered Body and an MD5OfBody that is the MD5 of that Body.
Emulator: answers 200 with a completely empty body — replay: "$: empty_body (expected {'Messages': [{'Body': '<shaped-not-asserted>', 'MD5OfBody': '0af1f8afaff0628470385feaf85d34d0', 'MessageId': '<shaped-not-asserted>', 'ReceiptHandle': '<shaped-not-asserted>'}]}, got {})". No Messages member and no message at all, consistent with Cause 1: nothing was ever enqueued.
Why: the receive is how a client consumes a fan-out. An empty receive on a queue that AWS would have a message on means the consumer side of every SNS->SQS integration (and of SES event publication into a queue) silently processes nothing, with no error to distinguish "not delivered yet" from "never delivered".
Fix: same component as Cause 1 — the delivery path that must enqueue the notification into the subscribed queue, plus the ReceiveMessage handler's read of that queue state so a stored message is returned with MessageId/ReceiptHandle/Body and an MD5OfBody computed over the body it serves.
Traces: sesv2-configuration-set-publishes-delivery-event-to-sns#task#10.

Note on ordering: in this fixture the publish that should feed the queue is a sesv2 SendEmail at task#8 which the emulator refuses 400 (filed separately as 20261121T085411Z-sesv2-sendemail-through-a-configuration-set-with-an-sns.md, sesv2). A fixer must therefore verify these two sqs steps against a delivery that actually occurs — the earlier sns/sqs setup steps of the same fixture (CreateTopic, CreateQueue, SetQueueAttributes with a policy allowing sns.amazonaws.com to sqs:SendMessage, Subscribe with Protocol 'sqs') all replay as passing, so the subscription itself exists; what is unobserved is any enqueue into the subscribed queue.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture sesv2-configuration-set-publishes-delivery-event-to-sns.

Cause 1 — step task#9, service sqs, operation GetQueueAttributes, verdict mismatch, expected_status 200, actual_status 200, affected_count 1, owned_by empty, stub_path empty:
  $.Attributes.ApproximateNumberOfMessages: value_mismatch (expected '1', got '0')
  $.Attributes.ApproximateNumberOfMessages: poll: not in ['1'] after 40 attempt(s) (expected '1', got '0')
Expected (AWS, recorded): the attribute reaches the number of messages delivered and not yet consumed (one, in this recording) inside the poll window.
Actual (emulator): 200 with the attribute stuck at '0' across all 40 poll attempts.

Cause 2 — step task#10, service sqs, operation ReceiveMessage, verdict mismatch, expected_status 200, actual_status 200, affected_count 1, owned_by empty, stub_path empty:
  $: empty_body (expected {'Messages': [{'Body': '<shaped-not-asserted>', 'MD5OfBody': '0af1f8afaff0628470385feaf85d34d0', 'MessageId': '<shaped-not-asserted>', 'ReceiptHandle': '<shaped-not-asserted>'}]}, got {})
Expected (AWS, recorded): 200 carrying one Message with MessageId, ReceiptHandle, Body (shape-asserted only — per-run values) and MD5OfBody equal to the MD5 of the served Body; the recorded digest 0af1f8afaff0628470385feaf85d34d0 is that one run's body digest, not a fixed requirement.
Actual (emulator): 200 with an entirely empty response body — no Messages member.

Both statuses are already correct (200/200); the divergence is purely that the queue holds no message. Smithy grounding: sqs GetQueueAttributes -> {Attributes: QueueAttributeMap}, errors InvalidAddress/InvalidAttributeName/InvalidSecurity/QueueDoesNotExist/RequestThrottled/UnsupportedOperation; sqs ReceiveMessage -> {Messages: MessageList}, errors InvalidAddress/InvalidSecurity/KMS.*/OverLimit/QueueDoesNotExist/RequestThrottled/UnsupportedOperation. No modeled refusal applies, so the only divergence is the missing message.

Prior coverage checked: 20261113T161204Z-sqs-an-ses-configuration-set-delivery-event-never.md and 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md are both RESOLVED (fixed folder) and do not close these steps; the replay report lists both steps with owned_by empty.

## Steps to Reproduce
1. git_clone() the repository and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
3. Let fixture sesv2-configuration-set-publishes-delivery-event-to-sns run its setup and task prefix: sns CreateTopic; sqs CreateQueue; sqs GetQueueAttributes(QueueArn); sqs SetQueueAttributes with a policy allowing sns.amazonaws.com to sqs:SendMessage from that topic; sns Subscribe(Protocol='sqs', Endpoint=<queue arn>); sesv2 CreateConfigurationSet and CreateConfigurationSetEventDestination with an enabled SnsDestination on that topic; establish the sending identity. All of these answer as recorded.
4. Cause 1 — step task#9: poll sqs GetQueueAttributes on that queue URL for AttributeNames=[ApproximateNumberOfMessages]. Observe it answer 200 with '0' on every one of the 40 attempts, where the recording reaches the delivered-message count.
5. Cause 2 — step task#10: call sqs ReceiveMessage on the same queue URL. Observe 200 with an empty body (no Messages member), where the recording returns one Message with MessageId, ReceiptHandle, Body and an MD5OfBody over that Body.
6. To separate the legs from the upstream sesv2 refusal, publish directly to the topic with sns Publish instead of relying on task#8's SendEmail (which is refused 400 and filed as 20261121T085411Z-sesv2-sendemail-through-a-configuration-set-with-an-sns.md) and repeat steps 4-5: the queue must then count and return the notification.
7. After the fix, re-replay sesv2-configuration-set-publishes-delivery-event-to-sns#task#9 AND sesv2-configuration-set-publishes-delivery-event-to-sns#task#10 and confirm the counter becomes non-zero and the receive returns the delivered message.
