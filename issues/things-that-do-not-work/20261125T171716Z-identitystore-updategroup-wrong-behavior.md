Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: f9465bc2186e307f05476a9cadaf16956fa09a02

# UpdateGroup compares the Revision precondition as a string while DeleteGroup compares it as a number

_Filed: 2026-11-25T17:17:16Z_
_Operation: UpdateGroup | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `f9465bc2186e307f05476a9cadaf16956fa09a02`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Same defect as on `UpdateUser`, in the group half of the crate. `DeleteGroup` parses the `Revision` precondition and compares numbers, stating the rule for the member ("`01` and `1` name one revision"); `UpdateGroup` compares the raw strings against `Group::revision()`, which always renders the unpadded number. So `DeleteGroup` with `Revision: "01"` deletes a group at revision 1 — a row this crate's own suite pins — while `UpdateGroup` with `Revision: "01"` on the same group answers HTTP 409 ConflictException and applies nothing.

## Evidence

Normalising side — services/service-com-amazonaws-identitystore-awsidentitystore/src/ops/delete_group.rs:199-218:
```rust
// `Revision` is the caller's EXPECTED current revision ... The member is a decimal string (`^[0-9]+$`), so it is compared as
// the number it denotes — `01` and `1` name one revision — ...
if let Some(expected) = input.revision.as_deref() {
    if let Some(group) = store.groups.get(&group_key) {
        if expected.parse::<u64>().ok() != Some(group.revision) {
            return Err(DeleteGroupErrorModeled::ConflictException(...));
```

Non-normalising side — src/ops/update_group.rs:455-466:
```rust
// `Revision` is "the expected current revision of the group … the update is
// applied only if it matches", so a request carrying one that is not the
// group's current revision is refused and changes nothing.
if let Some(expected) = input.revision.as_deref() {
    if expected != stored.revision() {
        return Err(UpdateGroupErrorModeled::ConflictException(
            crate::state::revision_conflict(expected, &stored.revision()),
```

The delete side is pinned by tests/all/delete_group.rs:435 (`"the current revision with a leading zero"`) and tests/all/delete_user.rs:155-157/:193; the update suite's rows (tests/all/update_group.rs:424-428: current / absent / stale / "999999" / "0") never send a zero-padded value, so nothing pins the string comparison.

## Steps to Reproduce

1. CreateGroup in the seeded identity store; the response reports `Revision: "1"`.
2. Call UpdateGroup with `Revision: "01"` and `Operations: [{"AttributePath": "description", "AttributeValue": "desc"}]` → HTTP 409 ConflictException; DescribeGroup shows the group unchanged at revision 1.
3. Call DeleteGroup for the same group with `Revision: "01"` → HTTP 200 and the group is gone.
4. Observe the same precondition value is accepted by the delete and refused by the update.
