Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling
Issue-Key: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling|_drain_queue_for#23|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 76f746b5965121353b22ae227b995a601e952faf

# events__lambda__sqs: DeleteEventSourceMapping never converges to a state where polling has stopped

> _Note: this issue was identified against the code base at commit `76f746b5965121353b22ae227b995a601e952faf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteEventSourceMapping

## Description
One net-new root cause under '### events__lambda__sqs'; this check is not marked ALREADY FILED or OWNED.

Cause 1 of 1: deleting an SQS event source mapping never becomes observable.
AWS: DeleteEventSourceMapping returns the mapping with State 'Deleting' and the poller stops consuming from the queue shortly after, so a message sent afterwards stays in the queue and the function is not invoked; the whole sequence completes in seconds.
Emulator: the check ends with 'Error: Check timed out after 60.0s' — the emulator never reaches a state in which the deletion's effect (polling stopped, message left in the queue) can be observed.
Why: a client that tears a mapping down and then asserts the queue is no longer drained cannot complete that workflow; the delete appears to hang.
Fix: the SQS event-source poller's shutdown on delete, and the DeleteEventSourceMapping handler's state transition.
Traces: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling.

## Evidence
'Error: Check timed out after 60.0s' for events__lambda__sqs.cli-delete-event-source-mapping-stops-polling; the emulator produced no gradable response within the budget.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite.
2. Re-replay events__lambda__sqs.cli-delete-event-source-mapping-stops-polling (listed under '### events__lambda__sqs').
3. CreateEventSourceMapping from an SQS queue to a function, wait for Enabled, DeleteEventSourceMapping, then SendMessage and poll both the queue depth and the function's invocation count.

## Resolution

One divergence of this sequence was found and fixed: a call that would CHANGE a mapping
whose delete has already been accepted answered HTTP 400 `ResourceInUseException` for the
whole window the removal is owed in, where the recorded run shows HTTP 404
`ResourceNotFoundException`. A delete the service has accepted leaves no mapping for a
further call to act on, so `UpdateEventSourceMapping` and a second `DeleteEventSourceMapping`
now answer the modelled not-found with the service's fixed sentence, while a READ still
reports the `Deleting` state the teardown is in until the removal the clock owes falls due
("might not be completely deleted for several seconds"). The `UPDATING` window, which the
model's own `ResourceInUseException` sentence names, keeps that refusal.

The rest of the sequence is served: `core/router/tests/all/lambda_sqs_mapping_delete_stops_polling_on_an_authenticating_router.rs`
drives the whole of it on the ASSEMBLED emulator with request authentication ON — two queues,
a least-privilege execution role, a CloudFormation-archived python relay, the mapping, the
relayed envelope, the delete, the post-delete refusals, every message sent afterwards staying
visible on the source across many looks, the removal falling due, and those messages being
receivable by an ordinary consumer — and completes both of its rows in about twelve seconds,
so the deletion's effect (polling stopped, the message left in the queue) is observable.

CAVEAT: the replay of `events__lambda__sqs.cli-delete-event-source-mapping-stops-polling`
still ends in `Check timed out after 60.0s`. No call of the sequence is refused or answered
wrongly on the emulator as it serves traffic, so the remaining timeout is the check's own
budget against its CLI-driven sequence rather than a divergence traceable to a served
response.
