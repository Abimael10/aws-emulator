Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns__sqs.fifo-missing-dedup-id-rejected-until-content-based-dedup-enabled sns__sqs.publish-batch-fifo-entry-missing-dedup-id-rejects-the-whole-batch
Issue-Key: sns__sqs|Publish|InvalidParameter
Issue-Key: sns__sqs.publish-batch-fifo-entry-missing-dedup-id-rejects-the-whole-batch|run#12|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# sns__sqs: the FIFO missing-MessageDeduplicationId refusal message is emulator-authored, diverging on Publish and PublishBatch

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Publish

## Description
One net-new root cause, grounded in two failing checks.

Cause 1 of 1: publishing to a FIFO topic without a MessageDeduplicationId when ContentBasedDeduplication is not enabled is refused with the right status and code but an emulator-authored message, on both the single and the batch path.
AWS: Smithy sns#Publish models InvalidParameterException (httpError 400, awsQueryError code 'InvalidParameter'); GetTopicAttributes documents ContentBasedDeduplication — "By default, ContentBasedDeduplication is set to false. If you create a FIFO topic and this attribute is false, you must specify a value for the MessageDeduplicationId parameter for the Publish action." The recorded refusal is HTTP 400, Code 'InvalidParameter', Message 'Invalid parameter: The topic should either have ContentBasedDeduplication enabled or MessageDeduplicationId provided explicitly'. Every word is fixed text for this class — the message names neither the topic, the entry, nor any request value. PublishBatch raises the identical message, and it rejects the WHOLE batch (no Successful entries) when one entry is missing its MessageDeduplicationId.
Emulator: the status (400) and the code ('InvalidParameter') are right, but the message is 'Invalid parameter: MessageDeduplicationId - required for a FIFO topic whose ContentBasedDeduplication is not enabled' on both paths — a reworded sentence that names the member instead of AWS's fixed wording.
Why: 'InvalidParameter' is shared by dozens of unrelated SNS validation failures, so the message is the only discriminator for this one; clients and SDK-level tooling that classify or surface the "enable content-based dedup OR supply a dedup id" condition by its text do not recognise the refusal, and AWS's wording is the one that tells the caller it has two ways out (enable the attribute, or supply the id) rather than naming only the missing member.
Fix: the Publish handler's FIFO MessageDeduplicationId validation and the per-entry validation in the PublishBatch handler — emit AWS's fixed text for this class from a single shared site so the two paths cannot drift.
Traces: sns__sqs.fifo-missing-dedup-id-rejected-until-content-based-dedup-enabled, sns__sqs.publish-batch-fifo-entry-missing-dedup-id-rejects-the-whole-batch. Not a reopen of 20261025T121736Z-sns-four-net-new-behavioral-divergences-a-deleted.md: that issue's FIFO cause is the missing ContentBasedDeduplication attribute in GetTopicAttributes, not this refusal's wording.

## Evidence
Both checks refuse correctly (HTTP 400, Code 'InvalidParameter') and diverge only in the message text.

1. sns__sqs.fifo-missing-dedup-id-rejected-until-content-based-dedup-enabled (mismatch):
  Expected (AWS): ('InvalidParameter', 'Invalid parameter: The topic should either have ContentBasedDeduplication enabled or MessageDeduplicationId provided explicitly')
  Actual (emulator): ('InvalidParameter', 'Invalid parameter: MessageDeduplicationId - required for a FIFO topic whose ContentBasedDeduplication is not enabled') — check file lines 47/148.

2. sns__sqs.publish-batch-fifo-entry-missing-dedup-id-rejects-the-whole-batch (mismatch):
  Expected (AWS) message: 'Invalid parameter: The topic should either have ContentBasedDeduplication enabled or MessageDeduplicationId provided explicitly'
  Actual (emulator): 'Invalid parameter: MessageDeduplicationId - required for a FIFO topic whose ContentBasedDeduplication is not enabled' — check file line 155.

## Steps to Reproduce
Against the emulator's SNS endpoint (any region/account):
1. CreateTopic(Name='<n>.fifo', Attributes={'FifoTopic': 'true'}) — ContentBasedDeduplication left at its default (false).
2. Publish(TopicArn=<topic>, Message='x', MessageGroupId='g') with no MessageDeduplicationId -> observe Code 'InvalidParameter' with Message 'Invalid parameter: MessageDeduplicationId - required for a FIFO topic whose ContentBasedDeduplication is not enabled'. AWS answers 'Invalid parameter: The topic should either have ContentBasedDeduplication enabled or MessageDeduplicationId provided explicitly'.
3. PublishBatch(TopicArn=<topic>, PublishBatchRequestEntries=[one entry WITH a MessageDeduplicationId, one entry WITHOUT]) -> the whole batch must be refused with that same AWS message; the emulator refuses with the reworded one.
4. SetTopicAttributes(AttributeName='ContentBasedDeduplication', AttributeValue='true') and repeat step 2: the publish must now succeed.
