Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2.event-source-lifecycle-against-active-bus
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2.event-source-lifecycle-against-active-bus|run#24|AssertionError|assert message == f"EventSource not found: {source_arn}", f"unexpected message {message!r}"
Issue-Commit: 6e2d7185f30747a2356e601c1d412ac8e0be11c4

# eventbridgev2: DescribeEventSource on a deleted event source answers an emulator-authored not-found message instead of AWS's "EventSource not found: <arn>"

> _Note: this issue was identified against the code base at commit `6e2d7185f30747a2356e601c1d412ac8e0be11c4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEventSource

## Description
ONE net-new behavioral cause on the bootstrapped eventbridgev2 service (sdkId EventBridgeV2, rpcv2Cbor, apiVersion 2025-05-15). The event-source lifecycle now works end to end — CreateEventSource against an ACTIVE bus answers ACTIVE, DescribeEventSource and ListEventSources agree, DeleteEventSource removes the record and the follow-up read is correctly refused with HTTP 404 / ResourceNotFoundException — but the refusal carries the emulator's own prose instead of the service's message, so the only remaining divergence in this lifecycle is the message text.

Cause 1 of 1: the ResourceNotFoundException raised by DescribeEventSource for an event source that is no longer present is worded by the emulator, not by AWS.
AWS: Smithy: DescribeEventSource (input EventSourceArn @required, target EventSourceArn, @length 1..1011, pattern ^arn:aws(-[a-z0-9]+)*:events:...:event-sourcev2\/(aws\.service|aws\.partner)\/...$) models ResourceNotFoundException (com.amazonaws.eventbridgev2#ResourceNotFoundException, @error client, @httpError 404, single member `Message`, doc "The resource does not exist."). The recorded behavior is HTTP 404, error code ResourceNotFoundException, and a `Message` of the form "EventSource not found: <eventSourceArn>". Fixed text: "EventSource not found: ". The single variable part is the event-source ARN exactly as supplied in the request's EventSourceArn member — one run's ARN only fills that slot and is not part of the requirement.
Emulator: answers HTTP 404 with code ResourceNotFoundException (both correct) but a differently worded, emulator-authored sentence of the form "event source <arn> does not exist" — the replay records 'event source arn:aws:events:<region>:<account>:event-sourcev2/aws.service/<name>/<id> does not exist' where the check asserted "EventSource not found: <arn>". The wording is produced by the shared constructor `crate::state::event_source_not_found` that the handler's not-found branch calls.
Why: the error message is part of the contract and is the only thing that distinguishes a missing event source from a missing event bus or subscriber, since all three refuse with the same ResourceNotFoundException/404. A client, teardown helper, log scraper or test that matches AWS's "EventSource not found: " prefix — or that surfaces the message to an operator — never matches against the emulator, so delete-then-confirm-gone logic that is correct against AWS reports an unrecognised failure here.
Fix: the DescribeEventSource handler's not-found branch and, properly, the shared ResourceNotFoundException constructor it calls — emit AWS's "EventSource not found: <arn>" wording for the event-source resource kind instead of the generic "<kind> <arn> does not exist" sentence.
Related open issue that does NOT cover this: 20261107T005809Z-eventbridgev2-event-source-error-paths-return-the-wrong.md reports the same wording defect reached from a WIRE fixture describing an ARN that never existed; it carries that fixture's replay key only and nothing in it exercises a create/delete lifecycle, so it does not carry this behavioral check and cannot close it. 20261111T025659Z-eventbridgev2-deleted-event-sources-stay-readable-describeeventsource-answers.md is about the delete not taking effect at all (200 after delete) — that part now passes here; only the message diverges.
Traces: behavioral check eventbridgev2.event-source-lifecycle-against-active-bus. Code: services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/describe_event_source.rs:188-193 (not-found branch) and the `crate::state::event_source_not_found` constructor it calls in services/service-com-amazonaws-eventbridgev2-awseventsv2/src/state.rs.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (16/42 passed; emulator serving — 8 of 17 eventbridgev2 checks pass).

[mismatch] eventbridgev2.event-source-lifecycle-against-active-bus
  check_eventbridgev2_event_source_lifecycle_against_active_bus.py, line 106:
    assert message == f"EventSource not found: {source_arn}", f"unexpected message {message!r}"
  AssertionError: unexpected message 'event source arn:aws:events:us-west-2:000000000000:event-sourcev2/aws.service/sim2real-behav-ebv2-src-8e597943a539/6jvk6hei1oml7952mbcxr6mku does not exist'

The check reached line 106, so every earlier assertion held: CreateEventSource against an ACTIVE bus answered State 'ACTIVE' with the requesting bus's EventBusArn; DescribeEventSource reported the created Name, State 'ACTIVE' and no Revoked member; ListEventSources returned exactly one matching source with the created ARN; DeleteEventSource succeeded and the follow-up DescribeEventSource was refused with error code ResourceNotFoundException (HTTP 404). Only the `Error.Message` text diverges.

  expected Message: 'EventSource not found: <eventSourceArn>'  (fixed text 'EventSource not found: ' + the request's EventSourceArn)
  actual   Message: 'event source <eventSourceArn> does not exist'

The ARN, generated source name and 25-character id in the recorded instance are per-run values filling the template; the 404 status, the ResourceNotFoundException code and the message's fixed text plus its one ARN-valued part are the requirement.

Model reference (services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json, as quoted in the handler's generated contract comment at src/ops/describe_event_source.rs:89): DescribeEventSource errors include ResourceNotFoundException (HTTP 404) — "The resource does not exist."

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. Run the behavioral suite and read check eventbridgev2.event-source-lifecycle-against-active-bus.

Directly, against the eventsv2 rpcv2Cbor endpoint:
1. CreateEventBus(Name=<unique>, ClientToken=<unique>); poll DescribeEventBus until State == 'ACTIVE'.
2. CreateEventSource(Name=<unique>, EventBusArn=<bus arn>, ClientToken=<unique>, Configuration={'AwsServiceEventsConfiguration': {'AwsService': 'aws.ec2'}}) and keep the returned EventSourceArn.
3. DeleteEventSource(EventSourceArn=<source arn>).
4. DescribeEventSource(EventSourceArn=<source arn>). Both AWS and the emulator refuse with HTTP 404 / ResourceNotFoundException, but AWS's Error.Message is 'EventSource not found: <source arn>' while the emulator answers 'event source <source arn> does not exist'.
