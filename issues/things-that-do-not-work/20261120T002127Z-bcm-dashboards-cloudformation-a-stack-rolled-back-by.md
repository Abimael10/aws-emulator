Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bcm-dashboards__cloudformation.deploy-bcm-dashboard-invalid-widget-title-rejected-and-rolled-back
Issue-Key: bcm-dashboards__cloudformation.deploy-bcm-dashboard-invalid-widget-title-rejected-and-rolled-back|failed_reason#8|AssertionError|assert len(rows) == 1, f"expected one {status} row for {logical_id}, got {rows!r}"
Issue-Commit: 7a863d1ad62c4708957aba7eef669d745200ca52

# bcm-dashboards__cloudformation: a stack rolled back by AWS::BCM::Dashboard property validation emits no stack-level CREATE_FAILED event, so the validation reason is unreadable

> _Note: this issue was identified against the code base at commit `7a863d1ad62c4708957aba7eef669d745200ca52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
One net-new cause behind the single net-new failing bcm-dashboards__cloudformation check. (The other bcm-dashboards open issues — 20261022T215638Z-bcm-dashboards-dashboardname-pattern-unenforced-constraint-not-found.md, 20261114T112305Z-bcm-dashboards-getdashboard-serves-the-wrong-widget-list-for-every.md and the two ListDashboards @range message issues — are about the bcm-dashboards data plane and do not cover this CloudFormation event-stream cause. The analogous cause is filed for a different service in 20261109T123922Z-apigateway-cloudformation-6-net-new-causes-a-not.md, Cause 4; that issue carries only apigateway__cloudformation replay keys and does not cover this one.)

Cause 1 of 1: when a CloudFormation create fails the resource registry's property validation and rolls the stack back, the stack's own event stream carries no CREATE_FAILED row, so the aggregate validation reason is lost.

AWS: a stack whose resource property violates the resource type's schema fails before provisioning, and DescribeStackEvents contains exactly one row whose LogicalResourceId is the stack's own name with ResourceStatus CREATE_FAILED and ResourceStatusReason exactly `Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation.` — that text is fixed; the only variable part is the error count (`1 error(s)` becomes the number of validation errors found). The per-property detail is then readable from DescribeEvents as a VALIDATION_ERROR operation event naming the resource's logical id, PROPERTY_VALIDATION / FAILED / FAIL, the reason `Property value [<rejected value>] does not match pattern: <pattern>` and the ValidationPath of the offending member — here `/Resources/Dashboard/Properties/Widgets/0/Title`. Smithy (services/service-com-amazonaws-bcmdashboards-awsbcmdashboardsservice): Widget.title is @required, @length min 2 max 50, @pattern `^(?!.* {2})[a-zA-Z0-9_-][ a-zA-Z0-9_-]*[a-zA-Z0-9_-]$`, so a title with a leading space is invalid and the AWS::BCM::Dashboard registry schema carries the same pattern; the stack-event wording itself is recorded behavior only.

Emulator: the stack does settle ROLLBACK_COMPLETE (the property violation is detected), but DescribeStackEvents returns no row at all whose LogicalResourceId is the stack name and whose ResourceStatus is CREATE_FAILED — the filtered list is empty. There is therefore no `Validation failed with N error(s). ...` reason anywhere in the stack's events.

Why: a client that deploys a template and, on failure, reads DescribeStackEvents to learn why, finds a stack that merely "rolled back" with no reason and no resource named. It cannot distinguish a schema violation it must fix in the template from a throttle, a quota, a permissions failure or an emulator-side refusal, and CI that surfaces the CREATE_FAILED reason prints nothing.

Fix: the stack-create path's resource-schema/property validation step and the stack-event writer it feeds in the CloudFormation service — the validation failure must emit the stack-level CREATE_FAILED event with the aggregate reason (and the matching VALIDATION_ERROR operation event) before the rollback is recorded, for every registered resource type, not only for the types that already do.

Traces: bcm-dashboards__cloudformation.deploy-bcm-dashboard-invalid-widget-title-rejected-and-rolled-back

## Evidence
Failing check (mismatch): bcm-dashboards__cloudformation.deploy-bcm-dashboard-invalid-widget-title-rejected-and-rolled-back

The check creates a stack containing
  "Dashboard": {"Type": "AWS::BCM::Dashboard", "Properties": {"Name": <name>, "Widgets": [{"Title": " Leading Space", "Configs": [...]}]}}
The leading space violates Widget.title's pattern.

Expected (AWS): stack settles ROLLBACK_COMPLETE; DescribeStackEvents has exactly one row with LogicalResourceId == <stack name>, ResourceStatus == CREATE_FAILED and
  ResourceStatusReason == "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation."
and DescribeEvents reports the VALIDATION_ERROR row
  ("Dashboard", "PROPERTY_VALIDATION", "FAILED", "FAIL", "Property value [ Leading Space] does not match pattern: ^(?!.* {2})[a-zA-Z0-9_-][ a-zA-Z0-9_-]*[a-zA-Z0-9_-]$", "/Resources/Dashboard/Properties/Widgets/0/Title").

Actual (emulator): the ROLLBACK_COMPLETE assertion passes, then the event lookup fails:
  File "check_bcm_dashboards__cloudformation_deploy_bcm_dashboard_invalid_widget_title_rejected_and_rolled_back.py", line 240, in run
    reason = failed_reason(stack_events(cf, bad_id), bad_stack)
  File "...", line 123, in failed_reason
    assert len(rows) == 1, f"expected one {status} row for {logical_id}, got {rows!r}"
  AssertionError: expected one CREATE_FAILED row for sim2real-bcmdashneg-<suffix>, got []

The stack name in the message is a per-run generated value; the requirement is that one stack-level CREATE_FAILED row exists and carries the fixed `Validation failed with N error(s). Call DescribeEvents ...` text.

## Steps to Reproduce
1. CreateStack with a template whose only resource is
   AWS::BCM::Dashboard with Properties {Name: <any valid name>, Widgets: [{Title: " Leading Space", Configs: [{QueryParameters: {CostAndUsage: {Metrics: ["UnblendedCost"], TimeRange: {StartTime: {Type: RELATIVE, Value: "-P1M"}, EndTime: {Type: RELATIVE, Value: "-P0D"}}, Granularity: MONTHLY}}, DisplayConfig: {Table: {}}}]}]}
   (the leading space violates Widget.title's @pattern).
2. Poll DescribeStacks by StackId until a terminal status — the emulator reports ROLLBACK_COMPLETE, as AWS does.
3. Page DescribeStackEvents for that StackId and filter rows where LogicalResourceId == the stack name and ResourceStatus == "CREATE_FAILED".
   Expected: exactly one row, ResourceStatusReason "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation."
   Actual: no such row ([]).
4. Page DescribeEvents for the same stack and filter EventType == "VALIDATION_ERROR" to confirm the per-property PROPERTY_VALIDATION row with ValidationPath "/Resources/Dashboard/Properties/Widgets/0/Title" is reported.
5. Re-replay the check in trace_ids to confirm the fix.
