Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow
Issue-Key: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period|_assert_parent_is_blind_to_child_drift#35|AssertionError|assert alarm and alarm["StackResourceDriftStatus"] == "IN_SYNC", (
Issue-Key: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow|_assert_parent_is_blind_to_child_drift#35|AssertionError|assert alarm and alarm["StackResourceDriftStatus"] == "IN_SYNC", (
Issue-Commit: 78eeb93e75738c0cf0c5c47abcc6a48fc1935112

# cloudformation: drift detection reports an untouched AWS::CloudWatch::Alarm as MODIFIED because the actual double Threshold is re-serialized as 0.0 and compared textually to the template's 0

> _Note: this issue was identified against the code base at commit `78eeb93e75738c0cf0c5c47abcc6a48fc1935112`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackResourceDrifts

## Description
1 net-new cause, seen on both nested-stack platform checks of this service group.

Cause 1 of 1: a resource nobody touched is reported as drifted because the drift comparison is textual over JSON-rendered numbers.
AWS: Smithy: cloudformation StackResourceDrift.StackResourceDriftStatus (enum IN_SYNC|MODIFIED|DELETED|NOT_CHECKED|UNKNOWN|UNSUPPORTED) — 'IN_SYNC: The resource's actual configuration matches its expected template configuration'; 'MODIFIED: One or more resource properties differ from their expected values (as defined in the stack template and any values specified as template parameters)'. StackResourceDrift.PropertyDifferences is documented as present 'only for resources whose StackResourceDriftStatus is MODIFIED', and PropertyDifference.DifferenceType NOT_EQUAL means 'The current property value differs from its expected value'. The property in question is AWS::CloudWatch::Alarm Threshold, whose live value is a double (Smithy: cloudwatch#Threshold = double), so a template literal of 0 and a live value of 0 are the SAME value. On AWS, detecting drift on a freshly deployed alarm whose properties were never changed out of band yields StackResourceDriftStatus=IN_SYNC with no PropertyDifferences; a numeric property is compared by value, not by the decimal spelling the serializer happens to choose.
Emulator: DetectStackDrift / DescribeStackResourceDrifts on the nested ObservabilityStack returns, for the untouched alarm (LogicalResourceId StartedAlarm, ResourceType AWS::CloudWatch::Alarm), StackResourceDriftStatus='MODIFIED' with PropertyDifferences=[{PropertyPath '/Threshold', ExpectedValue '0', ActualValue '0.0', DifferenceType 'NOT_EQUAL'}]. ExpectedProperties carries "Threshold":0 (the template's integer literal, echoed verbatim) while ActualProperties carries "Threshold":0.0 (the stored double re-rendered with a fractional part); the two strings differ, so the comparison declares a difference. The fixed parts of this divergence are the status value MODIFIED instead of IN_SYNC, the presence of a PropertyDifferences entry at all, and DifferenceType NOT_EQUAL; the property path, the logical id, the alarm name and the number itself are values taken from the template and from state and are not part of the requirement — any numeric property whose template literal and stored double render differently (0 vs 0.0, 1 vs 1.0, 60 vs 60.0) will trip it.
Why: drift detection is the API clients use to decide whether a stack still matches its template. A client that deploys a stack and immediately asks 'is anything drifted?' is told yes and handed a bogus /Threshold difference, so remediation tooling, deployment gates and compliance scans either fail the deployment or try to 'correct' a property that was never changed; worse, a real out-of-band change becomes indistinguishable from this permanent false positive, and the parent stack's blindness-to-child-drift contract cannot be asserted at all.
Fix: the CloudFormation drift-detection comparison (the component that builds StackResourceDrift.ExpectedProperties/ActualProperties and diffs them for DescribeStackResourceDrifts) must compare property values semantically by JSON type — numbers by numeric value, not by their rendered text — and/or normalize the actual value's rendering to the template's for numeric properties, so an unchanged resource reports IN_SYNC with no PropertyDifferences. Related open issues cover different drift faults and do NOT cover this one: 20261113T153150Z-cloudformation-elbv2-a-freshly-deployed-elbv2-stack-detects.md (every resource counted as drifted in an ELBv2 stack), 20261029T171940Z-cloudformation-drift-reporting-returns-live-resource-attributes-and.md (drift status never recorded) and 20261121T153730Z-cloudformation-cloudwatch-lambda-logs-sq-cloudformation-describestackresourcedrifts-reports-a-status-for-aws-lambda.md (status reported for an unsupported type).
Traces: cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-alarm-echoes-declared-period, cloudformation__cloudwatch__ecr__ecs__elbv2__logs.cfn-nested-stack-microservice-platform-workflow.

## Evidence
Both checks fail in _assert_parent_is_blind_to_child_drift on `assert alarm and alarm["StackResourceDriftStatus"] == "IN_SYNC"`.

Expected (AWS): the untouched AWS::CloudWatch::Alarm in the nested ObservabilityStack drifts as StackResourceDriftStatus='IN_SYNC', with no PropertyDifferences entries.

Actual (emulator), from the replay of cfn-nested-stack-microservice-platform-alarm-echoes-declared-period:
  StackResourceDriftStatus = 'MODIFIED'
  PropertyDifferences = [{'PropertyPath': '/Threshold', 'ExpectedValue': '0', 'ActualValue': '0.0', 'DifferenceType': 'NOT_EQUAL'}]
  ExpectedProperties = '{...,"Statistic":"Sum","Period":60,"EvaluationPeriods":1,"Threshold":0,...}'
  ActualProperties   = '{...,"Statistic":"Sum","Period":60,"Threshold":0.0,...}'
The cfn-nested-stack-microservice-platform-workflow check reports the identical status, identical single PropertyDifferences entry and the identical 0 vs 0.0 spelling on its own run (different generated alarm name and stack id), confirming the cause is the numeric rendering and not any one recorded value. No status code or error code is involved: the drift calls succeed, the reported drift verdict is wrong.

## Steps to Reproduce
1. Deploy the nested-stack platform template set against the emulator (parent platform-parent.yaml with the network/service/observability children), where the observability child declares an AWS::CloudWatch::Alarm with Threshold: 0 (any integer-literal numeric property will do).
2. Change nothing out of band.
3. Call DetectStackDrift on the nested ObservabilityStack and wait for the detection to complete.
4. Call DescribeStackResourceDrifts for that stack and read the entry whose LogicalResourceId is the alarm.
5. Observe StackResourceDriftStatus='MODIFIED' with PropertyDifferences=[{PropertyPath:'/Threshold', ExpectedValue:'0', ActualValue:'0.0', DifferenceType:'NOT_EQUAL'}]; AWS reports IN_SYNC with no PropertyDifferences.
6. Re-replay checks cfn-nested-stack-microservice-platform-alarm-echoes-declared-period and cfn-nested-stack-microservice-platform-workflow to confirm the fix.
