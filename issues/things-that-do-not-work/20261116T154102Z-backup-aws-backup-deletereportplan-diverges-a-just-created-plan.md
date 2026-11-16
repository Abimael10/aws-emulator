Issue-Source: trace-sweep-divergence
Issue-Trace: backup-report-plan-error-paths
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-report-plan-error-paths backup-report-plan-errors
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|DeleteReportPlan|4xx->4xx|$.Message
Issue-Key: backup|DeleteReportPlan|2xx->4xx|status
Issue-Commit: 782d16bdd1ce5a2a9842801291a869e14f8231ec

# AWS Backup DeleteReportPlan diverges: a just-created plan is refused 409 and the not-found refusal mis-capitalizes "Report plan"

> _Note: this issue was identified against the code base at commit `782d16bdd1ce5a2a9842801291a869e14f8231ec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteReportPlan

## Description
DeleteReportPlan (DELETE /audit/report-plans/{ReportPlanName}) diverges from AWS in TWO independent ways: its not-found refusal is worded with a capitalized resource type AWS never emits, and a delete that arrives shortly after the plan's create is refused ConflictException instead of succeeding. TWO causes, both on service `backup`, both on DeleteReportPlan; BOTH must be closed before this issue is submitted.

Neither cause is covered by an open issue. The open issue 20261115T150653Z-backup-aws-backup-report-plan-plane-diverges-deleted-plans.md covers the report-plan plane's OTHER divergences (DescribeReportPlan's not-found wording, CreateReportPlan's already-exists wording, the Describe 200 body's omitted defaults, and deleted plans still reading back) — its causes are keyed on CreateReportPlan/DescribeReportPlan and it explicitly hypothesizes that DeleteReportPlan is an unimplemented stub. DeleteReportPlan now DOES answer, and the two clusters below were reported by the replay with no stub_path and no owning issue. Cause 2 here is in fact the mechanism behind that issue's Cause 1 (a plan that reads back after a delete), because the delete never took effect; it still has to be fixed here, on the delete path.

Cause 1 of 2: the DeleteReportPlan not-found refusal capitalizes the resource type.
AWS: Smithy (services/service-com-amazonaws-backup-cryocontrollerusermanager): DeleteReportPlan DELETE /audit/report-plans/{ReportPlanName}, @idempotent, output Unit, success code 200; ReportPlanName is an @httpLabel, @required, length 1-256, pattern ^[a-zA-Z][_a-zA-Z0-9]*$; modeled errors ConflictException (client, httpError 409), InvalidParameterValueException (400), MissingParameterValueException (400), ResourceNotFoundException (client, httpError 400 defaulted), ServiceUnavailableException (500). Deleting a name the account holds no report plan under MUST answer HTTP 400 ResourceNotFoundException with Message of the form "Report plan <ReportPlanName> not found in account <accountId>." — the FIXED text is "Report plan ", " not found in account " and the trailing "."; the VARIABLE parts are the ReportPlanName the request carried in the path label and the caller's 12-digit account id. (Recorded behavior supplies the sentence; the model declares only the error class.)
Emulator: the status (400) and error class are right, but the body carries Message = "Report Plan <ReportPlanName> not found in account <accountId>." — "Plan" capitalized where AWS writes it lower-case. The sentence is built by one shared helper, so the same wrong wording is served for every report-plan read as well as this delete.
Why: a client or operator runbook that matches on AWS's exact sentence (log-scraping, an error-message assertion, a support-tooling diff) sees a message AWS never emits, and the status/code checks pass so the difference is only visible in text a human reads or code string-matches.
Fix: the report-plan state's not-found message template (the one helper every report-plan refusal words itself from), not the DeleteReportPlan handler's control flow — lower-case the "plan" in the fixed text and keep the name and account id as the only variable parts. Note this is the same template behind Cause 3 of the open report-plan-plane issue, so the two fixes coincide; this cluster is still unowned and must be confirmed on DeleteReportPlan.
Traces: backup-report-plan-error-paths#task#5 (also backup-report-plan-errors#task#2).

Cause 2 of 2: deleting a report plan that was just created is refused 409 ConflictException instead of succeeding, so the plan is never removed.
AWS: Smithy: DeleteReportPlan is @idempotent with success code 200 and output Unit; ConflictException (409) is documented as "Backup can't perform the action that you requested until it finishes performing a previous action. Try again later." Recorded behavior: the delete of an existing report plan answers HTTP 200 with an empty body, and the plan is then gone (the following read refuses it). AWS does not make a report plan undeletable for a settling window after its create.
Emulator: the delete answers HTTP 409 ConflictException ("Operation failed because of a concurrent update for the report plan <name> in account <accountId>. Try again later.") and leaves the plan in the store. The handler settles due deployments and then refuses the delete whenever the record's DeploymentStatus is anything other than COMPLETED; the stored plan is given an emulator-chosen 3-second deployment window after its create, so any delete inside that window — which is every delete in a programmatic create/use/delete sequence — is refused. Status class diverges 200 -> 409.
Why: deletion of a report plan is unavailable for the first seconds of its life, with no way for a client to tell this synthetic conflict from a real concurrent-action conflict. A teardown or cleanup step that does not retry on 409 leaks the plan, its name stays taken (so a later create of the same name is refused AlreadyExists), and any confirm-it-is-gone read keeps answering 200 with the live plan body forever — which is exactly the observable behavior the open report-plan-plane issue records as "a deleted plan still reads back".
Fix: the DeleteReportPlan handler's refusal branch (and the deployment-window the report-plan state assigns a freshly created plan). The delete must remove the record and answer 200 for a plan that exists; the ConflictException branch must be reserved for a genuinely concurrent action on the plan, not for the emulator's own settling delay.
Traces: backup-report-plan-error-paths#teardown#6 (the following DescribeReportPlan at backup-report-plan-error-paths#teardown#7 then polls for the not-found refusal 30 times and never gets it; that cluster is owned by the open report-plan-plane issue).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health 200, data listener accepting; 92 fixtures replayed, 583 steps, score 64.7%). 39 divergence clusters; 37 carried an owning issue or a stub path. The two below carried owned_by=[] and stub_path="", both service=backup, operation=DeleteReportPlan.

Cause 1 — DeleteReportPlan 400 -> 400, $.Message value_mismatch
  cluster key: service=backup|DeleteReportPlan|status=400->400|keys=$.Message
  representative: backup-report-plan-error-paths#task#5 (also affects backup-report-plan-errors#task#2)
    expected Message: 'Report plan <ReportPlanName> not found in account <accountId>.'
    actual   Message: 'Report Plan <ReportPlanName> not found in account <accountId>.'
  Recorded instance (per-run values, NOT requirements): ReportPlanName sim2real_probe_backup_NoSuchReportPlan_1e86e515, account 000000000000. Fixed text = "Report plan " / " not found in account " / "."; variable = the requested name and the account id.

Cause 2 — DeleteReportPlan expected 200, got 409
  cluster key: service=backup|DeleteReportPlan|status=200->409|keys=status
  representative: backup-report-plan-error-paths#teardown#6 (affected_count 1)
    expected status 200 (empty Unit body), actual status 409 ConflictException.
  Downstream observation in the same fixture (cluster owned by 20261115T150653Z-backup-aws-backup-report-plan-plane-diverges-deleted-plans.md): backup-report-plan-error-paths#teardown#7 DescribeReportPlan expected 400, got 200 — "poll: status 200 not in [400] after 30 attempt(s)" with the live body {'ReportPlan': {'ReportPlanArn': 'arn:aws:backup:us-west-2:000000000000:report-plan:<name>-<uniquifier>', 'ReportPlanName': '<name>', 'ReportSetting': {'ReportTemplate': 'BACKUP_JOB_REPORT', 'NumberOfFrameworks': 0}, 'ReportDeliveryChannel': {...}, 'DeploymentStatus': 'COMPLETED', 'CreationTime': <ts>}} — i.e. the plan was never removed, and by the time it is read its deployment has settled, confirming the 409 came from the settling window rather than any real concurrent action.

Model consulted: smithy_query query=operation operation=DeleteReportPlan,CreateReportPlan on services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json — DeleteReportPlan: http {DELETE, /audit/report-plans/{ReportPlanName}, code 200}, output_shape Unit, idempotent, errors ConflictException/409, ResourceNotFoundException/400, InvalidParameterValueException/400, MissingParameterValueException/400, ServiceUnavailableException/500.
Code read for the fix locations: services/service-com-amazonaws-backup-cryocontrollerusermanager/src/ops/delete_report_plan.rs lines 187-212 (settle, resolve, in-flight refusal, remove) and services/service-com-amazonaws-backup-cryocontrollerusermanager/src/state/report_plan.rs lines 117-122 (not-found message template), 67 (REPORT_PLAN_SETTLE_AFTER = 3s), 296-300 (deployment_in_flight), 334-340 (settle_report_plans).

## Steps to Reproduce
1. Clone this checkout, boot the emulator from it and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture backup-report-plan-error-paths step task#5 (and backup-report-plan-errors step task#2). Send DELETE /audit/report-plans/<a ReportPlanName that was never created>. The response is HTTP 400 ResourceNotFoundException, but compare its Message against AWS's 'Report plan <name> not found in account <accountId>.' — the emulator answers 'Report Plan <name> not found in account <accountId>.'
3. Cause 2: replay fixture backup-report-plan-error-paths step teardown#6. Equivalently by hand: POST /audit/report-plans to create a report plan (ReportSetting.ReportTemplate BACKUP_JOB_REPORT, a ReportDeliveryChannel with an S3 bucket), then immediately (within ~3 seconds) DELETE /audit/report-plans/<ReportPlanName>. The emulator answers HTTP 409 ConflictException 'Operation failed because of a concurrent update for the report plan <name> in account <accountId>. Try again later.' and keeps the plan; AWS answers HTTP 200 with an empty body and the plan is gone.
4. Confirm the leak: after step 3's delete, GET /audit/report-plans/<ReportPlanName> (fixture step backup-report-plan-error-paths#teardown#7). The emulator keeps answering HTTP 200 with the live ReportPlan body (DeploymentStatus COMPLETED) instead of the 400 ResourceNotFoundException AWS gives for a deleted plan.
