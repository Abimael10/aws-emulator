Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__ecs.percent-change-sub-unit-rounding-and-magnitude-floor application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it application-autoscaling__ecs.step-scaling-exact-bound-picks-the-step-farther-from-the-threshold-on-both-sides application-autoscaling__ecs.step-scaling-scale-in-cooldown-suppresses-then-permits
Issue-Key: application-autoscaling__ecs.percent-change-sub-unit-rounding-and-magnitude-floor|_poll_until#10|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it|_poll_until#5|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__ecs.step-scaling-exact-bound-picks-the-step-farther-from-the-threshold-on-both-sides|_poll_until#5|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Key: application-autoscaling__ecs.step-scaling-scale-in-cooldown-suppresses-then-permits|_poll_until#35|AssertionError|raise AssertionError(f'{description} not observed within {timeout_s}s; last saw {last!r}')
Issue-Commit: cfd2de20f5625688b105ada019c4f72768e66f30

# application-autoscaling__ecs: 2 net-new causes — a Fargate service's runningCount never leaves 0, and a scaling activity that moved desiredCount never reaches StatusCode 'Successful'

> _Note: this issue was identified against the code base at commit `cfd2de20f5625688b105ada019c4f72768e66f30`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServices

## Description
Two distinct net-new root causes, both found under the `### application-autoscaling__ecs` heading of the behavioral replay. A fixer must close BOTH before submitting.

Cause 1 of 2: a FARGATE launch-type ECS service created with desiredCount N in a VPC/subnet the caller created never places any task — DescribeServices keeps reporting runningCount 0 (and the PRIMARY deployment likewise) while desiredCount already reads N, indefinitely. Failing checks: application-autoscaling__ecs.percent-change-sub-unit-rounding-and-magnitude-floor, application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it, application-autoscaling__ecs.step-scaling-exact-bound-picks-the-step-farther-from-the-threshold-on-both-sides.

AWS: Smithy (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model) — ECS Service.runningCount is documented as "The number of tasks in the cluster that are in the RUNNING state", Service.pendingCount as "The number of tasks in the cluster that are in the PENDING state", and Deployment.runningCount as "The number of tasks in the deployment that are in the RUNNING status" with Deployment.rolloutState IN_PROGRESS -> COMPLETED "when the service reaches a steady state". The model states no timing rule, so the convergence itself is recorded behavior only: for a REPLICA service with launchType FARGATE, networkMode awsvpc, a reachable public image and desiredCount N, the scheduler places N tasks and runningCount rises from 0 through pendingCount to N (the PRIMARY deployment reaching desiredCount == runningCount == N, rolloutState COMPLETED) within the task start time, and stays there. The requirement is that runningCount eventually equals desiredCount for a service whose tasks can start; the particular counts (1, 2, 4, 8) and the service names in the replay are values from the requests, not part of it. Application Auto Scaling's step arithmetic reads RUNNING capacity, so this is a prerequisite for every step-scaling behavior on an ECS target.

Emulator: desiredCount is set the instant CreateService returns, but runningCount never becomes non-zero. Replay: "ECS reporting runningCount 8 for 'sim2real-behav-aas-round-svc-...' once its Fargate tasks have started ... not observed within 240.0s; last saw 0"; "ECS reporting runningCount 4 for 'sim2real-behav-aas-cdnet-svc-...' not observed within 180.0s; last saw 0"; "ECS reporting desiredCount and runningCount 2 for 'sim2real-behav-aas-xbound-svc-...' not observed within 180.0s; last saw (2, 0)" — i.e. desiredCount 2 with runningCount 0. Hypothesis: these three checks build their own VPC/subnet/security group and pass it in awsvpcConfiguration, while the sibling check that DOES converge (step-scaling-scale-in-cooldown-suppresses-then-permits, see Cause 2) uses the default VPC, so the service scheduler appears not to place tasks for a caller-created awsvpc network configuration (or places a task that is immediately stopped and never counted).

Why: a client cannot observe its service ever becoming healthy — anything that waits for runningCount == desiredCount or for the deployment to reach steady state hangs forever, and any behavior layered on running capacity (Application Auto Scaling's ChangeInCapacity / PercentChangeInCapacity arithmetic, which reads the running count) can never be exercised or trusted.

Fix: the ECS service scheduler / task placement component that reconciles a service's desiredCount into tasks, and the DescribeServices state it reports (Service.runningCount / pendingCount and the PRIMARY Deployment's runningCount / rolloutState). Related open issues describe Fargate tasks in other scenarios going straight to STOPPED (20261118T145001Z-ecs-2-net-new-causes-a-long-running.md, 20261118T134608Z-ec2-ecs-elbv2-a-fargate-task-whose-subnet.md); neither carries any of these three check ids, so neither covers this cause.

Traces: application-autoscaling__ecs.percent-change-sub-unit-rounding-and-magnitude-floor (line 405 in run -> _poll_until line 70), application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it (line 193 in _ecs_estate), application-autoscaling__ecs.step-scaling-exact-bound-picks-the-step-farther-from-the-threshold-on-both-sides (line 220 in _ecs_estate).

Cause 2 of 2: on an ecs:service:DesiredCount scalable target whose ECS estate DID converge, a scale-in that really moved desiredCount leaves no activity with a terminal StatusCode — DescribeScalingActivities(IncludeNotScaledActivities=true) filtered on StatusCode == 'Successful' stays EMPTY after ECS has finished reconciling to the new count. Failing check: application-autoscaling__ecs.step-scaling-scale-in-cooldown-suppresses-then-permits.

AWS: Smithy (services/service-com-amazonaws-applicationautoscaling-anyscalefrontendservice/smithy-model) — ScalingActivity.StatusCode is REQUIRED and targets the enum ScalingActivityStatusCode with values [Pending, InProgress, Successful, Overridden, Unfulfilled, Failed]; ScalingActivity.EndTime is the optional "Unix timestamp for when the scaling activity ended"; StartTime is required. The model states the enum but not the transition rule, so the lifecycle is recorded behavior only: an activity is a job record that settles on a terminal status — 'Successful' with EndTime set once the requested capacity was applied ('Unfulfilled'/'Failed' only when it was not) — while Pending/InProgress are transient. A cooldown window is derived from that EndTime plus the policy's Cooldown, so a client cannot even compute the window until the activity terminates.

Emulator: the step policy fires, ECS DescribeServices reports the new desiredCount (the check's ChangeInCapacity assertion passes), and then the Successful-filtered activity list is empty for the whole 90 s observation window. Replay: AssertionError "leg 1: exactly one Successful scaling activity for 'service/<cluster>/<service>' once ECS finishes reconciling to 10 not observed within 90.0s; last saw []" (line 440 in _await_successful_count -> _poll_until line 59). Hypothesis: the activity ledger writes the activity with the non-terminal status it was created with and never writes the terminal StatusCode or EndTime once the capacity change has landed.

Why: the activity ledger is the documented way to answer "did my policy scale, and did it succeed?" — a client filtering DescribeScalingActivities on StatusCode=Successful sees zero successful events on a target that is in fact scaling, anything waiting for a terminal status hangs, and the scale-in/scale-out cooldown window (first activity EndTime + Cooldown) can never be computed, so cooldown-dependent client logic is unverifiable.

Fix: the scaling-activity state machine in the application-autoscaling scaling engine — whatever creates a ScalingActivity when a policy acts must also transition it to its terminal StatusCode and set EndTime when the capacity change is applied. Open issue 20261123T142820Z-application-autoscaling-ecs-a-scaling-activity-that-really.md describes the same ledger defect but carries only the check ids application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops and ...two-policies-triggered-together-scale-by-the-largest-not-the-sum, not this check's replay key, so it does not cover this failure; fixing both together is expected.

Traces: application-autoscaling__ecs.step-scaling-scale-in-cooldown-suppresses-then-permits.

## Evidence
Replay of behavioral suite simparity-behavioral-aws-v1 against the booted emulator: 138/150 passed, bootstrap_signal false. Service heading `### application-autoscaling__ecs`: 10/18 (56%), 8 failed; four of those are marked OWNED by existing issues and are excluded here, the four below carry no OWNED / ALREADY FILED marker.

Cause 1 — Fargate service runningCount never leaves 0:
[mismatch] application-autoscaling__ecs.percent-change-sub-unit-rounding-and-magnitude-floor
  File "check_...percent_change_sub_unit_rounding_and_magnitude_floor.py", line 405, in run -> _poll_until line 70
  AssertionError: ECS reporting runningCount 8 for 'sim2real-behav-aas-round-svc-88c4738b3637' once its Fargate tasks have started -- the capacity every leg below is arithmetic on not observed within 240.0s; last saw 0
[mismatch] application-autoscaling__ecs.step-scaling-cooldown-nets-the-prior-step-then-adds-the-whole-step-after-it
  File "check_...step_scaling_cooldown_nets_the_prior_step_then_adds_the_whole_step_after_it.py", line 193, in _ecs_estate -> _poll_until line 46
  AssertionError: ECS reporting runningCount 4 for 'sim2real-behav-aas-cdnet-svc-b2e79c0053c6' not observed within 180.0s; last saw 0
[mismatch] application-autoscaling__ecs.step-scaling-exact-bound-picks-the-step-farther-from-the-threshold-on-both-sides
  File "check_...step_scaling_exact_bound_picks_the_step_farther_from_the_threshold_on_both_sides.py", line 220, in _ecs_estate -> _poll_until line 68
  AssertionError: ECS reporting desiredCount and runningCount 2 for 'sim2real-behav-aas-xbound-svc-347b529aa2c5' not observed within 180.0s; last saw (2, 0)

Expected (AWS): DescribeServices on a FARGATE/awsvpc service created with desiredCount N (and a reachable public image, here public.ecr.aws/docker/library/busybox:1.36 running `sleep 3600`) reports runningCount == desiredCount == N once its tasks have started, and the PRIMARY deployment reports desiredCount == runningCount == N.
Actual (emulator): desiredCount reads N immediately and runningCount stays 0 for the whole 180-240 s window. The counts 2/4/8 and the generated cluster/service names are this run's values; the defect is that runningCount never converges at all.
Smithy consulted: ECS Service.runningCount "The number of tasks in the cluster that are in the RUNNING state"; Service.pendingCount; Deployment.runningCount / Deployment.rolloutState (IN_PROGRESS -> COMPLETED at steady state).

Cause 2 — activity never terminal:
[mismatch] application-autoscaling__ecs.step-scaling-scale-in-cooldown-suppresses-then-permits
  File "check_...step_scaling_scale_in_cooldown_suppresses_then_permits.py", line 440, in _await_successful_count -> _poll_until line 59
  AssertionError: leg 1: exactly one Successful scaling activity for 'service/sim2real-behav-aas-cool-cl-9c0c9cff2ee9/sim2real-behav-aas-cool-svc-3f5bbedc4668' once ECS finishes reconciling to 10 not observed within 90.0s; last saw []

Expected (AWS): after the scale-in step policy has moved ECS desiredCount (the check's ChangeInCapacity assertion on the new count already PASSES), DescribeScalingActivities(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', IncludeNotScaledActivities=true) contains exactly one activity with StatusCode 'Successful' and an EndTime strictly after its StartTime; the cooldown window is EndTime + the policy's Cooldown.
Actual (emulator): the Successful-filtered list is empty for the whole window, so no EndTime and no cooldown window exist. The count 10 is this run's post-scale-in capacity, not part of the requirement.
Smithy consulted: ScalingActivity.StatusCode required -> ScalingActivityStatusCode enum [Pending, InProgress, Successful, Overridden, Unfulfilled, Failed]; ScalingActivity.EndTime optional timestamp "for when the scaling activity ended"; ScalingActivity.StartTime required.

## Steps to Reproduce
Cause 1 (Fargate runningCount):
1. git_clone the repo and replay simparity-behavioral-aws-v1; the three check ids reproduce under `### application-autoscaling__ecs`.
2. CreateVpc 10.x.0.0/24 + CreateSubnet, CreateInternetGateway + AttachInternetGateway, a route table with 0.0.0.0/0 to the IGW associated with the subnet, ModifySubnetAttribute MapPublicIpOnLaunch=true; take the VPC's default security group.
3. IAM CreateRole trusting ecs-tasks.amazonaws.com and attach AmazonECSTaskExecutionRolePolicy.
4. ECS CreateCluster; RegisterTaskDefinition(networkMode='awsvpc', requiresCompatibilities=['FARGATE'], cpu='256', memory='512', executionRoleArn=<role>, containerDefinitions=[{name, image 'public.ecr.aws/docker/library/busybox:1.36', essential true, command ['sh','-c','sleep 3600']}]).
5. CreateService(launchType='FARGATE', desiredCount=N (e.g. 2, 4 or 8), networkConfiguration.awsvpcConfiguration={subnets:[<subnet>], securityGroups:[<sg>], assignPublicIp:'ENABLED'}).
6. Poll DescribeServices(cluster, services=[service]).
   Expected: desiredCount == N and runningCount rising to N (PRIMARY deployment desiredCount == runningCount == N).
   Observed: desiredCount == N and runningCount == 0 for 180-240 s and beyond.

Cause 2 (activity never terminal):
1. Build the same Fargate estate in the DEFAULT VPC with desiredCount=12 and wait for the PRIMARY deployment to converge (this part does work today).
2. RegisterScalableTarget(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', MinCapacity/MaxCapacity spanning the counts).
3. PutScalingPolicy a StepScaling scale-in policy (AdjustmentType='ChangeInCapacity', negative ScalingAdjustment, a non-zero Cooldown) and PutMetricAlarm on a high-resolution custom metric with AlarmActions=[policy ARN].
4. Publish a breaching value until the alarm reaches ALARM and ECS DescribeServices shows desiredCount moved by the adjustment.
5. Poll DescribeScalingActivities(..., IncludeNotScaledActivities=true) and filter on StatusCode == 'Successful'.
   Expected: exactly one activity, StatusCode 'Successful', EndTime > StartTime.
   Observed: the filtered list stays empty; no activity ever becomes terminal.
6. Re-replay the four check ids in trace_ids to confirm both causes are fixed.
