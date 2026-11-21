Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup.list-tags-vault-and-plan-round-trip-and-deleted-vault-rejected
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup.list-tags-vault-and-plan-round-trip-and-deleted-vault-rejected|run#19|AssertionError|assert err == want, f"ListTags on a deleted vault's ARN returned {err!r}, expected {want!r}"
Issue-Commit: d6e38bce995a2eb9882841f5c2c7a74feca7cac2

# AWS Backup ListTags on a deleted vault's ARN returns ResourceNotFoundException 'Cannot find resource' instead of AWS's AccessDeniedException authorization sentence

> _Note: this issue was identified against the code base at commit `d6e38bce995a2eb9882841f5c2c7a74feca7cac2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListTags

## Description
Cause 1 of 1: `ListTags` refuses an ARN that no longer resolves (a backup vault deleted by `DeleteBackupVault`) with the wrong error class, the wrong error code and an emulator-authored message.

AWS: AWS Backup does not disclose the existence of a resource the caller's ARN names but which Backup cannot resolve for the caller. For a vault ARN whose vault has been deleted, `ListTags` is refused with error code `AccessDeniedException` and the fixed message `Insufficient privileges to perform this action.` — the whole message is fixed text for this error class; it interpolates nothing from the request or from state (no vault name, no ARN, no account id). Smithy: `ListTags` (`GET /tags/{ResourceArn}`, `ResourceArn` @httpLabel required, targets recovery points, backup plans and backup vaults) models `InvalidParameterValueException`, `MissingParameterValueException`, `ResourceNotFoundException` and `ServiceUnavailableException` and does NOT model `AccessDeniedException`; the model therefore says nothing about this refusal — recorded behavior only, and the recording shows the AWS response is `AccessDeniedException` / `Insufficient privileges to perform this action.` for the deleted vault's ARN, while the same call against a still-live vault ARN returns 200 with that vault's tags.

Emulator: the deleted vault's ARN is refused with error code `ResourceNotFoundException` and the message `Cannot find resource`. The handler resolves the ARN against the tag store and maps a miss straight onto the modeled not-found refusal (`services/service-com-amazonaws-backup-cryocontrollerusermanager/src/ops/list_tags.rs:166-168`, `store.tagged_resource_tags(...).map_err(ListTagsErrorModeled::ResourceNotFoundException)`, whose message comes from `crate::state::tagged_resource_not_found`). There is no authorization-style refusal path on this read at all, so an unresolvable ARN is always reported as not-found.

Why: a client that distinguishes "this ARN is not mine / I may not read it" from "this resource does not exist" takes the wrong branch. Code that matches on `AccessDeniedException` to surface a permissions or cross-account problem (or to stop retrying and escalate) never fires, while code that treats `ResourceNotFoundException` as a benign, idempotent "already gone" signal will silently swallow a real authorization failure. Tag-reconciliation and cleanup loops keyed on the refusal code therefore converge differently against the emulator than against AWS, and the message text differs too, so assertions on the error sentence fail. It also leaks resource existence/non-existence information AWS deliberately withholds.

Fix: the `ListTags` handler's refusal path for an ARN that resolves to no taggable resource of this account — i.e. the `tagged_resource_tags` miss branch in the operation's validation/refusal ordering, which must emit the authorization refusal (code `AccessDeniedException`, message `Insufficient privileges to perform this action.`) rather than the modeled `ResourceNotFoundException` carrying the generic `Cannot find resource` sentence. The emulator already has this authorization sentence as a requirement on other Backup reads (see open issue `20261113T123757Z-backup-aws-backup-describerecoverypoint-access-denied-body-returns-a.md`, which is about `DescribeRecoveryPoint`'s access-denied BODY, and `20261119T114132Z-backup-associatebackupvaultmpaapprovalteam-on-a-nonexistent-vault-returns-invalidparametervalueexception-instead.md`, which is about `AssociateBackupVaultMpaApprovalTeam`); neither of those covers `ListTags`, so this is a separate cause in a separate handler.

Traces: failing check `backup.list-tags-vault-and-plan-round-trip-and-deleted-vault-rejected`. The check's earlier phases pass — create-time tags, `TagResource` merge and `UntagResource` single-key removal all round-trip correctly on both a vault ARN and a plan ARN — and it fails only at the deleted-vault refusal assertion.

## Evidence
Check: backup.list-tags-vault-and-plan-round-trip-and-deleted-vault-rejected [mismatch]

AssertionError (check_backup_list_tags_vault_and_plan_round_trip_and_deleted_vault_rejected.py line 104):
  ListTags on a deleted vault's ARN returned ('ResourceNotFoundException', 'Cannot find resource'),
  expected ('AccessDeniedException', 'Insufficient privileges to perform this action.')

Expected (AWS, recorded): error code `AccessDeniedException`, message `Insufficient privileges to perform this action.` (entire message is fixed text for this error class; no request/state values interpolated).
Actual (emulator): error code `ResourceNotFoundException`, message `Cannot find resource`.

All preceding assertions in the same check pass, so this is isolated to the refusal of an ARN that no longer resolves:
  - ListTags(vault_arn) after create == {"team": "storage"}  -> OK
  - ListTags(vault_arn) after TagResource == {"team":"storage","env":"dev","owner":"data"} -> OK
  - ListTags(vault_arn) after UntagResource(["team"]) == {"env":"dev","owner":"data"} -> OK
  - same three for the plan ARN with {"tier":"gold"} / {"env":"dev"} / removed "tier" -> OK
  - ListTags(deleted vault ARN) -> DIVERGES as above

Smithy (services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json):
  ListTags GET /tags/{ResourceArn}, success 200; modeled errors = InvalidParameterValueException(400), MissingParameterValueException(400), ResourceNotFoundException(400), ServiceUnavailableException(500). AccessDeniedException is NOT modeled for this operation, so the refusal AWS actually returns is recorded behavior only.

Emulator source: services/service-com-amazonaws-backup-cryocontrollerusermanager/src/ops/list_tags.rs:166-168 maps the tag-store miss directly to ListTagsErrorModeled::ResourceNotFoundException (message from crate::state::tagged_resource_not_found); no AccessDeniedException path exists in this handler.

## Steps to Reproduce
1. backup.CreateBackupVault(BackupVaultName=<vault-a>, BackupVaultTags={"team":"storage"}) and keep the returned BackupVaultArn.
2. backup.CreateBackupVault(BackupVaultName=<vault-b>, BackupVaultTags={"team":"storage"}) and keep its BackupVaultArn (the "doomed" vault).
3. backup.ListTags(ResourceArn=<vault-b ARN>) -> 200, Tags == {"team":"storage"} (this works today).
4. backup.DeleteBackupVault(BackupVaultName=<vault-b>).
5. backup.ListTags(ResourceArn=<vault-b ARN>).
   Expected: refusal with Error.Code == "AccessDeniedException" and Error.Message == "Insufficient privileges to perform this action."
   Actual:   refusal with Error.Code == "ResourceNotFoundException" and Error.Message == "Cannot find resource".
6. backup.ListTags(ResourceArn=<vault-a ARN>) still returns vault-a's tags, confirming only the deleted ARN's refusal classification diverges.

Replay: run_behavioral_suite and observe check backup.list-tags-vault-and-plan-round-trip-and-deleted-vault-rejected.
