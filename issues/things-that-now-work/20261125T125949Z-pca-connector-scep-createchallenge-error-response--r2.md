Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: bcebc9bd412408dab15ac6f3ee97594b2c3118d1

# CreateChallenge's INVALID_CONNECTOR_TYPE refusal is worded inline and omits the "Validation error detected: " scaffold state.rs declares every precondition refusal opens with

_Filed: 2026-11-25T12:59:49Z_
_Operation: CreateChallenge | Class: error-response_

> _Note: this issue was identified against the code base at commit `bcebc9bd412408dab15ac6f3ee97594b2c3118d1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`state.rs` declares ONE opening for every sentence this service refuses a precondition with — `VALIDATION_ERROR_PREFIX = "Validation error detected: "` — and its doc states that the builders under it are "the clauses this service states, so a caller reads one scaffold whichever precondition refused the call".

`CreateChallenge` holds a caller to TWO preconditions over the connector its `ConnectorArn` names, and words them two different ways:

* the connector must be GENERAL_PURPOSE — refused with a `format!` written inline in the handler, carrying neither the declared scaffold nor a terminating period: `"Connector <arn> has type INTUNE; challenge passwords can only be created for GENERAL_PURPOSE connectors"`;
* the connector must be ACTIVE — refused ten lines further down through `crate::state::resources_not_active_message`, which DOES carry it: `"Validation error detected: Resource(s) are not active: <arn>."`

Both are the same error class (`ValidationException`, HTTP 400) over the same named connector, and both carry a `Reason` out of the same `ValidationExceptionReason` enum (`INVALID_CONNECTOR_TYPE` / `INVALID_STATE`). So a client that reads this service's declared precondition scaffold gets it from one branch of this one handler and not from the other, and the type branch is the only refusal sentence in the crate that is written inline in a handler instead of by a state.rs builder.

This is distinct from the two issues that already touch this area: the FIXED CreateChallenge/error-response issue (20261116T052430Z) was about the `ResourceType` member of `ResourceNotFoundException`, and the OPEN TagResource/error-response issue (20261117T171539Z) enumerates the three state.rs *builders* and covers `tags_cannot_be_changed_message` — it does not mention this fourth, inline-worded precondition refusal in `create_challenge.rs`.

Nothing states a reason for the divergence: the only test that exercises this branch asserts the status, the error code and `Reason`, and never looks at `Message`.

## Evidence

The rule the crate declares — services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/state.rs:889-895:
```
/// The opening of every sentence this service refuses a PRECONDITION with: a request whose
/// input is well-formed but names resources whose state does not admit the call.
///
/// The clause that follows says which precondition failed and over which resource; the
/// builders below are the clauses this service states, so a caller reads one scaffold
/// whichever precondition refused the call.
const VALIDATION_ERROR_PREFIX: &str = "Validation error detected: ";
```

The builders that honour it — src/state.rs:905-907 and src/state.rs:941-943:
```
pub fn resources_not_active_message(resource_arn: &str) -> String {
    format!("{VALIDATION_ERROR_PREFIX}Resource(s) are not active: {resource_arn}.")
}
...
pub fn resource_cannot_be_in_state_message(status: &str, resource_arn: &str) -> String {
    format!("{VALIDATION_ERROR_PREFIX}Resource cannot be in state {status}: {resource_arn}.")
}
```

The branch that breaks it — src/ops/create_challenge.rs:274-284:
```
        if connector.connector_type != crate::state::TYPE_GENERAL_PURPOSE {
            return Err(CreateChallengeErrorModeled::ValidationException(ValidationException {
                message: format!(
                    "Connector {connector_arn} has type {}; challenge passwords can only be created for {} connectors",
                    connector.connector_type,
                    crate::state::TYPE_GENERAL_PURPOSE
                ),
                reason: Some(crate::state::REASON_INVALID_CONNECTOR_TYPE.to_string()),
            })
            .into());
        }
```

The sibling refusal in the SAME handler, which does use the scaffold — src/ops/create_challenge.rs:291-297:
```
        if !connector.is_active() {
            return Err(CreateChallengeErrorModeled::ValidationException(ValidationException {
                message: crate::state::resources_not_active_message(&connector_arn),
                reason: Some(crate::state::REASON_INVALID_STATE.to_string()),
            })
            .into());
        }
```

Test coverage, and the absence of a stated reason — tests/all/connector_lifecycle.rs:1545-1609, `challenge_creation_is_refused_outside_an_active_general_purpose_connector`, drives exactly these two branches (`"INVALID_CONNECTOR_TYPE"` at :1559 and `"INVALID_STATE"` at :1564) and asserts only:
```
        assert_eq!(answer.status, 400, ...);
        assert_eq!(answer.error_type, "ValidationException", ...);
        assert_eq!(answer.body["Reason"], json!(expected_reason), ...);
```
It never asserts `Message`, so the inline sentence is pinned by nothing. The prefixed form IS pinned for the sibling refusals: tests/all/connector_ca_active_precondition.rs:52 and tests/all/connector_provisioning_authorization.rs:59 / :64 both spell `"Validation error detected: Resource(s) are not active: {resource_arn}."` / `"Validation error detected: Resource cannot be in state {status}: {resource_arn}."` as wire expectations.

## Steps to Reproduce

1. CreateConnector with an ACTIVE `CertificateAuthorityArn` and a `MobileDeviceManagement.Intune` configuration; let the clock settle the connector to ACTIVE (`harness::release_pending`).
2. POST /challenges with `{"ConnectorArn": "<that connector>"}`.
3. Observe 400 `ValidationException` with `Reason: "INVALID_CONNECTOR_TYPE"` and `Message: "Connector <arn> has type INTUNE; challenge passwords can only be created for GENERAL_PURPOSE connectors"` — no `Validation error detected: ` opening and no terminating period.
4. For contrast, repeat against a general-purpose connector whose CREATING transition is held undue (`harness::hold_pending`): the same operation, the same 400 `ValidationException`, `Reason: "INVALID_STATE"`, and `Message: "Validation error detected: Resource(s) are not active: <arn>."` — the scaffold src/state.rs:889-895 says every precondition refusal opens with.
5. Compare src/ops/create_challenge.rs:276-280 (inline `format!`) with src/ops/create_challenge.rs:293 (`crate::state::resources_not_active_message`) to see the two wordings of one class in one handler.
