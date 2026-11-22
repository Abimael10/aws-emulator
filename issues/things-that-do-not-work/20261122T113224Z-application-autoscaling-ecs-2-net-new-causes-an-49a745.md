Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__ecs.a-metric-that-never-breaches-produces-no-scaling-activity application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it
Issue-Key: application-autoscaling__ecs.a-metric-that-never-breaches-produces-no-scaling-activity|run#39|AssertionError|assert repaired[0]['StatusCode'] == 'InProgress', (
Issue-Key: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it|_poll_until#39|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Commit: ce4ee9e56b2f664f3ccf49966acace7716de13e8

# application-autoscaling__ecs: 2 net-new causes — an unfulfilled capacity change is stamped StatusCode 'Successful' at once, and a post-cooldown breach applies no step at all

> _Note: this issue was identified against the code base at commit `ce4ee9e56b2f664f3ccf49966acace7716de13e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
2 net-new root causes, grounded in the two failing checks the suite lists under `### application-autoscaling__ecs` that are marked neither ALREADY FILED nor OWNED (the other three checks under that heading are OWNED by 20261120T132657Z-application-autoscaling-ecs-2-net-new-causes-an--r2.md and are not covered here). Related open issue that does NOT carry either replay key: 20261119T124426Z-application-autoscaling-ecs-3-net-new-causes-a.md.

Cause 1 of 2: DescribeScalingActivities stamps a scaling activity 'Successful' the moment it writes the new capacity, even when the scalable target has not fulfilled it, so the InProgress/waiting-for-fulfilment state is never observable. Failing check: application-autoscaling__ecs.a-metric-that-never-breaches-produces-no-scaling-activity.
AWS: Smithy: ScalingActivityStatusCode is an enum of [Pending, InProgress, Successful, Overridden, Unfulfilled, Failed]; ScalingActivity.StatusCode is "Indicates the status of the scaling activity" and ScalingActivity.StatusMessage is "A simple message about the current status of the scaling activity". The model does not state when each code applies, so the transition is recorded behavior only: after Application Auto Scaling sets an ECS service's desiredCount it reports the activity as StatusCode='InProgress' with Description='Setting desired count to <N>.' and StatusMessage='Successfully set desired count to <N>. Waiting for change to be fulfilled by ecs.', and only moves it to 'Successful' once ECS has actually fulfilled the new count. In the recording the cluster has no container instances, so ECS can never place the tasks and the activity can never leave InProgress. Fixed text of that StatusMessage is "Successfully set desired count to " / ". Waiting for change to be fulfilled by ecs."; the only variable part is <N>, the capacity taken from the policy's adjustment and the target's bounds.
Emulator: the newest activity is already terminal — `('Successful', 'Setting desired count to 3.')` — for a desiredCount ECS has not fulfilled (runningCount can never reach it on a cluster with no container instances). The Description matches AWS; the StatusCode does not. Two fixed issues bracket this: 20261108T003241Z-application-autoscaling-ecs-an-ecs-step-scaling-activity.md claimed this exact behaviour, and 20261111T133737Z-application-autoscaling-ecs-a-scaling-activity-that-already.md claimed the opposite (never reaching Successful); the current code has settled on stamping Successful immediately. Hypothesis: the activity is closed out in the same step that writes the capacity, with no later reconciliation against the target's fulfilled state.
Why: a client polling DescribeScalingActivities to find out whether a scaling action actually took effect is told it succeeded while nothing was placed. Stuck capacity (no hosts, no subnet addresses, a quota) is invisible, so operators and automation that gate on StatusCode never see InProgress, never see a stall, and never read the "Waiting for change to be fulfilled by ecs." explanation.

Cause 2 of 2: once a step policy's Cooldown window has closed, a fresh breach selecting a larger step applies no adjustment at all — capacity stays at the value the in-window action left. Failing check: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it.
AWS: Smithy: StepScalingPolicyConfiguration.Cooldown — "The amount of time, in seconds, to wait for a previous scaling activity to take effect. If not specified, the default value is 300." — with StepAdjustments / AdjustmentType=ChangeInCapacity. The netting rule is recorded behavior only: inside an active cooldown a more aggressive step adds only the difference between it and the step already in flight, and after the cooldown window has closed the same breach adds the WHOLE adjustment (desiredCount = current + ScalingAdjustment), with a new activity whose Description is 'Setting desired count to <N>.' and whose StartTime is at or after the window's end. Legs 1 and 2 of the recording (first step, then the in-window netted step) are reproduced correctly by the emulator; only the after-the-window leg is not.
Emulator: with the cooldown window closed and the alarm driven back to ALARM above the larger step's MetricIntervalLowerBound, desiredCount never moves — `leg 3: desiredCount moving off 7 after the window closed ... not observed within 150.0s; last saw 7`. The value 7 is this run's in-window capacity; the requirement is that the full ScalingAdjustment of the matched step is added to whatever the current capacity is. Hypothesis: the netting that is correct inside the window is still being applied after it, so the already-counted prior step cancels the new adjustment, or the cooldown is re-armed by the in-window action and never expires.
Why: step scaling never escalates. A client whose load keeps rising past a higher step boundary gets one escalation and then a permanently frozen capacity, so the whole point of multi-step policies (bigger breach, bigger response) is lost and capacity planning against the policy's steps is wrong.
Fix: the application-autoscaling scaling-activity lifecycle (StatusCode/StatusMessage must stay InProgress until the ECS target reports the capacity fulfilled, then move to Successful) and the step-scaling evaluator's cooldown gate (netting applies only while the cooldown is unexpired; after it closes the matched step's full adjustment is applied and recorded as a new activity).
Traces: application-autoscaling__ecs.a-metric-that-never-breaches-produces-no-scaling-activity (cause 1); application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it (cause 2).

## Evidence
Suite: `application-autoscaling__ecs` 13/18 (72%), 5 failed — 3 of those 5 are OWNED; the 2 below are net-new.

Cause 1 — expected (AWS): the newest DescribeScalingActivities entry for the scale-in that was just applied reports StatusCode='InProgress', Description='Setting desired count to <N>.', StatusMessage='Successfully set desired count to <N>. Waiting for change to be fulfilled by ecs.' while the ECS cluster has no container instances to place the tasks on.
Actual (mismatch verdict):
  File "check_application_autoscaling__ecs_a_metric_that_never_breaches_produces_no_scaling_activity.py", line 415, in run
    assert repaired[0]['StatusCode'] == 'InProgress'
  AssertionError: leg 3: the subject's newest activity is [('Successful', 'Setting desired count to 3.')], expected InProgress -- this estate has no container instances, so ECS can never finish fulfilling the new desiredCount and the activity cannot reach a different terminal state

Cause 2 — expected (AWS): after the policy's 120 s-class cooldown window has closed, driving the alarm to ALARM above the larger step's lower bound adds that step's full ScalingAdjustment to the current desiredCount, and the newest activity's StartTime is at or after the window's end.
Actual (mismatch verdict):
  File "check_application_autoscaling__ecs_step_scaling_cooldown_nets_the_prior_step_then_adds_the_whole_step_after_it.py", line 394, in run
    third_count = _poll_until(...)
  AssertionError: leg 3: desiredCount moving off 7 after the window closed at 2026-11-22T10:44:53.419415+00:00 not observed within 150.0s; last saw 7

The capacities (3, 7), the window timestamp and the poll ceilings are per-run values, not the requirement.

## Steps to Reproduce
Cause 1:
1. Create an ECS cluster with NO container instances, a task definition and a service with desiredCount=3.
2. RegisterScalableTarget(ecs / service/<cluster>/<service> / ecs:service:DesiredCount, MinCapacity=1, MaxCapacity=…).
3. PutScalingPolicy StepScaling with a negative ChangeInCapacity step; PutMetricAlarm (LessThanThreshold) wired to the policy ARN and drive it to ALARM.
4. When DescribeServices shows desiredCount decreased, call DescribeScalingActivities: AWS reports StatusCode='InProgress' with StatusMessage 'Successfully set desired count to <N>. Waiting for change to be fulfilled by ecs.'; the emulator reports StatusCode='Successful'.

Cause 2:
1. ECS service with desiredCount=1; RegisterScalableTarget with MinCapacity=1, MaxCapacity=20.
2. PutScalingPolicy StepScaling, ChangeInCapacity, StepAdjustments=[{LowerBound:0, UpperBound:<B>, ScalingAdjustment:<small>}, {LowerBound:<B>, ScalingAdjustment:<big>}], Cooldown=<C>.
3. Drive the alarm to ALARM with a small breach: desiredCount += <small>; note the activity's EndTime (cooldown start).
4. Return to OK, then drive a breach above <B> while still inside the cooldown: desiredCount nets to 1 + <big> (the emulator does this correctly).
5. Return to OK, wait past EndTime + <C>, then drive the same above-<B> breach again: AWS adds the whole <big> to the current count; the emulator leaves desiredCount unchanged.
6. Re-replay both named checks to confirm.
