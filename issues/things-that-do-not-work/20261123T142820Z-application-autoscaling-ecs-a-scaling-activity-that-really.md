Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum
Issue-Key: application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops|run#45|AssertionError|assert 1 <= len(scaled) <= 6, (
Issue-Key: application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum|_arbitrate#22|AssertionError|assert successful, (
Issue-Commit: 94f7b27553b727586aac9de0add3c17de28dbaa9

# application-autoscaling__ecs: a scaling activity that really moved ECS desiredCount never reaches StatusCode 'Successful' in DescribeScalingActivities

> _Note: this issue was identified against the code base at commit `94f7b27553b727586aac9de0add3c17de28dbaa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScalingActivities

## Description
Cause 1 of 1: Application Auto Scaling records scaling activities for an ecs:service:DesiredCount scalable target, and those activities carry the right Description and the right Cause, but NONE of them ever reports StatusCode 'Successful' — not even after ECS's own DescribeServices independently confirms the capacity change has already landed and settled. Failing checks: application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops, application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum.

AWS: Smithy — DescribeScalingActivities returns ScalingActivities: list<ScalingActivity>. ScalingActivity.StatusCode is REQUIRED and targets the enum ScalingActivityStatusCode with values [Pending, InProgress, Successful, Overridden, Unfulfilled, Failed]; ScalingActivity.EndTime is the optional 'Unix timestamp for when the scaling activity ended'; ScalingActivity.Description is 'A simple description of what action the scaling activity intends to accomplish' and ScalingActivity.Cause is 'A simple description of what caused the scaling activity to happen'. The model states the enum but not the transition rule, so for the lifecycle this is recorded behavior only: a scaling activity is a job record with a terminal state, and an activity that actually applied the capacity change settles on the terminal value 'Successful' (with EndTime set), while Pending/InProgress are transient. Pending/InProgress may still be read for a few seconds after ECS's desiredCount has moved (AAS bookkeeping lags ECS), but the activity must reach a terminal status; 'Unfulfilled' or 'Failed' are reserved for an activity that did NOT achieve the requested capacity.

Emulator: for a target-tracking policy that scaled an ECS service from desiredCount 2 to 8 and then held there, DescribeScalingActivities returned exactly one activity, Description 'Setting desired count to 8.', and the set of activities with StatusCode == 'Successful' was EMPTY. For two simultaneously triggered step policies on one target, the emulator correctly arbitrated (the losing policy left no activity, the winning policy's activity is present, desiredCount moved to 14 and stayed), yet again no activity in the list reported StatusCode == 'Successful'. Hypothesis: the activity ledger writes an activity at the moment the engine decides to act and leaves its StatusCode on the non-terminal value it was created with (Pending or InProgress), never writing the terminal status (and EndTime) once the capacity change has been applied.

Why: a client that drives scaling from Application Auto Scaling and verifies it through the activity ledger — the documented way to answer 'did my policy actually scale, and did it succeed?' — can never get a yes. Operators' dashboards, CI gates and `aws application-autoscaling describe-scaling-activities` runbooks that filter on StatusCode=Successful see zero successful scaling events on a target that is in fact scaling, and anything that waits for a terminal activity status before proceeding hangs forever. Because Pending/InProgress is also how AWS reports an activity still in flight, a client cannot distinguish 'still working' from 'finished' at all.

Fix: the scaling-activity state machine in the application-autoscaling scaling engine — the component that creates a ScalingActivity when a policy acts must also transition it to its terminal StatusCode (Successful when the requested capacity was applied, Unfulfilled/Failed when it was not) and set EndTime at that point. This is the activity ledger's bookkeeping, not the controller: both checks show the controller itself now computing and applying the correct capacity.

Traces: application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops (AssertionError at `assert 1 <= len(scaled) <= 6`, where scaled = activities with StatusCode == 'Successful'); application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum (AssertionError at `assert successful`, same filter).

Related but distinct, and not covering this: open issue 20261119T124426Z-application-autoscaling-ecs-3-net-new-causes-a.md carries these same two check ids but is keyed to earlier assertions in both checks (`assert len(set(readings)) == 1` / `assert len(set(settle)) == 1`) — the capacity-algebra causes (runaway to MaxCapacity, summed adjustments, re-firing per evaluation). Both checks now get PAST those assertions and fail further along, at the activity ledger's StatusCode. Also distinct: 20261120T132657Z-application-autoscaling-ecs-2-net-new-causes-an--r2.md and 20261122T112017Z-application-autoscaling-cloudwatch-ec2-e-application-autoscaling-a-step-scaling-policy-whose-alarm--r2.md.

## Evidence
Check application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops
Expected (AWS): after a target-tracking policy has moved the ECS service's desiredCount from the start count to the converged count and ECS's DescribeServices confirms it, DescribeScalingActivities on ResourceId service/<cluster>/<service>, ScalableDimension ecs:service:DesiredCount contains between 1 and 6 activities with StatusCode 'Successful', one of which attributes itself to the policy via Cause.
Actual (emulator): the only activity returned is Description 'Setting desired count to 8.' and the Successful-filtered list is empty.
AssertionError: "DescribeScalingActivities reports 0 successful activities reaching desiredCount 8 from 2: ['Setting desired count to 8.']. At least one is required (something must have moved capacity) and more than six from 2 to 8 is thrashing, not proportional control"
(Fixed text of the assertion is the harness's; the requirement it encodes is AWS's: >=1 terminal 'Successful' activity. The numbers 8 and 2 are this run's start/converged counts, not part of the requirement.)

Check application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum
Expected (AWS): after two simultaneously triggered scale-out step policies are arbitrated and ECS reports the single resulting desiredCount, DescribeScalingActivities contains at least one activity with StatusCode 'Successful'.
Actual (emulator): the preceding assertions all PASS — no activity is attributable to the losing policy, an activity IS attributable to the winning policy, desiredCount settled on one value — and then the Successful-filtered list is empty.
AssertionError: "leg 1: DescribeScalingActivities has no successful activity even though desiredCount moved to 14"
(14 is this run's arbitrated count; the requirement is that SOME activity be terminal-Successful once ECS's count has moved.)

Model citation used: ScalingActivity.StatusCode required -> ScalingActivityStatusCode enum [Pending, InProgress, Successful, Overridden, Unfulfilled, Failed]; ScalingActivity.EndTime optional timestamp 'for when the scaling activity ended'.

## Steps to Reproduce
1. Create an ECS cluster, register a task definition, and create a service with desiredCount=2.
2. RegisterScalableTarget(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', MinCapacity=1, MaxCapacity=20).
3. PutScalingPolicy(PolicyType='TargetTrackingScaling', TargetValue=50.0, CustomizedMetricSpecification over a custom metric published as 400 / current desiredCount, ScaleOutCooldown=0, ScaleInCooldown=0).
4. Publish the closed-loop metric every 10 s until ECS DescribeServices reports desiredCount has risen to the fixed point and stopped moving.
5. Call DescribeScalingActivities(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount') and filter the returned ScalingActivities on StatusCode == 'Successful'. AWS returns at least one; the emulator returns none, and the one activity it does return ('Setting desired count to <N>.') never leaves its non-terminal StatusCode however long you poll.

Step-scaling variant:
1. Same estate with desiredCount=5, MinCapacity=1, MaxCapacity=60.
2. PutScalingPolicy two StepScaling policies (AdjustmentType='ChangeInCapacity', Cooldown=0) of +2 and +9 on MetricIntervalLowerBound=0.0.
3. PutMetricAlarm one alarm per policy (Period=10, EvaluationPeriods=1, DatapointsToAlarm=1, Threshold=50.0, AlarmActions=[policy ARN]) and publish a breaching value so both transition to ALARM together.
4. Wait for ECS DescribeServices desiredCount to move and settle, then call DescribeScalingActivities and filter on StatusCode == 'Successful': empty on the emulator.
