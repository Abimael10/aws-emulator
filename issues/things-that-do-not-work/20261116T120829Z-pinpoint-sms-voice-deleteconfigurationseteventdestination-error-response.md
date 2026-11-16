Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 620a92e1f100d1212f8dcca86858ecf2965928fa

# Two-label refusal reports only the FIRST unusable path label, though the handler comment says "Both labels are read" and the wording helper is built to list them together

_Filed: 2026-11-16T12:08:29Z_
_Operation: DeleteConfigurationSetEventDestination | Class: error-response_

> _Note: this issue was identified against the code base at commit `620a92e1f100d1212f8dcca86858ecf2965928fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`DeleteConfigurationSetEventDestination` and `UpdateConfigurationSetEventDestination` are the only two operations of this service with TWO `@required` URI labels (`ConfigurationSetName` and `EventDestinationName`). Both handlers walk the labels in a `for` loop and `return Err(...)` on the first one that is unusable, so when BOTH labels are unusable the refusal names only `ConfigurationSetName`.

That contradicts two statements the crate makes about this very refusal:

1. The handler comment directly above the loop states "Both labels are read, in the order the input declares them" — with the early `return` the second label is never read at all once the first fails.
2. `ops/types/refusal_wording.rs` builds both sentences from a SLICE of member names precisely so several can be listed in one sentence, and documents the reason: "they are listed in the order given — the order the operation's input declares them, so a request missing two parameters reads the same way every time". That documented two-parameter sentence can never be produced: `unusable_required_label` takes a single member and every call site in the crate passes exactly one name.

So the one place in this crate where the multi-member sentence applies is the one place the code cannot emit it. Expected, per the crate's own stated rule: one `BadRequestException` naming both unusable parameters in input-declaration order. Actual: a sentence naming only the first.

## Evidence

Rule stated by the wording module — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/types/refusal_wording.rs:28-35:
```
/// The refusal for `members`: `@required` request parameters that arrived with no value.
///
/// Each name is the parameter's modelled member name, as this layer reports it, and they
/// are listed in the order given — the order the operation's input declares them, so a
/// request missing two parameters reads the same way every time.
pub fn missing_request_parameters(members: &[&str]) -> String {
```
and refusal_wording.rs:38-42 for the other class: "`members` are MODELLED member names; each is rendered through [`parameter_name`] and single-quoted, in the order given."

The single-member funnel — refusal_wording.rs:66-73:
```
pub fn unusable_required_label(member: &str, value: &str) -> Option<String> {
    if value.is_empty() {
        return Some(missing_request_parameters(&[member]));
    }
    if value.trim().is_empty() {
        return Some(invalid_parameters(&[member]));
    }
```

Comment vs. code in the handler — src/ops/delete_configuration_set_event_destination.rs:136-157:
```
// A `@required` path label the request binds no usable value to names nothing to
// delete, so it is refused ahead of any lookup ... Both labels are read, in the
// order the input declares them, and the layer (and so the sentence) is
// `refusal_wording`'s to decide.
for (member, value) in [
    ("ConfigurationSetName", &input.configuration_set_name),
    ("EventDestinationName", &input.event_destination_name),
] {
    if let Some(message) =
        super::types::refusal_wording::unusable_required_label(member, value)
    {
        return Err(
            DeleteConfigurationSetEventDestinationErrorModeled::BadRequestException(
```
The identical loop, with the identical "Both labels are read" comment, is src/ops/update_configuration_set_event_destination.rs:140-161. Every other call site also passes exactly one name (src/ops/create_configuration_set.rs:181, src/ops/create_configuration_set_event_destination.rs:158 and :183, src/ops/send_voice_message.rs:178, src/ops/list_configuration_sets.rs:188, src/ops/delete_configuration_set.rs:139, src/ops/get_configuration_set_event_destinations.rs:147), so the list form is unreachable crate-wide.

Test coverage: tests/all/parameter_refusal_wording.rs:198-238 makes exactly ONE label unusable per row ("GetConfigurationSetEventDestinations with an EMPTY path label" → `Missing required request parameters: [ConfigurationSetName]` at :170-171, "CreateConfigurationSetEventDestination with a BLANK path label" → `The following parameter is null, empty or invalid: ['configurationSetName']` at :177-178); no row of any suite sends a request whose two labels are both unusable, so nothing pins the first-only behaviour.

## Steps to Reproduce

1. Boot the emulator (the entitlement is irrelevant: request-shape validation is answered ahead of the authorization decision, src/ops/types/authorization.rs:26-29).
2. Send `DELETE /v1/sms-voice/configuration-sets/%20/event-destinations/%20` — both `@required` labels bind a BLANK value, so both are unusable by src/ops/types/refusal_wording.rs:66-73.
3. Observe HTTP 400 `BadRequestException` with message `The following parameter is null, empty or invalid: ['configurationSetName']` — only the first label is named, although the crate's own rule (refusal_wording.rs:28-35, :38-42) is that the unusable parameters are listed together in input-declaration order, and the handler comment above the loop claims "Both labels are read".
4. Repeat with `PUT /v1/sms-voice/configuration-sets/%20/event-destinations/%20` (body `{}`) for the identical loop in src/ops/update_configuration_set_event_destination.rs:140-161.
