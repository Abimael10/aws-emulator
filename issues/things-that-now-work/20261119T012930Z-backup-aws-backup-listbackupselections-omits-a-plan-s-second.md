Issue-Source: trace-sweep-divergence
Issue-Trace: backup-rbx-traj-second-selection-mistakes-and-recovery
Issue-Step: task#13
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-rbx-traj-second-selection-mistakes-and-recovery
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|ListBackupSelections|2xx->2xx|$.BackupSelectionsList[*]
Issue-Commit: 1dc698be9e80e0112562bfdd5ab001c28f556c88

# AWS Backup ListBackupSelections omits a plan's second selection from BackupSelectionsList

> _Note: this issue was identified against the code base at commit `1dc698be9e80e0112562bfdd5ab001c28f556c88`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListBackupSelections

## Description
One net-new wire divergence on AWS Backup's selection-read path: ListBackupSelections returns a BackupSelectionsList that is SHORT one element, so a plan that carries two backup selections is reported as carrying only one. ONE cause must be closed.

Cause 1 of 1: ListBackupSelections returns only the first of the plan's two selections (fixture backup-rbx-traj-second-selection-mistakes-and-recovery, step task#13).
AWS: Smithy backup#ListBackupSelections is GET /backup/plans/{BackupPlanId}/selections, 200, output BackupSelectionsList (list of backup#BackupSelectionsListMember with members SelectionId, SelectionName, BackupPlanId, CreationDate, CreatorRequestId, IamRoleArn) plus NextToken; it is @paginated on NextToken/MaxResults (MaxResults @range 1-1000). Every selection that CreateBackupSelection (PUT /backup/plans/{BackupPlanId}/selections, 200) accepted for that plan is a member of the list, so when a plan holds two accepted selections an unpaginated call returns two members, one per selection, each carrying that selection's own SelectionId, SelectionName, BackupPlanId, CreationDate and IamRoleArn. The recording shows exactly that: BackupSelectionsList has two entries, the second one being the plan's second selection (a distinct SelectionName and SelectionId, same BackupPlanId, its own IamRoleArn, CreatorRequestId null).
Emulator: the response body carries only index 0 — $.BackupSelectionsList[1] is entirely missing (expected a BackupSelectionsListMember, got None) — while the request specified no MaxResults and no NextToken was returned, so this is not pagination: the second selection simply is not in the plan's selection collection the read projects from.
Why: a client enumerating a plan's selections sees an incomplete inventory. It cannot discover, describe, update or delete the selection AWS does have, and because DeleteBackupPlan refuses while any selection is still attached, a dependency-ordered teardown driven by this list deletes what it can see and then hits a refusal it has no way to resolve. Any reconciler comparing desired against listed selections also concludes a selection is absent and re-creates it.
Fix: the backup crate's selection state and the ListBackupSelections handler that projects it — every selection accepted by CreateBackupSelection for the plan must be committed under that plan id and emitted as its own BackupSelectionsListMember, with pagination only ever truncating the list when MaxResults/NextToken ask for it. Related open issue 20261103T061309Z-backup-aws-backup-plan-selection-and-vault-refusals-diverge.md covers the same fixture's CreateBackupSelection refusals and the IamRoleArn/SelectionId content of BackupSelectionsList[0], but it carries none of this cluster's replay key for the MISSING element, so it does not cover this cause.
Traces: backup-rbx-traj-second-selection-mistakes-and-recovery#task#13.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, score 71.4%).

backup-rbx-traj-second-selection-mistakes-and-recovery#task#13 ListBackupSelections, status 200 -> 200 (status agrees), divergence key backup|ListBackupSelections|status=200->200|keys=$.BackupSelectionsList[1]:
  $.BackupSelectionsList[1]: missing — expected a BackupSelectionsListMember for the plan's second selection {BackupPlanId: the plan's id, CreationDate: the selection's create time, CreatorRequestId: null, IamRoleArn: the role the accepted selection was created with, SelectionId: the id CreateBackupSelection returned, SelectionName: the second selection's name}; got None. (The recorded run's literal plan id, role ARN, selection name and timestamp are per-run values filling that template, not requirements; the requirement is that a second member exists and describes the plan's second selection.)
  No NextToken was requested or returned and no MaxResults was sent, so the omission is not pagination.

Model checked: backup#ListBackupSelections (http GET /backup/plans/{BackupPlanId}/selections, code 200, paginated items=BackupSelectionsList, errors InvalidParameterValueException/MissingParameterValueException/ResourceNotFoundException all 400), backup#BackupSelectionsList, backup#BackupSelectionsListMember, backup#CreateBackupSelection.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus, trace set simparity-wire-aws-v1).
2. Replay fixture backup-rbx-traj-second-selection-mistakes-and-recovery: it creates a backup plan and then two backup selections on it (the second after correcting the IAM role it uses).
3. At step task#13 the fixture issues ListBackupSelections (GET /backup/plans/{BackupPlanId}/selections) for that plan with no MaxResults and no NextToken.
4. Compare the response's BackupSelectionsList against the recording: the emulator returns a single member and omits BackupSelectionsList[1], the member describing the plan's second selection. Both selections accepted for the plan must appear, each as its own BackupSelectionsListMember.
