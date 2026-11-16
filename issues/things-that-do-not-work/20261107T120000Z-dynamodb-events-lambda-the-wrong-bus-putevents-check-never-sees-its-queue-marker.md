Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__events__lambda.lambda-putevents-wrong-event-bus-arn-negative
Issue-Key: dynamodb__events__lambda.lambda-putevents-wrong-event-bus-arn-negative|_drain_until_marker#22|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 7e76a60d222014fc333ca38c7ab1741bc244623e

# dynamodb__events__lambda: the wrong-event-bus PutEvents check never sees a message on its SQS queue, so it reaches no verdict (1 cause)

> _Note: this issue is carried over from 20261106T151148Z-dynamodb-events-lambda-in-function-sdk-calls-the.md, whose other six checks are fixed. It records the ONE cause of that check which is NOT the refusal that issue claimed: the recording shows the publication is SERVED, and the check then hangs waiting for a message on a queue._

## Operation
PutEvents

## Description
Cause 1 of 1: in `dynamodb__events__lambda.lambda-putevents-wrong-event-bus-arn-negative` the handler's publication onto the wrong event bus is accepted — the check's own `assert ghost_result["ok"] is True` passes against this emulator, as it did against AWS — and the check then drains an Amazon SQS queue for a marker that never arrives, ending in `Error: Check timed out after 60.0s` at `_drain_until_marker#22` (`sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)`).

AWS: the recorded run reaches a verdict in seconds, so whatever puts the marker on that queue — a rule of the correct bus delivering to the queue, or the handler's own `sqs:SendMessage` — happens there. Either way a message is on the queue within the check's budget.

Emulator: nothing is ever received from that queue, so the drain spends the whole 60 s budget. What is NOT the cause: the bus-resolution refusal the previous issue claimed (`ResourceNotFoundException` for a bus the account does not hold). Implementing that refusal was tried and REFUTED by the recording: the check then failed on its own `assert ghost_result["ok"] is True` with `{'ok': False, 'error_code': 'ResourceNotFoundException'}`, and `smithy_query(query="operation", operation="PutEvents")` models no such error. The emulator's served answer (the entry ingested with an `EventId` and routed nowhere) is AWS's, and it is now pinned by
`services/service-com-amazonaws-eventbridge-awsevents/tests/all/rule_read_authorization.rs::a_publication_naming_a_bus_the_account_does_not_hold_is_routed_nowhere` and by
`tests/all/entry_level_semantics.rs::an_entry_naming_an_absent_bus_is_ingested_and_routed_nowhere`.

Why: the check cannot distinguish "the wrong-bus event was correctly routed nowhere" from "nothing is delivered to this queue at all", so the whole wrong-bus negative path is unobservable.

Fix: find what the check expects to put the marker on the queue and make that path deliver. The candidates, in the order they are worth checking:
1. the rule → SQS queue target delivery of the correct bus under the fixture's own queue policy and target configuration (`services/service-com-amazonaws-eventbridge-awsevents/src/ops/put_events.rs`'s delivery pass with `crate::state::target_policy_admits`, and the role-assumption gate beside it, which this emulator applies to a queue target as well as to the kinds AWS invokes under a role);
2. the handler's own in-function `sqs:SendMessage` under its execution role (`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/aws_api.rs` and the SQS authorization gate it reaches).

Neither reproduces in this tree's own end-to-end suites: this crate's `tests/all/target_delivery.rs`, `tests/all/queue_target_policy_authorization.rs` and the router's `core/router/tests/all/events_*` queue-target suites all deliver and all pass, so the divergence is in some property of the fixture's setup those tables do not cover.

## Evidence
Expected (AWS): a message on the queue within seconds of the publication, so `_drain_until_marker` returns and the check asserts on what it drained.

Actual (emulator): `lambda-putevents-wrong-event-bus-arn-negative` -> `Error: Check timed out after 60.0s`, the first divergence being the `sqs.receive_message` call of `_drain_until_marker`. The step before it — the publication onto the wrong bus — is served exactly as recorded (`{'ok': True}`).

## Steps to Reproduce
1. Replay `dynamodb__events__lambda.lambda-putevents-wrong-event-bus-arn-negative`.
2. Observe the publication accepted (`ok: True`) and then the 60 s timeout in `_drain_until_marker`.
3. Compare what the fixture puts on the queue (a rule target of the correct bus, or the handler's own `sqs:SendMessage`) with what this emulator delivers for the same setup.
