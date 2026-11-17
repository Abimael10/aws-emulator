Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: xray.update-trace-segment-destination-invalid-value-fails
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray.update-trace-segment-destination-invalid-value-fails|run#11|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: 00a0ac935e2138d6f06f7a4aca8a94ec16848545

# xray: UpdateTraceSegmentDestination ValidationException omits Composite from the TraceSegmentDestination enum value set

> _Note: this issue was identified against the code base at commit `00a0ac935e2138d6f06f7a4aca8a94ec16848545`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateTraceSegmentDestination

## Description
One net-new behavioral root cause in services/service-com-amazonaws-xray-awsxray, from the behavioral suite (simparity-behavioral-aws-v1). The check is not marked ALREADY FILED and not marked OWNED by any open issue. The open issue 20261022T142349Z-xray-x-ray-group-and-trace-segment-destination-responses.md covers neighbouring trace-segment-destination defects (the same-value refusal, the PENDING/ACTIVE settling, the CloudWatchLogs permission precondition) — all of those now pass in this check, which gets past them and fails only on the enum value set reported for an out-of-enum destination. That issue does not describe this cause.

Cause 1 of 1: the enum value set in the out-of-enum ValidationException message is missing the 'Composite' destination.
AWS: UpdateTraceSegmentDestination rejects a Destination outside the TraceSegmentDestination enum with HTTP 400, error code ValidationException and the standard constraint message "1 validation error detected: Value '<supplied-value>' at 'destination' failed to satisfy constraint: Member must satisfy enum value set: [Composite, CloudWatchLogs, XRay]". The fixed text is everything but the quoted value: '1 validation error detected: Value ', then "' at 'destination' failed to satisfy constraint: Member must satisfy enum value set: [Composite, CloudWatchLogs, XRay]"; the only variable part is the rejected value copied verbatim from the request's Destination member. The enum list is fixed text belonging to the service's TraceSegmentDestination enum and is rendered in AWS's order (Composite, CloudWatchLogs, XRay). Smithy: the on-disk model services/service-com-amazonaws-xray-awsxray/smithy-model/com-amazonaws-xray-awsxray.json declares com.amazonaws.xray#TraceSegmentDestination with values [XRay, CloudWatchLogs] only, and UpdateTraceSegmentDestination models InvalidRequestException (400) / ThrottledException (429) — so the ValidationException class and the third enum member are recorded behavior only: the checked-in model is stale relative to the live service, which also accepts Composite. Status 400 and code ValidationException already match.
Emulator: the framework's pre-handler enum validation answers ValidationException with "1 validation error detected: Value 'NotARealDestination' at 'destination' failed to satisfy constraint: Member must satisfy enum value set: [CloudWatchLogs, XRay]" — the same template, but with a two-member value set that omits Composite and uses the model's declaration order. (Consequently the emulator also refuses Destination='Composite', a value AWS accepts; the handler's own out-of-enum branch at update_trace_segment_destination.rs never runs because the schema validation rejects first.)
Why: a client that parses the enum value set out of the validation message to discover the destinations the account may be switched to is told Composite does not exist, and any client that actually tries to enable the Composite destination — the dual-write mode Transaction Search uses — is rejected with a validation error instead of performing the switch. The enum is part of the API contract, so an emulator that advertises a short set makes the operation unusable for that destination and makes negative-path assertions on the message text fail.
Fix: the TraceSegmentDestination enum this service validates and serializes against — add Composite as an accepted member (updating the enum in the service's Smithy model / the TRACE_SEGMENT_DESTINATION_VALUES constant the handler and the input schema share) so the schema-level validation message lists [Composite, CloudWatchLogs, XRay] in that order, and so GetTraceSegmentDestination/UpdateTraceSegmentDestination can carry it; the destination state machine must treat Composite like any other destination.
Traces: xray.update-trace-segment-destination-invalid-value-fails.

## Evidence
Check xray.update-trace-segment-destination-invalid-value-fails [mismatch], check_xray_update_trace_segment_destination_invalid_value_fails.py line 65:
AssertionError: expected message "1 validation error detected: Value 'NotARealDestination' at 'destination' failed to satisfy constraint: Member must satisfy enum value set: [Composite, CloudWatchLogs, XRay]", got "1 validation error detected: Value 'NotARealDestination' at 'destination' failed to satisfy constraint: Member must satisfy enum value set: [CloudWatchLogs, XRay]"

The earlier assertions in the same check passed: GetTraceSegmentDestination settled to Status ACTIVE, and the same-value update was refused with code InvalidRequestException and message "The destination is already set to <current destination>". Error code (ValidationException) and HTTP status (400) for the out-of-enum call match; only the enum value set inside the message diverges.

Model evidence: smithy_query(query="shape", shape="TraceSegmentDestination", model_path="services/service-com-amazonaws-xray-awsxray/smithy-model/com-amazonaws-xray-awsxray.json") -> {"type":"enum","values":["XRay","CloudWatchLogs"]} — the emulator's message is a faithful rendering of a stale enum. Handler: services/service-com-amazonaws-xray-awsxray/src/ops/update_trace_segment_destination.rs lines 189-200 build the out-of-enum message from TRACE_SEGMENT_DESTINATION_VALUES (same two members) and are unreachable because the input-schema enum validation rejects first.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite (simparity-behavioral-aws-v1); emulator_up true, bootstrap_signal false.
2. Call GetTraceSegmentDestination and poll until Status == ACTIVE; note the current Destination.
3. Call UpdateTraceSegmentDestination(Destination=<the current destination>) and confirm it is refused with InvalidRequestException "The destination is already set to <current destination>" (this part passes).
4. Call UpdateTraceSegmentDestination(Destination='NotARealDestination'). Expect HTTP 400 / ValidationException with message "1 validation error detected: Value 'NotARealDestination' at 'destination' failed to satisfy constraint: Member must satisfy enum value set: [Composite, CloudWatchLogs, XRay]"; observe the same message with the value set "[CloudWatchLogs, XRay]".
5. Also call UpdateTraceSegmentDestination(Destination='Composite'): AWS accepts the switch, the emulator rejects it with the same ValidationException.
6. Re-replay check xray.update-trace-segment-destination-invalid-value-fails to confirm the fix.
