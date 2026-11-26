Issue-Source: trace-sweep-divergence
Issue-Trace: eventbridgev2-create-event-bus-validation-errors
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eventbridgev2-create-event-bus-validation-errors eventbridgev2-event-source-and-subscriber-reference-missing-bus
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2|CreateEventBus|4xx->2xx|$.Message
Issue-Key: eventbridgev2|CreateEventBus|4xx->2xx|$.__type
Issue-Key: eventbridgev2|CreateEventBus|4xx->2xx|status
Issue-Key: eventbridgev2|CreateEventSource|4xx->4xx|$.Message
Issue-Key: eventbridgev2|CreateEventSource|4xx->4xx|$.__type
Issue-Key: eventbridgev2|CreateEventSource|4xx->4xx|status
Issue-Commit: fbb1a06f1c20de0c01a698c058987963365385aa

# eventbridgev2 creates accept a blank ClientToken instead of refusing it, and CreateEventSource resolves the bus before validating parameters

> _Note: this issue was identified against the code base at commit `fbb1a06f1c20de0c01a698c058987963365385aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventBus

## Description
Two causes (2 of 2 below), both on service token eventbridgev2 (sdkId EventBridgeV2, AWSEventsV2 v2025-05-15, rpcv2Cbor; model services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json). They share one fault: the ClientToken refusal the recorded service applies to its resource-creating operations is not served at all any more, and where another refusal exists the emulator reaches it by resolving the named resource BEFORE it validates the request's parameters. Every other divergence cluster in this replay reports a non-empty owned_by (open eventbridgev2 issues) and is not repeated here; these are the only two clusters with owned_by and stub_path both empty.

Cause 1 of 2: CreateEventBus answers HTTP 200 and mints the bus where the recorded service refuses the create with HTTP 400 (fixture eventbridgev2-create-event-bus-validation-errors, step task#1).
AWS: recorded behavior only for the refusal condition. Smithy (com.amazonaws.eventbridgev2#CreateEventBus) marks only Name @required; ClientToken is @idempotencyToken with @length 1..64 and @pattern ^[\u0021-\u007E]+$ and carries the member doc "The SDK generates one automatically when the field is omitted", and the operation's 400-class errors are LimitExceededException and the service-common InvalidInputException ("A request parameter is missing or not valid."). The recording for this validation-probe fixture shows the service answering HTTP 400 with __type 'com.amazon.nexus.v20250515.exceptions#InvalidInputException' and Message 'ClientToken is required' — that Message is wholly FIXED text: it interpolates nothing from the request and nothing from state, so it is the same string for every such refusal and only the member name is intrinsic to it. The requirement is therefore: a CreateEventBus whose ClientToken does not satisfy the member's own constraints (the recorded probe's token value is not usable as an idempotency token — the model's @length min 1 and @pattern already exclude an empty or non-printable token) is refused with 400 InvalidInputException / 'ClientToken is required', and NO bus is created. Hypothesis (neither the model nor the recording states it): the discriminating condition is a present-but-blank ClientToken rather than an omitted one, because the same corpus records CreateEventBus succeeding with 200 in fixture eventbridgev2-subscriber-delivers-published-event-to-sqs#task#4, which the emulator now passes — so the refusal must be keyed to the token being present and unusable, not to its absence.
Emulator: the step answers HTTP 200 with a full success body; the response carries no __type and no Message at all (the replay reports both as 'missing'), so the bus is minted and the caller is told the create succeeded. The handler has no ClientToken check left: src/ops/create_event_bus.rs consults input.client_token only inside `if let Some(token) = ...` for the idempotent-replay lookup, and src/service/validation.rs restates only the model's own refusals.
Why: a client that sends an unusable idempotency token gets a bus it was never allowed to create and no error to react to. Worse, the create is silently non-idempotent: the caller believes the token it supplied is the replay key, so its own retry mints a SECOND bus (or trips ResourceAlreadyExistsException) instead of replaying the first, and the account accumulates buses no caller intended. Any test or deployment step that asserts the validation refusal passes against the emulator and fails against AWS.
Fix: the CreateEventBus handler's pre-store validation — the ClientToken check must run ahead of the store lock so a refused create mints nothing, and must serve the modeled InvalidInputException with that fixed message. Note that the still-open issue 20261121T184930Z-eventbridgev2-createeventbus-still-refuses-a-create-that-omits.md asked for the OPPOSITE change (it reported an absent token being refused) and the checkout now has no check at all; the two recordings reconcile only if the rule is keyed to a present-but-unusable token, so re-adding a blanket "client_token.is_none()" refusal will simply re-break that issue's fixture.
Traces: eventbridgev2-create-event-bus-validation-errors#task#1.

Cause 2 of 2: CreateEventSource answers 404 ResourceNotFoundException for a request that should first be refused as invalid, so parameter validation is ordered after resource resolution (fixture eventbridgev2-event-source-and-subscriber-reference-missing-bus, step task#2).
AWS: recorded behavior only. Smithy (com.amazonaws.eventbridgev2#CreateEventSource) requires Name, EventBusArn and Configuration, models ClientToken the same way as CreateEventBus, and models both InvalidInputException (400) and ResourceNotFoundException (404). This step names an event bus that does not exist AND carries the same unusable ClientToken as cause 1, and the recording shows the service answering HTTP 400 with __type 'com.amazon.nexus.v20250515.exceptions#InvalidInputException' and the same fixed Message 'ClientToken is required' — i.e. request-parameter validation precedes any lookup of the bus the request names, so the not-found answer is never reached.
Emulator: answers HTTP 404 with __type 'ResourceNotFoundException' and Message 'Event bus not found: sim2real-probe-ebv2-missing' (the fixed text 'Event bus not found: ' plus the bus identifier taken from the request). src/ops/create_event_source.rs:211-216 resolves `state.bus(&input.event_bus_arn)` as the first thing after taking the store lock, ahead of the configuration check at :236 and ahead of any token handling at :256.
Why: a client gets the wrong error class and status for a malformed request — 404 (retry/create-the-bus) instead of 400 (fix the parameter) — so retry and error-mapping logic keyed on ResourceNotFoundException takes the wrong branch, and the real defect in the request is never reported. The ordering fault is general: any CreateEventSource that is both invalid and points at a missing bus reports the missing bus instead of the invalidity.
Fix: the CreateEventSource handler's validation ordering — move request-parameter validation (the ClientToken rule of cause 1, and the Configuration check) ahead of the bus lookup, so an invalid request is refused with 400 InvalidInputException before the store is consulted. The not-found path itself stays as-is for requests that are otherwise valid.
Traces: eventbridgev2-event-source-and-subscriber-reference-missing-bus#task#2.

Serializer note for both causes: the recorded __type for this error class is the namespaced shape id 'com.amazon.nexus.v20250515.exceptions#InvalidInputException', while the emulator serves bare names ('ResourceNotFoundException' above). That prefix divergence also shows up in clusters owned by 20261113T174325Z-eventbridgev2-subscriber-error-paths-answer-the-wrong-status.md; it is mentioned here only so the two statuses/messages above are restored in the shape the recording carries.

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health 200, data listener accepting): 17 fixtures replayed, 200 steps — 162 passed, 38 mismatched, 0 blocked, 0 errored, score 81.0%, 13 divergence clusters. 11 clusters carry a non-empty owned_by; the 2 below are the unowned, non-stub-blocked ones.

[mismatch] cause 1 — eventbridgev2 CreateEventBus, fixture eventbridgev2-create-event-bus-validation-errors, step task#1 (cluster of 1):
  divergence_key: service=eventbridgev2|CreateEventBus|status=400->200|keys=$.Message,$.__type,status
  status: expected 400, got 200
  $.__type: missing (expected 'com.amazon.nexus.v20250515.exceptions#InvalidInputException', got None)
  $.Message: missing (expected 'ClientToken is required', got None)
  Expected: HTTP 400, error class InvalidInputException (@httpError 400), Message exactly 'ClientToken is required' (entirely fixed text — no request or state value is interpolated), and no bus created.
  Actual: HTTP 200 with a success body and no error members at all; the bus is minted.
  Code (orientation only, current checkout): services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/create_event_bus.rs:234-246 reads input.client_token only for the idempotent-replay lookup, so no refusal exists on any ClientToken value; src/service/validation.rs adds no requirement of its own.

[mismatch] cause 2 — eventbridgev2 CreateEventSource, fixture eventbridgev2-event-source-and-subscriber-reference-missing-bus, step task#2 (cluster of 1):
  divergence_key: service=eventbridgev2|CreateEventSource|status=400->404|keys=$.Message,$.__type,status
  status: expected 400, got 404
  $.__type: value_mismatch (expected 'com.amazon.nexus.v20250515.exceptions#InvalidInputException', got 'ResourceNotFoundException')
  $.Message: value_mismatch (expected 'ClientToken is required', got 'Event bus not found: sim2real-probe-ebv2-missing')
  Expected: HTTP 400 InvalidInputException with the fixed Message 'ClientToken is required' — parameter validation decided before the named bus is looked up.
  Actual: HTTP 404 ResourceNotFoundException whose Message is the fixed text 'Event bus not found: ' followed by the bus identifier taken from the request (the probe's generated bus name is a per-run value, not part of the requirement).
  Code (orientation only, current checkout): src/ops/create_event_source.rs:211-216 resolves the bus as the first post-lock step, ahead of the Configuration check at :236 and the ClientToken handling at :256.

Model references (services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json): CreateEventBusRequest required = {Name}; CreateEventSourceRequest required = {Name, EventBusArn, Configuration}; ClientToken @idempotencyToken, @length 1..64, @pattern ^[\u0021-\u007E]+$ on both; CreateEventBus errors include InvalidInputException/400 and LimitExceededException/400 (no ResourceNotFoundException); CreateEventSource errors include InvalidInputException/400 and ResourceNotFoundException/404; InvalidInputException doc 'A request parameter is missing or not valid.'

## Steps to Reproduce
1. git_clone this repo, then replay_wire_corpus (trace set simparity-wire-aws-v1), which builds and boots the emulator from the checkout.
2. Cause 1: run fixture eventbridgev2-create-event-bus-validation-errors to step task#1 — the rpcv2Cbor request for AWSEventsV2.CreateEventBus that the fixture's first validation probe sends. Observe HTTP 200 with a minted bus (EventBusArn/Name/State/StorageConfiguration/CreationTime present, no __type, no Message) where the recording expects HTTP 400 with __type InvalidInputException and Message 'ClientToken is required' and no bus created. Confirm the complement still holds: fixture eventbridgev2-subscriber-delivers-published-event-to-sqs#task#4, whose CreateEventBus must still answer 200, currently passes and must keep passing after the fix.
3. Cause 2: run fixture eventbridgev2-event-source-and-subscriber-reference-missing-bus to step task#2 — AWSEventsV2.CreateEventSource naming an event bus that was never created, with the same ClientToken the probe uses. Observe HTTP 404 __type 'ResourceNotFoundException' Message 'Event bus not found: <bus name from the request>' where the recording expects HTTP 400 __type InvalidInputException Message 'ClientToken is required'.
4. Independently check the ordering: send a CreateEventSource that is valid in every respect except that its EventBusArn names a missing bus — that one must still answer 404 ResourceNotFoundException, so only an invalid request may be re-ordered to 400.
5. After the fix, re-replay eventbridgev2-create-event-bus-validation-errors#task#1 and eventbridgev2-event-source-and-subscriber-reference-missing-bus#task#2 and confirm both statuses, both error classes and both messages. BOTH causes must be closed before submitting.
