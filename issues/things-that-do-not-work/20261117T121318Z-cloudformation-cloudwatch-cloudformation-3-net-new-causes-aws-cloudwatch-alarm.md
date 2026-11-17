Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch.deploy-cloudwatch-alarm-wall-clock-window-invalid-period-rejected-rollback cloudformation__cloudwatch.deploy-cloudwatch-alarm-warm-up-and-evaluation-window-through-update-and-replacement cloudformation__cloudwatch.cfn-dashboard-invalid-body-reason-then-delete-removes-dashboard cloudformation__cloudwatch.cfn-cloudwatch-anomaly-detector-configuration-update-in-place-then-stat-replacement-deletes-cleanly cloudformation__cloudwatch.cfn-cloudwatch-anomaly-detector-update-to-invalid-stat-rolls-back-unchanged
Issue-Key: cloudformation__cloudwatch.deploy-cloudwatch-alarm-wall-clock-window-invalid-period-rejected-rollback|run#9|AssertionError|assert status == "CREATE_COMPLETE", f"positive-control stack ended {status!r}"
Issue-Key: cloudformation__cloudwatch.deploy-cloudwatch-alarm-warm-up-and-evaluation-window-through-update-and-replacement|run#12|AssertionError|assert status == "CREATE_COMPLETE", f"stack {stack_name!r} ended {status!r}, not CREATE_COMPLETE"
Issue-Key: cloudformation__cloudwatch.cfn-dashboard-invalid-body-reason-then-delete-removes-dashboard|_split_reason_ids#13|AssertionError|assert tail, f"reason {reason!r} carries no 'Request ID: ' marker"
Issue-Key: cloudformation__cloudwatch.cfn-cloudwatch-anomaly-detector-configuration-update-in-place-then-stat-replacement-deletes-cleanly|_eventually#14|Timeout|time.sleep(_POLL_S)
Issue-Key: cloudformation__cloudwatch.cfn-cloudwatch-anomaly-detector-update-to-invalid-stat-rolls-back-unchanged|_eventually#13|Timeout|time.sleep(_POLL_S)
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# cloudformation: 3 net-new causes — AWS::CloudWatch::Alarm rolls the stack back when EvaluationWindow/WarmUpConfiguration are declared, CloudWatch resource failure reasons use the registry-handler wrapper instead of the legacy service-error format, and the AnomalyDetector update/replace lifecycle never converges

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
This issue bundles 3 net-new root causes, all reached through CloudFormation stacks whose resources are AWS::CloudWatch::*. Each is numbered with its own failing check ids; all three must be closed before submitting.

Cause 1 of 3: a stack whose AWS::CloudWatch::Alarm declares EvaluationWindow (and/or WarmUpConfiguration) fails creation and the stack ends ROLLBACK_COMPLETE instead of CREATE_COMPLETE.
AWS: AWS::CloudWatch::Alarm supports the alarm evaluation window and warm-up properties (EvaluationWindow -> {WallClockWindow: {Timezone}} and WarmUpConfiguration -> {WarmUpPeriodDurationInMinutes, OnlyStartEvaluatingAfterWarmUpPeriodEnds}); a template carrying a valid Period together with these properties deploys to CREATE_COMPLETE and DescribeAlarms reads both structures back unchanged, then both update IN PLACE on a stack update while AlarmName remains the only createOnly property. Recorded behavior only (the resource schema is not in the Smithy API model); the underlying PutMetricAlarm is modelled by cloudwatch 2010-08-01.
Emulator: both stacks — the positive-control one with a VALID Period and EvaluationWindow={'WallClockWindow': {}}, and the one with WarmUpConfiguration + EvaluationWindow={'WallClockWindow': {'Timezone': 'America/New_York'}} — end ROLLBACK_COMPLETE, so the alarm is never created and every read-back, in-place-update and replacement assertion behind it is unreachable.
Why: no alarm using an evaluation window or a warm-up period can be managed as infrastructure-as-code; a template AWS accepts rolls back, and the valid-vs-invalid Period rejection path cannot be distinguished from this blanket failure.
Fix: the AWS::CloudWatch::Alarm resource handler's property mapping for EvaluationWindow and WarmUpConfiguration (pass them through to PutMetricAlarm and read them back in the resource's describe), not the stack engine.
Failing checks: cloudformation__cloudwatch.deploy-cloudwatch-alarm-wall-clock-window-invalid-period-rejected-rollback, cloudformation__cloudwatch.deploy-cloudwatch-alarm-warm-up-and-evaluation-window-through-update-and-replacement.

Cause 2 of 3: the ResourceStatusReason of a failed AWS::CloudWatch::Dashboard is the resource-handler wrapper and carries none of the service-error fields.
AWS: when a CloudWatch resource's underlying API call is refused, the CREATE_FAILED event's ResourceStatusReason carries the service's own error envelope — the message followed by a parenthesised '(Service: CloudWatch, Status Code: 400, Request ID: <id>, ...)' tail, i.e. it always contains a 'Request ID: ' marker (and, for a service error, the error code). The fixed text is the service error's own message plus those field labels; the variable parts are the message detail, the status code, the request id and the error code.
Emulator: for an AWS::CloudWatch::Dashboard whose DashboardBody is not a JSON object the reason is 'Resource handler returned message: "The field DashboardBody must be a valid JSON object" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)' — the registry-handler wrapper only, with a RequestToken and a HandlerErrorCode and no 'Request ID: ' and no service error code.
Why: tooling and humans parse the reason to learn which service refused, with which error code and under which request id; none of that is present, so a failure cannot be correlated with the service call that caused it. The same defect reached through a composite alarm is filed under the cloudwatch service heading (its reason must carry 'Error Code: ValidationError').
Fix: the CloudFormation resource-failure reason builder for CloudWatch-backed resources — propagate the service error's code, status code and request id into the reason instead of replacing them with the handler wrapper.
Failing check: cloudformation__cloudwatch.cfn-dashboard-invalid-body-reason-then-delete-removes-dashboard.

Cause 3 of 3: the AWS::CloudWatch::AnomalyDetector configuration-update / Stat-replacement lifecycle never converges within the check's budget.
AWS: an anomaly detector declared in a template deploys, its Configuration (excluded time ranges / metric timezone) updates IN PLACE on a stack update, changing its Stat replaces the resource, an update to an invalid Stat fails and the stack rolls back with the prior detector unchanged, and stack deletion removes it — each stack settling into a terminal status promptly. Recorded behavior only.
Emulator: both lifecycle checks exhaust their budget ('Check timed out after 60.0s') without reaching the comparison, so the stack/detector never reaches the terminal state the next step waits for. Hypothesis: the detector resource's update/rollback path leaves the stack non-terminal (or the detector unreadable), which is adjacent to the already-open 20261109T183935Z-cloudformation-cloudwatch-2-net-new-causes-four-aws.md (that issue covers unknown AWS::CloudWatch::* types and the detector's Ref physical id, and does not carry these two checks' replay keys).
Fix: the AWS::CloudWatch::AnomalyDetector resource handler's update/replace/rollback transitions and the stack-status progression they drive.
Failing checks: cloudformation__cloudwatch.cfn-cloudwatch-anomaly-detector-configuration-update-in-place-then-stat-replacement-deletes-cleanly, cloudformation__cloudwatch.cfn-cloudwatch-anomaly-detector-update-to-invalid-stat-rolls-back-unchanged.

## Evidence
Cause 1 — Expected: 'CREATE_COMPLETE'. Actual: "AssertionError: positive-control stack ended 'ROLLBACK_COMPLETE'" and "AssertionError: stack 'sim2real-behav-cwwustk-<suffix>' ended 'ROLLBACK_COMPLETE', not CREATE_COMPLETE" (stack name is per-run; the divergence is the status). Templates: a single AWS::CloudWatch::Alarm with Namespace/MetricName/Statistic='Average'/Period/EvaluationPeriods=1/Threshold=50.0/ComparisonOperator='GreaterThanThreshold' plus EvaluationWindow={'WallClockWindow': {}} (control) or WarmUpConfiguration={'WarmUpPeriodDurationInMinutes': 5, 'OnlyStartEvaluatingAfterWarmUpPeriodEnds': True} + EvaluationWindow={'WallClockWindow': {'Timezone': 'America/New_York'}}.

Cause 2 — Expected: a CREATE_FAILED ResourceStatusReason containing a 'Request ID: ' marker (the service-error tail). Actual: 'AssertionError: reason \'Resource handler returned message: "The field DashboardBody must be a valid JSON object" (RequestToken: c9540c45-e073-0f44-283e-5b10cb7d3ab9, HandlerErrorCode: InvalidRequest)\' carries no \'Request ID: \' marker' (the token is per-run; the missing fields are the divergence).

Cause 3 — Expected: each stack reaches a terminal status (CREATE_COMPLETE, then UPDATE_COMPLETE for the in-place Configuration change, UPDATE_ROLLBACK_COMPLETE for the invalid-Stat update, DELETE_COMPLETE after deletion) and the detector reads back accordingly. Actual: 'Error: Check timed out after 60.0s' for both checks.

## Steps to Reproduce
Cause 1: CreateStack with one AWS::CloudWatch::Alarm resource whose Properties include a valid Period plus EvaluationWindow={'WallClockWindow': {}} — poll DescribeStacks and observe ROLLBACK_COMPLETE instead of CREATE_COMPLETE. Repeat with WarmUpConfiguration={'WarmUpPeriodDurationInMinutes':5,'OnlyStartEvaluatingAfterWarmUpPeriodEnds':true} and EvaluationWindow={'WallClockWindow':{'Timezone':'America/New_York'}}, then DescribeAlarms and compare WarmUpConfiguration/EvaluationWindow.
Cause 2: CreateStack with one AWS::CloudWatch::Dashboard whose DashboardBody is not a JSON object (e.g. the string 'not-a-json-object'); after the stack fails, DescribeStackEvents and read the CREATE_FAILED ResourceStatusReason for the dashboard resource.
Cause 3: CreateStack with one AWS::CloudWatch::AnomalyDetector (namespace, metric, one dimension, Stat); after CREATE_COMPLETE, UpdateStack changing only its Configuration (metric timezone / excluded time ranges) and wait for UPDATE_COMPLETE; then UpdateStack changing its Stat and wait for the replacement; then UpdateStack to an invalid Stat and wait for UPDATE_ROLLBACK_COMPLETE with the detector unchanged; finally DeleteStack.
