Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 578593829c1a368ba20df7789ec02dda7b0cd956

# Tag-write state refusal omits the "Validation error detected: " scaffold state.rs declares every precondition refusal opens with

_Filed: 2026-11-17T17:15:39Z_
_Operation: TagResource | Class: error-response_

> _Note: this issue was identified against the code base at commit `578593829c1a368ba20df7789ec02dda7b0cd956`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

state.rs declares one scaffold for every sentence this service refuses a *precondition* with (a well-formed request naming a resource whose state does not admit the call): the constant `VALIDATION_ERROR_PREFIX` = "Validation error detected: ", whose doc says it is "The opening of every sentence this service refuses a PRECONDITION with" and that "the builders below are the clauses this service states, so a caller reads one scaffold whichever precondition refused the call".

Two of the three builders under that comment honour it: `resources_not_active_message` (used by CreateChallenge over a non-ACTIVE connector and by CreateConnector over a non-ACTIVE CA) and `resource_cannot_be_in_state_message` (used by ListChallengeMetadata over a FAILED connector) both emit `Validation error detected: <clause>: <arn>.`

The third builder, `tags_cannot_be_changed_message` — written between those two, and the sentence the tag writes are refused with when the owning connector's teardown has been accepted — emits neither the prefix nor the terminating period: "Connector <arn> has status DELETING; its tags cannot be changed". It is lifted into exactly the same error class and the same modelled reason as the other two (`ValidationException` with `Reason: INVALID_STATE`), so a client that reads the service's precondition scaffold sees it on two of the three refusals and not on this one. `UntagResource` is refused by the same helper through the same `tag_write_refusal` path, so both tagging writes carry the divergent sentence.

## Evidence

The rule, services/.../src/state.rs:797-803:
```
/// The opening of every sentence this service refuses a PRECONDITION with: a request whose
/// input is well-formed but names resources whose state does not admit the call.
///
/// The clause that follows says which precondition failed and over which resource; the
/// builders below are the clauses this service states, so a caller reads one scaffold
/// whichever precondition refused the call.
const VALIDATION_ERROR_PREFIX: &str = "Validation error detected: ";
```

The two builders that follow it, state.rs:814 and state.rs:850:
```
    format!("{VALIDATION_ERROR_PREFIX}Resource(s) are not active: {resource_arn}.")
...
    format!("{VALIDATION_ERROR_PREFIX}Resource cannot be in state {status}: {resource_arn}.")
```

The builder that breaks it, state.rs:827-840:
```
fn tags_cannot_be_changed_message(
    resource_arn: &str,
    connector_arn: &str,
    status: &str,
) -> String {
    if resource_arn == connector_arn {
        format!("Connector {connector_arn} has status {status}; its tags cannot be changed")
    } else {
        format!(
            "{} {resource_arn} belongs to connector {connector_arn}, which has status {status}; \
             its tags cannot be changed",
            resource_type_of(resource_arn)
        )
    }
}
```

It is the same error class and reason as the prefixed refusals — services/.../src/ops/tag_resource.rs:303-309:
```
fn invalid_state(message: String) -> TagResourceError {
    TagResourceErrorModeled::ValidationException(ValidationException {
        message,
        reason: Some(crate::state::REASON_INVALID_STATE.to_string()),
    })
```
and services/.../src/ops/untag_resource.rs:273-279 lifts the identical sentence for `UntagResource`.

Test coverage: the current (unprefixed) sentence is asserted in tests/all/tag_write_state_agreement.rs:238-248, whose own comment states only that it is "spelled here as a wire expectation rather than read back out of the implementation" — it gives no reason for diverging from the service's declared precondition scaffold. The prefixed form is asserted for the sibling refusals in tests/all/connector_ca_active_precondition.rs:52, tests/all/connector_provisioning_authorization.rs:59 and :64, and tests/all/connector_private_ca_reachability.rs:350.

## Steps to Reproduce

1. CreateConnector with an ACTIVE CA, wait for the connector to settle to ACTIVE, then call DeleteConnector so it reports Status DELETING.
2. Call TagResource on that connector ARN with any valid tag. Observe 400 ValidationException whose Message is `Connector <arn> has status DELETING; its tags cannot be changed` — no `Validation error detected: ` opening and no terminating period.
3. For contrast, call CreateChallenge against that same DELETING connector: 400 ValidationException whose Message is `Validation error detected: Resource(s) are not active: <arn>.` — the same error class and the same `Reason: INVALID_STATE`, but carrying the scaffold state.rs:797-803 says every precondition refusal opens with.
4. Repeat step 2 with UntagResource (and with a challenge ARN of that connector) to see the same unprefixed sentence from the shared `tag_write_refusal` helper.
