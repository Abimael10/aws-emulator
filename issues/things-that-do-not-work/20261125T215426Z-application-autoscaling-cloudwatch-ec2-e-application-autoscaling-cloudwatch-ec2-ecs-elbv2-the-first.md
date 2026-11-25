Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends
Issue-Key: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends|_poll_until#21|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# application-autoscaling__cloudwatch__ec2__ecs__elbv2: the first step-scaling action is never recorded — DescribeScalingActivities returns an empty list, so no activity 'Setting desired count to <n>.' ever gets an EndTime

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScalingActivities

## Description
Cause 1 of 1: when a CloudWatch alarm invokes an Application Auto Scaling StepScaling policy on an ECS service scalable target, the resulting scaling action is never recorded as a scaling activity. DescribeScalingActivities(IncludeNotScaledActivities=true) for the registered target returns an empty list, so the activity whose Description is 'Setting desired count to <new capacity>.' never appears and never reaches an EndTime, and the whole cooldown differential the check measures from that EndTime cannot even start.
Failing check: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends

AWS: Smithy (services/service-com-amazonaws-applicationautoscaling-anyscalefrontendservice/smithy-model/com-amazonaws-applicationautoscaling-anyscalefrontendservice.json) — ScalingActivity requires ActivityId, ServiceNamespace, ResourceId, ScalableDimension, Description ("A simple description of what action the scaling activity intends to accomplish"), Cause ("A simple description of what caused the scaling activity to happen"), StartTime ("The Unix timestamp for when the scaling activity began") and StatusCode, and carries the optional EndTime ("The Unix timestamp for when the scaling activity ended"), StatusMessage, Details and NotScaledReasons; ScalingActivityStatusCode enumerates the terminal states. DescribeScalingActivities returns these for a ServiceNamespace/ResourceId/ScalableDimension. The model does not state the invocation rule, so that part is recorded behavior only: every invocation of a scaling policy that changes capacity produces one activity for the target, whose Description for an ECS service is 'Setting desired count to <new desired count>.', whose Cause names the invoking alarm and the policy, whose StartTime is when the action began and which then settles with a terminal StatusCode and an EndTime once the capacity change has been applied. The fixed text here is the sentence 'Setting desired count to ' plus the trailing period; the number is the new desired count computed from the request's StepAdjustments, MinCapacity and MaxCapacity, and the alarm/policy names in Cause come from the request and state.

Emulator: no activity is produced at all. The check publishes a breaching datapoint so the alarm enters and stays in ALARM, and then polls DescribeScalingActivities(IncludeNotScaledActivities=true) for an activity 'Setting desired count to 2.' with an EndTime for 150 s; the filtered result is [] for the whole window. Replay: AssertionError 'the activity "Setting desired count to 2." reaching an EndTime not observed within 150.0s; last saw []'. Hypothesis: the capacity change (if applied at all) is written straight to the ECS service without appending a ScalingActivity record to the scalable target's activity history.

Why: a client has no audit trail for autoscaling. DescribeScalingActivities is the documented way to answer 'did my policy fire, when, why, and did it succeed' — runbooks, dashboards and CI gates poll it, and an empty history reads as 'autoscaling never acted', indistinguishable from a misconfigured policy. Any logic that sequences off a previous activity's EndTime (cooldown accounting in particular) can never proceed.

Fix: the Application Auto Scaling scaling-activity recorder — the component that must append a ScalingActivity to the scalable target's history on every policy invocation and settle it with a terminal StatusCode, EndTime and the 'Setting desired count to <n>.' Description plus a Cause naming the alarm and policy; DescribeScalingActivities must then serve that history (including not-scaled activities when IncludeNotScaledActivities is true).

Traces: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends. This is a DIFFERENT failure point from the open issue 20261122T112017Z-application-autoscaling-cloudwatch-ec2-e-application-autoscaling-a-step-scaling-policy-whose-alarm--r2.md, which carries this check id but is keyed on the later assertion (the first step landed, desiredCount moved 1 -> 2 and its activity had an EndTime; only the SECOND step after the cooldown was missing). The replay now fails earlier, at line 357, with no activity recorded at all, and marks the check neither OWNED nor ALREADY FILED. Also related but not carrying this key: 20261123T142820Z-application-autoscaling-ecs-a-scaling-activity-that-really.md (an activity that did move desiredCount never reaching StatusCode 'Successful').

## Evidence
Replay of behavioral suite simparity-behavioral-aws-v1 against the booted emulator (519/647 passed, bootstrap_signal false). Service heading `### application-autoscaling__cloudwatch__ec2__ecs__elbv2`: 1/2 (50%), 1 failed, with no OWNED / ALREADY FILED marker.

[mismatch] application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends
  File "check_application_autoscaling__cloudwatch__ec2__ecs__elbv2_step_scaling_continuing_alarm_adds_nothing_until_the_cooldown_ends.py", line 357, in run
    first = _poll_until(first_ended, bool, timeout_s=_APPLY_S, interval_s=4.0,
  File "...", line 46, in _poll_until
    raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
  AssertionError: the activity "Setting desired count to 2." reaching an EndTime not observed within 150.0s; last saw []

Expected (AWS): with a scalable target registered for ServiceNamespace 'ecs', ResourceId 'service/<cluster>/<service>', ScalableDimension 'ecs:service:DesiredCount' (MinCapacity 1, MaxCapacity 3) and a StepScaling policy (ChangeInCapacity +1, Cooldown 120) invoked by an alarm in ALARM, DescribeScalingActivities(IncludeNotScaledActivities=true) gains an activity with Description 'Setting desired count to 2.', a Cause naming the invoking alarm and the policy, a StartTime, and then a terminal StatusCode with an EndTime once the capacity change is applied.
Actual (emulator): the activity list filtered for that description is empty for the entire 150 s window — no activity is recorded for the first scaling action at all. The capacity numbers (1 -> 2), the 120 s cooldown and the 150 s window are this run's configuration; the defect is that no ScalingActivity record is ever created or settled.

Smithy consulted: ScalingActivity structure — required ActivityId/ServiceNamespace/ResourceId/ScalableDimension/Description/Cause/StartTime/StatusCode, optional EndTime ("The Unix timestamp for when the scaling activity ended"), StatusMessage, Details, NotScaledReasons; the model declares no alarm-invocation or activity-creation rule, so the recording requirement is recorded behavior.

## Steps to Reproduce
1. git_clone the repo and replay the behavioral suite (simparity-behavioral-aws-v1); the check in trace_ids reproduces under the `### application-autoscaling__cloudwatch__ec2__ecs__elbv2` heading.
2. Create a Fargate ECS cluster, task definition and service with desiredCount=1 and wait until it is running.
3. RegisterScalableTarget(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', MinCapacity=1, MaxCapacity=3).
4. PutScalingPolicy(PolicyType='StepScaling', StepScalingPolicyConfiguration={AdjustmentType:'ChangeInCapacity', MetricAggregationType:'Maximum', Cooldown:120, StepAdjustments:[{MetricIntervalLowerBound:0.0, ScalingAdjustment:1}]}).
5. PutMetricAlarm on a custom metric (Period=10, EvaluationPeriods=1, DatapointsToAlarm=1, Threshold=50.0, ComparisonOperator='GreaterThanThreshold', TreatMissingData='notBreaching') with AlarmActions=[the policy ARN], and publish 75.0 continuously so the alarm reaches and stays in ALARM.
6. Poll DescribeScalingActivities(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', IncludeNotScaledActivities=true).
   Expected: an activity with Description 'Setting desired count to 2.', a Cause naming the alarm and the policy, a StartTime, and then a terminal StatusCode with an EndTime.
   Observed: the activity list contains no such activity for the whole 150 s window (filtered result []).
7. Re-replay the check id in trace_ids to confirm the fix.
