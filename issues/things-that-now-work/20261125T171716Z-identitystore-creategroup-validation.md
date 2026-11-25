Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: f9465bc2186e307f05476a9cadaf16956fa09a02

# CreateGroup accepts the reserved DisplayName values the model says cannot be used

_Filed: 2026-11-25T17:17:16Z_
_Operation: CreateGroup | Class: validation_

> _Note: this issue was identified against the code base at commit `f9465bc2186e307f05476a9cadaf16956fa09a02`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The modelled documentation of `CreateGroupRequest$DisplayName` states that `Administrator` and `AWSAdministrators` are reserved and cannot be used for users or groups. The handler applies only the per-store uniqueness claim against `group_display_name_index` and then stores whatever it was given, so a group can be created under a reserved display name and `DescribeGroup`/`GetGroupId`/`ListGroups` report it.

## Evidence

Smithy — `smithy_query(query="doc",shape="CreateGroupRequest",member="DisplayName")`, verbatim in the handler's modelled-contract header, services/service-com-amazonaws-identitystore-awsidentitystore/src/ops/create_group.rs:54-57:
```text
//! * `DisplayName` (GroupDisplayName, len 1..1024, pattern ^[\p{L}\p{M}\p{S}\p{N}\p{P}\t\n\r
//!   ]+$) — A string containing the name of the group. This value is commonly displayed when
//!   the group is referenced. Administrator and AWSAdministrators are reserved names and
//!   can't be used for users or groups.
```

The handler's only name check, src/ops/create_group.rs:212-228:
```rust
// A `DisplayName` is a uniqueness claim within the identity store, so a second
// create naming one that is taken conflicts rather than storing a duplicate.
if input
    .display_name
    .as_deref()
    .is_some_and(|display_name| store.group_display_name_index.contains_key(display_name))
{
    return Err(CreateGroupErrorModeled::ConflictException(...));
}
```
followed by unconditional persistence (create_group.rs:236 `display_name: input.display_name,`, :255-259 index insert). No code in the crate reads the reserved names (`grep -n 'Administrator'` matches only create_user.rs:100 and create_group.rs:56), and no test sends one, so nothing justifies the divergence.

## Steps to Reproduce

1. Boot the emulator and resolve the seeded IdentityStoreId.
2. Call CreateGroup with `DisplayName: "AWSAdministrators"` → HTTP 200 with a minted GroupId.
3. Call DescribeGroup for that GroupId → `DisplayName: "AWSAdministrators"` is reported, and GetGroupId by `displayName` resolves it, despite the model stating the name "can't be used".
4. Repeat with `DisplayName: "Administrator"` → also accepted.
