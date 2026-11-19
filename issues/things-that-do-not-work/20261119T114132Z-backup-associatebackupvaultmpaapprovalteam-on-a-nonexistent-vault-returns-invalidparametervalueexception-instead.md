Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup.associate-mpa-team-missing-parameter-rejected
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup.associate-mpa-team-missing-parameter-rejected|run#16|AssertionError|assert other_code == "AccessDeniedException", f"actual_code={other_code}"
Issue-Commit: 2901dc6a3b1a6d3c36a8ae908870ebb8c494c947

# AssociateBackupVaultMpaApprovalTeam on a nonexistent vault returns InvalidParameterValueException instead of AWS's AccessDeniedException

> _Note: this issue was identified against the code base at commit `2901dc6a3b1a6d3c36a8ae908870ebb8c494c947`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateBackupVaultMpaApprovalTeam

## Description
Cause 1 of 1: AssociateBackupVaultMpaApprovalTeam validates MpaApprovalTeamArn against the team-minting service BEFORE it makes the vault-scoped access decision, so a call naming a vault that does not exist in the caller's account is refused as a bad parameter value rather than as unauthorized.

AWS: recorded behavior only for this error class — the Smithy model for AssociateBackupVaultMpaApprovalTeam (errors: InvalidParameterValueException, InvalidRequestException, MissingParameterValueException, ResourceNotFoundException, ServiceUnavailableException; input BackupVaultName @httpLabel @required pattern ^[a-zA-Z0-9\-\_]{2,50}$, MpaApprovalTeamArn target ARN @required; PUT /backup-vaults/{BackupVaultName}/mpaApprovalTeam, success 204) does not declare AccessDeniedException at all. The recording shows AWS decides resource-based authorization FIRST: for a vault name that names no vault the caller can see, the call is refused with error code AccessDeniedException and the message "Insufficient privileges to perform this action." — that whole sentence is fixed text for this error class, with no variable parts (no vault name, team ARN, account or region is interpolated). The same call shape against an EXISTING vault with an empty MpaApprovalTeamArn is refused with InvalidParameterValueException and the message "Parameter MpaApprovalTeamArn is invalid." (fixed text apart from the member name "MpaApprovalTeamArn", taken from the request), and that half the emulator already matches. The two rejections are therefore distinct and ordered: existence/authorization of the vault is settled before the team ARN's value is judged.

Emulator: for a nonexistent vault with a well-formed-but-unknown team ARN (arn:aws:mpa:<region>:<account>:approval-team/<name>), the emulator answers error code InvalidParameterValueException (HTTP 400) — the value-level refusal — because the handler resolves the team ARN through the MPA service and returns InvalidParameterValueException before it ever resolves the vault; the vault-visibility decision (which does yield AccessDeniedException) is never reached. Replay assertion: `actual_code=InvalidParameterValueException` where AccessDeniedException was expected.

Why: a client cannot distinguish "you may not touch this vault / it is not there" from "the approval-team ARN you passed is wrong". Callers and IaC tooling that retry or branch on AccessDeniedException (re-assume a role, fall back to a different vault, surface a permissions error) instead see a parameter-validation error and will "fix" a perfectly valid team ARN forever; conversely, the emulator discloses nothing about the vault where AWS's answer is specifically an authorization answer. Error-code-driven error handling and negative tests around MPA association behave differently against the emulator than against AWS.

Fix: the operation's handler, services/service-com-amazonaws-backup-cryocontrollerusermanager/src/ops/associate_backup_vault_mpa_approval_team.rs — the ordering of its validation steps: perform the vault-scoped access/existence resolution (state::visible_vault_mut, which already produces the AccessDeniedException answer) before the MpaApprovalTeamArn value resolution (state::rejected_approval_team), so the parameter-value refusal can only be reached for a vault the caller may act on, and so the unauthorized/missing-vault path returns AccessDeniedException with AWS's fixed sentence.

Traces: backup.associate-mpa-team-missing-parameter-rejected (the check's earlier assertions — vault creation, TagResource/ListTags positive control, the empty-ARN InvalidParameterValueException with message "Parameter MpaApprovalTeamArn is invalid.", and DescribeBackupVault showing no MpaApprovalTeamArn recorded — all pass; only the nonexistent-vault error class diverges).

## Evidence
Check: backup.associate-mpa-team-missing-parameter-rejected (mismatch)

Step that diverges: AssociateBackupVaultMpaApprovalTeam(BackupVaultName=<a vault name that was never created>, MpaApprovalTeamArn="arn:aws:mpa:<region>:<account>:approval-team/does-not-exist")

Expected (AWS, per the recording):
  Error.Code    = AccessDeniedException
  Error.Message = "Insufficient privileges to perform this action."   (entirely fixed text)

Actual (emulator):
  Error.Code    = InvalidParameterValueException   (HTTP 400)

Replay assertion:
  File "check_backup_associate_mpa_team_missing_parameter_rejected.py", line 55, in run
    assert other_code == "AccessDeniedException", f"actual_code={other_code}"
  AssertionError: actual_code=InvalidParameterValueException

Control that already matches (same operation, existing vault, empty ARN):
  Error.Code    = InvalidParameterValueException
  Error.Message = "Parameter MpaApprovalTeamArn is invalid."  (fixed text; "MpaApprovalTeamArn" is the request's member name)
  and DescribeBackupVault afterwards reports no MpaApprovalTeamArn — the refused association persists nothing.

Smithy (services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json): AssociateBackupVaultMpaApprovalTeam errors = InvalidParameterValueException, InvalidRequestException, MissingParameterValueException, ResourceNotFoundException, ServiceUnavailableException; AccessDeniedException is not modeled, so the authorization answer is recorded behavior only.

## Steps to Reproduce
1. backup.create_backup_vault(BackupVaultName=<vault>) and confirm the vault answers (tag_resource + list_tags round-trip on its ARN succeeds).
2. Call backup.associate_backup_vault_mpa_approval_team(BackupVaultName=<vault>, MpaApprovalTeamArn="") and observe InvalidParameterValueException / "Parameter MpaApprovalTeamArn is invalid." — this part matches AWS.
3. Call backup.associate_backup_vault_mpa_approval_team(BackupVaultName=<a name that was never created>, MpaApprovalTeamArn="arn:aws:mpa:<region>:<account>:approval-team/does-not-exist").
4. Observe the emulator returns ClientError code InvalidParameterValueException; AWS returns AccessDeniedException with message "Insufficient privileges to perform this action.".
5. Re-replay check backup.associate-mpa-team-missing-parameter-rejected to confirm a fix.
