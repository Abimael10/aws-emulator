Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling-plans.asp-delete-deregisters-the-scalable-target-it-created
Issue-Shape-Id: com.amazonaws.autoscalingplans#AnyScaleScalingPlannerFrontendService
Issue-Key: autoscaling-plans.asp-delete-deregisters-the-scalable-target-it-created|_await_no_targets#25|Timeout|time.sleep(5)
Issue-Commit: 925fe565299dec6eb0f742dd224c6e41f11f8969

# DeleteScalingPlan deletes the derived target-tracking policies but never deregisters the scalable target the plan registered, so the registration leaks and a cleanup poll never converges

> _Note: this issue was identified against the code base at commit `925fe565299dec6eb0f742dd224c6e41f11f8969`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteScalingPlan

## Description
Cause 1 of 1: applying a scaling plan registers a scalable target with Application Auto Scaling, but deleting the plan withdraws only the scaling policies - the scalable target stays registered forever.
AWS: ScalingInstruction is documented "AWS Auto Scaling creates target tracking scaling policies based on the scaling instructions", and a target tracking policy moves the capacity of a scalable target carrying the instruction's MinCapacity/MaxCapacity (Smithy: shape ScalingInstruction, members MinCapacity/MaxCapacity required; DeleteScalingPlan output is empty, so the cross-service state is the only observable). Recorded behavior only for the teardown side: after DeleteScalingPlan on a plan whose instruction named ServiceNamespace 'dynamodb', ResourceId 'table/<table>', ScalableDimension 'dynamodb:table:ReadCapacityUnits', application-autoscaling DescribeScalableTargets(ServiceNamespace='dynamodb', ResourceIds=['table/<table>']) returns an EMPTY ScalableTargets list and DescribeScalingPolicies(ServiceNamespace='dynamodb', ResourceId='table/<table>') returns an empty ScalingPolicies list - the plan takes its whole registration with it, not just the policy. The withdrawal is eventually consistent in the recording (the check polls for it), but it does converge.
Emulator: the create path registers the target and puts the policy correctly (that half of 20261022T175157Z-auto-scaling-plans-plan-lifecycle-has-no-application.md Cause 1 now works, and this check gets past its target/policy assertions), but the delete path diffs POLICIES only: the plan's shared install routine deletes every policy the previous instruction set asked for and the new set does not, and has no counterpart to register_scalable_target - DeregisterScalableTarget is never called by this service. DescribeScalableTargets therefore keeps reporting the plan's target, with the deleted plan's MinCapacity/MaxCapacity, indefinitely; the check's post-delete poll never converges and the harness kills it ('Check timed out after 60.0s').
Why: deleting a scaling plan is the documented way to stop AWS Auto Scaling managing a resource. Against the emulator the resource stays registered as a scalable target with the dead plan's capacity window, so (a) a client that deletes a plan and then re-creates one, or registers its own target with different bounds, inherits stale bounds or hits a conflicting registration; (b) a client that verifies cleanup by polling DescribeScalableTargets hangs instead of converging; (c) the resource looks managed by Application Auto Scaling when no plan or policy exists for it.
Fix: the plan's Application Auto Scaling projection (the install/diff routine the Create/Update/Delete handlers share) must diff the SCALABLE TARGETS as well as the policies: for every resource the previous instruction set registered that the current set no longer covers - which, for DeleteScalingPlan, is all of them - call DeregisterScalableTarget with that instruction's ServiceNamespace/ResourceId/ScalableDimension, after deleting its policies. Also covers the UpdateScalingPlan case where an instruction is dropped.
Related already-open issue: 20261022T175157Z-auto-scaling-plans-plan-lifecycle-has-no-application.md lists this check id, but its Cause 1 is about the plan registering NOTHING (that is fixed - the target and the policy are now registered, with the instruction's bounds and a plan-derived policy name); the un-deregistration on delete is what remains and is not described there, and its recorded replay key for this check (the targets-registered assertion) no longer matches.
Traces: autoscaling-plans.asp-delete-deregisters-the-scalable-target-it-created. Code sites: services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice/src/state/application_auto_scaling.rs:107-138 (install diffs installed_policies only) and :186-200 (register_scalable_target, with no deregister counterpart), called from src/ops/delete_scaling_plan.rs:197-204 with current = [].

## Evidence
Check autoscaling-plans.asp-delete-deregisters-the-scalable-target-it-created [mismatch]: 'Check timed out after 60.0s'.

The check gets past the create-side assertions (1 scalable target with ScalableDimension 'dynamodb:table:ReadCapacityUnits' and bounds (2, 8); 1 policy named 'AutoScaling-<plan>-...'), then calls DeleteScalingPlan, waits for the plan to disappear from DescribeScalingPlans, and polls _await_no_targets: application-autoscaling DescribeScalableTargets(ServiceNamespace='dynamodb', ResourceIds=['table/<table>']) until it answers []. That poll never converges, so the check is killed at the 60s budget.

Expected (recorded): after DeleteScalingPlan, DescribeScalableTargets for the instruction's resource -> ScalableTargets == [] and DescribeScalingPolicies -> ScalingPolicies == []; UpdateScalingPlan on the deleted plan then refuses with ObjectNotFoundException.
Actual: the scalable target the plan registered is still reported after the plan itself is gone; only the derived policy is removed.

Emulator source: install() (application_auto_scaling.rs:118-123) iterates installed_policies(previous) and calls DeleteScalingPolicy for policies no longer wanted, then registers targets for `current`; with current == [] on a delete, nothing deregisters the target registered at :199 ('RegisterScalableTarget'). No DeregisterScalableTarget call exists anywhere in this crate.

## Steps to Reproduce
1. Point the behavioral harness at the emulator and run check autoscaling-plans.asp-delete-deregisters-the-scalable-target-it-created.
2. Or directly: create a PROVISIONED DynamoDB table (RCU 2) and wait for ACTIVE. Call autoscaling-plans CreateScalingPlan with ApplicationSource={TagFilters:[{Key:'sim2real-asp',Values:['<table>']}]} and one ScalingInstruction (ServiceNamespace 'dynamodb', ResourceId 'table/<table>', ScalableDimension 'dynamodb:table:ReadCapacityUnits', MinCapacity 2, MaxCapacity 8, one predefined target-tracking configuration at TargetValue 55.0). Poll DescribeScalingPlans until StatusCode='Active'.
3. Confirm application-autoscaling DescribeScalableTargets(ServiceNamespace='dynamodb', ResourceIds=['table/<table>']) reports 1 target with bounds (2, 8).
4. Call DeleteScalingPlan(ScalingPlanName='<plan>', ScalingPlanVersion=1) and wait until DescribeScalingPlans no longer returns the plan.
5. Poll DescribeScalableTargets for the same resource: it keeps returning the plan's target (bounds 2/8) instead of converging to []. DescribeScalingPolicies for the resource does converge to [], which isolates the defect to the target registration.
