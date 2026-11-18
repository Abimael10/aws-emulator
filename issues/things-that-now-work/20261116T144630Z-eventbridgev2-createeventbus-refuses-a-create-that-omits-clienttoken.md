Issue-Source: trace-sweep-divergence
Issue-Trace: eventbridgev2-subscriber-delivers-published-event-to-sqs
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eventbridgev2-subscriber-delivers-published-event-to-sqs
Issue-Shape-Id: com.amazonaws.eventbridgev2#AWSEventsV2
Issue-Key: eventbridgev2|CreateEventBus|2xx->4xx|$.CreationTime
Issue-Key: eventbridgev2|CreateEventBus|2xx->4xx|$.EventBusArn
Issue-Key: eventbridgev2|CreateEventBus|2xx->4xx|$.Name
Issue-Key: eventbridgev2|CreateEventBus|2xx->4xx|$.State
Issue-Key: eventbridgev2|CreateEventBus|2xx->4xx|$.StorageConfiguration
Issue-Key: eventbridgev2|CreateEventBus|2xx->4xx|status
Issue-Commit: cb1fe1cd01dea9b9b76399bb5958c78b62c26090

# EventBridgeV2 CreateEventBus refuses a create that omits ClientToken

> _Note: this issue was identified against the code base at commit `cb1fe1cd01dea9b9b76399bb5958c78b62c26090`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventBus

## Description
One cause, on service token eventbridgev2.

Cause 1 of 1: CreateEventBus answers HTTP 400 instead of minting the bus when the request carries no ClientToken (fixture eventbridgev2-subscriber-delivers-published-event-to-sqs, step task#4).

AWS: Smithy (com.amazonaws.eventbridgev2#CreateEventBus): the input shape CreateEventBusRequest marks exactly ONE member @required — Name (EventBusName, len 1..256, pattern ^[A-Za-z0-9][\.\-_A-Za-z0-9]*$). ClientToken is modeled only as @idempotencyToken (ClientToken, len 1..64, pattern ^[\u0021-\u007E]+$) and is NOT @required; the operation doc states 'Retries carrying the same ClientToken are idempotent' and the member doc states 'The SDK generates one automatically when the field is omitted' — i.e. its absence is a legal request that is simply not idempotent, not a validation failure. StorageConfiguration is likewise optional (StorageConfiguration.RetentionPeriodInDays @range 1..365). A valid create therefore answers 200 with EventBusArn, Name, State=CREATING (provisioning is asynchronous; BusState CREATING -> ACTIVE) and StorageConfiguration (StorageConfigurationOutput: RetentionPeriodInDays plus RetentionWindowStartTime) carrying the bus's effective retention period. No modeled error of this operation covers 'the caller omitted an idempotency token': the 400-class errors are LimitExceededException (a quota) and the common InvalidInputException ('A request parameter is missing or not valid.'), and nothing about an omitted optional member is either.

Emulator: the step is refused with HTTP 400 and no success body at all — every output member of CreateEventBusResponse is absent (EventBusArn, Name, State, StorageConfiguration, CreationTime all 'missing'), so no bus is minted and the whole scenario built on that bus cannot proceed. The refusal is produced at the service's input-validation front door, ahead of the handler, as the modeled InvalidInputException with the fixed Message text 'ClientToken is required' — an emulator-invented requirement. That text is entirely fixed (it interpolates nothing from the request or from state), and the rule is applied to each of the three creates (CreateEventBus, CreateEventSource, CreateSubscriber) whenever input.client_token is None. Hypothesis (the model does not state it and the recording only shows the 200): this fixture's create names only Name — its expected StorageConfiguration.RetentionPeriodInDays of 1 is the service default rather than a requested value — so with no ClientToken on the wire the front door refuses it before the handler, where every other fixture in the corpus passes a token and so creates its bus successfully.

Why: a client that calls CreateEventBus without supplying its own idempotency token — which is every caller using a raw HTTP/CBOR request, a hand-rolled client, or any SDK path where the token is not auto-filled — cannot create an event bus at all against the emulator, and is told that a parameter AWS treats as optional is missing. Because the bus is the root of this service's resource tree, the refusal cascades: no event source, no subscriber, no PutEvents and no target delivery can be exercised for that bus (in this fixture it is what leaves the subscribed SQS queue empty at steps task#9/task#10).

Fix: the operation's input validation, not its handler — the create-specific 'requires an idempotency token' rule in this service's validation front door (the CreateEventBus/CreateEventSource/CreateSubscriber entries that require a client token, and the fixed CLIENT_TOKEN_REQUIRED_MESSAGE refusal). An omitted ClientToken must pass validation and reach the handler, which then performs a plain non-idempotent create (the existing token-replay path only applies when a token IS present). The model's own constraints must keep being enforced in the modeled InvalidInputException shape exactly as they are today.

Traces: eventbridgev2-subscriber-delivers-published-event-to-sqs#task#4.

Related open issues, none of which carries this step's replay key (the replay report lists task#4 as owned by no open issue), so none closes it: 20261111T025659Z-eventbridgev2-constraint-validation-refusals-answer-validationexception-with-no.md concerns the SHAPE of constraint refusals (ValidationException with no Message instead of InvalidInputException) and not a refusal of a request the model accepts; 20261110T210822Z-eventbridgev2-3-net-new-behavioral-causes-deleteeventbus-is.md covers event-bus READS omitting StorageConfiguration; 20261115T124416Z-eventbridgev2-updateeventbus-mutations-never-reach-the-stored-bus.md covers UpdateEventBus mutations. All of those presuppose a bus that was created.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture eventbridgev2-subscriber-delivers-published-event-to-sqs, step task#4 (eventbridgev2 CreateEventBus).

Expected 200 / actual 400:
  status: expected 200, got 400
  $.Name: missing (expected the requested bus name, got None)
  $.EventBusArn: missing (expected arn:aws:events:<region>:<account>:event-busv2/<name>/<25-char id>, got None)
  $.State: missing (expected 'CREATING', got None)
  $.StorageConfiguration: missing (expected {'RetentionPeriodInDays': 1, 'RetentionWindowStartTime': <shaped-not-asserted>}, got None)
  $.CreationTime: missing (expected <shaped-not-asserted>, got None)

Expected: 200 with the bus minted — EventBusArn, Name, State 'CREATING', StorageConfiguration carrying the effective RetentionPeriodInDays (1 here, the service default for a create naming no StorageConfiguration) and a RetentionWindowStartTime, and a CreationTime. ARN id, timestamps and the generated name segment are per-run values and are not requirements.

Actual: 400 with no success body. The only 400 path this handler can take other than the modeled quota error (LimitExceededException, bounded at 100 buses per region / 1000 per process — far above this corpus) is the service's front-door refusal, whose whole Message is the fixed text 'ClientToken is required' served as __type 'InvalidInputException' on status 400 — a requirement the Smithy model does not express, since ClientToken is @idempotencyToken and not @required.

Code that carries the invented rule (for orientation only): services/service-com-amazonaws-eventbridgev2-awseventsv2/src/service/validation.rs:67 (CLIENT_TOKEN_REQUIRED_MESSAGE), :142-145 (the refusal), :257-281 (the three creates registered as requiring the token); the handler itself is services/service-com-amazonaws-eventbridgev2-awseventsv2/src/ops/create_event_bus.rs:221-237, which already treats a missing token as 'no replay record to match'.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay wire fixture eventbridgev2-subscriber-delivers-published-event-to-sqs from trace set simparity-wire-aws-v1.
2. Let the setup steps run (queue creation, queue policy, role) — they answer as recorded.
3. Step task#4: POST the rpcv2Cbor request /service/AWSEventsV2/operation/CreateEventBus with only Name set to a valid bus name (no ClientToken, no StorageConfiguration).
4. Observe HTTP 400 with __type 'InvalidInputException' and Message 'ClientToken is required', and no bus created, where AWS answers 200 with EventBusArn, Name, State 'CREATING', StorageConfiguration (RetentionPeriodInDays defaulted) and CreationTime.
5. Repeat the same request WITH a ClientToken and observe it succeed, confirming the refusal is driven solely by the omitted optional token.
6. After the fix, re-replay fixture eventbridgev2-subscriber-delivers-published-event-to-sqs step task#4 and confirm the 200 and the full response body; the downstream steps of the same fixture (task#9, task#10) depend on this bus existing.
