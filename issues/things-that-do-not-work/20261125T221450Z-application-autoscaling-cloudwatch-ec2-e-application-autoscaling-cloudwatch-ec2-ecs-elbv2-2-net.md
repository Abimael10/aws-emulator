Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-suspended-scale-in-holds-while-scale-out-still-fires
Issue-Key: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends|_poll_until#21|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-suspended-scale-in-holds-while-scale-out-still-fires|_poll_until#5|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# application-autoscaling__cloudwatch__ec2__ecs__elbv2: 2 net-new causes — an ECS service's runningCount never follows the scaled desiredCount, and a scaling activity never reaches an EndTime

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScalingActivities

## Description
Cause 1 of 2: a step-scaling policy's scaling activity is created but never finishes.
AWS: Smithy (application-autoscaling): ScalingActivity carries StartTime, EndTime, StatusCode and StatusMessage; an activity whose capacity change is fulfilled by the target service transitions from InProgress to Successful and is stamped with an EndTime. An activity that stays InProgress with the message "Waiting for change to be fulfilled by ecs." is the pre-fulfilment state, not a resting one.
Emulator: DescribeScalingActivities returns no activity carrying an EndTime for the policy's adjustment within the whole observation budget (the probe's last observation is an empty list), so the cooldown semantics under test can never be evaluated.
Why: a client that waits for an activity to end before issuing the next adjustment, or that reads EndTime to measure a cooldown, never progresses; cooldown and suspension behaviour becomes unobservable.
Fix: the application-autoscaling activity state machine that must stamp EndTime/StatusCode once the ECS capacity change it requested has been applied.
Traces: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-continuing-alarm-adds-nothing-until-the-cooldown-ends.

Cause 2 of 2: the registered ECS service's runningCount never follows the desiredCount application-autoscaling set.
AWS: Smithy (ecs): Service.runningCount is the number of tasks in the service in the RUNNING state; after RegisterScalableTarget/PutScalingPolicy moves desiredCount, ECS launches tasks and runningCount converges to the new desiredCount.
Emulator: DescribeServices keeps reporting runningCount 0 for the scaled service for the entire 180s budget while desiredCount has moved, so the scale-out is never realised.
Why: a client that reads runningCount to confirm a scaling action concluded sees the action as永 never applied, and suspended/scale-in differentials cannot be told apart.
Fix: the ECS service scheduler's task launch path (the same data-plane defect that keeps tasks from reaching RUNNING), as surfaced through the scalable target's ECS dimension.
Traces: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-suspended-scale-in-holds-while-scale-out-still-fires.

## Evidence
Cause 1 — AssertionError: the activity "Setting desired count to 2." reaching an EndTime not observed within 150.0s; last saw []. Expected a ScalingActivity with StatusCode Successful and an EndTime; got no activity with an EndTime at all.
Cause 2 — AssertionError: ECS reporting runningCount 2 for '<service>' not observed within 180.0s; last saw 0. Expected runningCount == desiredCount (2); got 0.

## Steps to Reproduce
1. Create an ECS cluster, task definition and FARGATE service; RegisterScalableTarget for ServiceNamespace ecs / ScalableDimension ecs:service:DesiredCount.
2. PutScalingPolicy (StepScaling) and PutMetricAlarm on a custom metric; PutMetricData above the threshold so the alarm enters ALARM and invokes the policy.
3. Poll DescribeScalingActivities: the activity "Setting desired count to N." never acquires an EndTime (Cause 1).
4. Poll DescribeServices for the scaled service: runningCount stays 0 while desiredCount moved (Cause 2).
