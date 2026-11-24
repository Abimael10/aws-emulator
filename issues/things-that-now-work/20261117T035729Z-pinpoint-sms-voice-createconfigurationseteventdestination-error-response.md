Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 4619b37f3214bdc77fb47fb6f6837aa386bbff25

# A CreateConfigurationSetEventDestination refusal names only the unusable path label, never the unusable EventDestinationName in the same request

_Filed: 2026-11-17T03:57:29Z_
_Operation: CreateConfigurationSetEventDestination | Class: error-response_

> _Note: this issue was identified against the code base at commit `4619b37f3214bdc77fb47fb6f6837aa386bbff25`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states one rule for how a refusal is worded: a request is refused ONCE and the single sentence names EVERY parameter the refusing layer cannot use, in the order the input binds them. `CreateConfigurationSetEventDestination` breaks that rule for a request whose `{ConfigurationSetName}` path label AND whose body `EventDestinationName` are both unusable. The handler words the label refusal first and returns, so the sentence reads "The following parameter is null, empty or invalid: ['configurationSetName']" and never mentions `eventDestinationName`, even though both values fail in the SAME layer (this service's own validation) and are worded by the same helper, `refusal_wording::invalid_parameters`. The sibling two-label handlers (`Delete`/`Update...EventDestination`) hand both unusable parameters to the wording helper together and do produce the combined sentence "['configurationSetName', 'eventDestinationName']"; this handler is the one path that still reports the first failure it looked at. The fix is to collect both unusable parameters before wording the refusal, exactly as the two-label handlers do.

## Evidence

Rule stated in the crate's own wording module — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/types/refusal_wording.rs:23-25: "[`unusable_required_labels`] is which of the two a request's URI-bound parameters get, so the layer split is decided in one place rather than per handler. It is handed ALL of a request's `@required` labels before it words anything, because a sentence names every parameter the request failed on, not merely the first one looked at." and refusal_wording.rs:66-70: "A request is refused ONCE, so the one sentence names EVERY label unusable in the layer that refused it".

Same rule stated by the suite that pins the wording — tests/all/parameter_refusal_wording.rs:26-28: "A request is refused ONCE, so where SEVERAL parameters are unusable the one sentence names every one of them that the refusing layer cannot use, in the order the input binds them — which the two-label operations are where this can be seen."

The handler that breaks it — src/ops/create_configuration_set_event_destination.rs:158-170 refuses on the label alone and returns:
```
if let Some(message) = super::types::refusal_wording::unusable_required_labels(&[(
    "ConfigurationSetName",
    input.configuration_set_name.as_str(),
)]) {
    return Err(
        CreateConfigurationSetEventDestinationErrorModeled::BadRequestException(
            BadRequestException { message: Some(message) },
        )
        .into(),
    );
}
```
and only AFTER that return point does it look at the second parameter — create_configuration_set_event_destination.rs:177-188:
```
let destination_name = input
    .event_destination_name
    .filter(|name| !name.trim().is_empty())
    .ok_or_else(|| { ... refusal_wording::invalid_parameters(&["EventDestinationName"]) ... })?;
```
So a request with both unusable is answered with the one-name sentence.

Sibling path that follows the rule — src/ops/delete_configuration_set_event_destination.rs:142-151 hands BOTH unusable parameters over together: `unusable_required_labels(&[("ConfigurationSetName", ...), ("EventDestinationName", ...)])`, and tests/all/parameter_refusal_wording.rs:499-513 pins the combined sentence "The following parameter is null, empty or invalid: ['configurationSetName', 'eventDestinationName']" for those operations.

Test coverage: tests/all/parameter_refusal_wording.rs:310-322 covers `CreateConfigurationSetEventDestination` with no / with a blank `EventDestinationName` only, always under an existing, usable set path ("/v1/sms-voice/configuration-sets/refusal-wording-set/event-destinations"); no row makes the label AND the body member unusable in the same request, which is why the divergence is not caught.

## Steps to Reproduce

1. Boot the emulator and grant the account the entitlement the resource-scoped actions need (as tests/all/parameter_refusal_wording.rs:101-104 does: set `legacy_surface_entitled = true`).
2. POST /v1/sms-voice/configuration-sets/%20/event-destinations with body {"EventDestinationName": "  ", "EventDestination": {"Enabled": true}} — both the `{ConfigurationSetName}` label and `EventDestinationName` are blank, so both are unusable by this service's validation.
3. Observe HTTP 400 BadRequestException with message "The following parameter is null, empty or invalid: ['configurationSetName']" — `eventDestinationName` is missing from the sentence.
4. Compare with the same pair of blank values on DELETE /v1/sms-voice/configuration-sets/%20/event-destinations/%20, which answers the combined "The following parameter is null, empty or invalid: ['configurationSetName', 'eventDestinationName']" (tests/all/parameter_refusal_wording.rs:499-513).
