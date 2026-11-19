Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2__iam__sqs.update-subscriber-transformer-applies-to-later-events
Issue-Key: eventbridgev2__iam__sqs.update-subscriber-transformer-applies-to-later-events|run#21|TimeoutError|raise TimeoutError("the run budget ended before a transformed delivery; not graded")
Issue-Commit: 95422a199f71de611c9b4568260a34e03a6bfa1c

# eventbridgev2__iam__sqs: a JSONATA Transformer set by UpdateSubscriber is never evaluated, so every later event is silently dropped instead of delivered in the new form

> _Note: this issue was identified against the code base at commit `95422a199f71de611c9b4568260a34e03a6bfa1c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateSubscriber

## Description
Cause 1 of 1: UpdateSubscriber accepts a Transformer of Type JSONATA and DescribeSubscriber reports it, but the delivery path performs no JSONATA evaluation; a subscriber carrying that transformer is handed nothing at all, so after the swap the target queue receives no further event — neither the transformed body nor the previous RAW envelope.

AWS: Smithy — UpdateSubscriber is documented "Updates a subscriber. Fields omitted from the request are left unchanged" and accepts Transformer; Transformer$Type JSONATA names a transformation computed by JsonataConfiguration.Expression. Recorded behavior: once UpdateSubscriber has swapped a RAW transformer for a JSONATA one, events published afterwards are delivered to the same SQS target as the JSONATA result (here the expression `{% 'v2:' & $events.Data.detail.marker %}` yields the unquoted string `v2:<marker>`), and every delivery observed after the update is either the still-in-flight RAW envelope or the JSONATA result — the subscriber keeps delivering, in the new form.

Emulator: the check's earlier phases pass — the subscriber delivers the RAW envelope, UpdateSubscriber returns 200 and DescribeSubscriber reports Transformer.Type == "JSONATA" — but no delivery carrying the JSONATA result ever appears on the target queue, and the publish/poll loop (one PutEvents every 5 s, each answered FailedEntryCount=0) exhausts the check's budget, which reports "TimeoutError: the run budget ended before a transformed delivery; not graded". The cause is in the emulator's own delivery path, which is synchronous with the publish: for any Transformer$Type other than RAW/WITH_METADATA the body builder returns None and the event is dropped with only a tracing warning ("its transformer names a transformation this emulator does not perform"), so the subscriber stops delivering entirely after the update.

Why: PutEvents keeps answering success (FailedEntryCount 0, a per-entry EventId) while every event for that subscriber is discarded — a client that reconfigures a subscriber's transformer in place sees its consumer go permanently silent with no error, no dead-letter record and no failed entry to explain it; events accepted by the bus are lost.

Fix: the subscriber delivery path's transformer evaluation in this service's state (the JSONATA branch of the delivered-body builder) plus the UpdateSubscriber handler's persistence of Transformer — a transformer type the delivery path cannot evaluate must not silently suppress the delivery.

Traces: eventbridgev2__iam__sqs.update-subscriber-transformer-applies-to-later-events. Source citations: services/service-com-amazonaws-eventbridgev2-awseventsv2/src/state/subscribers.rs lines 822-848 (`delivered_body` returns None for every Transformer type other than RAW / WITH_METADATA); services/service-com-amazonaws-eventbridgev2-awseventsv2/src/state/subscriber_delivery.rs lines 59-80 (`deliver_due` turns that None into a dropped event and a warn, so nothing reaches the target). Related open issue that does NOT cover this check: 20261113T094431Z-eventbridgev2-iam-sqs-9-net-new-causes-putevents.md — its Cause 2 (UpdateSubscriber was a NotImplemented stub) and Cause 3 (a subscriber CREATED with a transformer never receives its warm-up event) no longer describe what this check now hits: UpdateSubscriber answers 200, the subscriber delivers RAW before the update, and the divergence is that the JSONATA transformer installed by the update is never evaluated.

## Evidence
Replayed check eventbridgev2__iam__sqs.update-subscriber-transformer-applies-to-later-events:

File "check_eventbridgev2__iam__sqs_update_subscriber_transformer_applies_to_later_events.py", line 285, in run
  raise TimeoutError("the run budget ended before a transformed delivery; not graded")
TimeoutError: the run budget ended before a transformed delivery; not graded

The raise is reached only after these steps succeeded against the emulator: bus ACTIVE, queue + delivery role created, subscriber created with InvokeConfiguration {RoleArn, TargetArn: <queue arn>, SqsParameters: {}}, warm-up event delivered, the first RAW delivery collected and asserted, UpdateSubscriber(Transformer={Type: JSONATA, JsonataConfiguration: {Expression: "{% 'v2:' & $events.Data.detail.marker %}"}}) returning 200, and DescribeSubscriber reporting Transformer.Type == "JSONATA".

Expected (AWS): at least one body on the target queue beginning with the JSONATA result prefix for a marker published after the update (`v2:<marker>`), with every post-update body either the RAW envelope of a sent marker or that JSONATA result.
Actual (emulator): no body carrying the JSONATA result ever arrives; repeated PutEvents calls answer FailedEntryCount=0 while the queue stays empty of post-update deliveries. In the source, `delivered_body` answers None for Transformer.Type == JSONATA and `deliver_due` then skips the event with a tracing warning only, so the delivery is dropped rather than transformed.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. CreateEventBus and wait for State=ACTIVE; create an SQS queue with a policy allowing events.amazonaws.com sqs:SendMessage, and an IAM role EventBridge can assume with sqs:SendMessage on that queue.
3. CreateSubscriber(EventBusArn=<bus arn>, InvokeConfiguration={RoleArn: <role arn>, TargetArn: <queue arn>, SqsParameters: {}}).
4. PutEvents one entry (Source="sim2real.ebv2.check", DetailType="sim2real-check", Detail={"marker": "m-raw"}) and receive the RAW envelope from the queue — this works.
5. UpdateSubscriber(SubscriberArn=<sub arn>, Transformer={Type: "JSONATA", JsonataConfiguration: {Expression: "{% 'v2:' & $events.Data.detail.marker %}"}}); DescribeSubscriber shows Type=JSONATA.
6. PutEvents further entries with fresh markers every few seconds and poll the queue: no message arrives at all — neither the transformed body `v2:<marker>` that AWS delivers nor the RAW envelope.
