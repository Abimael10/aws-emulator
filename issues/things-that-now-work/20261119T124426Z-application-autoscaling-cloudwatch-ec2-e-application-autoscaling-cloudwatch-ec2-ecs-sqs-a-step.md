Issue-Severity: HIGH
Issue-Commit: d0b314452d42d902975a5c2c289d754b82616615

# application-autoscaling__cloudwatch__ec2__ecs__sqs: a step policy fires again from an alarm that only STAYS in ALARM and inside its own 3600 s Cooldown, pushing desiredCount past the maximum the triggered policies allow

> _Note: this issue was identified against the code base at commit `d0b314452d42d902975a5c2c289d754b82616615`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
Cause 1 of 1: a StepScaling policy whose alarm is already in ALARM and whose Cooldown has not expired is invoked a second time, so desiredCount ends above the largest value the triggered policies can produce. Failing check: application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales.

AWS: Smithy — PutScalingPolicy.StepScalingPolicyConfiguration carries Cooldown ('The amount of time, in seconds, to wait for a previous scaling activity to take effect', default 300) and StepAdjustments with AdjustmentType=ChangeInCapacity; the model states no invocation rule, so the rest is recorded behavior only. A CloudWatch alarm invokes its AlarmActions on a state TRANSITION, so an alarm that merely remains in ALARM while its metric keeps breaching does not re-invoke its policy; and a step policy re-invoked inside its Cooldown adds at most the remainder after the in-flight step is netted out, which for a repeat of the identical +1 step is zero. In the recording: desiredCount starts at 1, the fed control alarm (policy +1, Cooldown=3600) transitions and takes it to 2, and later the repaired subject alarm (policy +2, Cooldown=0) transitions and takes it to at most 4 — 3 if the control policy's in-flight step is netted into the +2. The control policy's own 3600 s cooldown is still running throughout, so no value above 4 is reachable.

Emulator: desiredCount became 5. That is 2 + 2 (the subject policy's legitimate step) + an extra 1 — the control policy's +1 applied a second time, although its alarm never left ALARM after its first transition and its Cooldown=3600 was still in force. Hypothesis: the engine re-evaluates and re-applies every policy whose alarm is currently in ALARM each evaluation window, and does not gate that re-application on the policy's Cooldown.

Why: a client that sets a long Cooldown precisely to bound how fast an ECS service grows gets extra, unattributable capacity on every evaluation while the metric stays above the threshold. Capacity exceeds what the triggered policies permit, the scaling-activity ledger records actions real AWS never takes, and any assertion of the form 'after these alarms fire, desiredCount is at most N' fails against the emulator.

Fix: the alarm-to-policy invocation path and the Cooldown gate of the application-autoscaling step-scaling evaluator — a policy must be driven by its alarm's state transition, and a re-invocation inside an active Cooldown must apply only the netted remainder (zero for an identical repeated step), never the whole adjustment again.

Traces: application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales. Related but distinct open issue, which does not carry this check's replay key: 20261111T010150Z-application-autoscaling-ecs-step-scaling-action-inside-the-cooldown-never-nets.md (capacity frozen inside a cooldown rather than over-applied).

## Evidence
Expected (AWS): ECS DescribeServices desiredCount <= 4 after the repaired +2 policy acts — start 1, +1 from the control policy (Cooldown=3600, one transition only), +2 from the subject policy (Cooldown=0), i.e. 3 or 4 and never more.
Actual (emulator): desiredCount became 5. AssertionError: 'desiredCount became 5; the +2 subject policy from 2 gives at most 4'. The extra +1 is the control policy's step applied a second time with no new alarm transition and with its 3600 s Cooldown still running.

## Steps to Reproduce
1. Create a Fargate ECS cluster, task definition and service with desiredCount=1; RegisterScalableTarget(ServiceNamespace=ecs, ResourceId=service/<cluster>/<service>, ScalableDimension=ecs:service:DesiredCount, MinCapacity=1, MaxCapacity=6).
2. PutScalingPolicy 'control': StepScaling, AdjustmentType=ChangeInCapacity, StepAdjustments=[{MetricIntervalLowerBound: 0.0, ScalingAdjustment: 1}], Cooldown=3600.
3. PutScalingPolicy 'subject': StepScaling, same AdjustmentType, StepAdjustments=[{MetricIntervalLowerBound: 0.0, ScalingAdjustment: 2}], Cooldown=0.
4. PutMetricAlarm one alarm per policy (Period=10, EvaluationPeriods=1, DatapointsToAlarm=1, Threshold=50.0, ComparisonOperator=GreaterThanThreshold, TreatMissingData=missing), AlarmActions=[the matching policy ARN]; point the control alarm at a fed metric dimension and the subject alarm at a dimension that is never published.
5. Publish 75.0 on the fed dimension until the control alarm reaches ALARM; desiredCount goes to 2.
6. Keep publishing 75.0 on the fed dimension (the control alarm stays in ALARM, never transitions again) for ~60 s.
7. PutMetricAlarm the subject alarm again, repointed at the fed dimension, and drive it to ALARM.
8. Read DescribeServices desiredCount: AWS reports 3 or 4; the emulator reports 5.
