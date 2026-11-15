Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: f6d87ce57ca9b25a0fb0bb3a40dc7f114673a905

# Deduplicate the teardown-refusal rule shared by TagResource and UntagResource

_Filed: 2026-11-15T21:53:25Z_
_Operation: TagResource | Class: maintainability_

> _Note: this issue was identified against the code base at commit `f6d87ce57ca9b25a0fb0bb3a40dc7f114673a905`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The rule "a resource whose owning connector has been accepted for teardown may not have its tags changed" is implemented TWICE in this crate, once per tagging write handler, in two places each:

1. the lookup that decides whether the refusal applies (`store.tags_of(..).is_some()` gating `owning_connector(..).filter(is_deleting).map(|c| (c.arn.clone(), c.status.clone()))`, then the early return), and
2. the `invalid_state` builder that computes the sentence, including its two-way branch on `resource_arn == connector_arn` and its `crate::state::resource_type_of(resource_arn)` call.

Both copies are byte-for-byte the same logic with only the error-channel type differing (`TagResourceError` vs `UntagResourceError`), so the two can drift: a change to which statuses refuse a write, or to what the sentence names, applied to one handler and not the other would give the two sibling writes different answers for the same state.

This crate already centralises exactly this class of refusal sentence in `state.rs` — `resources_not_active_message` is documented there as "The one place this service words that refusal, for every operation that has such a precondition ... so the two cannot state one precondition in two sentences" — so the one-place-for-the-rule pattern is the crate's own, and these two handlers are the exception to it.

The fix is to hoist the lookup and the sentence into a single `state.rs` helper that both handlers call and map into their own error channel. Responses stay byte-identical (same 400, same `ValidationException`, same `Reason` `INVALID_STATE`, same message text), so the existing tests keep passing unchanged.

## Evidence

Copy 1 — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/tag_resource.rs:236-246:
```
        let under_teardown = if store.tags_of(&region, &resource_arn).is_some() {
            store
                .owning_connector(&region, &resource_arn)
                .filter(|connector| connector.is_deleting())
                .map(|connector| (connector.arn.clone(), connector.status.clone()))
        } else {
            None
        };
        if let Some((connector_arn, status)) = under_teardown {
            return Err(invalid_state(&resource_arn, &connector_arn, &status));
        }
```
and tag_resource.rs:308-322:
```
fn invalid_state(resource_arn: &str, connector_arn: &str, status: &str) -> TagResourceError {
    let message = if resource_arn == connector_arn {
        format!("Connector {connector_arn} has status {status}; its tags cannot be changed")
    } else {
        format!(
            "{} {resource_arn} belongs to connector {connector_arn}, which has status {status}; \
             its tags cannot be changed",
            crate::state::resource_type_of(resource_arn)
        )
    };
```

Copy 2 — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/untag_resource.rs:226-236:
```
        let under_teardown = if store.tags_of(&region, &resource_arn).is_some() {
            store
                .owning_connector(&region, &resource_arn)
                .filter(|connector| connector.is_deleting())
                .map(|connector| (connector.arn.clone(), connector.status.clone()))
        } else {
            None
        };
        if let Some((connector_arn, status)) = under_teardown {
            return Err(invalid_state(&resource_arn, &connector_arn, &status));
        }
```
and untag_resource.rs:278-292, identical bar the return type:
```
fn invalid_state(resource_arn: &str, connector_arn: &str, status: &str) -> UntagResourceError {
    let message = if resource_arn == connector_arn {
        format!("Connector {connector_arn} has status {status}; its tags cannot be changed")
```

The crate's own stated principle for a refusal sentence, services/.../src/state.rs:780-785:
```
/// The one place this service words that refusal, for every operation that has such a
/// precondition — `CreateConnector` over the `CertificateAuthorityArn` it links, and
/// `CreateChallenge` over the connector it mints a password under — so the two cannot state
/// one precondition in two sentences.
pub fn resources_not_active_message(resource_arn: &str) -> String {
```

Test coverage that must stay green and unchanged:
- tests/all/tag_write_under_teardown.rs:346-372 asserts `(400, "ValidationException")`, `Reason == INVALID_STATE`, and that the message contains the connector ARN, `DELETING`, and (for the challenge rows) the challenge ARN.
- tests/all/untag_resource.rs:799-819 asserts the same三 for `UntagResource`: `(400, "ValidationException")`, `Reason == REASON_INVALID_STATE`, message containing the connector ARN and `DELETING`.

## Steps to Reproduce

1. Open services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/tag_resource.rs and read lines 236-246 and 308-322.
2. Open services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/untag_resource.rs and read lines 226-236 and 278-292: the teardown lookup and the message-building branch are the same code twice, differing only in the error type named.
3. Compare with src/state.rs:768-796, where the crate keeps its other precondition refusal sentences in one place for exactly this reason.
4. Hoist the lookup plus sentence into one state.rs helper both handlers call; re-run tests/all/tag_write_under_teardown.rs and tests/all/untag_resource.rs, which must pass with no edits.
