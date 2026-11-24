Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling-plans.asp-update-replaces-instruction-without-new-version
Issue-Shape-Id: com.amazonaws.autoscalingplans#AnyScaleScalingPlannerFrontendService
Issue-Key: autoscaling-plans.asp-update-replaces-instruction-without-new-version|_await_target_value#19|Timeout|time.sleep(3)
Issue-Commit: e21d6e6f6ee0013c77eca32e21a13d15f059c07a

# UpdateScalingPlan discards the request's new TargetValue for any metric an already-applied target-tracking policy tracks, so an in-place update never becomes observable

> _Note: this issue was identified against the code base at commit `e21d6e6f6ee0013c77eca32e21a13d15f059c07a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateScalingPlan

## Description
This bundle has 1 cause; it must be closed before submitting.

Cause 1 of 1: UpdateScalingPlan deliberately refuses to re-point the TargetValue of a target-tracking configuration whose scaling metric a policy the plan has ALREADY APPLIED tracks, so the replacement instruction set the request carries is only partly adopted and the update leaves no observable trace at all.
Failing check ids for this cause: autoscaling-plans.asp-update-replaces-instruction-without-new-version.

AWS: UpdateScalingPlan is documented as "Updates the specified scaling plan. You cannot update a scaling plan if it is in the process of being created, updated, or deleted" and its ScalingInstructions member is "The scaling instructions" — the plan's WHOLE instruction set, so the list the request carries is the configuration in force afterwards, member for member. Nothing in the model carves out the target value of an already-applied policy. The one member that governs what happens to pre-existing policies is ScalingInstruction$ScalingPolicyUpdateBehavior, and the Smithy doc scopes it to policies the service did NOT create: "Controls whether a resource's externally created scaling policies are kept or replaced. The default value is KeepExternalPolicies. If the parameter is set to ReplaceExternalPolicies, any scaling policies that are external to AWS Auto Scaling are deleted and new target tracking scaling policies created." TargetTrackingConfiguration$TargetValue is simply "The target value for the metric", with no update-time exception. Recorded behavior: after a successful UpdateScalingPlan on an Active plan, DescribeScalingPlans reports the plan still at ScalingPlanVersion 1 with CreationTime unchanged and StatusCode Active, its single instruction now reporting the TARGET VALUE THE UPDATE SENT; DescribeScalingPlanResources reports the same new target value on the derived policy, under the SAME PolicyName as before; the Application Auto Scaling policy of that name reports the new target value too; and — because neither the version nor the creation time moves — the plan's StatusMessage is the only field that distinguishes an updated plan from a freshly created one, changing from "Scaling plan has been created and applied to all resources." to "Scaling plan has been updated successfully." while StatusCode stays Active. Those two sentences are fixed text with no variable parts.

Emulator: crate services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice. The update path computes the instruction set in force as ScalingPlanRecord::applied_update(requested), which — for every requested TargetTrackingConfiguration whose metric identity matches a metric an applied policy for that same resource identity already tracks — OVERWRITES the requested target_value with the value in force. For the ordinary case the check exercises (an instruction over a real DynamoDB table with dynamic scaling enabled, so the policy IS applied), that means the new target value is thrown away on every update: DescribeScalingPlans keeps reporting the original target value forever, DescribeScalingPlanResources keeps reporting the original value on the derived policy, and the install into Application Auto Scaling re-puts the OLD value. The same silent drop then suppresses the status narrative: applied_footprint() is compared before and after, is unchanged because the only thing the request changed was that target value, so applied_change is false, begin_update() never sets AppliedMutation::Updated, and status_message() keeps returning the create sentence instead of "Scaling plan has been updated successfully.". The replay therefore never converges: the check polls DescribeScalingPlans for the updated target value and then for the update status message, both of which never appear, and it is killed at the harness ceiling — 'Check timed out after 60.0s'. UpdateScalingPlan itself answers 200 with an empty body throughout, so the client is told the update succeeded.

Why: changing a target-tracking target value is the single most common reason to call UpdateScalingPlan, and the API makes the update observable ONLY through the instruction read-back, the derived policy and the status message — the version and the creation time are pinned by design. A client that lowers or raises a target value against this emulator gets 200, then reads back the OLD value from the plan, from the plan's resources and from Application Auto Scaling, and sees the create-time status message, so it cannot distinguish "update applied" from "update dropped". Any convergence loop, drift detector or infrastructure reconciler built on that read-back blocks forever or concludes the resource is already in the desired state while the resource is still scaling to the old target.

Fix: the UpdateScalingPlan handler and the shared plan state in this crate must treat the request's ScalingInstructions as the configuration in force verbatim — drop the keep-the-in-force-target-value carve-out from the record's update path rather than narrowing it, since ScalingPolicyUpdateBehavior is about externally created policies and is the only documented policy-retention control. With the requested value adopted, the applied-footprint comparison that drives the status narrative and the install of the derived policies into Application Auto Scaling both follow correctly. Note the earlier resolved issue 20261029T092948Z-auto-scaling-plans-updatescalingplan-wrong-behavior.md ("UpdateScalingPlan keeps the in-force TargetValue even for a resource that has no applied policy"), which narrowed this carve-out to applied policies instead of removing it, and the still-open 20261022T175157Z-auto-scaling-plans-plan-lifecycle-has-no-application.md, whose Cause 2 covers only the ABSENCE of StatusMessage (now emitted — this check's first poll for the create sentence succeeds); neither covers the dropped target value, which is why the update narrative is still unreachable.

Traces: autoscaling-plans.asp-update-replaces-instruction-without-new-version.

## Evidence
Suite: simparity-behavioral-aws-v1, service heading '### autoscaling-plans', 7/10 passed (3 failed). Two of the three failures are excluded as already covered: asp-absent-resource-plan-is-rejected-asynchronously and asp-predictive-forecast-without-history-has-no-datapoints are both marked OWNED by existing open issues. This issue is grounded in the third.

Check autoscaling-plans.asp-update-replaces-instruction-without-new-version -> [mismatch] 'Check timed out after 60.0s'.

Sequence the check drives and where it stalls:
1. CreateScalingPlan over a PROVISIONED DynamoDB table the check creates, one dynamodb:table:ReadCapacityUnits instruction, MinCapacity 1, MaxCapacity 10, one PredefinedScalingMetricSpecification DynamoDBReadCapacityUtilization, TargetValue 50.0.
2. Poll DescribeScalingPlans until StatusMessage == 'Scaling plan has been created and applied to all resources.' -> PASSES against the emulator (StatusCode Active, version 1).
3. UpdateScalingPlan(ScalingPlanName=<plan>, ScalingPlanVersion=1, ScalingInstructions=[same instruction with TargetValue 75.0, ScalingPolicyUpdateBehavior='ReplaceExternalPolicies']) -> 200 with an empty body (correct).
4. Poll DescribeScalingPlans until ScalingInstructions[0].TargetTrackingConfigurations[0].TargetValue == 75.0 while StatusCode == 'Active'.
   Expected: the plan reports 75.0.
   Actual: it keeps reporting 50.0 indefinitely; the poll never converges and the check is killed at 60s.
5. Never reached, and unreachable for the same cause: poll for StatusMessage == 'Scaling plan has been updated successfully.' (the emulator keeps the create sentence because the applied footprint it compares is unchanged), DescribeScalingPlanResources[0].ScalingPolicies[0].TargetTrackingConfiguration.TargetValue == 75.0 under the unchanged PolicyName, and application-autoscaling DescribeScalingPolicies(ServiceNamespace='dynamodb', ResourceId='table/<table>') reporting TargetTrackingScalingPolicyConfiguration.TargetValue == 75.0.

Note: 50.0 / 75.0 / the generated plan and table names are this recording's values, not requirements; the requirement is that the value the update SENDS is the value every read-back reports. The two status sentences in step 2 and step 5 are fixed text with no variable parts.

Source evidence (crate services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice):
- src/ops/update_scaling_plan.rs lines 260-266: `let in_force = plan.applied_update(instructions); plan.scaling_instructions = in_force;` then `applied_change |= plan.applied_footprint(&plan.scaling_instructions) != applied_before;`
- src/ops/update_scaling_plan.rs line 272: `plan.begin_update(applied_change, now);`
- src/state.rs lines 467-499 (`applied_update`): for a requested configuration whose `target_metric_identity` matches a metric in `applied_policies(in_force)`, `configuration.target_value = *in_force;` — the requested value is overwritten.
- src/state.rs lines 328-333 (`begin_update`): `AppliedMutation::Updated` is set only `if applied_change`.
- src/state.rs lines 354-358 (`status_message`): the update sentence is returned only for `AppliedMutation::Updated`.

Model evidence (com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice): UpdateScalingPlan doc 'Updates the specified scaling plan...'; UpdateScalingPlanRequest$ScalingInstructions 'The scaling instructions'; ScalingInstruction$ScalingPolicyUpdateBehavior scopes KeepExternalPolicies/ReplaceExternalPolicies to "a resource's externally created scaling policies" / "policies that are external to AWS Auto Scaling"; TargetTrackingConfiguration$TargetValue 'The target value for the metric' with no update-time exception; ScalingPlan declares StatusMessage next to the required StatusCode and ScalingPlanVersion is documented 'This value is always 1'.

## Steps to Reproduce
1. Point the behavioral harness at the emulator and run check autoscaling-plans.asp-update-replaces-instruction-without-new-version (crate services/service-com-amazonaws-autoscalingplans-anyscalescalingplannerfrontendservice).
2. Create a PROVISIONED DynamoDB table (ReadCapacityUnits 1, WriteCapacityUnits 1) and wait for table_exists.
3. CreateScalingPlan with ApplicationSource TagFilters matching the table and one ScalingInstruction: ServiceNamespace 'dynamodb', ResourceId 'table/<table>', ScalableDimension 'dynamodb:table:ReadCapacityUnits', MinCapacity 1, MaxCapacity 10, one TargetTrackingConfiguration with PredefinedScalingMetricType 'DynamoDBReadCapacityUtilization' and TargetValue 50.0.
4. Poll DescribeScalingPlans(ScalingPlanNames=['<plan>']) until StatusCode is 'Active' and StatusMessage is 'Scaling plan has been created and applied to all resources.'. Record DescribeScalingPlanResources(...)['ScalingPlanResources'][0]['ScalingPolicies'][0]['PolicyName'].
5. Call UpdateScalingPlan(ScalingPlanName='<plan>', ScalingPlanVersion=1, ScalingInstructions=[the same instruction with TargetValue 75.0 and ScalingPolicyUpdateBehavior='ReplaceExternalPolicies']) -> 200, empty body.
6. Read DescribeScalingPlans(ScalingPlanNames=['<plan>']) again: observe ScalingInstructions[0].TargetTrackingConfigurations[0].TargetValue is still 50.0 instead of 75.0, and StatusMessage is still the create sentence instead of 'Scaling plan has been updated successfully.' — polling for either value never converges, so the check dies at the 60s harness ceiling.
7. Read DescribeScalingPlanResources(ScalingPlanName='<plan>', ScalingPlanVersion=1): the derived policy (same PolicyName as step 4, which is correct) still reports TargetTrackingConfiguration.TargetValue 50.0; application-autoscaling DescribeScalingPolicies(ServiceNamespace='dynamodb', ResourceId='table/<table>') reports the same stale 50.0.
8. Repeat step 5 with any other TargetValue: every update is accepted with 200 and none of them ever changes the value the plan, its resources or Application Auto Scaling report.
