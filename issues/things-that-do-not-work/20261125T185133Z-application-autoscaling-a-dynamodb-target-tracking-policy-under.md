Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling.target-tracking-policy-scales-out-table-capacity
Issue-Shape-Id: com.amazonaws.applicationautoscaling#AnyScaleFrontendService
Issue-Key: application-autoscaling.target-tracking-policy-scales-out-table-capacity|run#23|AssertionError|assert newest['Description'] == 'Setting read capacity units to 10.', (
Issue-Commit: d106a8c46ceb0c813215c303d305b3ab0a972c39

# application-autoscaling: a DynamoDB target-tracking policy under saturating read load scales to a small fixed capacity (3) instead of the proportional value clamped at MaxCapacity

> _Note: this issue was identified against the code base at commit `d106a8c46ceb0c813215c303d305b3ab0a972c39`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
One net-new cause in the application-autoscaling target-tracking evaluator as it drives the dynamodb:table:ReadCapacityUnits dimension.

Cause 1 of 1: when the tracked utilization metric is far above TargetValue, the emulator moves capacity to a small value unrelated to the proportional result instead of the proportional new capacity clamped at the registered MaxCapacity. Failing check: application-autoscaling.target-tracking-policy-scales-out-table-capacity.

AWS: Smithy (com-amazonaws-applicationautoscaling-anyscalefrontendservice) — PutScalingPolicy.TargetTrackingScalingPolicyConfiguration.TargetValue (required, double, 'the target value for the metric'), PredefinedMetricSpecification.PredefinedMetricType = DynamoDBReadCapacityUtilization, ScaleOutCooldown/ScaleInCooldown (both 0 here, so no timer is involved); DescribeScalingActivities returns ScalingActivities whose Description/Cause describe the action taken. The model states no control algebra, so for the arithmetic this is recorded behavior only: AWS's target-tracking controller is proportional — the new capacity is the current capacity scaled by (observed metric / TargetValue), rounded up, then clamped to the registered [MinCapacity, MaxCapacity]. With 1 provisioned RCU, a 40% target (AlarmHigh threshold 0.40*60*1 = 24 consumed units per minute) and four strongly-consistent readers delivering several hundred consumed units per minute, the proportional result is far above the registered MaxCapacity, so AWS clamps it to MaxCapacity and the single scale-out activity reports Description 'Setting read capacity units to <MaxCapacity>.' (fixed text 'Setting read capacity units to ' + the resolved capacity from state + '.') and Cause 'monitor alarm <AlarmHigh name> in state ALARM triggered policy <PolicyName>' (fixed text with the alarm name minted by PutScalingPolicy and the request's PolicyName substituted). DescribeTable then reports ProvisionedThroughput.ReadCapacityUnits == MaxCapacity with WriteCapacityUnits untouched.

Emulator: the AlarmHigh does breach and one scaling activity is produced, but it reports Description 'Setting read capacity units to 3.' for a target registered with MinCapacity=1, MaxCapacity=10 under a load that saturates 1 RCU many times over. The chosen capacity (3) is neither the proportional result (which exceeds 10) nor the MaxCapacity clamp (10). Hypothesis: the evaluator applies a fixed or additive bump (e.g. a small multiplier/step off the current capacity) rather than computing capacity * metric/TargetValue from the observed metric value, so the magnitude of the breach is discarded.

Why: a client that sizes a provisioned DynamoDB table with target tracking gets a table that is left massively under-provisioned under exactly the load the policy exists to absorb — reads keep throttling while the policy reports success, and the recorded activity ledger understates the capacity the policy would have applied on AWS. Capacity planning, throttling-recovery tests and any assertion that a saturating load drives the dimension to its configured ceiling are all wrong.

Fix: the target-tracking evaluator of the application-autoscaling scaling engine — the component that computes a new capacity from TargetValue and the observed tracked metric before the [MinCapacity, MaxCapacity] clamp, and the activity Description it records — not the clamp and not the alarm wiring (the alarm breach and the activity record are both produced).

Related but distinct open issue, which does not carry this check's replay key: 20261119T124426Z-application-autoscaling-ecs-3-net-new-causes-a.md — its Cause 1 is the OPPOSITE symptom (a converged ECS target-tracking policy ratcheting on to MaxCapacity); this one is an unconverged DynamoDB policy stopping far short of MaxCapacity.

Traces: application-autoscaling.target-tracking-policy-scales-out-table-capacity.

## Evidence
Expected (AWS): PROVISIONED table with ReadCapacityUnits=1, RegisterScalableTarget(dynamodb, table/<name>, dynamodb:table:ReadCapacityUnits, MinCapacity=1, MaxCapacity=10), TargetTrackingScaling with TargetValue=40.0 on DynamoDBReadCapacityUtilization and both cooldowns 0, driven by four concurrent strongly-consistent readers. The newest DescribeScalingActivities entry has Description 'Setting read capacity units to 10.' (10 = the registered MaxCapacity, because the proportional result exceeds it), Cause 'monitor alarm <the -AlarmHigh- alarm minted by PutScalingPolicy> in state ALARM triggered policy <PolicyName>', ScalableDimension 'dynamodb:table:ReadCapacityUnits', ServiceNamespace 'dynamodb'; DescribeTable then reports ProvisionedThroughput ReadCapacityUnits=10, WriteCapacityUnits=1, and DescribeScalableTargets still reports MinCapacity=1/MaxCapacity=10.

Actual (emulator): the newest scaling activity reports Description 'Setting read capacity units to 3.'
AssertionError (check_application_autoscaling_target_tracking_policy_scales_out_table_capacity.py line 150): "scale-out activity described itself as 'Setting read capacity units to 3.'; real AWS resolves a 40% target under saturating load to the MaxCapacity of 10 and says \"Setting read capacity units to 10.\"". The capacity value is the diverging part; the table name, policy name and alarm name are per-run values, not requirements.

## Steps to Reproduce
1. CreateTable with BillingMode=PROVISIONED and ProvisionedThroughput {ReadCapacityUnits: 1, WriteCapacityUnits: 1}; wait for TableStatus ACTIVE and PutItem one item.
2. Start four threads issuing GetItem(ConsistentRead=True) on that item in a tight loop (saturating load, hundreds of consumed read units per minute against 1 provisioned RCU).
3. RegisterScalableTarget(ServiceNamespace='dynamodb', ResourceId='table/<name>', ScalableDimension='dynamodb:table:ReadCapacityUnits', MinCapacity=1, MaxCapacity=10).
4. PutScalingPolicy(PolicyType='TargetTrackingScaling', TargetTrackingScalingPolicyConfiguration={TargetValue: 40.0, PredefinedMetricSpecification: {PredefinedMetricType: 'DynamoDBReadCapacityUtilization'}, ScaleOutCooldown: 0, ScaleInCooldown: 0}); note the single '-AlarmHigh-' alarm in the response's Alarms.
5. Poll DescribeScalingActivities(ServiceNamespace='dynamodb', ResourceId='table/<name>', ScalableDimension='dynamodb:table:ReadCapacityUnits') until an activity appears and read its Description: AWS says 'Setting read capacity units to 10.' (the MaxCapacity clamp); the emulator says 'Setting read capacity units to 3.'
6. Poll DescribeTable: AWS converges ProvisionedThroughput.ReadCapacityUnits to 10 with WriteCapacityUnits left at 1.
