Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__ecs.percent-change-sub-unit-rounding-and-magnitude-floor application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it application-autoscaling__ecs.step-scaling-exact-bound-picks-the-step-farther-from-the-threshold-on-both-sides application-autoscaling__ecs.step-scaling-percent-change-with-min-adjustment-magnitude application-autoscaling__ecs.step-scaling-scale-in-cooldown-suppresses-then-permits
Issue-Key: application-autoscaling__ecs.percent-change-sub-unit-rounding-and-magnitude-floor|_poll_until#30|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it|_poll_until#5|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__ecs.step-scaling-exact-bound-picks-the-step-farther-from-the-threshold-on-both-sides|_poll_until#5|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__ecs.step-scaling-percent-change-with-min-adjustment-magnitude|_poll_until#9|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__ecs.step-scaling-scale-in-cooldown-suppresses-then-permits|_poll_until#8|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# application-autoscaling__ecs: 2 net-new causes — scaling activities are stranded in InProgress ("Waiting for change to be fulfilled by ecs"), and the scaled ECS service's runningCount never leaves 0 while failedTasks climbs

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScalingActivities

## Description
Related but NOT covering open issues: 20261123T142820Z-application-autoscaling-ecs-a-scaling-activity-that-really.md and 20261120T132657Z-application-autoscaling-ecs-2-net-new-causes-an--r2.md; the suite did not mark the checks below as owned by either, so they are filed here with their own replay keys.

Cause 1 of 2: a scaling activity never leaves InProgress.
AWS: Smithy (application-autoscaling): ScalingActivity.StatusCode moves from Pending/InProgress to Successful once the target service applies the capacity change, and EndTime is stamped at that point. The StatusMessage "Successfully set desired count to N. Waiting for change to be fulfilled by ecs." is the message of the IN-FLIGHT state; its fixed text is that sentence pair and N is the requested capacity from the policy adjustment.
Emulator: the activity stays StatusCode 'InProgress' with that message for the whole budget, with no EndTime and no Successful activity ever recorded, so a client cannot tell a stuck activity from a slow one.
Why: a client that gates the next adjustment (or a cooldown measurement) on activity completion never proceeds; a client that counts Successful activities to verify proportional control counts zero.
Fix: the application-autoscaling activity state machine — it must settle the activity once the ECS dimension reports the new capacity.
Traces: application-autoscaling__ecs.percent-change-sub-unit-rounding-and-magnitude-floor.

Cause 2 of 2: the ECS service behind the scalable target never runs any task, so no capacity change is ever realised.
AWS: Smithy (ecs): Service.runningCount converges to desiredCount and Deployment.rolloutState reaches COMPLETED for a healthy revision; Deployment.failedTasks is documented to stop being evaluated once one task runs successfully.
Emulator: DescribeServices reports runningCount 0 (and in one case desiredCount 12 with failedTasks 504, rolloutState stuck IN_PROGRESS) for the whole budget, both before any policy exists and after the policy fires — so step-scaling, percent-change and cooldown semantics are all unobservable.
Why: every application-autoscaling behaviour that reads back the realised capacity of the ECS dimension is unverifiable, and clients waiting for the scaled service to be healthy hang.
Fix: the ECS service scheduler's task launch path and its Deployment projection; application-autoscaling's ECS dimension reads it.
Traces: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it, application-autoscaling__ecs.step-scaling-exact-bound-picks-the-step-farther-from-the-threshold-on-both-sides, application-autoscaling__ecs.step-scaling-percent-change-with-min-adjustment-magnitude, application-autoscaling__ecs.step-scaling-scale-in-cooldown-suppresses-then-permits.

## Evidence
Cause 1 — AssertionError: leg 1: a scaling activity setting desired count to 9 leaving InProgress not observed within 60.0s; last saw {'StatusCode': 'InProgress', 'StatusMessage': 'Successfully set desired count to 9. Waiting for change to be fulfilled by ecs.', 'Description': 'Setting desired count to 9.', no 'EndTime'}. Expected StatusCode 'Successful' with an EndTime.
Cause 2 — AssertionError: ECS reporting runningCount 4 for '<service>' not observed within 180.0s; last saw 0; AssertionError: ECS reporting desiredCount and runningCount 2 ... last saw (2, 0); AssertionError: ECS reporting runningCount 6 ... last saw 0; and the PRIMARY deployment probe last saw {'desiredCount': 12, 'runningCount': 0, 'failedTasks': 504, 'rolloutState': 'IN_PROGRESS'}.

## Steps to Reproduce
1. CreateCluster + RegisterTaskDefinition + CreateService (FARGATE) with a desiredCount; RegisterScalableTarget on ecs:service:DesiredCount.
2. Poll DescribeServices before any policy exists: runningCount stays 0 while desiredCount is set, failedTasks climbs (Cause 2).
3. PutScalingPolicy (StepScaling, PercentChangeInCapacity with MinAdjustmentMagnitude) + PutMetricAlarm; breach the metric.
4. Poll DescribeScalingActivities: the activity stays StatusCode InProgress with "Waiting for change to be fulfilled by ecs." and no EndTime (Cause 1).
