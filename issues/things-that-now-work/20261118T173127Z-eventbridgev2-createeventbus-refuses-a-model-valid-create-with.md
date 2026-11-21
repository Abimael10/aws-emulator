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
Issue-Commit: 003b19ea8201179ec46ed1bcee5f22a6bf4a799a

# eventbridgev2: CreateEventBus refuses a model-valid create with HTTP 400 InvalidInputException instead of minting the bus

> _Note: this issue was identified against the code base at commit `003b19ea8201179ec46ed1bcee5f22a6bf4a799a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventBus

## Description
One net-new root cause found by replaying wire corpus simparity-wire-aws-v1 against the emulator built from this checkout. Service eventbridgev2 (sdkId EventBridgeV2, AWSEventsV2 v2025-05-15, rpcv2Cbor, model services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json); get_bootstrap_status reports no pending services, so eventbridgev2 is bootstrapped.

Cause 1 of 1: a CreateEventBus request that the model accepts is refused with HTTP 400 instead of returning the new bus in CREATING.

AWS: Smithy: CreateEventBus's input is CreateEventBusRequest{Name (EventBusName, @required, @length 1-256, @pattern ^[A-Za-z0-9][\.\-_A-Za-z0-9]*$), Description (@length 0-512), EncryptionConfiguration, StorageConfiguration (structure with RetentionPeriodInDays, @range 1-365), Tags (TagMap, @length max 200), ClientToken (@idempotencyToken, @length 1-64, @pattern ^[\u0021-\u007E]+$)}. ClientToken is the ONLY idempotency member and it is NOT @required — its own model doc says "Unique, case-sensitive identifier that ensures the idempotency of the request. The SDK generates one automatically when the field is omitted", i.e. the member is optional on the wire and a request omitting it is a well-formed create. The operation doc states "Creates an event bus. Provisioning is asynchronous: the bus is returned in the CREATING state and transitions to ACTIVE when ready (see the EventBusActive waiter)." So a create naming only Name (plus an in-range StorageConfiguration) must succeed with HTTP 200 and a body carrying EventBusArn (matching ^arn:aws...:events:<region>:<account>:event-busv2\/<name>\/[a-z0-9]{25}$), Name echoed from the request, State CREATING, StorageConfiguration{RetentionPeriodInDays as requested, RetentionWindowStartTime} and CreationTime. The recorded response for this step is exactly that: HTTP 200, State 'CREATING', StorageConfiguration.RetentionPeriodInDays 1. The modeled 400s for this operation are LimitExceededException ("The request would exceed a service quota for the account") and InvalidInputException ("A request parameter is missing or not valid.") — neither applies to a create that breaks no modeled constraint and is the account's only bus of that name.

Emulator: the create is refused — "status: expected 200, got 400" — and the whole success body is absent: $.EventBusArn, $.Name, $.State, $.StorageConfiguration and $.CreationTime are all missing. No bus is minted, so every later step of the flow addresses a bus that does not exist. Hypothesis for the mechanism: this service installs a pre-handler refusal of its own in services/service-com-amazonaws-eventbridgev2-awseventsv2/src/service/validation.rs, which registers CreateEventBus (with CreateEventSource and CreateSubscriber) through ModelledInputRefusal::requiring_client_token and answers InvalidInputException (HTTP 400, __type 'InvalidInputException') with the fixed message "ClientToken is required" (crate::state::CLIENT_TOKEN_REQUIRED_MESSAGE) whenever the deserialized input carries no ClientToken. That rule is not in the model — it promotes an @idempotencyToken member to @required — and the recorded request for this step is model-valid (AWS answered 200), so an extra-model requirement is what rejects it. A client-token requirement cannot be the service's behavior as recorded here: a create that omits the token is accepted and minted.

Why: the first call of the publish-and-deliver flow fails, so a client that creates an event bus the way the SDK does by default (letting the idempotency token be filled in, or not sending one) can never create a bus at all. Nothing downstream is reachable: no subscriber can be attached, nothing can be published, and the 400 is reported as a client-side input error, so SDK retry logic will not retry it and the caller is told its own request was malformed when it was not. Against AWS the same request returns a bus in CREATING.

Fix: CreateEventBus's input validation — the service's pre-handler refusal table must not require the optional @idempotencyToken ClientToken on a create (the model-constraint walk stays as-is); a create that states no token must reach the handler, which must mint the bus and store it with no replay token (the handler already stores input.client_token as an Option and its token lookup is keyed off the stored value, so a bus minted without one simply has no replay record). If the service keeps any extra-model requirement at all, it must not apply to CreateEventBus, whose recorded answer to a tokenless create is 200 with the bus in CREATING. The same front door promotes ClientToken to required for CreateEventSource and CreateSubscriber; only CreateEventBus is evidenced by this fixture step.

Related OPEN issues that do NOT cover this fixture step and do NOT carry its replay key: 20261116T144630Z-sqs-the-eventbridgev2-subscriber-delivery-never-raises-queue.md and 20261114T091733Z-sqs-the-ses-configuration-set-delivery-event-never.md (they own the DOWNSTREAM sqs steps task#9/task#10 of this same fixture — queue depth and ReceiveMessage — not this create), 20261117T141912Z-eventbridgev2-6-net-new-wire-causes-event-bus.md (six other event-bus/event-source causes; none is CreateEventBus refusing a valid create), 20261111T025659Z-eventbridgev2-constraint-validation-refusals-answer-validationexception-with-no.md (refusals that answer the wrong error CLASS for requests AWS also refuses — here the request is one AWS ACCEPTS) and 20261115T124416Z-eventbridgev2-updateeventbus-mutations-never-reach-the-stored-bus.md (update persistence).

Traces: eventbridgev2-subscriber-delivers-published-event-to-sqs#task#4 (cluster of 1).

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health answered 200): 17 fixtures replayed, 200 steps, 113 passed, 78 mismatched, 9 blocked, 0 errored, score 56.5%, 30 divergence clusters. 29 of those clusters are already owned by open eventbridgev2/sqs issues; this is the one net-new, unowned cluster.

[mismatch] CreateEventBus, fixture eventbridgev2-subscriber-delivers-published-event-to-sqs, step task#4 (affected_count 1, divergence_key service=eventbridgev2|CreateEventBus|status=200->400|keys=$.CreationTime,$.EventBusArn,$.Name,$.State,$.StorageConfiguration,status):
  expected HTTP 200 with
    $.EventBusArn 'arn:aws:events:<region>:<account>:event-busv2/<bus name>/<25-char id>' (this run's value was arn:aws:events:us-west-2:000000000000:event-busv2/sim2real-probe-ebv2-bus-793b3eb5/et2wqptl9wxzahpx6vlnlh2jt — the name and id are generated per run, not the requirement)
    $.Name '<bus name>' (echo of the request's Name; 'sim2real-probe-ebv2-bus-793b3eb5' this run)
    $.State 'CREATING'
    $.StorageConfiguration {'RetentionPeriodInDays': 1, 'RetentionWindowStartTime': <shaped-not-asserted>}
    $.CreationTime <shaped-not-asserted>
  actual   HTTP 400 — "status: expected 200, got 400"; $.EventBusArn, $.Name, $.State, $.StorageConfiguration and $.CreationTime all reported missing (got None), i.e. an error body, not a create response.

The only 400s this build can produce for CreateEventBus are (a) the modeled LimitExceededException from the quota checks in src/ops/create_event_bus.rs (MAX_EVENT_BUSES = 100 per region, MAX_EVENT_BUSES_PER_EMULATOR = 1000 — far above the handful of buses this corpus creates, and later fixtures in the same replay still create buses successfully, so the quota is not what fired) and (b) InvalidInputException from the pre-handler front door in src/service/validation.rs. That front door enforces the model's own constraints — which this request satisfies, since AWS recorded a 200 for it — plus one extra-model rule: src/service/validation.rs:272-276 registers create_event_bus::CreateEventBus via requiring_client_token(|input| input.client_token.as_deref()), and src/service/validation.rs:160-171 answers invalid_input(CLIENT_TOKEN_REQUIRED) when crate::state::client_token_missing() is true; src/state.rs:140 defines that message as the fixed text "ClientToken is required". src/ops/create_event_bus.rs:216-219 documents the same rule ("A request that states no token at all is refused before it reaches here (crate::service::validation)").

Model references (services/service-com-amazonaws-eventbridgev2-awseventsv2/smithy-model/com-amazonaws-eventbridgev2-awseventsv2.json): CreateEventBusRequest.Name @required; CreateEventBusRequest.ClientToken is @idempotencyToken, @length 1-64, @pattern ^[\u0021-\u007E]+$ and is NOT @required, doc "The SDK generates one automatically when the field is omitted"; StorageConfiguration.RetentionPeriodInDays @range 1-365; BusState enum includes CREATING; CreateEventBus errors ConcurrentModificationException/409, IdempotentParameterMismatchException/409, InvalidStateException/409, LimitExceededException/400, ResourceAlreadyExistsException/409, AccessDeniedException/403, InvalidInputException/400, ThrottlingException/429, InternalException/500.

## Steps to Reproduce
0. git_clone the repo, then replay_wire_corpus (simparity-wire-aws-v1), which builds and boots the emulator from this checkout. The divergence is fixture eventbridgev2-subscriber-delivers-published-event-to-sqs, step task#4.

By hand against the running emulator with an rpc-v2-cbor EventBridgeV2 client (or a raw rpcv2Cbor POST to /service/AWSEventsV2/operation/CreateEventBus):
  a. Send CreateEventBus with ONLY the modeled-required input and an in-range storage configuration, and NO ClientToken member on the wire: {"Name": "<a name matching ^[A-Za-z0-9][\.\-_A-Za-z0-9]*$ that no existing bus uses>", "StorageConfiguration": {"RetentionPeriodInDays": 1}}.
  b. AWS answers HTTP 200 with EventBusArn (event-busv2/<name>/<25-char id>), Name echoed, State 'CREATING', StorageConfiguration {RetentionPeriodInDays: 1, RetentionWindowStartTime: <timestamp>} and CreationTime, and a DescribeEventBus poll then reaches ACTIVE.
  c. The emulator answers HTTP 400 with an error body and none of those members (__type 'InvalidInputException', Message "ClientToken is required" from src/state.rs:140).
  d. Re-send the identical request WITH a ClientToken (e.g. "tok-1") and it is accepted with 200 — confirming the refusal is driven by the absent optional idempotency token, not by any modeled constraint.
  e. Because the bus is never minted in (c), the rest of the flow (CreateSubscriber on that bus ARN, PutEvents, then SQS GetQueueAttributes/ReceiveMessage) has no bus to address.
