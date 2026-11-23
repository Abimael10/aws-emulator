Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ram.permission-version-lifecycle-and-default-propagation
Issue-Shape-Id: com.amazonaws.ram#AmazonResourceSharing
Issue-Key: ram.permission-version-lifecycle-and-default-propagation|run#25|AssertionError|assert message == (
Issue-Commit: f85f98f2ce22a299384412ddd2d0ac00fcbe668d

# ram DeletePermissionVersion: the attached-to-a-share refusal carries an emulator-authored message instead of AWS's fixed OperationNotPermittedException sentence

> _Note: this issue was identified against the code base at commit `f85f98f2ce22a299384412ddd2d0ac00fcbe668d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeletePermissionVersion

## Description
One net-new cause in RAM's managed-permission version lifecycle.

Cause 1 of 1: DeletePermissionVersion refuses the delete of a non-default permission version that a live resource share is still attached to with the right error class but the wrong message text.
Failing check ids: ram.permission-version-lifecycle-and-default-propagation.

AWS: DeletePermissionVersion "deletes one version of a customer managed permission. The version you specify must not be attached to any resource share and must not be the default version for the permission", and its permissionVersion member states "You can't delete a version if it is attached to any resource shares." Smithy (com.amazonaws.ram#DeletePermissionVersion): the operation models OperationNotPermittedException (HTTP 400, "The operation failed because the requested operation isn't permitted") for exactly this refusal class; neither the operation nor the error shape models any message content, so the message wording is recorded behavior only. The recording shows this refusal as HTTP 400, Error.Code OperationNotPermittedException, with Error.Message being a FIXED, parameter-free two-sentence text: "The customer-managed permission version is associated to resource shares. Remove all associations of this permission version and try again." There are no variable parts in that message — no permission ARN, no version number and no share ARN are interpolated into it; it is the same text for every permission, every version and every share.

Emulator: the same call is refused 400 OperationNotPermittedException, but with an emulator-authored message that interpolates request and state values: "Version <permissionVersion> of permission <permissionArn> is attached to resource share <first attached share ARN>. You can't delete a version if it is attached to any resource shares." (In the replay: "Version 1 of permission arn:aws:ram:...:permission/sim2real-ram-propagate-... is attached to resource share arn:aws:ram:...:resource-share/... . You can't delete a version if it is attached to any resource shares." — the ARNs and the version number there are this run's values filling that emulator template, not part of any requirement.) The emulator's template reuses the model's documentation sentence plus a generated preamble naming the permission, the version and one share, where AWS's text names none of them.

Why: a client that keys off the refusal message — matching the documented sentence, surfacing it to an operator, or branching on "Remove all associations of this permission version and try again" to drive ReplacePermissionAssociations/DisassociateResourceSharePermission before retrying the delete — never sees the text AWS emits. The emulator's message additionally leaks a single arbitrarily-chosen share ARN (the first in ARN order) where AWS deliberately names none, so test fixtures and log assertions written against AWS do not match, and an operator reading the emulator's message believes only that one share blocks the delete when any number may.

Fix: the message renderer for this refusal, crate::state::permission_version_attached_to_share_message, used by the DeletePermissionVersion handler's "attached to a share" branch. It must render AWS's fixed text with no interpolation (the permission ARN, version number and share ARN arguments become unnecessary); the detection of the blocking shares (shares_attaching_permission_version) and the error class and 400 status are already correct and must not change.

Traces: ram.permission-version-lifecycle-and-default-propagation — create a customer-managed permission, attach it to a share at version 1, create version 2 (which becomes the default), then DeletePermissionVersion(version=1) while the share still carries version 1. Note: the related open issue 20261119T221252Z-ram-managed-permission-reads-serve-wrong-permission-state.md covers per-version STATUS reporting (UNATTACHABLE/DELETED) on the read path and does not cover this refusal's message; the fixed issue 20261118T201041Z-ram-deletepermissionversion-stub.md stood the operation up (the call is now served, and refused with the correct code), so this is a net-new message-fidelity defect, not a reopen.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout. Service ram: 10/12 passed; the surviving failure is [mismatch] ram.permission-version-lifecycle-and-default-propagation.

Assertion that failed (check_ram_permission_version_lifecycle_and_default_propagation.py:116), after DeletePermissionVersion(permissionArn=<customer-managed permission>, permissionVersion=1) while a live resource share is attached to version 1:

  assert code == "OperationNotPermittedException"   -> PASSED (emulator returns the correct error code and HTTP 400)
  assert message == (
      "The customer-managed permission version is associated to resource shares. "
      "Remove all associations of this permission version and try again."
  )                                                  -> FAILED

  AssertionError: unexpected message "Version 1 of permission arn:aws:ram:us-west-2:000000000000:permission/sim2real-ram-propagate-d56b0a2bf263 is attached to resource share arn:aws:ram:us-west-2:000000000000:resource-share/737e862d-fdbb-6b5b-118d-a3ec7261fece. You can't delete a version if it is attached to any resource shares."

Expected (AWS, fixed text, no variable parts):
  "The customer-managed permission version is associated to resource shares. Remove all associations of this permission version and try again."

Actual (emulator, a template filled from the request and the store):
  "Version {permissionVersion} of permission {permissionArn} is attached to resource share {first attached share ARN}. You can't delete a version if it is attached to any resource shares."
  — the ARNs, the share id and the version number in the replayed text are this run's values and are not requirements; the divergence is that the template exists at all and that its fixed text differs from AWS's two sentences.

Source of the diverging text in the checkout:
  services/service-com-amazonaws-ram-amazonresourcesharing/src/state/permission_version_deletion.rs:181-191 (permission_version_attached_to_share_message), reached from
  services/service-com-amazonaws-ram-amazonresourcesharing/src/ops/delete_permission_version.rs:348-366 (the shares_attaching_permission_version branch).

Status (400) and error code (OperationNotPermittedException) match AWS; only the message diverges, and the check never reaches the subsequent ReplacePermissionAssociations/DELETED-status assertions because of it.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. ram.CreatePermission(name=<new name>, resourceType="codebuild:Project", policyTemplate=<policy allowing codebuild:BatchGetProjects>) -> note the permission ARN; version 1 is created and is the default.
3. ram.CreateResourceShare(name=<new name>, permissionArns=[<permission ARN>]) -> the share is attached to version 1 (confirm with ListResourceSharePermissions: version "1").
4. ram.CreatePermissionVersion(permissionArn=<permission ARN>, policyTemplate=<a different policy>) -> version 2, which becomes the permission's default; the share still carries version 1.
5. ram.DeletePermissionVersion(permissionArn=<permission ARN>, permissionVersion=1).
6. Observe the refusal: HTTP 400 / OperationNotPermittedException (correct), but Error.Message is "Version 1 of permission <arn> is attached to resource share <share arn>. You can't delete a version if it is attached to any resource shares." instead of AWS's fixed "The customer-managed permission version is associated to resource shares. Remove all associations of this permission version and try again."
7. Equivalently, replay behavioral check ram.permission-version-lifecycle-and-default-propagation; it fails at the message assertion on line 116.
