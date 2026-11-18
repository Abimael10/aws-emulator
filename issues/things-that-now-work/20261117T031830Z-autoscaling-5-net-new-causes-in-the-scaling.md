Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.delete-policy-nonexistent-noop autoscaling.op-contract-deletePolicy autoscaling.op-contract-executePolicy autoscaling.scaling-policy-delete-removes-policy autoscaling.execute-policy-honors-cooldown autoscaling.percent-change-min-adjustment-magnitude autoscaling.step-scaling-selects-matching-step-adjustment autoscaling.target-tracking-policy-creates-alarms
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: auto-scaling|DeletePolicy|ValidationError
Issue-Key: autoscaling.op-contract-deletePolicy|run#15|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: autoscaling.op-contract-executePolicy|run#19|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: autoscaling.execute-policy-honors-cooldown|_error#23|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded with {result!r}")
Issue-Key: autoscaling.percent-change-min-adjustment-magnitude|_error#35|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: autoscaling.step-scaling-selects-matching-step-adjustment|run#39|AssertionError|assert message == "Metric value must be specified for policy type StepScaling", (
Issue-Key: autoscaling.target-tracking-policy-creates-alarms|run#40|AssertionError|assert error.get("Message") == expected_message, f'expected the message {expected_message!r}, got {error.get("Message")!r}'
Issue-Commit: 9335e6a6f0eaf1e6ae0c62968f13439343c093f5

# autoscaling: 5 net-new causes in the scaling-policy plane — three refusal messages are emulator-authored, ExecutePolicy defers inside a cooldown instead of refusing ScalingActivityInProgress, and PutScalingPolicy accepts MinAdjustmentMagnitude on ChangeInCapacity

> _Note: this issue was identified against the code base at commit `9335e6a6f0eaf1e6ae0c62968f13439343c093f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecutePolicy

## Description
FIVE independent root causes are bundled here, all in the Auto Scaling scaling-policy plane (PutScalingPolicy / ExecutePolicy / DeletePolicy). ALL FIVE must be closed before submitting. Each cause below carries its own failing check ids.

Related open issues that do NOT cover any of these: 20261029T172317Z-autoscaling-auto-scaling-6-fidelity-divergences-putscalingpolicy-rejects-none.md (three TargetTrackingConfiguration validation rules of PutScalingPolicy: both-metric-specifications, missing Statistic, TargetValue range — none of them MinAdjustmentMagnitude), 20261112T181500Z-autoscaling-putscalingpolicy-lists-the-metrictype-enum-value-set-in.md (enum value ORDER only), and 20261107T124656Z-autoscaling-auto-scaling-argument-validation-never-happens-8-operations.md (a different operation set). The two open cooldown issues found by keyword (20261110T220525Z-..., 20261111T010150Z-...) are application-autoscaling, a different service.

Cause 1 of 5: the policy-not-found refusal sentence is emulator-authored for every policy-scoped operation.
AWS: Smithy (Auto Scaling 2011-01-01, awsQuery): DeletePolicy input DeletePolicyType.PolicyName (ResourceName, required, len 1..1600) "The name or Amazon Resource Name (ARN) of the policy"; ExecutePolicy input ExecutePolicyType.PolicyName likewise. Neither operation models a not-found error, so the refusal arrives as the service-common awsQuery error ValidationError at HTTP 400 — recorded behavior for the text. Recorded message: `Policy <policyName> not found`. Fixed text: the word `Policy `, then the name, then ` not found`; the ONLY variable part is the policy name taken verbatim from the request's PolicyName.
Emulator: the same 400 ValidationError carries `Policy name not found - <policyName>` — a different sentence with the name moved to a trailing detail. Produced by the single shared helper `absent_policy_message` (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/state/policy_execution.rs:194-196), reached from DeletePolicy (src/ops/delete_policy.rs:194-198) and from ExecutePolicy via PolicyExecutionRefusal::NoSuchPolicy (same module, line 248).
Why: a client that matches on the refusal text (CDK/Terraform-style "treat not-found as already-deleted" idempotency shims, CloudFormation delete handlers, and test harnesses that assert the message) does not recognise the refusal and turns an expected not-found into a hard failure; the error code alone (ValidationError) cannot distinguish not-found from any other argument defect.
Fix: the refusal wording in the shared policy-resolution helper of the autoscaling service state (state/policy_execution.rs absent_policy_message), which both the DeletePolicy and the ExecutePolicy handlers call.
Traces: autoscaling.delete-policy-nonexistent-noop, autoscaling.op-contract-deletePolicy, autoscaling.op-contract-executePolicy, autoscaling.scaling-policy-delete-removes-policy.

Cause 2 of 5: ExecutePolicy(HonorCooldown=True) inside a SimpleScaling policy's cooldown is ACCEPTED 200 (the change is deferred) instead of being refused with the modeled ScalingActivityInProgress fault.
AWS: Smithy: ExecutePolicy models ScalingActivityInProgressFault (errorKind client, HTTP 400, awsQueryError code `ScalingActivityInProgress`) and ExecutePolicyType.HonorCooldown is documented "Indicates whether Amazon EC2 Auto Scaling waits for the cooldown period to complete before executing the policy. Valid only if the policy type is SimpleScaling." Recorded behavior (real AWS): a second ExecutePolicy with HonorCooldown=true, issued while the group is still inside the cooldown started by the previous policy-driven change, returns HTTP 400 `ScalingActivityInProgress` with the message `Scaling activity is in progress and blocks the execution of this policy` — entirely fixed text, no request-derived part — and DesiredCapacity does not move.
Emulator: the call returns HTTP 200 with an empty body; no error is raised at all. The handler instead STORES the capacity change as pending, due at the cooldown's end instant (state/policy_execution.rs:385-412, the `waits_for`/PendingCapacity branch), so the adjustment is silently re-applied later rather than refused.
Why: a client cannot observe that the cooldown blocked anything — the refusal it keys retry/back-off logic on never arrives — and worse, the adjustment it believed was rejected lands later, so capacity overshoots: a policy executed N times inside one cooldown window eventually applies N adjustments where AWS applies one.
Fix: the cooldown branch of ExecutePolicy in the autoscaling service state (state/policy_execution.rs AutoScalingState::execute_scaling_policy) must return a refusal that the ExecutePolicy handler serializes as the modeled ScalingActivityInProgressFault, instead of deferring the change.
Traces: autoscaling.execute-policy-honors-cooldown.

Cause 3 of 5: PutScalingPolicy accepts MinAdjustmentMagnitude paired with AdjustmentType=ChangeInCapacity and stores it as a dead field, instead of refusing the request.
AWS: Smithy: PutScalingPolicyType.MinAdjustmentMagnitude (integer, no @range) and .AdjustmentType (XmlStringMaxLen255) are both optional, so the model alone admits the pairing; MinAdjustmentMagnitude's own documentation scopes it to PercentChangeInCapacity. Recorded behavior (real AWS): PutScalingPolicy with PolicyType=SimpleScaling, AdjustmentType=ChangeInCapacity, ScalingAdjustment=1 and MinAdjustmentMagnitude=2 returns HTTP 400 ValidationError with the message `MinAdjustmentMagnitude is not supported by the specified adjustment type.` — entirely fixed text — and no policy is created (DescribePolicies does not list it).
Emulator: the call is served HTTP 200 and returns a PolicyARN plus an empty Alarms list, i.e. the policy IS created with a MinAdjustmentMagnitude its adjustment type never consults.
Why: a misconfiguration AWS rejects at write time becomes a silently stored policy that scales by a different amount than its author declared; infrastructure-as-code tools that rely on the write being refused converge on state real AWS cannot hold.
Fix: input validation in the PutScalingPolicy handler of the autoscaling service (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/put_scaling_policy.rs) — reject MinAdjustmentMagnitude (and MinAdjustmentStep) for any AdjustmentType other than PercentChangeInCapacity, before the policy is stored.
Traces: autoscaling.percent-change-min-adjustment-magnitude.

Cause 4 of 5: ExecutePolicy on a StepScaling policy without MetricValue is refused with the generic missing-member constraint sentence instead of Auto Scaling's step-policy sentence.
AWS: Smithy: ExecutePolicyType.MetricValue (MetricScale, double, optional in the model) is documented "The metric value to compare to BreachThreshold ... Required if the policy type is StepScaling and not supported otherwise", so the rule is service-side, not modeled. Recorded behavior (real AWS): ExecutePolicy naming a StepScaling policy with no MetricValue returns HTTP 400 ValidationError with the message `Metric value must be specified for policy type StepScaling`. Fixed text apart from the policy type word, which is the type of the stored policy the request named; DesiredCapacity does not move.
Emulator: the same 400 ValidationError carries `1 validation error detected: Value null at 'metricValue' failed to satisfy constraint: Member must not be null` — the generic member-must-not-be-null template, which AWS uses for model-level null violations and not for this rule. Produced by PolicyExecutionRefusal::MemberRequired(METRIC_VALUE_MEMBER_PATH) (state/policy_execution.rs:451-455) routed to scaling_policies::member_must_not_be_null_message (state/policy_execution.rs:252-254).
Why: the two messages mean different things to a client — one says "you omitted a required field of the request shape", the other says "this field is required for THIS policy's type". A caller (or an operator reading logs) cannot tell that the fix is to supply MetricValue for a step policy, and message-matching clients mis-classify the refusal.
Fix: the refusal wording for the step-policy metric members in the autoscaling service state (state/policy_execution.rs — the MemberRequired arm of PolicyExecutionRefusal::message, for metricValue and breachThreshold), which must carry the step-policy sentence rather than the generic null-member template.
Traces: autoscaling.step-scaling-selects-matching-step-adjustment.

Cause 5 of 5: the ExecutePolicy-on-a-target-tracking-policy refusal message is emulator-authored.
AWS: Smithy: ExecutePolicy's documentation and PutScalingPolicyType.PolicyType (values SimpleScaling / StepScaling / TargetTrackingScaling / PredictiveScaling) — a TargetTrackingScaling policy states a target, not an adjustment, so it cannot be executed. Recorded behavior (real AWS): ExecutePolicy naming a TargetTrackingScaling policy returns HTTP 400 ValidationError with the message `ExecutePolicy is not allowed for policy type TargetTrackingScaling` (no trailing period). Fixed text `ExecutePolicy is not allowed for policy type ` plus the stored policy's type as its only variable part.
Emulator: the same 400 ValidationError carries `ExecutePolicy is not supported for scaling policies of type TargetTrackingScaling.` — different wording and a trailing period. Produced by not_supported_for_policy_type_message (state/policy_execution.rs:219-221) via PolicyExecutionRefusal::UnexecutablePolicyType (same module, line 249-251).
Why: the refusal class is right but its text is not AWS's, so clients and test suites that assert the documented sentence (and operators grepping logs) see a divergence; the same helper also words every other "not supported for this policy type" refusal of this operation, so the deviation is systematic across that family.
Fix: the refusal wording in the shared helper of the autoscaling service state (state/policy_execution.rs not_supported_for_policy_type_message and the UnexecutablePolicyType arm that calls it), keeping the member-level "not supported" sentences distinct from the operation-level "not allowed for policy type" one.
Traces: autoscaling.target-tracking-policy-creates-alarms.

## Evidence
Cause 1 — expected `Policy <name> not found`; actual `Policy name not found - <name>`:
  autoscaling.delete-policy-nonexistent-noop: AssertionError: expected message 'Policy sim2real-policy-ghost-a408bc447877 not found', got 'Policy name not found - sim2real-policy-ghost-a408bc447877'
  autoscaling.op-contract-deletePolicy: AssertionError: expected message 'Policy sim2real-behav-policy-ed568e590db3 not found', got 'Policy name not found - sim2real-behav-policy-ed568e590db3'
  autoscaling.op-contract-executePolicy: AssertionError: expected message 'Policy sim2real-behav-nopolicy-90bb91eaab3b not found', got 'Policy name not found - sim2real-behav-nopolicy-90bb91eaab3b'
  autoscaling.scaling-policy-delete-removes-policy: AssertionError: expected message 'Policy sim2real-behav-as-pol-d449ba7692cd not found', got 'Policy name not found - sim2real-behav-as-pol-d449ba7692cd'
(the names above are per-run values that fill the template; the requirement is the template.)

Cause 2 — expected HTTP 400 `ScalingActivityInProgress` with message 'Scaling activity is in progress and blocks the execution of this policy' for ExecutePolicy(HonorCooldown=True) issued inside a 600s cooldown; actual HTTP 200 with an empty body — AssertionError: expected the call to be rejected, but it succeeded with {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'text/xml', 'content-length': '243', ...}}}. The preceding legs passed (the policy's Cooldown=600 and PolicyType=SimpleScaling round-trip, and the first HonorCooldown=False execution raised DesiredCapacity 0 -> 1 with one InService instance), so the group was demonstrably inside its cooldown when the second call was served 200.

Cause 3 — expected HTTP 400 ValidationError 'MinAdjustmentMagnitude is not supported by the specified adjustment type.' and the policy absent from DescribePolicies; actual HTTP 200 — AssertionError: expected the call to be rejected, but it SUCCEEDED and returned {'PolicyARN': 'arn:aws:autoscaling:<region>:<account>:scalingPolicy:<id>:autoScalingGroupName/<group>:policyName/<policy>', 'Alarms': [], 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}. Earlier legs of the same check passed (PercentChangeInCapacity floored by MinAdjustmentMagnitude=2 lifted desired 1 -> 3, ExactCapacity=1 scaled back in), so only the validation rule is missing.

Cause 4 — expected ValidationError message 'Metric value must be specified for policy type StepScaling'; actual "1 validation error detected: Value null at 'metricValue' failed to satisfy constraint: Member must not be null" (AssertionError: unexpected ValidationError message). The error CODE is ValidationError in both, only the sentence diverges.

Cause 5 — expected ValidationError message 'ExecutePolicy is not allowed for policy type TargetTrackingScaling'; actual 'ExecutePolicy is not supported for scaling policies of type TargetTrackingScaling.' (AssertionError: expected the message ... got ...).

## Steps to Reproduce
Replay the eight check ids in trace_ids. Minimal repros (us-west-2, any region):

Cause 1: autoscaling CreateAutoScalingGroup (any launch template, Min=0/Max=1/Desired=0), then DeletePolicy(AutoScalingGroupName=<group>, PolicyName=<a name no policy has>) and ExecutePolicy(AutoScalingGroupName=<group>, PolicyName=<same ghost name>). Expect HTTP 400 ValidationError with message 'Policy <ghost name> not found' from both.

Cause 2: same group with Min=0/Max=3/Desired=0; PutScalingPolicy(PolicyName=<p>, PolicyType='SimpleScaling', AdjustmentType='ChangeInCapacity', ScalingAdjustment=1, Cooldown=600); ExecutePolicy(PolicyName=<p>, HonorCooldown=False) and wait until DesiredCapacity==1 and the member instance is InService; then ExecutePolicy(PolicyName=<p>, HonorCooldown=True). Expect HTTP 400 ScalingActivityInProgress 'Scaling activity is in progress and blocks the execution of this policy', DesiredCapacity still 1 and no second launch (now or after the cooldown elapses).

Cause 3: on the same group, PutScalingPolicy(PolicyName=<bad>, PolicyType='SimpleScaling', AdjustmentType='ChangeInCapacity', ScalingAdjustment=1, MinAdjustmentMagnitude=2, Cooldown=0). Expect HTTP 400 ValidationError 'MinAdjustmentMagnitude is not supported by the specified adjustment type.' and <bad> absent from DescribePolicies.

Cause 4: PutScalingPolicy(PolicyName=<s>, PolicyType='StepScaling', AdjustmentType='ChangeInCapacity', MetricAggregationType='Average', StepAdjustments=[{'MetricIntervalLowerBound':0,'ScalingAdjustment':1}]); then ExecutePolicy(AutoScalingGroupName=<group>, PolicyName=<s>, HonorCooldown=False) with NO MetricValue/BreachThreshold. Expect HTTP 400 ValidationError 'Metric value must be specified for policy type StepScaling' and DesiredCapacity unchanged.

Cause 5: PutScalingPolicy(PolicyName=<t>, PolicyType='TargetTrackingScaling', TargetTrackingConfiguration={'PredefinedMetricSpecification':{'PredefinedMetricType':'ASGAverageCPUUtilization'},'TargetValue':50.0}); then ExecutePolicy(AutoScalingGroupName=<group>, PolicyName=<t>). Expect HTTP 400 ValidationError 'ExecutePolicy is not allowed for policy type TargetTrackingScaling'.
