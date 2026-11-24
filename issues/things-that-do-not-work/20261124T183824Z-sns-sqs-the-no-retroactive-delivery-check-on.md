Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed
Issue-Key: sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed|_drain#38|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 8b958dd9ac2b7b6aba3fca63da2d876136547e6d

# sns__sqs: the no-retroactive-delivery check on a widened subscription FilterPolicy reaches no verdict — a SetSubscriptionAttributes widening is not observable on the live subscription inside the replay budget

> _Note: this issue was identified against the code base at commit `8b958dd9ac2b7b6aba3fca63da2d876136547e6d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetSubscriptionAttributes

## Description
One net-new root cause.

Cause 1 of 1: the scenario that proves SNS evaluates a subscription FilterPolicy at PUBLISH time — a message the narrow policy rejected is never replayed once the policy is widened, while the widened policy demonstrably admits new messages — never completes, so neither the withholding nor the widening is graded.

AWS: Smithy (services/service-com-amazonaws-sns-amazonsimplenotificationservice) models SetSubscriptionAttributes(SubscriptionArn required, AttributeName required, AttributeValue) with errors AuthorizationErrorException (403 AuthorizationError), FilterPolicyLimitExceededException (403 FilterPolicyLimitExceeded), InvalidParameterException (400 InvalidParameter), NotFoundException (404 NotFound), ReplayLimitExceededException (403 ReplayLimitExceeded) and InternalErrorException (500 InternalError); GetSubscriptionAttributes reports the stored FilterPolicy. Beyond the model this is recorded behavior only: the recording shows the narrow policy {"kind":["wanted"]} admitting a kind=wanted publish to the subject queue while an unfiltered twin subscription receives every publish; a kind=unwanted publish reaching the twin and NOT the subject; and after SetSubscriptionAttributes(AttributeName='FilterPolicy') stores {"kind":["wanted","unwanted"]}, a FRESH kind=unwanted publish being admitted to the subject queue while the earlier, publish-time-rejected message is never delivered and the queue's ApproximateNumberOfMessages / ...NotVisible / ...Delayed all read 0.

Emulator: the replay reports only `Error: Check timed out after 60.0s`. The check never returned, so no assertion in it was evaluated and no status, error code or response body can be quoted. Its own waits far exceed the harness cap — 120 s for the narrow policy's warm-up admission, 120 s for the unfiltered twin to receive the withheld publish, and 900 s (AWS's own documented filter-policy propagation ceiling, polled every 10 s with a fresh nonce each round) for the widened policy to begin admitting kind=unwanted — so the run is cut before any of them expires. Hypothesis: the widened FilterPolicy stored by SetSubscriptionAttributes is not picked up by the live publish-time evaluation, so every probe publish after the widening is still withheld and the restore poll spins out the whole budget; the other 37 of 38 checks under this heading pass, so SNS->SQS delivery and publish-time filtering in general are not what is blocked.

Why: a client that widens a subscription filter and waits for the first admitted message hangs instead of receiving it, and the guarantee that a message SNS already told the publisher was filtered out is not sitting in a backlog waiting to be flushed cannot be confirmed. A target that flushes such a backlog is holding customer messages the customer was told were dropped, and that outcome is indistinguishable here from a subscription that simply stopped delivering.

Fix: SNS's subscription-attribute write path and the publish-time filter evaluation that reads it — SetSubscriptionAttributes(AttributeName='FilterPolicy') must replace the policy the live fan-out consults for subsequent Publish calls, and the fan-out must evaluate only the incoming message (no re-evaluation or replay of already-decided messages).

Traces: sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed, the only failure under this heading and the only one here not already owned. Related but NOT covering it: open 20261120T082601Z-sqs-the-sns-notification-delivered-into-a-subscribed.md (the SigningCertURL host in an SNS notification delivered to a queue) and 20261115T165530Z-sns-sqs-sendmessage-on-a-deleted-queue-carries.md (NonExistentQueue wording); neither concerns the subscription filter policy.

## Evidence
Check sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed (verdict: mismatch), the only failure under this heading (37/38 of its checks pass).

Expected (AWS): GetSubscriptionAttributes on the subject reports FilterPolicy {"kind":["wanted"]} and the twin reports none; a kind=wanted publish arrives at the subject; a kind=unwanted publish arrives at the UNFILTERED twin and not at the subject, whose ApproximateNumberOfMessages + ...NotVisible + ...Delayed sum to 0; SetSubscriptionAttributes(AttributeName='FilterPolicy', AttributeValue='{"kind":["wanted","unwanted"]}') is stored and read back; a fresh kind=unwanted publish is then admitted to the subject; the earlier withheld nonce and the already-delivered warm-up nonce are absent from everything the subject queue ever received; the queue's depth reads 0 at the end.

Actual (emulator): `Error: Check timed out after 60.0s`. No assertion ran. The check's waits are timeout_s=120 (warm-up admission), timeout_s=120 (twin receives the withheld publish) and timeout_s=900 (the widened policy begins admitting, probed every 10 s with a new nonce), all beyond the 60 s harness cap, so no beat produced a diagnosis.

## Steps to Reproduce
Against the emulator's sns/sqs endpoints (any region/account):
1. CreateTopic; create two queues (subject and twin) and set each queue's Policy to allow sqs:SendMessage for Principal Service sns.amazonaws.com with Condition ArnEquals aws:SourceArn = the topic ARN.
2. Subscribe the subject queue with Attributes {FilterPolicy: '{"kind":["wanted"]}', RawMessageDelivery: 'false'} and the twin queue with {RawMessageDelivery: 'false'} and NO FilterPolicy. Confirm both via GetSubscriptionAttributes.
3. Publish Message='{"nonce":"warm-..."}' with MessageAttributes kind=String 'wanted'; ReceiveMessage on the subject until the warm nonce arrives (AWS well inside 120 s). Drain the twin.
4. Publish a second message with kind='unwanted'; confirm the twin receives it and the subject receives nothing, and that the subject's ApproximateNumberOfMessages/...NotVisible/...Delayed sum to 0.
5. SetSubscriptionAttributes(SubscriptionArn=<subject sub>, AttributeName='FilterPolicy', AttributeValue='{"kind":["wanted","unwanted"]}'); read it back with GetSubscriptionAttributes.
6. Publish a FRESH kind='unwanted' message and ReceiveMessage on the subject: the new nonce must be admitted, while the nonce from step 4 and the warm nonce from step 3 must never appear.
7. Replay sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed: the check is killed at 60 s with no verdict. Note whether step 3, 4 or 6 is the beat that never settles.
