Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda|CreateEventSourceMapping|2xx->2xx|$.LastModified
Issue-Key: lambda|GetEventSourceMapping|2xx->2xx|$.LastModified
Issue-Key: lambda|UpdateEventSourceMapping|2xx->2xx|$.LastModified
Issue-Key: lambda|UpdateEventSourceMapping|2xx->2xx|$.State
Issue-Key: lambda|DeleteEventSourceMapping|2xx->2xx|$.LastModified
Issue-Key: lambda|DeleteEventSourceMapping|2xx->2xx|$.LastProcessingResult
Issue-Related: 20261103T165236Z-lambda-event-source-mapping-payloads-omit-fields-and.md
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# Lambda SQS event source mapping responses carry a raw epoch LastModified, a fabricated LastProcessingResult and a wrong disabling State

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Every EventSourceMappingConfiguration the emulator serializes for an SQS-sourced mapping diverges from AWS in three ways on the same shared projection: LastModified is emitted as a bare high-precision floating-point counter value instead of the modelled timestamp form AWS puts on the wire, LastProcessingResult is fabricated as 'OK' on a mapping for which AWS reports no last-processing result at all, and the State an Enabled=false UpdateEventSourceMapping reports is the generic 'Updating' instead of the disable-specific 'Disabling'. 5 causes, one per diverging response, all in the event-source-mapping projection/serializer and the update handler's state transition. All five are on fixture lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found.

Cause 1 of 5: CreateEventSourceMapping serializes LastModified as a raw epoch float.
AWS: Smithy: CreateEventSourceMapping output is EventSourceMappingConfiguration; member LastModified targets com.amazonaws.lambda#Date (type timestamp), documented as 'The date that the event source mapping was last updated or that its state changed'. Recorded behavior only: the recorded 202 response's LastModified has the service's timestamp form, and the corpus asserts that member by shape (its instant is volatile and is not asserted); the emulator's value does not satisfy that shape. The precedent is already established in this emulator by the resolved issue 20261023T022015Z-lambda-core-getnetworkconnector-response-omits-statereason-and-serializes.md, 'serializes LastModified as an epoch number' — the same member family must be rendered through the service's timestamp serializer rather than dumped as a numeric seconds value.
Emulator: returns 202 with LastModified as a bare JSON number carrying microsecond-level fractional digits taken straight from an internal clock counter (the recorded run shows a value of the form 1794916978.3990493, and the next step in the same fixture is exactly half a second later, i.e. the number is an internal counter reading, not a serialized timestamp).
Why: a client that deserializes LastModified through the modelled timestamp type gets a value whose form the service never emits; code that renders or compares the mapping's last-change time against other Lambda responses (FunctionConfiguration.LastModified, which this emulator renders as a formatted instant) sees two incompatible representations of the same concept and cannot order them.
Fix: the event-source-mapping response serializer — route LastModified through the service's Date/timestamp serialization instead of writing the stored clock value directly.
Traces: lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found#task#7 (CreateEventSourceMapping, 202 vs 202).

Cause 2 of 5: GetEventSourceMapping serializes the same raw epoch float on the read path.
AWS: Smithy: GetEventSourceMapping output is the same EventSourceMappingConfiguration with the same LastModified -> Date member; the recorded 200 response asserts the same shape.
Emulator: returns 200 with the same bare high-precision number, confirming the defect is in the shared projection rather than in the create handler alone.
Why: as cause 1, on the read a client uses to reconcile mapping state.
Fix: as cause 1 (shared projection, so one serializer change should cover both).
Traces: lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found#task#8 (GetEventSourceMapping, 200 vs 200).

Cause 3 of 5: UpdateEventSourceMapping with Enabled=false reports State 'Updating' instead of 'Disabling', fabricates LastProcessingResult='OK', and repeats the raw LastModified.
AWS: Smithy: EventSourceMappingConfiguration.State (String) is documented as one of Creating, Enabling, Enabled, Disabling, Disabled, Updating or Deleting; an update that disables a mapping moves it through the disable-specific transient state, and the recorded 202 response for this step carries State='Disabling'. LastProcessingResult (String, 'The result of the event source mapping's last processing attempt') is an optional member and is absent from the recorded response for this mapping — AWS reports no last-processing result here.
Emulator: returns 202 with State='Updating', with LastProcessingResult='OK' present, and with LastModified as the same bare counter number.
Why: a client that disables a mapping and polls State until it leaves the disabling transient never observes the documented Disabling -> Disabled path and cannot distinguish a disable from any other configuration update; a client that reads LastProcessingResult to report mapping health is told the last poll succeeded for a mapping whose processing result the service does not report, which hides a non-polling or never-polled mapping behind a healthy-looking value.
Fix: the UpdateEventSourceMapping handler's state transition (pick the disable transient when the request clears Enabled) and the event-source-mapping projection, which must omit LastProcessingResult rather than defaulting it to a success token.
Traces: lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found#task#13 (UpdateEventSourceMapping, 202 vs 202).

Cause 4 of 5: GetEventSourceMapping after the disable still fabricates LastProcessingResult='OK' and repeats the raw LastModified.
AWS: as cause 3 — the recorded 200 read-back carries no LastProcessingResult member and a shaped LastModified.
Emulator: returns 200 with LastProcessingResult='OK' and the bare counter LastModified.
Why: as cause 3; the fabricated success token is durable in state, not just in the update response, so every subsequent read repeats it.
Fix: as cause 3 (projection side).
Traces: lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found#task#14 (GetEventSourceMapping, 200 vs 200).

Cause 5 of 5: DeleteEventSourceMapping's final configuration payload carries the same fabricated LastProcessingResult and raw LastModified.
AWS: Smithy: DeleteEventSourceMapping also returns EventSourceMappingConfiguration (http code 202); the recorded response carries no LastProcessingResult and a shaped LastModified.
Emulator: returns 202 with LastProcessingResult='OK' and the bare counter LastModified.
Why: the delete response is the last description of the mapping a client ever receives; it is the one most often logged or asserted on, and it reports a processing result the service never reported plus a timestamp in a form it never emits.
Fix: as causes 1 and 3 — both defects are in the shared projection used by the delete path too.
Traces: lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found#task#19 (DeleteEventSourceMapping, 202 vs 202).

Note: the open issue 20261103T165236Z-lambda-event-source-mapping-payloads-omit-fields-and.md covers the same projection for a DynamoDB-stream mapping (omitted members, wrong initial State, FilterCriteria storage) and the open issue 20261029T233239Z-lambda-event-source-mappings-are-created-for-sources.md covers create-time refusals, but neither carries any of these five clusters' replay keys: there the defect is a member AWS sends and the emulator omits, here it is a member AWS omits and the emulator fabricates, plus the timestamp serialization and the disable transition.

## Evidence
Fixture lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found, replayed against the emulator in trace set simparity-wire-aws-v1:

Cause 1 - task#7 CreateEventSourceMapping, status 202 expected and 202 returned: $.LastModified value_mismatch - expected the recorded (shape-asserted) timestamp form, got the JSON number 1794916978.3990493.
Cause 2 - task#8 GetEventSourceMapping, 200 vs 200: $.LastModified value_mismatch - expected the shape-asserted timestamp form, got 1794916978.8990493 (exactly 0.5 s after the create reading, i.e. an internal counter).
Cause 3 - task#13 UpdateEventSourceMapping (Enabled=false), 202 vs 202: $.State value_mismatch - expected 'Disabling', got 'Updating'; $.LastProcessingResult type_mismatch - expected absent (None), got 'OK'; $.LastModified value_mismatch - expected the shape-asserted timestamp form, got 1794916988.4686453.
Cause 4 - task#14 GetEventSourceMapping, 200 vs 200: $.LastProcessingResult type_mismatch - expected absent (None), got 'OK'; $.LastModified value_mismatch - got 1794916988.9686453.
Cause 5 - task#19 DeleteEventSourceMapping, 202 vs 202: $.LastProcessingResult type_mismatch - expected absent (None), got 'OK'; $.LastModified value_mismatch - got 1794917053.522071.

The quoted numeric instants are per-run values and are not themselves requirements; the requirements are the serialized FORM of LastModified, the ABSENCE of LastProcessingResult for a mapping whose processing result AWS does not report, and the State token 'Disabling' for an update that disables the mapping.

Model evidence: Smithy com.amazonaws.lambda#EventSourceMappingConfiguration - LastModified -> com.amazonaws.lambda#Date (timestamp); LastProcessingResult -> String, optional, doc 'The result of the event source mapping's last processing attempt'; State -> String, doc 'It can be one of the following: Creating, Enabling, Enabled, Disabling, Disabled, Updating, or Deleting'. The same output shape is bound to CreateEventSourceMapping (POST /2015-03-31/event-source-mappings, 202), GetEventSourceMapping (GET .../{UUID}, 200), UpdateEventSourceMapping (PUT .../{UUID}, 202) and DeleteEventSourceMapping (DELETE .../{UUID}, 202).

## Steps to Reproduce
1. Replay fixture lambda-sqs-event-source-mapping-explicit-defaults-delivers-then-disabled-tags-and-not-found against the emulator.
2. At task#7 (CreateEventSourceMapping on the SQS queue) and task#8 (GetEventSourceMapping) inspect $.LastModified in the JSON body: the emulator writes a bare floating-point seconds value with microsecond digits instead of the service's timestamp form.
3. At task#13 (UpdateEventSourceMapping with Enabled=false) compare $.State ('Updating' returned, 'Disabling' recorded) and check that $.LastProcessingResult is present as 'OK' when the recorded response has no such member.
4. At task#14 (GetEventSourceMapping read-back) confirm the fabricated LastProcessingResult='OK' persists in state, and $.LastModified is still a raw number.
5. At task#19 (DeleteEventSourceMapping) confirm the final 202 payload repeats both defects.
6. All five steps must pass before the issue is closed - re-replay every fixture#step listed in the Traces lines, not just task#7.
