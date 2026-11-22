Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends
Issue-Key: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends|_poll_until#25|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Commit: ce4ee9e56b2f664f3ccf49966acace7716de13e8

# application-autoscaling: a step-scaling policy whose alarm STAYS in ALARM never applies the next step once its Cooldown has elapsed — ECS desiredCount freezes at the first step (recurrence after a claimed fix)

> _Note: this issue was identified against the code base at commit `ce4ee9e56b2f664f3ccf49966acace7716de13e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
RECURRENCE of the fixed issue 20261110T220525Z-application-autoscaling-cloudwatch-ec2-e-application-autoscaling-a-step-scaling-policy-whose-alarm.md: that issue carried exactly this check id and was closed as fixed, but the check fails again with the same assertion, so the fix did not hold. The replay did not mark the check OWNED by any open issue.

Cause 1 of 1: an Application Auto Scaling StepScaling policy that is driven by a CloudWatch alarm which REMAINS in ALARM adds nothing after its Cooldown has elapsed — the scalable target's capacity stops at the first step and never advances toward MaxCapacity.
Failing check: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends

AWS: Smithy (services/service-com-amazonaws-applicationautoscaling-anyscalefrontendservice/smithy-model/com-amazonaws-applicationautoscaling-anyscalefrontendservice.json) — PutScalingPolicy with PolicyType 'StepScaling' and StepScalingPolicyConfiguration {AdjustmentType, MetricAggregationType, StepAdjustments[].{MetricIntervalLowerBound, ScalingAdjustment}, Cooldown}; Cooldown is documented as "The amount of time, in seconds, to wait for a previous scaling activity to take effect. If not specified, the default value is 300" (doc links the Cooldown period section of the step-scaling-policy-overview page the check cites). The model declares no invocation rule, so the invocation semantics are recorded behavior only: a CloudWatch alarm that stays in ALARM re-invokes its AlarmActions periodically (about once a minute); invocations that land inside the policy's Cooldown window add nothing, and the FIRST invocation after the cooldown has elapsed from the previous activity's EndTime applies the next ScalingAdjustment, so the scalable target's capacity advances by one more step, bounded by the registered MaxCapacity. The second action is also recorded: DescribeScalingActivities(IncludeNotScaledActivities=true) gains an activity whose Description is 'Setting desired count to <new capacity>.', whose StartTime is at or after first activity EndTime + Cooldown, and whose Cause names the invoking alarm and the policy. The suppress-then-resume rule is the requirement; the cooldown seconds, step size and min/max capacity are values taken from the request (here Cooldown=120, ScalingAdjustment=1, MinCapacity=1, MaxCapacity=3, start 1), and the capacity numbers in the replay are not part of the requirement.

Emulator: the first step lands (ECS DescribeServices desiredCount moves 1 -> 2) and then nothing ever happens again. With the check publishing a breaching datapoint on every probe so the alarm is continuously in ALARM (the check also verifies the alarm never left ALARM), desiredCount stays at the first-step value for the entire Cooldown + 150 s window and no second scaling activity is recorded. Replay: AssertionError "the continuing alarm adding the next step after the 120s cooldown (alarm ALARM) not observed within 270.0s; last saw 2" (check_...step_scaling_continuing_alarm_adds_nothing_until_the_cooldown_ends.py line 368 in run -> _poll_until line 46). Hypothesis: the policy is applied only on an alarm state TRANSITION into ALARM, so a policy whose alarm merely remains in ALARM is never re-evaluated once the cooldown gate opens.

Why: a client that relies on step scaling to keep adding capacity while a breach persists gets one step and then a flat line — capacity never reaches the level the policy's steps and MaxCapacity describe, so autoscaling under sustained load is silently a no-op after the first action. Cooldown tests also pass for the wrong reason (nothing happens, so nothing happens too early), and runbooks that read DescribeScalingActivities see no record of the actions AWS would have taken.

Fix: the application-autoscaling step-scaling component — the alarm-to-policy invocation path and the Cooldown gate in the scaling-activity scheduler. A policy must remain subscribed to its alarm's ALARM state and re-evaluate/apply its step adjustment on the first invocation after Cooldown elapses (clamped to MaxCapacity), recording the resulting activity with its 'Setting desired count to <n>.' description and a Cause naming the alarm and the policy, rather than acting only once per alarm transition.

Related open issues that do NOT carry this check's replay key and describe the opposite direction (over-application rather than no application): 20261119T124426Z-application-autoscaling-cloudwatch-ec2-e-application-autoscaling-cloudwatch-ec2-ecs-sqs-a-step.md (a policy re-applied inside an active Cooldown) and 20261119T173202Z-application-autoscaling-cloudwatch-ec2-e-application-autoscaling-a-step-scaling-policy-over-applies.md.

Traces: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends.

## Evidence
Replay of behavioral suite simparity-behavioral-aws-v1 against the booted emulator (497/647 passed, bootstrap_signal false). Service heading `### application-autoscaling__cloudwatch__ec2__ecs__elbv2`: 1/2 (50%), 1 failed, with no OWNED / ALREADY FILED marker.

[mismatch] application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends
  File "check_application_autoscaling__cloudwatch__ec2__ecs__elbv2_step_scaling_continuing_alarm_adds_nothing_until_the_cooldown_ends.py", line 368, in run
    final = _poll_until(probe, lambda d: d > first_desired, timeout_s=_SECOND_ACTION_S, interval_s=4.0, ...)
  File "...", line 46, in _poll_until
    raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
  AssertionError: the continuing alarm adding the next step after the 120s cooldown (alarm ALARM) not observed within 270.0s; last saw 2

Expected (AWS): the first step takes ECS desiredCount 1 -> 2; while the 120 s Cooldown runs the still-breaching alarm adds nothing; after the cooldown has elapsed from the first activity's EndTime the same alarm (still in ALARM, never transitioning out — the check asserts it never leaves ALARM) drives the policy again and desiredCount advances to 3 (MinCapacity 1 / MaxCapacity 3, ScalingAdjustment +1), with a second scaling activity Description 'Setting desired count to 3.', StartTime >= first EndTime + 120 s, and a Cause naming the alarm and the policy.
Actual (emulator): desiredCount remains 2 for the whole 270 s observation window (Cooldown 120 s + 150 s for the alarm's re-invocation and the activity) and DescribeScalingActivities records no second activity. The numbers 2/3/120/270 are this run's configuration; the defect is that capacity never changes again after the first step.

Smithy consulted: PutScalingPolicy.StepScalingPolicyConfiguration.Cooldown documentation — "The amount of time, in seconds, to wait for a previous scaling activity to take effect. If not specified, the default value is 300."; StepScalingPolicyConfiguration {AdjustmentType, StepAdjustments, MetricAggregationType, Cooldown}; the model declares no alarm-invocation rule.

## Steps to Reproduce
1. git_clone the repo and replay the behavioral suite (simparity-behavioral-aws-v1); the check in trace_ids reproduces under the `### application-autoscaling__cloudwatch__ec2__ecs__elbv2` heading.
2. Create a Fargate ECS cluster, task definition and service with desiredCount=1 and wait until runningCount==1.
3. RegisterScalableTarget(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', MinCapacity=1, MaxCapacity=3).
4. PutScalingPolicy(PolicyType='StepScaling', StepScalingPolicyConfiguration={AdjustmentType:'ChangeInCapacity', MetricAggregationType:'Maximum', Cooldown:120, StepAdjustments:[{MetricIntervalLowerBound:0.0, ScalingAdjustment:1}]}) and confirm DescribeScalingPolicies echoes Cooldown=120.
5. PutMetricAlarm on a high-resolution custom metric (Period=10, EvaluationPeriods=1, DatapointsToAlarm=1, Threshold=50.0, ComparisonOperator='GreaterThanThreshold', TreatMissingData='notBreaching') with AlarmActions=[the policy ARN].
6. Publish 75.0 on that metric continuously (every few seconds) so the alarm reaches ALARM and never leaves it.
7. Observe ECS DescribeServices desiredCount move 1 -> 2, and poll DescribeScalingActivities(IncludeNotScaledActivities=true) until the activity 'Setting desired count to 2.' has an EndTime.
8. Keep publishing 75.0 and keep reading desiredCount for Cooldown + 150 s.
   Expected: desiredCount advances to 3 at or after first-activity EndTime + 120 s, with a second activity 'Setting desired count to 3.' whose Cause names the alarm and the policy.
   Observed: desiredCount stays at 2 for the whole window and no second activity is recorded.
9. Re-replay the check id in trace_ids to confirm the fix.
