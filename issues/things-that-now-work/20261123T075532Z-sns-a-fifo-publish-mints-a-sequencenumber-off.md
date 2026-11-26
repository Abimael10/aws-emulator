Issue-Source: trace-sweep-divergence
Issue-Trace: sns-fifo-topic-publish-requires-message-group-id
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sns-fifo-topic-publish-requires-message-group-id
Issue-Shape-Id: com.amazonaws.sns#AmazonSimpleNotificationService
Issue-Key: sns|Publish|2xx->2xx|$.PublishResponse.PublishResult.SequenceNumber
Issue-Commit: 35d7082131f9491ad89c2e4b5f0079c864ac9dfa

# sns: a FIFO Publish mints a SequenceNumber off AWS's 1000-step run, so the per-message-group numbers are unreproducible

> _Note: this issue was identified against the code base at commit `35d7082131f9491ad89c2e4b5f0079c864ac9dfa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Publish

## Description
ONE net-new root cause of service `sns`, found by replaying the wire fixture corpus (simparity-wire-aws-v1) against the emulator built from this checkout: 44 fixtures, 370 steps, 364 passed, 5 mismatched, 1 blocked (score 98.4%, emulator_up=true, bootstrap_signal=false). Of the five mismatch clusters, four are OWNED by open issues (20261116T130313Z-sns-5-wire-divergences-the-sms-opt-out.md owns the ListSubscriptions NextToken, GetTopicAttributes SubscriptionsDeleted and CreateSMSSandboxPhoneNumber clusters; 20261120T082601Z-sqs-the-sns-notification-delivered-into-a-subscribed.md owns the SQS ReceiveMessage SigningCertURL host cluster). This cluster has an empty stub_path and an empty owned_by and is the only cause filed here.

=== Cause 1 of 1: a FIFO topic's Publish is answered with a SequenceNumber that does not sit on the run AWS hands out — a random step inside [1000, 1000000] instead of the recorded 1000-per-message advance from the 10^19 floor ===
Fixture/step: sns-fifo-topic-publish-requires-message-group-id#task#3.

AWS: Smithy (com.amazonaws.sns#PublishResponse$SequenceNumber doc, read via smithy_query query=doc shape=PublishResponse member=SequenceNumber): "This response element applies only to FIFO (first-in-first-out) topics. The sequence number is a large, non-consecutive number that Amazon SNS assigns to each message. The length of SequenceNumber is 128 bits. SequenceNumber continues to increase for each MessageGroupId." The model therefore constrains the member's FORM (large, 128-bit-scale decimal, increasing within a MessageGroupId) but not its digits. Recorded behavior only, for the digits: on this step — the third publish of the fixture to the FIFO topic's message group — the response carries SequenceNumber exactly '10000000000000003000'. That is the 10^19 floor advanced by 3 x 1000, i.e. the recording shows SNS advancing a message group's run by a FIXED unit of 1000 from a 10^19 base, so the value a given publish receives is a function of how many publishes that message group has already taken and is reproducible across a replay of the same request sequence. Every other recorded response field of this step (MessageId and the envelope RequestId) is a per-run value and the comparator normalizes those; it did NOT normalize this member, because on AWS the member is determined by the request sequence rather than by the run.

Emulator: the step answers HTTP 200 (status matches) but SequenceNumber = '10000000000000213305' — the 10^19 floor advanced by a single step of 213305. services/service-com-amazonaws-sns-amazonsimplenotificationservice/src/ops/publish.rs mints the value through `store.next_sequence_number(&topic.arn, input.message_group_id.as_deref())`, which lands in services/service-com-amazonaws-sns-amazonsimplenotificationservice/src/state/fifo_sequence.rs. There `Sequences::next` advances the group from `SEQUENCE_NUMBER_FLOOR` (10^19) by `Sequences::step()`, and `step()` draws a value `MIN_SEQUENCE_NUMBER_STEP + u128::from_be_bytes(..) % span` with MIN=1_000 and MAX=1_000_000, where the digest material is `super::minted_digest(self.minted)` — this store's monotone counter together with the CLOCK and the process's randomly seeded hasher. So the step is unpredictable per process and per call: the Nth publish to a message group is answered with a different number on every boot, and the number is not a multiple of 1000 off the floor as the recording's are.

Why: a client that records or asserts the SequenceNumber of a FIFO publish — an ordering test that checks the numbers of a message group increase by the service's own unit, a fixture that replays a publish sequence and compares the response, or a consumer that correlates the Publish response's SequenceNumber with the SequenceNumber carried in the notification envelope delivered to a subscriber and then re-derives the group's position — agrees with AWS and disagrees with the emulator. Against AWS the run of a message group is a stable function of the publishes made to it; against the emulator it is a per-run random walk, so nothing about the sequence can be reproduced, recorded or compared, and only the "increases within a group" half of the documented contract survives.

Fix: the FIFO sequence-number model in the SNS state (state/fifo_sequence.rs — `Sequences::next` / `Sequences::step` and the `MIN_SEQUENCE_NUMBER_STEP`/`MAX_SEQUENCE_NUMBER_STEP` window), which is the single place ops/publish.rs and PublishBatch reach through `next_sequence_number`. The run must advance each MessageGroupId by the service's own fixed unit off the 10^19 floor, as the recording shows, so that the number a publish is answered with is decided by the group's publish count and not by the clock or the process hasher; the existing per-group keying and the monotone-within-a-group property must be kept, and the deduplicated-publish path in publish.rs must keep answering with the stored number of the copy that was delivered.

Traces: sns-fifo-topic-publish-requires-message-group-id#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator: trace_set_id=simparity-wire-aws-v1, emulator_up=true ("/__admin/health answered 200 and the :39429 data listener accepts"), bootstrap_signal=false, 44 fixtures replayed, 370 steps total, 364 passed, 5 mismatched, 0 errored, 1 blocked, score_pct=98.4, divergence_count=5.

Cause 1 — sns-fifo-topic-publish-requires-message-group-id#task#3, operation Publish, service sns, verdict mismatch, stub_path empty, owned_by empty (divergence_key service=sns|Publish|status=200->200|keys=$.PublishResponse.PublishResult.SequenceNumber, affected_count=1):
  expected_status 200, actual_status 200 (the status and the rest of the response, MessageId included, matched)
  $.PublishResponse.PublishResult.SequenceNumber: value_mismatch (expected '10000000000000003000', got '10000000000000213305')
  Requirement form: a plain decimal at the 10^19 scale advancing the named MessageGroupId by the service's fixed unit of 1000 — the recorded value is the floor plus 3 x 1000 for the third publish of that group. The digits of any ONE recording fill that template from the request sequence; they are not themselves the requirement, which is why this is stated as the advance rule and not as the literal 20 digits.
  Model fact: PublishResponse$SequenceNumber doc — "a large, non-consecutive number ... The length of SequenceNumber is 128 bits. SequenceNumber continues to increase for each MessageGroupId." Nothing in the model fixes the step; the step is recorded behavior.
  Code facts: ops/publish.rs calls `store.next_sequence_number(&topic.arn, input.message_group_id.as_deref())` for a FIFO topic; state/fifo_sequence.rs has `SEQUENCE_NUMBER_FLOOR = 10_u128.pow(19)`, `MIN_SEQUENCE_NUMBER_STEP = 1_000`, `MAX_SEQUENCE_NUMBER_STEP = 1_000_000`, and `Sequences::step()` = `MIN + u128::from_be_bytes(minted_digest(self.minted)[..16]) % span`, whose material is documented in state.rs as the store's monotone counter plus the clock and the process's randomly seeded hasher.

Other four clusters of this replay are skipped as already owned: sns-cnt-list-subscriptions-token-refused-then-listed#task#4, sns-cnt-topic-subscription-counters-settle#task#7 and sns-sms-sandbox-number-verification-flow#task#2 (owned_by 20261116T130313Z-sns-5-wire-divergences-the-sms-opt-out.md), and sns-sqs-subscription-defaults-envelope-then-raw-delivery-queue-policy-cleared-and-not-found#task#9 (owned_by 20261120T082601Z-sqs-the-sns-notification-delivered-into-a-subscribed.md).

## Steps to Reproduce
0. git_clone the repo, boot the emulator built from that checkout, and replay_wire_corpus() over simparity-wire-aws-v1; re-replay exactly the fixture#step pair in Issue-Trace/Issue-Step (sns-fifo-topic-publish-requires-message-group-id#task#3).
1. CreateTopic with Name ending '.fifo' and FifoTopic=true, so the topic is a FIFO topic.
2. Publish to it three times in a row with the SAME MessageGroupId (and a distinct MessageDeduplicationId each time, so no publish is deduplicated), and read PublishResult.SequenceNumber of each response. AWS walks the message group's run in its own fixed unit off the 10^19 base — the third publish of the group is answered '10000000000000003000' in the recording, i.e. floor + 3 x 1000. The emulator answers a value advanced by one random step inside [1000, 1000000] (here '10000000000000213305'), and the step differs on every boot, so re-running this sequence against two boots of the emulator yields two different numbers for the same third publish while AWS yields the same one.
3. Controls that must keep holding: the numbers of ONE MessageGroupId still strictly increase; two different MessageGroupIds of the same topic still advance on independent runs (publishing to group B does not move group A's number); a repeat publish with an already-used MessageDeduplicationId inside the deduplication interval is still answered with the MessageId AND the SequenceNumber of the copy that was delivered, without minting a new number; and a non-FIFO topic's Publish still carries no SequenceNumber member at all.
4. Cross-check the delivered envelope: subscribe an SQS queue to the FIFO topic, publish once, and confirm the SequenceNumber in the notification envelope delivered to the queue equals the one the Publish response returned.
