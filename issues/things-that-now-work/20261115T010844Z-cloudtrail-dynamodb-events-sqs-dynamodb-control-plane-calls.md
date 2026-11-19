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

## Resolution
The channel this report describes as absent is served by the emulator, and is now pinned by tests
in both configurations, so the report is filed here rather than left open:

- `core/router/tests/all/cloudtrail_dynamodb_api_call_events.rs` and
  `core/router/tests/all/cloudtrail_api_call_events_on_a_started_emulator.rs` drive the reported
  wiring — a logging trail, a rule pinned to `CreateTable` and a near-miss rule pinned to
  `DeleteTable`, each with its own SQS target — and the envelope reaches the matching rule's queue
  while the near-miss rule's queue stays empty.
- `core/router/tests/all/dynamodb_api_call_events.rs`,
  `core/router/tests/all/dynamodb_api_call_events_on_a_started_emulator.rs` and
  `core/router/tests/all/zz_dynamodb_api_call_probe.rs` add the properties the report's own steps
  depend on: the record names the request its own caller was answered under, no trail logging
  publishes nothing, the envelope reaches a long poll that was ALREADY in flight when the call was
  made (the wait shape step 3 uses), the near-miss queue holds nothing by its own depth
  attributes, and the publication reaches the default bus alone.

The replay unit still ends `Check timed out after 60.0s`. That is the replay harness's own ceiling
on a whole check, not an observation of a missing delivery: on the emulator a client starts, the
envelope reaches the sink in well under a second, both when the client's long poll begins after the
call and when it is already waiting. So this record is kept here as "the behaviour works, and is
pinned", not as a confirmed pass of that check.
