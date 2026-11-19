Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 95422a199f71de611c9b4568260a34e03a6bfa1c

# The IAM refusal sentence and its three reason clauses are hand-rolled in this crate's authorization gate, though service_common already states them

_Filed: 2026-11-19T15:07:13Z_
_Operation: GetAccountPlanState | Class: maintainability_

> _Note: this issue was identified against the code base at commit `95422a199f71de611c9b4568260a34e03a6bfa1c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`src/service/authorization.rs` is the gate `GetAccountPlanState` (and `UpgradeAccountPlan`) call before touching plan state. It already reaches into `::service_common::iam_authorization` for the DECISION, and its own module header says each of the three pieces is "held in exactly one place — the shape every other gate in this tree takes, because this is one rule and not one per service". But the WORDS of the refusal are not: this crate re-writes the whole `User: <principal> is not authorized to perform: <action> on resource: <resource> <reason>` sentence and all three reason clauses by hand, while the shared crate it already depends on exposes exactly those strings (`not_authorized_by_identity_policy`, `not_authorized_to_perform`, `IDENTITY_POLICY_EXPLICIT_DENY`, `no_identity_policy_allows`, `PERMISSIONS_BOUNDARY_EXPLICIT_DENY`, `no_permissions_boundary_allows`) — and documents that the inlined copies "are to be migrated onto this one rather than copied again". The two renderings are byte-identical today, so the copies can only drift: a correction to the shared sentence (a punctuation change, a reworded clause) would silently leave this service answering the old text. The fix is to call the shared builders; every response byte and every existing test stays the same.

## Evidence

The hand-rolled sentence — `services/service-com-amazonaws-freetier-awsfreetierservice/src/service/authorization.rs:187-199`:
```
fn not_authorized(
    principal: &str,
    action: &str,
    resource: &str,
    reason: &str,
) -> AccessDeniedException {
    AccessDeniedException {
        message: format!(
            "User: {principal} is not authorized to perform: {action} on resource: {resource} \
             {reason}"
        ),
    }
}
```
The shared statement of the SAME sentence, in the crate this file already imports (`authorization.rs:74`: `use ::service_common::iam_authorization::{self as iam, Decision};`) — `services/common/src/iam_authorization.rs:705-719`:
```
/// Every gate that decides the identity-based question inlined this sentence before it lived
/// here — `grep -rln 'because no identity-based policy allows' services/*/src` lists them —
/// and they are to be migrated onto this one rather than copied again.
#[must_use]
pub fn not_authorized_by_identity_policy(
    principal: &str,
    action: &str,
    resource: &str,
    reason: &str,
) -> String {
    format!(
        "{} {reason}",
        not_authorized_to_perform(principal, action, resource)
    )
}
```
and `services/common/src/iam_authorization.rs:736-738`:
```
pub fn not_authorized_to_perform(principal: &str, action: &str, resource: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} on resource: {resource}")
}
```
The three reason clauses, each written twice. This crate — `src/service/authorization.rs:141-146`:
```
        return Err(not_authorized(
            &caller.arn,
            &action,
            &resource,
            "with an explicit deny in an identity-based policy",
        ));
```
`src/service/authorization.rs:155`: `&format!("because no identity-based policy allows the {action} action")`; `:167`: `"with an explicit deny in a permissions boundary".to_string()`; `:169`: `format!("because no permissions boundary allows the {action} action")`.
The shared crate — `services/common/src/iam_authorization.rs:764`: `pub const IDENTITY_POLICY_EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";`; `:776-777`: `pub const PERMISSIONS_BOUNDARY_EXPLICIT_DENY: &str = "with an explicit deny in a permissions boundary";`; `:787-789`: `pub fn no_identity_policy_allows(action: &str) -> String { format!("because no identity-based policy allows the {action} action") }`; `:795-797`: `pub fn no_permissions_boundary_allows(action: &str) -> String { format!("because no permissions boundary allows the {action} action") }`.
This crate's own claim that the rule is held in one place — `src/service/authorization.rs:16-18`: "Three pieces, each held in exactly one place — the shape every other gate in this tree takes, because this is one rule and not one per service".
Test coverage that pins the text and must stay green unchanged: the gate's own unit test `src/service/authorization.rs:372-388` (`the_refusal_names_the_principal_the_action_and_the_resource`, asserting `format!("User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}")`), and the wire suite `tests/all/identity_authorization.rs`, which restates the same three clauses a THIRD time at `:495-505`:
```
    fn clause(self, action: &str) -> String {
        match self {
            Self::NothingAllows => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
            Self::BoundaryWithholds => {
                format!("because no permissions boundary allows the {action} action")
            }
        }
    }
```
and the sentence shell at `tests/all/identity_authorization.rs:778` and `:895` (`"User: {principal} is not authorized to perform: {} on resource: {} {}"`).

## Steps to Reproduce

1. Open services/service-com-amazonaws-freetier-awsfreetierservice/src/service/authorization.rs and read `not_authorized` (lines 187-199) plus the four reason strings at lines 145, 155, 167 and 169.
2. Open services/common/src/iam_authorization.rs and read `not_authorized_by_identity_policy` (705-719), `not_authorized_to_perform` (736-738), `IDENTITY_POLICY_EXPLICIT_DENY` (764), `PERMISSIONS_BOUNDARY_EXPLICIT_DENY` (776-777), `no_identity_policy_allows` (787-789) and `no_permissions_boundary_allows` (795-797): each is the same string this service builds by hand.
3. Note that services/common/src/iam_authorization.rs:705-707 states that gates which inlined the sentence "are to be migrated onto this one rather than copied again", and that this crate is one of them (`grep -rn 'because no identity-based policy allows' services/service-com-amazonaws-freetier-awsfreetierservice/src` matches authorization.rs:155).
4. Confirm the shared module is already a dependency of this file: src/service/authorization.rs:74 imports `::service_common::iam_authorization`.
5. Replace the hand-rolled sentence and clauses with the shared builders and re-run `src/service/authorization.rs`'s unit tests and tests/all/identity_authorization.rs: both pass unchanged, because the rendered text is identical.
