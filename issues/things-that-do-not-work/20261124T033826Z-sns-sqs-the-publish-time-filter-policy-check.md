Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed
Issue-Key: sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed|_drain#38|Timeout|resp = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 1a1e541ed0ec0b82db4256674fd34013fe89b9af

# sns__sqs: the publish-time filter-policy check never reaches a verdict — a filtered SQS subscription's narrow-admit / withhold / widen-and-readmit sequence produces no observable step inside the check budget

> _Note: this issue was identified against the code base at commit `1a1e541ed0ec0b82db4256674fd34013fe89b9af`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Publish

## Description
Cause 1 of 1: SNS's publish-time subscription-filter evaluation (and the effect of widening that filter with SetSubscriptionAttributes) is not observable against the emulator — the behavioral check that exercises it makes no gradeable progress and is cut off by its 60s budget, so none of its three beats reach an assertion.

AWS: SNS evaluates a subscription's FilterPolicy against the INCOMING message, at publish time. A publish whose message attributes match the policy is fanned out to that subscription's endpoint (here an SQS queue, RawMessageDelivery=false, so the queue body is the SNS envelope JSON whose "Message" member is the published payload); a publish that does not match is discarded at that moment and is never queued, re-evaluated or replayed. SetSubscriptionAttributes with AttributeName="FilterPolicy" replaces the stored policy, GetSubscriptionAttributes then serves the new value, and the widened policy admits SUBSEQUENT publishes only — AWS documents filter-policy changes as taking effect within up to 15 minutes and documents no backlog flush, so an already-rejected message must stay absent forever and an already-delivered message must not be delivered a second time. Smithy (com-amazonaws-sns-amazonsimplenotificationservice): Subscribe carries the policy in Attributes (SubscriptionAttributesMap) and returns SubscriptionArn; SetSubscriptionAttributes requires SubscriptionArn + AttributeName with output Unit; Publish requires Message, accepts MessageAttributes (MessageAttributeMap) and returns MessageId. The model says nothing about filter evaluation or propagation timing — that part is AWS-documented/recorded behavior only.

Emulator: the check is graded [mismatch] with the single detail "Check timed out after 60.0s". It therefore reached NO assertion: not the positive control (narrow policy {"kind":["wanted"]} admitting a kind=wanted publish to the subject queue), not the unfiltered-twin control (the twin subscription receiving the kind=unwanted publish, which is what makes the subject's emptiness attributable to the filter), and not the restore beat (the widened policy {"kind":["wanted","unwanted"]} admitting a fresh kind=unwanted probe). Every one of those steps is a plain Publish followed by ReceiveMessage drains, so the emulator served no timely observable for the whole sequence. Hypothesis: one of (a) a subscription that carries a FilterPolicy never delivers even a matching publish through the SNS->SQS fan-out, (b) the policy written by SetSubscriptionAttributes is stored but never consulted by the next Publish, so no probe is ever admitted and the 900s propagation poll is entered and runs out, or (c) SQS ReceiveMessage with WaitTimeSeconds=5 holds the full wait even when a message is already visible, so the check's fixed sequence of drains cannot finish in budget. The replay output does not distinguish these; all three are emulator-side.

Why: a client cannot establish the one property SNS filtering guarantees — that a message rejected by a filter policy is dropped at publish time and never resurfaces. If the emulator instead holds non-matching messages and flushes them when the policy is relaxed, a consumer receives messages it was told were discarded (duplicate or unexpected work, and in the warm-up case a second copy of an already-delivered message). If instead a filtered subscription simply never delivers, every filtered fan-out silently loses matching traffic.

Fix: SNS's Publish fan-out path — the per-subscription filter-policy evaluation against the incoming message's MessageAttributes, and the subscription state SetSubscriptionAttributes(FilterPolicy) writes that the next Publish must read; plus the SQS ReceiveMessage long-poll wakeup if the budget is being consumed there rather than in the filter path. No message may be retained for later re-evaluation.

Traces: sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed (the only failing check in this cluster; the rest of the sns__sqs suite passes, so basic unfiltered SNS->SQS delivery is not implicated). Related open issues do not cover it: 20261115T165530Z-sns-sqs-sendmessage-on-a-deleted-queue-carries.md is about NonExistentQueue wording and 20261120T082601Z-sqs-the-sns-notification-delivered-into-a-subscribed.md is about the SigningCertURL host in the delivered envelope.

## Evidence
Failing check: [mismatch] sns__sqs.a-message-filtered-out-at-publish-time-is-never-redelivered-when-the-policy-is-relaxed
Error detail (verbatim, the only detail reported): "Check timed out after 60.0s"

Expected (AWS): Beat 1 — Publish with MessageAttributes {kind: {DataType: String, StringValue: "wanted"}} to a topic whose SQS subscription has FilterPolicy {"kind":["wanted"]} is delivered; ReceiveMessage on the subject queue returns exactly that one envelope and the envelope's "Message" parses to the published payload. Beat 2 — Publish with kind="unwanted" is delivered to the unfiltered twin subscription but NOT to the subject; the subject queue stays empty and GetQueueAttributes reports ApproximateNumberOfMessages + ...NotVisible + ...Delayed = 0. Beat 3 — after SetSubscriptionAttributes(SubscriptionArn=<subject sub>, AttributeName="FilterPolicy", AttributeValue={"kind":["wanted","unwanted"]}), GetSubscriptionAttributes serves the widened policy, a NEW kind="unwanted" publish is admitted, and the earlier withheld message (and the already-delivered warm-up message) are still absent from everything the queue ever receives, with final depth 0.

Actual (emulator): no beat produced an observable result; the check was cut off at its 60s budget having asserted nothing. Every variable part above (topic name sim2real-nd-noretro-*, queue names sim2real-nd-noretro-subj-<suffix>/-twin-<suffix>, nonces warm-*/withheld-*/probe-*, MessageIds) is generated per run and is not part of the requirement — the requirement is the admit/withhold/readmit polarity and the final queue depth of 0.

## Steps to Reproduce
1. sns.CreateTopic(Name=<unique>).
2. sqs.CreateQueue twice (subject, twin); for each, GetQueueAttributes[QueueArn] and SetQueueAttributes with a Policy allowing sns.amazonaws.com sqs:SendMessage on that ARN with Condition ArnEquals aws:SourceArn=<topic arn>.
3. sns.Subscribe(TopicArn, Protocol="sqs", Endpoint=<subject queue arn>, Attributes={FilterPolicy: {"kind":["wanted"]}, RawMessageDelivery: "false"}, ReturnSubscriptionArn=true); sns.Subscribe the twin queue with RawMessageDelivery="false" and NO FilterPolicy.
4. sns.Publish(TopicArn, Message={"nonce":"warm-..."}, MessageAttributes={kind: String "wanted"}) and poll sqs.ReceiveMessage(MaxNumberOfMessages=10, WaitTimeSeconds=5) on the subject queue: the warm nonce must arrive (positive control). Drain the twin.
5. sns.Publish the same shape with kind="unwanted". The twin must receive it; the subject must receive nothing over a hold of max(3x measured latency, 15s), and GetQueueAttributes on the subject must sum to 0.
6. sns.SetSubscriptionAttributes(SubscriptionArn=<subject sub>, AttributeName="FilterPolicy", AttributeValue={"kind":["wanted","unwanted"]}); GetSubscriptionAttributes must serve it.
7. Publish a FRESH kind="unwanted" probe and drain the subject queue: the probe must arrive.
8. Drain again and assert the step-5 withheld nonce and the step-4 warm nonce are absent from everything the subject queue ever received, and that final depth is 0.
Against the emulator this sequence produces no observable step and is cut off at 60s.
