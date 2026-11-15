Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__dynamodb__events__sqs.xsvc-dynamodb-3-create-table-reaches-only-its-rule-not-the-delete-table-near-miss
Issue-Key: cloudtrail__dynamodb__events__sqs.xsvc-dynamodb-3-create-table-reaches-only-its-rule-not-the-delete-table-near-miss|_collect#66|Timeout|msgs = sqs.receive_message(
Issue-Commit: 3cd41981ee722cd642660e763820306da1bf304f

# cloudtrail__dynamodb__events__sqs: DynamoDB control-plane calls never reach the default event bus as 'AWS API Call via CloudTrail' management events, so no rule can match them

> _Note: this issue was identified against the code base at commit `3cd41981ee722cd642660e763820306da1bf304f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTable

## Description
Cause 1 of 1: the CloudTrail→EventBridge management-event fan-out never delivers, so a rule matching a DynamoDB control-plane API call is never triggered and the scenario cannot reach any verdict.

AWS: DynamoDB emits no native EventBridge events; the only path from a DynamoDB control-plane call to the default bus is CloudTrail's management-event fan-out. With a trail recording management events, a CreateTable call produces an event on the default bus with `detail-type` = `AWS API Call via CloudTrail` and `detail.eventSource` = `dynamodb.amazonaws.com`, `detail.eventName` = `CreateTable`. A rule whose pattern matches that detail-type/eventSource/eventName delivers it to its target (here an SQS queue), while a near-miss rule pinned to a different eventName (DeleteTable) receives nothing. Recorded behavior only — Smithy models neither the fan-out nor the event envelope; CreateTable itself answers 200 in both cases, so the divergence is in the delivery, not in the call.

Emulator: nothing is ever delivered. The check drains its queue for the delivery and never observes the event, so it runs to the harness cap (`Check timed out after 60.0s`) without reaching its assertions. The same 60-second-cap symptom is how every other never-delivers defect in this suite presents.

Why: a client that routes control-plane activity through EventBridge — audit automation, tag-compliance remediation, drift reaction, anything keyed on `AWS API Call via CloudTrail` — sees no events at all, so its rules and targets are dead. Absence of delivery is also indistinguishable from a correctly non-matching rule, so the near-miss half of the contract (a rule for DeleteTable must NOT see a CreateTable) cannot be verified either.

Fix: the CloudTrail management-event fan-out into EventBridge — record dynamodb.amazonaws.com control-plane calls as trail management events and publish them onto the default bus with the `AWS API Call via CloudTrail` envelope, so rule matching and target delivery run.

Traces: cloudtrail__dynamodb__events__sqs.xsvc-dynamodb-3-create-table-reaches-only-its-rule-not-the-delete-table-near-miss.

## Evidence
Expected: after a trail recording management events is live, a CreateTable call yields one default-bus event with `detail-type` `AWS API Call via CloudTrail`, `detail.eventSource` `dynamodb.amazonaws.com` and `detail.eventName` `CreateTable`, delivered to the matching rule's SQS target; the DeleteTable-pinned near-miss rule's queue stays empty.

Actual: no message is ever delivered to the matching rule's queue. The check never reaches its assertions and ends `Error: Check timed out after 60.0s` while draining for the delivery.

## Steps to Reproduce
1. Create a trail recording management events, plus two EventBridge rules on the default bus: one pattern matching {detail-type: 'AWS API Call via CloudTrail', detail.eventSource: 'dynamodb.amazonaws.com', detail.eventName: 'CreateTable'} and a near-miss one pinned to 'DeleteTable', each targeting its own SQS queue.
2. Call DynamoDB CreateTable.
3. Long-poll both queues. Expected: exactly one message on the CreateTable rule's queue, none on the other. Actual: both stay empty.
Replay check cloudtrail__dynamodb__events__sqs.xsvc-dynamodb-3-create-table-reaches-only-its-rule-not-the-delete-table-near-miss to confirm the fix.
