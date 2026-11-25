Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ram.permission-version-lifecycle-and-default-propagation
Issue-Shape-Id: com.amazonaws.ram#AmazonResourceSharing
Issue-Key: ram.permission-version-lifecycle-and-default-propagation|run#34|AssertionError|assert statuses == {"1": "DELETED", "2": "ATTACHABLE"}, (
Issue-Commit: 2507d9036784efe79624e06996096400cc112ab2

# DeletePermissionVersion leaves the version DELETING, so the next ListPermissionVersions reports DELETING where AWS reports DELETED

> _Note: this issue was identified against the code base at commit `2507d9036784efe79624e06996096400cc112ab2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeletePermissionVersion

## Description
Cause 1 of 1: a customer-managed permission version that DeletePermissionVersion accepted is held in the non-terminal DELETING status for a fixed emulator-chosen span, so a read issued right after the delete reports DELETING instead of the terminal DELETED status AWS reports there.

AWS: Smithy model — DeletePermissionVersion (DELETE /deletepermissionversion, input permissionArn + permissionVersion, output returnValue/clientToken/permissionStatus: PermissionStatus) and ListPermissionVersions (POST /listpermissionversions, output permissions: ResourceSharePermissionList). PermissionStatus is the enum [ATTACHABLE, UNATTACHABLE, DELETING, DELETED]; ResourceSharePermissionDetail$status documents DELETING as "this permission or version is in the process of being deleted" and DELETED as "this permission or version is deleted". The model's only statement about timing is "this operation is performed asynchronously" on DeletePermissionVersionResponse$permissionStatus — it publishes no DELETING dwell time. Recorded behavior: for a version that is neither the permission's default nor attached to any resource share (both preconditions the operation documents), the delete is already complete by the time the caller's next call is answered — the immediately following ListPermissionVersions(permissionArn) returns the deleted version with status DELETED, with its record retained for audit, and the surviving version still ATTACHABLE. No polling is performed between the two calls in the recording.

Emulator: ListPermissionVersions immediately after the accepted delete reports the deleted version as DELETING. Version 2 is correctly ATTACHABLE, so only the deleted version's status diverges; the enum value is a legal member of PermissionStatus, it is simply the wrong one at this point in the lifecycle. The cause is a deliberate, non-zero DELETING window: services/service-com-amazonaws-ram-amazonresourcesharing/src/state/permission_version_deletion.rs::delete_permission_version records a PermissionDeletion with status DELETING plus an OwedMove to DELETED due_at = now + PERMISSION_DELETED_AFTER_MILLIS, and that constant (services/service-com-amazonaws-ram-amazonresourcesharing/src/state/permission_deletion.rs) is 3000 ms and is documented as "deliberately not zero, so a caller that reads straight back still observes DELETING". The settle path is wired correctly (ListPermissionVersions does tick() + settle_due_permissions()), so nothing is stuck — the owed move is simply not yet due when the next read lands.

Why: a client that deletes a non-default, unattached version and then reads the version list back — the exact sequence AWS's own documented preconditions lead to — sees a status AWS never shows it there. Code that treats DELETING as "not finished" either declares the delete incomplete, loops, or (as here) fails an equality assertion on the version-status map. Any consistency contract a client builds on "the delete of an unattached, non-default version is observable as DELETED on the next call" is broken, and the divergence is timing-dependent, so it reads as flakiness rather than a fixed behavior.

Fix: the version-delete state transition for DeletePermissionVersion — delete_permission_version in services/service-com-amazonaws-ram-amazonresourcesharing/src/state/permission_version_deletion.rs and the span constant it borrows from the whole-permission delete. A version whose delete the operation accepted (non-default, no live share attaching it) must be observable as DELETED by the next request, i.e. the owed move must already be due when the delete returns, rather than borrowing the DeletePermission dwell time. Keep the record itself in place: ListPermissionVersions must still list the deleted version, with status DELETED, alongside the untouched ATTACHABLE versions. Hypothesis (neither the model nor the recording states it): the DeletePermissionVersionResponse$permissionStatus of that same call is then DELETED as well; the recording only constrains the subsequent read, so do not treat the response field as the requirement here.

Traces: ram.permission-version-lifecycle-and-default-propagation — assertion `statuses == {"1": "DELETED", "2": "ATTACHABLE"}` got `{'1': 'DELETING', '2': 'ATTACHABLE'}`. Every earlier step of that check passed (default-version propagation, the OperationNotPermittedException refusal for deleting an attached version, and the ReplacePermissionAssociations migration), so the version-status lifecycle after a successful delete is the only diverging part.

## Evidence
Check: ram.permission-version-lifecycle-and-default-propagation [mismatch]

Sequence (no sleep between the last two calls):
  CreatePermission(name, resourceType=codebuild:Project, policyTemplate=v1) -> version "1"
  CreateResourceShare(name, permissionArns=[permArn])                      -> share attached to version "1"
  CreatePermissionVersion(permissionArn, policyTemplate=v2)                -> version "2", now default
  ReplacePermissionAssociations(from version 1 -> toPermissionArn)          -> work COMPLETED, share now on version "2"
  DeletePermissionVersion(permissionArn, permissionVersion=1)              -> accepted
  ListPermissionVersions(permissionArn)

Expected (recorded AWS):  {"1": "DELETED",  "2": "ATTACHABLE"}
Actual   (emulator):      {"1": "DELETING", "2": "ATTACHABLE"}

AssertionError: expected version 1 DELETED and version 2 ATTACHABLE, got {'1': 'DELETING', '2': 'ATTACHABLE'}
  at check_ram_permission_version_lifecycle_and_default_propagation.py:140

Source of the divergence in the checkout:
  src/state/permission_version_deletion.rs — delete_permission_version() sets
    deletion.status = PERMISSION_STATUS_DELETING with OwedMove{status: DELETED,
    due_at: now + PERMISSION_DELETED_AFTER_MILLIS}
  src/state/permission_deletion.rs:46 — PERMISSION_DELETED_AFTER_MILLIS = 3_000, documented as
    "deliberately not zero, so a caller that reads straight back still observes DELETING"
  src/ops/list_permission_versions.rs — does call tick()/settle_due_permissions(), so the read
    path is correct; the move is simply not yet due.

No status code or error code diverges: both calls return HTTP 200. The diverging value is the
enum member in ResourceSharePermissionDetail$status for the deleted version (DELETING vs DELETED);
the version number "1" and the permission ARN come from the request, and the generated permission
name/ARN and timestamps in the run are per-run values, not part of the requirement.

## Steps to Reproduce
1. CreatePermission with a unique name, resourceType "codebuild:Project" and any valid policyTemplate -> version "1".
2. CreateResourceShare with permissionArns=[that permission ARN] -> the share attaches version "1".
3. CreatePermissionVersion on the same permission with a different policyTemplate -> version "2", which becomes the permission's default.
4. ReplacePermissionAssociations(fromPermissionArn=perm, fromPermissionVersion=1, toPermissionArn=perm) and wait for the work to reach COMPLETED, so no live share attaches version 1 any more.
5. DeletePermissionVersion(permissionArn=perm, permissionVersion=1) — accepted (HTTP 200).
6. IMMEDIATELY (no sleep) call ListPermissionVersions(permissionArn=perm) and read {version: status}.
   AWS: {"1": "DELETED", "2": "ATTACHABLE"}. Emulator: {"1": "DELETING", "2": "ATTACHABLE"}.
   Waiting ~3 s and repeating step 6 yields DELETED, which confirms the fixed DELETING window is the cause.
