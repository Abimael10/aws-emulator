Issue-Source: trace-sweep-divergence
Issue-Trace: eventbridgev2-subscriber-delivers-published-event-to-sqs
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eventbridgev2-subscriber-delivers-published-event-to-sqs
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: sqs|GetQueueAttributes|2xx->2xx|$
Issue-Related: 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md
Issue-Commit: cb1fe1cd01dea9b9b76399bb5958c78b62c26090

# sqs: the EventBridgeV2 subscriber delivery never raises queue depth above zero

> _Note: this issue was identified against the code base at commit `cb1fe1cd01dea9b9b76399bb5958c78b62c26090`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetQueueAttributes

## Description
One cause, on service token sqs.

Cause 1 of 1: GetQueueAttributes reports the subscribed queue as permanently empty after an event was published to the bus a subscriber targets that queue from (fixture eventbridgev2-subscriber-delivers-published-event-to-sqs, step task#9).

AWS: Smithy (com.amazonaws.sqs#GetQueueAttributes): input QueueUrl (@required) plus AttributeNames; output is a single member Attributes (QueueAttributeMap, xmlName 'Attribute', flattened), answered on 200. ApproximateNumberOfMessages is the decimal-string count of the messages currently available for retrieval on that queue. The requirement is the COUNTING rule, not any literal: once a message has been enqueued on the queue — here by the EventBridge v2 subscriber delivering a published event to its SQS target — the attribute must read as at least one, and a poller waiting on queue depth must converge. Recorded behavior: the fixture polls this attribute after the publish and it becomes '1'. No status or error diverges (200 on both sides).

Emulator: the attribute stays '0' for the entire poll window ($.Attributes.ApproximateNumberOfMessages: expected '1', got '0'; 'poll: not in [1] after 37 attempt(s)', poll capped at 180s). The count is internally consistent — nothing was ever enqueued on that queue — so what a client observes is a queue that is never fed, with a 200 and a healthy-looking attribute map.

Why: queue depth is the standard readiness signal for a bus -> subscriber -> queue fan-out. A client that publishes an event and waits for ApproximateNumberOfMessages to become non-zero waits forever, and every consumer-side behavior built on that signal (drain loops, batching, backlog/DLQ thresholds, test harness readiness gates) is unreachable, while the control plane keeps reporting the queue as existing and the publish as accepted.

Fix: not the GetQueueAttributes serializer or the sqs attribute computation — those faithfully report an empty queue. The missing component is the cross-service delivery leg that must enqueue the message into sqs queue state. In this fixture that leg never even starts, because the same fixture's earlier step task#4 (eventbridgev2 CreateEventBus) is refused with 400 (filed separately in this same sweep as the eventbridgev2 issue titled 'EventBridgeV2 CreateEventBus refuses a create that omits ClientToken'): with no bus there is no subscriber, no published event and nothing to deliver. So the bus-create refusal must be fixed first, and then the eventbridgev2 subscriber's SQS target delivery path must write the delivered event into the target queue's sqs state so depth is observable there. Hypothesis (neither the model nor the recording states it): once the bus exists the subscriber delivery path itself may well enqueue — this step does not decide that, which is why it must be re-replayed after the upstream refusal is fixed rather than closed on inspection.

Traces: eventbridgev2-subscriber-delivers-published-event-to-sqs#task#9.

Related open issues, none of which carries this step's replay key (the replay report lists task#9 as owned by no open issue), so none closes it: 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md is the same symptom on a DIFFERENT fixture and a different upstream leg (sesv2 -> SNS -> SQS) and owns step task#10 of this fixture but not task#9; 20261113T174325Z-sqs-eventbridgev2-subscriber-delivery-writes-an-sqs-message-body.md concerns the BODY SHAPE an eventbridgev2 subscriber delivery writes, i.e. a delivery that did happen; 20261113T094431Z-eventbridgev2-iam-sqs-9-net-new-causes-putevents.md covers filtered/transformed delivery causes on other fixtures.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture eventbridgev2-subscriber-delivers-published-event-to-sqs, step task#9 (sqs GetQueueAttributes).

Expected 200 / actual 200:
  $.Attributes.ApproximateNumberOfMessages: value_mismatch (expected '1', got '0')
  $.Attributes.ApproximateNumberOfMessages: poll: not in ['1'] after 37 attempt(s) (expected '1', got '0')
  $: poll capped at 180s (expected {'Attributes': {'ApproximateNumberOfMessages': '1'}}, got {'Attributes': {'ApproximateNumberOfMessages': '0'}})

Expected: the attribute converges to a non-zero count once the subscriber has delivered the published event to its SQS target. The requirement is that the count reflect the messages the queue holds; the literal '1' is this recording's single delivered event.

Actual: '0' through all 37 polls across the 180 s window. No HTTP status, error code or error message diverges on this step — the divergence is the absence of the message the queue must hold.

Upstream context in the same fixture: step task#4 (eventbridgev2 CreateEventBus) answers 400 instead of 200 and mints no bus, so no subscriber exists to deliver anything; that refusal is filed separately in this sweep against service eventbridgev2. Step task#10 (sqs ReceiveMessage) of this fixture returns an empty body and is reported as already owned by 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay wire fixture eventbridgev2-subscriber-delivers-published-event-to-sqs from trace set simparity-wire-aws-v1.
2. Let the setup run: create the SQS queue, read its QueueArn, set a queue policy allowing EventBridge to sqs:SendMessage, create the delivery role.
3. Step task#4: eventbridgev2 CreateEventBus. It currently answers 400 and creates no bus — fix or bypass that refusal (filed separately for service eventbridgev2) so the rest of the scenario can run.
4. Create the subscriber with the queue as its SQS target and publish an event to the bus with PutEvents, as the fixture does.
5. Step task#9: poll sqs GetQueueAttributes(QueueUrl=<queue>, AttributeNames=['ApproximateNumberOfMessages']). Observe it stays '0' for all 37 polls / 180 s where AWS converges to a non-zero count.
6. After wiring the eventbridgev2 subscriber's SQS-target delivery into sqs queue state, re-replay step task#9 (and task#10 on the same queue) and confirm the depth converges and the message is retrievable.
