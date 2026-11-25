Issue-Source: trace-sweep-divergence
Issue-Trace: backup-report-plan-lifecycle
Issue-Step: task#3
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-report-plan-lifecycle
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|ListReportPlans|2xx->2xx|$.ReportPlans[*]
Issue-Commit: f468f37ea639de53f535b93618c112f23df311d1

# ListReportPlans emits empty arrays for unset ReportSetting scoping members

> _Note: this issue was identified against the code base at commit `f468f37ea639de53f535b93618c112f23df311d1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListReportPlans

## Description
One cause.

Cause 1 of 1: ListReportPlans serializes the optional scoping list members of ReportSetting as empty JSON arrays for a report plan that was created without them, where AWS omits those members from the body entirely.

AWS: Smithy ListReportPlans -> GET /audit/report-plans, success 200, output {ReportPlans: ReportPlanList, NextToken} (smithy_query closure=ListReportPlans on services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json). ReportPlan.ReportSetting targets the ReportSetting structure, whose members are ReportTemplate (string, required), FrameworkArns (stringList), NumberOfFrameworks (integer, @default 0), Accounts (stringList), OrganizationUnits (stringList) and Regions (stringList). The four stringList members are neither required nor defaulted, so an unset one is ABSENT from the serialized structure; only ReportTemplate and the defaulted NumberOfFrameworks are present for a plan created with a report template alone. The recording agrees: the listed plan's ReportSetting carries ReportTemplate and NumberOfFrameworks 0 and no FrameworkArns, Accounts, OrganizationUnits or Regions member. The same recording shows the sibling optional members behaving that way elsewhere in the body (ReportDeliveryChannel.S3KeyPrefix, LastAttemptedExecutionTime, LastSuccessfulExecutionTime all absent), and the emulator already omits those — the fault is confined to the ReportSetting lists.

Emulator: 200, but the listed plan's ReportSetting carries FrameworkArns [], Accounts [], OrganizationUnits [] and Regions [] — present-and-empty where AWS has no member at all. Everything else about the entry (ARN, name, description, ReportTemplate, NumberOfFrameworks, ReportDeliveryChannel, DeploymentStatus, ReportTiming) matches.

Why: the response loses the distinction between "this plan states no scoping" and "this plan states an explicitly empty scope". A client that tests member presence (`if 'Accounts' in report_setting`, or a null check in a typed SDK) takes the unscoped plan for a scoped one, and a read-modify-write that feeds the listed ReportSetting straight back into UpdateReportPlan now sends explicit empty lists AWS never returned, changing the request it submits. Note this is the opposite direction from the open issue 20261119T231535Z-backup-aws-backup-restore-testing-selection-read-bodies-omit.md, which is about restore-testing SELECTION reads dropping members AWS does send; that issue names no report-plan operation, member or fixture, so it does not cover this.

Fix: the ListReportPlans response serializer / the ReportPlan.ReportSetting projection in the backup service crate — an unset optional list on the stored report plan must be omitted from the serialized ReportSetting rather than rendered as an empty array (the same projection must keep emitting ReportTemplate and the defaulted NumberOfFrameworks).

Traces: backup-report-plan-lifecycle#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up: /__admin/health answered 200 and the data listener accepts).

Cause 1 - backup ListReportPlans, fixture backup-report-plan-lifecycle step task#3, expected 200 / actual 200:
  $.ReportPlans[0]: mismatch. Expected ReportSetting = {'ReportTemplate': 'BACKUP_JOB_REPORT', 'NumberOfFrameworks': 0, 'FrameworkArns': None, 'Accounts': None, 'OrganizationUnits': None, 'Regions': None} (i.e. the four list members absent from the body); actual ReportSetting = {'ReportTemplate': 'BACKUP_JOB_REPORT', 'FrameworkArns': [], 'NumberOfFrameworks': 0, 'Accounts': [], 'OrganizationUnits': [], 'Regions': []}.
  All other members of the entry agree, including the absences the emulator gets right (ReportDeliveryChannel.S3KeyPrefix, LastAttemptedExecutionTime, LastSuccessfulExecutionTime). CreationTime is shape-asserted only and is not part of this finding.
  verdict: mismatch, divergence_key service=backup|ListReportPlans|status=200->200|keys=$.ReportPlans[0]
  affected: backup-report-plan-lifecycle#task#3 (1 step)

Model reference: ReportSetting members {ReportTemplate: string (required), FrameworkArns: stringList, NumberOfFrameworks: integer default 0, Accounts: stringList, OrganizationUnits: stringList, Regions: stringList}; ListReportPlans GET /audit/report-plans code 200, output {ReportPlans: ReportPlanList, NextToken}.

## Steps to Reproduce
1. git clone the repository and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1.
3. Replay fixture backup-report-plan-lifecycle through step task#3: CreateReportPlan with a ReportSetting naming only ReportTemplate BACKUP_JOB_REPORT, then ListReportPlans.
4. Inspect ReportPlans[0].ReportSetting in the 200 body: it carries FrameworkArns, Accounts, OrganizationUnits and Regions as empty arrays; AWS omits all four members for a plan created without them.
