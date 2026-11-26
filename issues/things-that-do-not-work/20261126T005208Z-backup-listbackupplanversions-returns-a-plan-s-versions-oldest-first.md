Issue-Source: trace-sweep-divergence
Issue-Trace: backup-rbx-traj-update-adds-cross-region-copy-action
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-rbx-traj-update-adds-cross-region-copy-action
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|ListBackupPlanVersions|2xx->2xx|$.BackupPlanVersionsList[*].DeletionDate
Issue-Key: backup|ListBackupPlanVersions|2xx->2xx|$.BackupPlanVersionsList[*].VersionId
Issue-Commit: bf205090aef11dede75ce528b7b57e0b8feca92b

# ListBackupPlanVersions returns a plan's versions oldest-first, so the current version is not first

> _Note: this issue was identified against the code base at commit `bf205090aef11dede75ce528b7b57e0b8feca92b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListBackupPlanVersions

## Description
Cause 1 of 1: ListBackupPlanVersions orders BackupPlanVersionsList the wrong way round after an UpdateBackupPlan.

AWS: recorded behavior only — the Smithy model documents ListBackupPlanVersions as 'Returns version metadata of your backup plans, including Amazon Resource Names (ARNs), backup plan IDs, creation and deletion dates, plan names, and version IDs' and states no ordering for BackupPlanVersionsList (Smithy: ListBackupPlanVersions, output member BackupPlanVersionsList -> list of BackupPlansListMember; BackupPlansListMember.DeletionDate is documented as 'The date and time a backup plan is deleted, in Unix format and Coordinated Universal Time (UTC)', i.e. it is set only on a version the plan no longer holds). The recording (HTTP 200) shows the list led by the plan's CURRENT version — the one with no DeletionDate — followed by the version the update superseded, which carries its DeletionDate: i.e. versions are reported most-recent-first, so element 0 is always the version a subsequent GetBackupPlan/UpdateBackupPlan would act on.

Emulator: the same 200 body carries exactly the same two version records with exactly the same version ids and the same DeletionDate assignment (current version undated, superseded version dated), but in the OPPOSITE order: the superseded, deleted version is element 0 and the current version is element 1. The replayed step reports it positionally as four key diffs on one cluster — $.BackupPlanVersionsList[0].VersionId / [1].VersionId swapped, and DeletionDate consequently 'type_mismatch (expected None, got <ts>)' at index 0 and 'missing (expected <ts>, got None)' at index 1. Those are one fault, not four: the handler's own doc comment says it walks a plan's history 'oldest first' (services/service-com-amazonaws-backup-cryocontrollerusermanager/src/ops/list_backup_plan_versions.rs, module doc and the `readable_versions` collect in `handle`).

Why: a client that reads BackupPlanVersionsList[0] to find the plan's live version — the normal way to discover the current VersionId before an update, or to show 'current vs previous' in a console — gets the version that was already deleted, and its DeletionDate is populated, so a client that treats a dated version as inactive concludes the plan has no active version. Because paging is layered on the same walk, MaxResults=1 returns the deleted version as the whole first page and hides the current one behind a NextToken.

Fix: the ordering of the version walk feeding this operation's handler — services/service-com-amazonaws-backup-cryocontrollerusermanager/src/ops/list_backup_plan_versions.rs `handle` (the `held` vector built from `crate::state::BackupPlanRecord::readable_versions`), i.e. in state's `readable_versions` projection or where the handler consumes it, so the list is emitted most-recent-first (undeleted current version first, superseded versions after it in descending recency) before the page window and NextToken are computed from it. The DeletionDate values themselves need no change — only element order. Timestamps and version ids in the recording are per-run values and are not requirements.

Traces: backup-rbx-traj-update-adds-cross-region-copy-action#task#6 (ListBackupPlanVersions, 200 vs 200, keys $.BackupPlanVersionsList[0].DeletionDate, [0].VersionId, [1].DeletionDate, [1].VersionId).

## Evidence
Fixture backup-rbx-traj-update-adds-cross-region-copy-action, step task#6, operation ListBackupPlanVersions, expected status 200, actual status 200.

Expected (AWS recording) BackupPlanVersionsList:
  [0] VersionId = <current version id>, DeletionDate = None
  [1] VersionId = <superseded version id>, DeletionDate = <deletion timestamp>

Actual (emulator) BackupPlanVersionsList:
  [0] VersionId = <superseded version id>, DeletionDate = <deletion timestamp>
  [1] VersionId = <current version id>, DeletionDate = None

Replay detail verbatim:
  $.BackupPlanVersionsList[0].DeletionDate: type_mismatch (expected None, got 1795654015.999)
  $.BackupPlanVersionsList[0].VersionId: value_mismatch (expected 'OTZkODY0M2ItZjFhZC00MzUwLWJhMjgtZWIwZTU5NDBiMmY0', got 'YmIxNGI5ZTMtYWRlZS00ZWNlLWFlNDktZjhiMmI0OWUyYzIx')
  $.BackupPlanVersionsList[1].DeletionDate: missing (expected 1791546749.448, got None)
  $.BackupPlanVersionsList[1].VersionId: value_mismatch (expected 'YmIxNGI5ZTMtYWRlZS00ZWNlLWFlNDktZjhiMmI0OWUyYzIx', got 'OTZkODY0M2ItZjFhZC00MzUwLWJhMjgtZWIwZTU5NDBiMmY0')

The two sets of records are identical; only their positions differ, so the single observable divergence is list order (current version not first). The literal version ids and timestamps above are per-run values quoted only to show the swap.

## Steps to Reproduce
1. Create a backup plan (CreateBackupPlan) and note its BackupPlanId and VersionId.
2. Call UpdateBackupPlan on that plan (e.g. adding a cross-region copy action), producing a second, current VersionId; the first version becomes superseded and acquires a DeletionDate.
3. Call ListBackupPlanVersions with that BackupPlanId.
4. Inspect BackupPlanVersionsList: AWS returns the current version (DeletionDate absent) at index 0 and the superseded version (DeletionDate set) at index 1; the emulator returns them in the reverse order.
5. Equivalently, replay fixture backup-rbx-traj-update-adds-cross-region-copy-action and observe step task#6.
