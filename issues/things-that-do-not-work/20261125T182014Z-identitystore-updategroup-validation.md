Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: c8c1a6886de773856b080208b347842cb2a65b75

# UpdateGroup renames a group to the reserved DisplayName values the model says cannot be used

_Filed: 2026-11-25T18:20:14Z_
_Operation: UpdateGroup | Class: validation_

> _Note: this issue was identified against the code base at commit `c8c1a6886de773856b080208b347842cb2a65b75`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The modelled documentation of the group display name states that `Administrator` and `AWSAdministrators` are reserved and "can't be used for users or groups". `UpdateGroup` accepts an `AttributeOperation` on `displayName` and the only check it applies to the new value is (a) the type/length/pattern walk over the shadow member schema and (b) the per-store uniqueness claim against `group_display_name_index`. Nothing in the handler (or anywhere in the crate) compares the value against the reserved names, so a group created under an ordinary name can be renamed to `Administrator` or `AWSAdministrators` with HTTP 200, and `DescribeGroup`/`ListGroups`/`GetGroupId` then report and resolve a group using a name the model states cannot be used.

This is a different code path from the already-open `CreateGroup`/`CreateUser` reserved-name issues, which are about the CREATE door only: fixing those by adding a check inside `create_group.rs` leaves this rename door open. (The `userName` door of `UpdateUser` is the same shape; its `(UpdateUser, validation)` pair is already filed for the missing `@pattern` check, so it is noted here rather than filed again.)

## Evidence

Smithy — `smithy_query(query="doc",shape="CreateGroupRequest",member="DisplayName")`, quoted verbatim in the sibling handler's modelled-contract header, services/service-com-amazonaws-identitystore-awsidentitystore/src/ops/create_group.rs:54-57:
```text
//! * `DisplayName` (GroupDisplayName, len 1..1024, pattern ^[\p{L}\p{M}\p{S}\p{N}\p{P}\t\n\r
//!   ]+$) — A string containing the name of the group. This value is commonly displayed when
//!   the group is referenced. Administrator and AWSAdministrators are reserved names and
//!   can't be used for users or groups.
```

The attribute catalog this handler accepts a rename through, src/ops/update_group.rs:259-298:
```rust
enum GroupAttribute {
    DisplayName,
    Description,
}
...
    fn from_path(path: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|attribute| attribute.path().eq_ignore_ascii_case(path))
    }
```
The unconditional assignment, src/ops/update_group.rs:474-483:
```rust
        for (attribute, value) in changes {
            match attribute {
                // `Group$DisplayName` is not `@required` in the model
                ...
                GroupAttribute::DisplayName => updated.display_name = value,
```
and the ONLY name-level check that follows, src/ops/update_group.rs:489-507 — the per-store uniqueness claim:
```rust
        // `DisplayName` is a uniqueness claim within the identity store, which an
        // update makes exactly as a create does ...
        if let Some(display_name) = &updated.display_name {
            if store
                .group_display_name_index
                .get(display_name)
                .is_some_and(|owner| *owner != updated.group_id)
```
Nothing in the crate reads the reserved names: `grep -rn 'Administrator' services/service-com-amazonaws-identitystore-awsidentitystore/src` matches only the two doc headers (create_group.rs:56, create_user.rs:100) and never any code. No test sends one either — the UpdateGroup rename suite's rows are ordinary names plus pattern/length cases (tests/all/update_group.rs:157-161 `let spaced = "no-break\u{a0}and\u{3000}ideographic"`, :551-559 the excluded-character rows), so nothing in the crate justifies the divergence.

## Steps to Reproduce

1. Boot the emulator and resolve the seeded IdentityStoreId.
2. CreateGroup(IdentityStoreId=<store>, DisplayName="engineering") → 200 with a minted GroupId.
3. UpdateGroup(IdentityStoreId=<store>, GroupId=<id>, Operations=[{"AttributePath":"displayName","AttributeValue":"AWSAdministrators"}]) → HTTP 200 with a bumped Revision.
4. DescribeGroup(IdentityStoreId=<store>, GroupId=<id>) → `DisplayName: "AWSAdministrators"`, and GetGroupId by `displayName`="AWSAdministrators" resolves the group, although the model states the name "can't be used for users or groups".
5. Repeat step 3 with "Administrator" → also accepted.
