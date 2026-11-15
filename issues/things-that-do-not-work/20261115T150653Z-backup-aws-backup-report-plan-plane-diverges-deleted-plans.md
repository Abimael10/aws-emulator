Issue-Source: trace-sweep-divergence
Issue-Trace: backup-report-jobs-listing
Issue-Step: teardown#16
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-report-jobs-listing backup-report-plan-lifecycle backup-report-plan-error-paths backup-report-plan-errors
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|DescribeReportPlan|4xx->2xx|$
Issue-Key: backup|DescribeReportPlan|4xx->2xx|$.Message
Issue-Key: backup|DescribeReportPlan|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: backup|DescribeReportPlan|4xx->2xx|status
Issue-Key: backup|CreateReportPlan|4xx->4xx|$.Message
Issue-Key: backup|DescribeReportPlan|4xx->4xx|$.Message
Issue-Key: backup|DescribeReportPlan|2xx->2xx|$.ReportPlan.ReportSetting.Accounts
Issue-Key: backup|DescribeReportPlan|2xx->2xx|$.ReportPlan.ReportSetting.FrameworkArns
Issue-Key: backup|DescribeReportPlan|2xx->2xx|$.ReportPlan.ReportSetting.OrganizationUnits
Issue-Key: backup|DescribeReportPlan|2xx->2xx|$.ReportPlan.ReportSetting.Regions
Issue-Key: backup|DescribeReportPlan|2xx->2xx|$.ReportPlan.ReportTiming
Issue-Commit: f3d9beb81be6394518942adf27ca6307cb143231

# AWS Backup report-plan plane diverges: deleted plans still read back, refusal messages re-worded, read body omits defaults

> _Note: this issue was identified against the code base at commit `f3d9beb81be6394518942adf27ca6307cb143231`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReportPlan

## Description
The Backup Audit Manager report-plan surface diverges from AWS in four independent ways: a deleted report plan is still served, both report-plan refusal messages are re-worded by the emulator, and the DescribeReportPlan 200 body omits members AWS always serializes. FOUR causes, all on service `backup`; all four must be closed before this issue is submitted.

No open issue covers the report-plan plane. The three open stub issues 20261115T122714Z-backup-deletereportplan-stub.md, 20261115T122714Z-backup-listreportplans-stub.md and 20261115T122714Z-backup-startreportjob-stub.md cover only the unimplemented operations themselves; the divergences below are observed on operations that DO answer (CreateReportPlan / DescribeReportPlan) and none of them carried a stub_path or an owning issue in the replay report.

Cause 1 of 4: a deleted report plan is still readable, so the not-found refusal never happens (two clusters, same root cause).
AWS: Smithy (services/service-com-amazonaws-backup-cryocontrollerusermanager): DeleteReportPlan DELETE /audit/report-plans/{ReportPlanName}, @idempotent, output Unit, http code 200; DescribeReportPlan GET /audit/report-plans/{ReportPlanName} with modeled error ResourceNotFoundException (errorKind client, httpError 400). Once a report plan is deleted it no longer exists, so DescribeReportPlan on that name MUST answer HTTP 400 ResourceNotFoundException (header x-amzn-errortype: ResourceNotFoundException) with Message of the form "Report plan <ReportPlanName> not found in account <accountId>." — fixed text "Report plan ", " not found in account ", "."; variable parts are the requested ReportPlanName and the caller's 12-digit account id.
Emulator: after the plan is deleted, DescribeReportPlan keeps answering HTTP 200 with the full ReportPlan body (ReportPlanArn / ReportPlanName / ReportSetting / ReportDeliveryChannel / DeploymentStatus COMPLETED / CreationTime). The fixture polls for the 400 and never gets it — "poll: status 200 not in [400] after 37 attempt(s)" / "after 30 attempt(s)", one poll capped at 180s. $.Message and $headers.x-amzn-errortype are absent entirely.
Why: a client that deletes a report plan and then confirms the deletion (or that recreates the same name after checking it is gone) sees the plan as still live forever. Deletion is silently a no-op in observable state, teardown never completes, and any poll-until-gone loop hangs until it times out.
Fix: backup report-plan state — the delete path must remove the plan from the report-plan store, and the DescribeReportPlan handler must raise ResourceNotFoundException for a name that is not in that store. Hypothesis: because DeleteReportPlan is an unimplemented scaffold (see the stub issue named above) nothing ever mutates the store; fixing the stub is necessary but the Describe-side not-found branch must exist too, otherwise a plan deleted by any path still reads back 200.
Traces: backup-report-jobs-listing#teardown#16 (also backup-report-plan-lifecycle#teardown#6) and backup-report-plan-error-paths#teardown#7.

Cause 2 of 4: CreateReportPlan's already-exists Message is re-worded.
AWS: Smithy: CreateReportPlan POST /audit/report-plans with modeled error AlreadyExistsException (client, httpError 400); ReportPlanName is required, length 1-256, pattern ^[a-zA-Z][_a-zA-Z0-9]*$. Recorded behavior for this error class: Message = "Report plan with name <ReportPlanName> already exists in account <accountId> ." — the fixed text is "Report plan with name ", " already exists in account " and the trailing " ."; the variable parts are the requested ReportPlanName and the caller's account id.
Emulator: HTTP 400 is correct, but the body carries Message = "Report Plan <ReportPlanName> already exists in account <accountId>." — "Report Plan" is capitalized, the words "with name" are dropped and the trailing space before the period is missing.
Why: the status and error class match, so the divergence only shows up in the message a client logs, surfaces to an operator or string-matches on. Any assertion or operator runbook written against AWS's sentence fails against the emulator.
Fix: the CreateReportPlan handler's AlreadyExistsException construction (message template), not its state.
Traces: backup-report-plan-error-paths#task#3 (also backup-report-plan-errors#task#4).

Cause 3 of 4: DescribeReportPlan's not-found Message capitalizes the resource type.
AWS: Smithy: DescribeReportPlan modeled error ResourceNotFoundException (client, httpError 400). Recorded behavior for this error class: Message = "Report plan <ReportPlanName> not found in account <accountId>." — fixed text "Report plan ", " not found in account ", "."; variable parts are the requested ReportPlanName and the account id.
Emulator: HTTP 400 and the error class are right, but Message reads "Report Plan <ReportPlanName> not found in account <accountId>." — "Plan" capitalized where AWS writes it lower-case. Same wording defect as Cause 2, on the other operation, so both message templates need the fix.
Why: as Cause 2 — a client matching on AWS's exact sentence (or an operator diffing logs) sees a message AWS never emits.
Fix: the DescribeReportPlan handler's ResourceNotFoundException construction (message template).
Traces: backup-report-plan-error-paths#task#4 (also backup-report-plan-errors#task#1).

Cause 4 of 4: the DescribeReportPlan 200 body omits ReportSetting's defaulted list members and the report's timing block.
AWS: Smithy shape ReportSetting has members FrameworkArns, Accounts, OrganizationUnits and Regions, all targeting stringList; a report plan created with only ReportTemplate serializes these as empty lists in the Describe body (recorded behavior: each is []). The recording additionally shows ReportPlan carrying a ReportTiming block {ReportFrequencyHours, ReportStartHour} with the service-chosen defaults (24-hour frequency, start hour 0) — recorded behavior only: the checked-in model's ReportPlan/ReportSetting shapes define no ReportTiming member, so the recording is the authority for its presence.
Emulator: ReportSetting is serialized with only ReportTemplate and NumberOfFrameworks; $.ReportPlan.ReportSetting.Accounts, .FrameworkArns, .OrganizationUnits and .Regions are all absent (expected [], got None) and $.ReportPlan.ReportTiming is absent entirely (expected {ReportFrequencyHours: 24, ReportStartHour: 0}).
Why: a client reading back a report plan it just created cannot distinguish "no accounts/regions/frameworks scoped" from "this field is not reported", and sees no schedule for the report at all; code that iterates ReportSetting.Regions or reads ReportTiming.ReportFrequencyHours gets a null dereference against the emulator where AWS gives it an empty list and the defaulted timing.
Fix: the DescribeReportPlan serializer (and the report-plan state defaults it reads) — the four list members must be emitted as empty lists rather than omitted, and the stored plan must carry the defaulted report timing so it can be serialized.
Traces: backup-report-plan-lifecycle#task#2.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200). Five net-new divergence clusters, all service=backup, none carrying stub_path or owned_by.

Cause 1 — DescribeReportPlan, status expected 400, got 200:
  backup-report-jobs-listing#teardown#16 (affects backup-report-plan-lifecycle#teardown#6):
    expected {'Code': None, 'Context': None, 'Message': 'Report plan sim2real_probe_backup_ReportPlanJobs_146c48f6 not found in account 000000000000.', 'Type': None}
    got      {'ReportPlan': {'ReportPlanArn': 'arn:aws:backup:...:report-plan:sim2real_probe_backup_ReportPlanJobs_146c48f6-...', 'ReportPlanName': 'sim2real_probe_backup_ReportPlanJobs_146c48f6', 'ReportSetting': {'ReportTemplate': 'COPY_JOB_REPORT', 'NumberOfFrameworks': 0}, 'ReportDeliveryChannel': {...}, 'DeploymentStatus': 'COMPLETED', 'CreationTime': ...}}
    $.Message: missing; $headers.x-amzn-errortype: missing; "poll: status 200 not in [400] after 37 attempt(s)", poll capped at 180s.
  backup-report-plan-error-paths#teardown#7: same shape — "poll: status 200 not in [400] after 30 attempt(s)", body still the live ReportPlan with DeploymentStatus COMPLETED.

Cause 2 — CreateReportPlan 400 -> 400, $.Message value_mismatch (backup-report-plan-error-paths#task#3, backup-report-plan-errors#task#4):
    expected 'Report plan with name <name> already exists in account <accountId> .'
    got      'Report Plan <name> already exists in account <accountId>.'
  (recorded instance: name sim2real_probe_backup_ReportPlanErr_82a15200, account 000000000000 — per-run values, not requirements.)

Cause 3 — DescribeReportPlan 400 -> 400, $.Message value_mismatch (backup-report-plan-error-paths#task#4, backup-report-plan-errors#task#1):
    expected 'Report plan <name> not found in account <accountId>.'
    got      'Report Plan <name> not found in account <accountId>.'
  (recorded instance: name sim2real_probe_backup_NoSuchReportPlan_5d361c56.)

Cause 4 — DescribeReportPlan 200 -> 200, missing members (backup-report-plan-lifecycle#task#2):
    $.ReportPlan.ReportSetting.Accounts: missing (expected [], got None)
    $.ReportPlan.ReportSetting.FrameworkArns: missing (expected [], got None)
    $.ReportPlan.ReportSetting.OrganizationUnits: missing (expected [], got None)
    $.ReportPlan.ReportSetting.Regions: missing (expected [], got None)
    $.ReportPlan.ReportTiming: missing (expected {'ReportFrequencyHours': 24, 'ReportStartHour': 0}, got None)

Model used: smithy_query operation=CreateReportPlan,DescribeReportPlan,DeleteReportPlan and shape=ReportPlan,ReportSetting,ReportDeliveryChannel on services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay backup-report-jobs-listing step teardown#16 (and backup-report-plan-error-paths step teardown#7). Create a report plan, delete it, then GET /audit/report-plans/<ReportPlanName>: the emulator answers 200 with the live ReportPlan body instead of 400 ResourceNotFoundException "Report plan <name> not found in account <accountId>."
3. Cause 2: replay backup-report-plan-error-paths step task#3. POST /audit/report-plans twice with the same ReportPlanName; compare the 400 body's Message against 'Report plan with name <name> already exists in account <accountId> .'
4. Cause 3: replay backup-report-plan-error-paths step task#4. GET /audit/report-plans/<a name that was never created>; compare the 400 body's Message against 'Report plan <name> not found in account <accountId>.'
5. Cause 4: replay backup-report-plan-lifecycle step task#2. Create a report plan whose ReportSetting carries only ReportTemplate, then DescribeReportPlan it; check that ReportSetting.Accounts / .FrameworkArns / .OrganizationUnits / .Regions are present as empty lists and that ReportPlan.ReportTiming is present with the defaulted frequency and start hour.
