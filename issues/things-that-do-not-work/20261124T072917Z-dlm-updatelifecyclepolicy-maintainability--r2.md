Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: a6beefae630d63d931f6e3dc50dceb328128e26d

# UpdateLifecyclePolicy computes the policy's PolicyKind (and its default resource type) twice in one handler, so the completeness check and the materialization can drift apart

_Filed: 2026-11-24T07:29:17Z_
_Operation: UpdateLifecyclePolicy | Class: maintainability_

> _Note: this issue was identified against the code base at commit `a6beefae630d63d931f6e3dc50dceb328128e26d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`UpdateLifecyclePolicy::handle` derives the same rule twice: "the kind this record is — for a default policy, the kind of default policy named by the replacement's ResourceType, else the one the policy already holds". Once to decide which configuration members the replacement must state (`omitted_configuration_members`), and again, forty lines later, to decide which members get materialized onto the stored configuration (`materialized_policy_details`). The two copies differ only in borrowing versus cloning; they must agree, because the first decides whether the request is refused and the second decides what is stored for the very same request. Nothing ties them together, so a later change to one (for instance to how an omitted `ResourceType` resolves) would silently leave the other judging a different kind — a request accepted as a complete default-policy configuration could then be stored with a custom policy's materialization, or vice versa.

The rule belongs in one place (a single helper that takes the record and the replacement configuration and answers its `PolicyKind`), which both call sites then share. Every response and every existing test stays byte-for-byte the same: the two expressions compute the same value today.

## Evidence

First copy — the completeness check, `services/service-com-amazonaws-dlm-dlm-20180112/src/ops/update_lifecycle_policy.rs:309-320`:
```
        if let Some(details) = input.policy_details.as_ref() {
            let kind = if policy.default_policy {
                crate::state::PolicyKind::Default {
                    resource_type: details
                        .resource_type
                        .as_deref()
                        .or(policy.default_policy_type.as_deref()),
                }
            } else {
                crate::state::PolicyKind::Custom
            };
            let omitted = crate::state::omitted_configuration_members(details, kind);
```
Second copy — the materialization, `src/ops/update_lifecycle_policy.rs:412-423`:
```
            let default_resource_type = policy_details
                .resource_type
                .clone()
                .or_else(|| policy.default_policy_type.clone());
            let kind = if policy.default_policy {
                crate::state::PolicyKind::Default {
                    resource_type: default_resource_type.as_deref(),
                }
            } else {
                crate::state::PolicyKind::Custom
            };
            let mut details = crate::state::materialized_policy_details(policy_details, kind);
```
The crate already treats this kind of duplication as the thing to avoid: `PolicyKind`'s own doc says "The kind is the write path's to decide, because only it knows one: a create reads it off the request and an update off the policy it addresses" (`src/state.rs:399-405`) — one decision per write path, not two inside one.

Test coverage for both call sites: `tests/all/default_policy_requires_a_resource_type.rs::an_update_cannot_change_the_kind_a_create_stated`, `tests/all/default_policy_resource_type.rs::an_update_restating_the_stored_resource_type_is_accepted`, `tests/all/policy_read_projection.rs::an_update_restamps_the_materialized_members`, and `tests/all/default_policy_configuration_precedence.rs::a_policy_details_update_replaces_the_whole_configuration` — all of which must keep passing unchanged. (Distinct from the already-fixed UpdateLifecyclePolicy maintainability issue, which folded the `[Default policies only]` request-BODY member list into the `DefaultPolicyOnlyBody` trait; this one is the PolicyKind derivation.)

## Steps to Reproduce

1. Open services/service-com-amazonaws-dlm-dlm-20180112/src/ops/update_lifecycle_policy.rs
2. Read lines 309-320: the handler builds a PolicyKind from policy.default_policy and `details.resource_type … .or(policy.default_policy_type …)` to run omitted_configuration_members
3. Read lines 412-423: the handler builds the same PolicyKind from the same two inputs again to run materialized_policy_details
4. Note that no helper is shared between them, so the refusal decision and the storage decision read the same rule from two independent expressions
