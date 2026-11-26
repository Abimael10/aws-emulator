Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it application-autoscaling__ecs.step-scaling-percent-change-with-min-adjustment-magnitude
Issue-Key: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it|_poll_until#21|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__ecs.step-scaling-percent-change-with-min-adjustment-magnitude|_poll_until#8|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Commit: 9f7bf23473ff024f6e39d27cef35bcce93712a4b

# application-autoscaling__ecs: 2 net-new causes — a step-scaling activity that moved ECS desiredCount never gets an EndTime (no cooldown window is computable), and a Fargate service in a caller-created VPC never reports runningCount > 0

> _Note: this issue was identified against the code base at commit `9f7bf23473ff024f6e39d27cef35bcce93712a4b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScalingActivities

## Description
Two distinct net-new root causes, both found under the `### application-autoscaling__ecs` heading of the behavioral replay, neither carrying an OWNED / ALREADY FILED marker. A fixer must close BOTH before submitting.

=== Cause 1 of 2: a scaling activity that really moved ECS desiredCount never reaches an EndTime, so the step-scaling cooldown window cannot be derived. Failing check: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it ===

AWS: Smithy (services/service-com-amazonaws-applicationautoscaling-anyscalefrontendservice/smithy-model) — ScalingActivity.StatusCode is REQUIRED and targets enum ScalingActivityStatusCode with values [Pending, InProgress, Successful, Overridden, Unfulfilled, Failed]; ScalingActivity.StartTime is REQUIRED ("The Unix timestamp for when the scaling activity began"); ScalingActivity.EndTime is the optional "Unix timestamp for when the scaling activity ended"; ScalingActivity.Description is REQUIRED ("A simple description of what action the scaling activity intends to accomplish"). The model states the enum but not the transition rule, so the lifecycle itself is recorded behavior only: when a StepScaling policy on an ecs:service:DesiredCount target acts, Application Auto Scaling records one activity, and once the requested capacity has been applied that activity SETTLES — a terminal StatusCode ('Successful' when the capacity landed, 'Unfulfilled'/'Failed' when it did not) together with an EndTime; Pending/InProgress are transient. For this dimension the Description AWS writes is the fixed text "Setting desired count to <N>.", where <N> is the capacity the policy computed from the request and the target's current capacity (here start 4 plus the +2 step, i.e. 6) — the number is a value from the request and state, the sentence around it is the requirement. The cooldown window a client must honour is derived from that EndTime plus the policy's Cooldown, so with no EndTime the window does not exist at all. The 180 s Cooldown, the +2/+3 steps and the generated cluster/service names in this check are request values, not part of the requirement.

Emulator: the policy DOES act — the check's earlier assertions pass (ECS DescribeServices reports desiredCount moving off 4 to exactly 6, i.e. the step arithmetic is right) — but DescribeScalingActivities(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', IncludeNotScaledActivities=true), filtered for activities that have an EndTime, stays EMPTY for the whole 150 s observation window. Replay: AssertionError: "leg 1: the initiating activity reaching an EndTime, where the cooldown starts not observed within 150.0s; last saw []" (check_...step_scaling_cooldown_nets_the_prior_step_then_adds_the_whole_step_after_it.py line 350 in run -> _poll_until line 46). Hypothesis: the activity ledger writes the activity with its creation-time status and never writes the terminal StatusCode or the EndTime after ECS has reconciled; open issue 20261125T221450Z-application-autoscaling-ecs-2-net-new-causes-scaling.md reports activities stranded InProgress with StatusMessage "Waiting for change to be fulfilled by ecs", which is consistent, and 20261125T225717Z-application-autoscaling-ecs-2-net-new-causes-a.md (Cause 2) and 20261123T142820Z-application-autoscaling-ecs-a-scaling-activity-that-really.md describe the same ledger defect — but none of them carries THIS check's replay key (this check's recorded key was the earlier runningCount frame, which no longer fires), so none covers this failure.

Why: the activity ledger is the only documented answer to "did my policy scale, did it succeed, and when did the cooldown start?". A client that waits for a terminal activity hangs forever; a client that computes the scale-out/scale-in cooldown window as EndTime + Cooldown cannot compute it at all, so every cooldown-dependent behaviour (netting a larger step inside the window, adding the whole step after it) is unverifiable against the emulator even though the capacity arithmetic itself is correct.

Fix: the scaling-activity state machine in the application-autoscaling scaling engine — whatever creates a ScalingActivity when a policy acts must also transition it to its terminal StatusCode and stamp EndTime once the capacity change has been applied by the target service, and the DescribeScalingActivities serializer must emit EndTime when present.

Traces: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it.

=== Cause 2 of 2: a FARGATE/awsvpc ECS service created in a caller-created VPC never places a task — runningCount stays 0 while desiredCount already reads N. Failing check: application-autoscaling__ecs.step-scaling-percent-change-with-min-adjustment-magnitude ===

AWS: Smithy (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model) — ECS Service.runningCount is "The number of tasks in the cluster that are in the RUNNING state", Service.pendingCount "...in the PENDING state", Deployment.runningCount "The number of tasks in the deployment that are in the RUNNING status", with Deployment.rolloutState moving IN_PROGRESS -> COMPLETED when the service reaches a steady state. The model states no timing rule, so convergence is recorded behavior only: for a REPLICA service with launchType FARGATE, networkMode awsvpc, a reachable public image and desiredCount N, the scheduler places N tasks and runningCount rises from 0 (through pendingCount) to N and stays there, the PRIMARY deployment reaching desiredCount == runningCount == N. The requirement is that runningCount eventually equals desiredCount for a service whose tasks can start; the particular N (4 and 6 in this check), the image public.ecr.aws/docker/library/busybox:1.36 with command `sh -c sleep 3600`, cpu '256'/memory '512' and the generated names are values from the request, not part of it. Application Auto Scaling's PercentChangeInCapacity arithmetic reads that RUNNING capacity, so this is a prerequisite for the MinAdjustmentMagnitude worked example (25% of 4 floored to 2; 60% of 6 rounded down to 3).

Emulator: CreateService returns and desiredCount reads N immediately, but DescribeServices keeps reporting runningCount 0 indefinitely. Replay: AssertionError: "leg floor: ECS reporting runningCount 4 for 'sim2real-behav-aas-floor-svc-<id>', the capacity step scaling reads not observed within 300.0s; last saw 0" (check_...step_scaling_percent_change_with_min_adjustment_magnitude.py line 283 in _run_leg -> _poll_until line 67). The service is created with networkConfiguration.awsvpcConfiguration={subnets:[<caller-created subnet>], securityGroups:[<that VPC's default SG>], assignPublicIp:'ENABLED'}. Open issue 20261125T225717Z-application-autoscaling-ecs-2-net-new-causes-a.md (Cause 1) describes the same symptom but carries only the check ids percent-change-sub-unit-rounding-and-magnitude-floor, step-scaling-cooldown-nets..., step-scaling-exact-bound... — not this check's replay key — so it does not cover this failure; fixing them together is expected.

Why: a client cannot observe its service ever becoming healthy — anything waiting for runningCount == desiredCount or for the PRIMARY deployment to reach steady state hangs forever, and every behaviour layered on running capacity (here PercentChangeInCapacity with MinAdjustmentMagnitude, which is arithmetic on the running count) can never be exercised or trusted.

Fix: the ECS service scheduler / task-placement component that reconciles a service's desiredCount into tasks for a caller-supplied awsvpc network configuration, and the DescribeServices state it reports (Service.runningCount / pendingCount and the PRIMARY Deployment's runningCount / rolloutState).

Traces: application-autoscaling__ecs.step-scaling-percent-change-with-min-adjustment-magnitude.

## Evidence
Replay of behavioral suite simparity-behavioral-aws-v1 against the booted emulator: 613/886 passed (69.2%), mismatched 227, errored 46, bootstrap_signal false. Service heading `### application-autoscaling__ecs`: 9/18 (50%), 9 failed. Seven of those nine are marked OWNED by existing issues and are excluded here; the two below carry no OWNED / ALREADY FILED marker and are the whole content of this issue.

Cause 1 — the activity never gets an EndTime:
[mismatch] application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it
  File "check_application_autoscaling__ecs_step_scaling_cooldown_nets_the_prior_step_then_adds_the_whole_step_after_it.py", line 350, in run -> line 46, in _poll_until
  AssertionError: leg 1: the initiating activity reaching an EndTime, where the cooldown starts not observed within 150.0s; last saw []

Expected (AWS): after a StepScaling policy on ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount' has moved desiredCount from 4 to 6 (+2 step), DescribeScalingActivities(..., IncludeNotScaledActivities=true) contains an activity whose Description is "Setting desired count to 6." — fixed text "Setting desired count to <N>." with <N> taken from the computed capacity — that settles on a terminal StatusCode ('Successful') with EndTime set; the 180 s cooldown window then runs from that EndTime.
Actual (emulator): the list of activities carrying an EndTime is empty for the full 150 s window ([]), so no cooldown start exists and the check cannot proceed to leg 2/leg 3. The step arithmetic itself passed: ECS reported desiredCount 6. The numbers 4/6, the 180 s Cooldown and the generated names are this run's values; the defect is that no activity ever terminates.
Smithy consulted: ScalingActivity.StatusCode (required) -> ScalingActivityStatusCode enum [Pending, InProgress, Successful, Overridden, Unfulfilled, Failed]; ScalingActivity.EndTime (optional timestamp, "for when the scaling activity ended"); ScalingActivity.StartTime (required); ScalingActivity.Description (required).

Cause 2 — Fargate runningCount never leaves 0:
[mismatch] application-autoscaling__ecs.step-scaling-percent-change-with-min-adjustment-magnitude
  File "check_application_autoscaling__ecs_step_scaling_percent_change_with_min_adjustment_magnitude.py", line 283, in _run_leg -> line 67, in _poll_until
  AssertionError: leg floor: ECS reporting runningCount 4 for 'sim2real-behav-aas-floor-svc-532f8e1024eb', the capacity step scaling reads not observed within 300.0s; last saw 0

Expected (AWS): DescribeServices(cluster, services=[service]) on a FARGATE/awsvpc service created with desiredCount 4, image public.ecr.aws/docker/library/busybox:1.36, command ['sh','-c','sleep 3600'], cpu '256', memory '512', networkConfiguration.awsvpcConfiguration={subnets:[<subnet>], securityGroups:[<sg>], assignPublicIp:'ENABLED'} reports runningCount == desiredCount == 4 once its tasks have started, the PRIMARY deployment reporting desiredCount == runningCount == 4 and rolloutState COMPLETED.
Actual (emulator): desiredCount reads 4 immediately, runningCount stays 0 for the whole 300 s window. The count 4 and the generated service name are this run's values; the defect is that runningCount never converges at all.
Smithy consulted: ECS Service.runningCount / Service.pendingCount; Deployment.runningCount; Deployment.rolloutState (IN_PROGRESS -> COMPLETED at steady state).

## Steps to Reproduce
Cause 1 (activity never gets an EndTime):
1. git_clone the repo and run the behavioral suite; the check id reproduces under `### application-autoscaling__ecs`.
2. Build a Fargate ECS estate that DOES converge: ECS CreateCluster; IAM CreateRole trusting ecs-tasks.amazonaws.com with AmazonECSTaskExecutionRolePolicy; RegisterTaskDefinition(networkMode='awsvpc', requiresCompatibilities=['FARGATE'], cpu='256', memory='512', executionRoleArn=<role>, containerDefinitions=[{name, image 'public.ecr.aws/docker/library/busybox:1.36', essential true, command ['sh','-c','sleep 3600']}]); CreateService(launchType='FARGATE', desiredCount=4, awsvpcConfiguration with assignPublicIp='ENABLED'). Wait for DescribeServices desiredCount == 4.
3. RegisterScalableTarget(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', MinCapacity=1, MaxCapacity=20).
4. PutScalingPolicy a StepScaling policy (AdjustmentType='ChangeInCapacity', StepAdjustments +2 for [0,20) and +3 for [20,null), Cooldown=180) and PutMetricAlarm GreaterThanThreshold 50 on a 10 s high-resolution custom metric with AlarmActions=[policy ARN].
5. Publish a value of 55 until the alarm reads ALARM; confirm ECS DescribeServices desiredCount moved 4 -> 6.
6. Poll DescribeScalingActivities(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', IncludeNotScaledActivities=true) and keep only activities with an EndTime.
   Expected: one activity, Description "Setting desired count to 6.", terminal StatusCode 'Successful', EndTime > StartTime; the cooldown window is EndTime + 180 s.
   Observed: the filtered list stays empty for 150 s and beyond; no activity ever carries an EndTime.

Cause 2 (Fargate runningCount never leaves 0):
1. CreateVpc 10.x.0.0/24 + CreateSubnet; CreateInternetGateway + AttachInternetGateway; a route table with 0.0.0.0/0 to the IGW associated with the subnet; ModifySubnetAttribute MapPublicIpOnLaunch=true; take that VPC's default security group.
2. IAM CreateRole trusting ecs-tasks.amazonaws.com + AmazonECSTaskExecutionRolePolicy.
3. ECS CreateCluster; RegisterTaskDefinition as in step 2 of Cause 1.
4. CreateService(launchType='FARGATE', desiredCount=4, networkConfiguration.awsvpcConfiguration={subnets:[<caller-created subnet>], securityGroups:[<that VPC's default SG>], assignPublicIp:'ENABLED'}).
5. Poll DescribeServices(cluster, services=[service]).
   Expected: desiredCount == 4 and runningCount rising to 4 (PRIMARY deployment desiredCount == runningCount == 4, rolloutState COMPLETED).
   Observed: desiredCount == 4 and runningCount == 0 for 300 s and beyond.
6. Re-replay both check ids in trace_ids to confirm both causes are fixed.
