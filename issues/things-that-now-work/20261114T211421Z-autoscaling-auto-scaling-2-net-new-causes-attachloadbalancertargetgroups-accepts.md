Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.op-contract-attachLoadBalancerTargetGroups autoscaling.termination-policy-selects-instance
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.op-contract-attachLoadBalancerTargetGroups|_error#19|AssertionError|raise AssertionError(
Issue-Key: autoscaling.termination-policy-selects-instance|run#27|AssertionError|assert message == (
Issue-Commit: a96ded234c891227942f65156e56a199ba16999f

# Auto Scaling: 2 net-new causes — AttachLoadBalancerTargetGroups accepts a nonexistent target group ARN, and the invalid-TerminationPolicies refusal is emulator-authored prose

> _Note: this issue was identified against the code base at commit `a96ded234c891227942f65156e56a199ba16999f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachLoadBalancerTargetGroups

## Description
Two independent net-new root causes in the Auto Scaling emulator, both grounded in failing behavioral checks. Neither is covered by an open issue: 20261107T081710Z-autoscaling-describeautoscalinggroups-omits-suspendedprocesses-the-launch-template-identity-and.md covers DescribeAutoScalingGroups omissions and scale-in ignoring TerminationPolicies (the *selection* behaviour, which this run's check passed), not the refusal wording below; 20261107T124656Z-autoscaling-auto-scaling-argument-validation-never-happens-8-operations.md covers a different, enumerated set of operations and does not name AttachLoadBalancerTargetGroups target-group existence.

Cause 1 of 2: AttachLoadBalancerTargetGroups does not validate that the target groups exist.
  AWS:      Smithy (com-amazonaws-autoscaling-autoscaling, AttachLoadBalancerTargetGroups): input AttachLoadBalancerTargetGroupsType with required AutoScalingGroupName (XmlStringMaxLen255) and required TargetGroupARNs (list); modeled errors are InstanceRefreshInProgressFault, ResourceContentionFault, ServiceLinkedRoleFailure. The model does not enumerate an existence check, so this is recorded behavior: real AWS refuses an attach naming a well-formed but nonexistent target group ARN with HTTP 400, awsQuery error code `ValidationError`, and the FIXED message (no variable parts at all — the ARN is not echoed) `Provided Target Groups may not be valid. Please ensure they exist and try again.` The group's attachment list is left unchanged by the refusal.
  Emulator: the call SUCCEEDS — HTTP 200 with an empty AttachLoadBalancerTargetGroupsResult body (the replay shows `{'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}`) for a target-group ARN that was never created. No error is raised at all.
  Why:      a client that provisions a target group and then attaches it can no longer detect a typo, a stale ARN, or an ARN from another account/region: the emulator reports success, the group silently carries an attachment to a target group that does not exist, and the client's error path (retry, rollback, surfacing ValidationError to the operator) is never exercised. Infrastructure code that asserts the refusal — and CloudFormation/CDK rollback logic built on it — passes against the emulator and breaks against AWS.
  Fix:      the AttachLoadBalancerTargetGroups handler's validation in the autoscaling service crate: before mutating the group, resolve every ARN in TargetGroupARNs against the ELBv2 target-group state and, when any one does not resolve, refuse the WHOLE request with ValidationError and the fixed sentence above, leaving the group's existing attachments untouched.
  Traces:   autoscaling.op-contract-attachLoadBalancerTargetGroups (the success half — attach a real target group and see it reach state Added/InService — passed; the failure is at the error boundary).

Cause 2 of 2: UpdateAutoScalingGroup refuses an unknown termination policy with the emulator's own sentence instead of AWS's.
  AWS:      Smithy (UpdateAutoScalingGroup): TerminationPolicies targets the TerminationPolicies list of XmlStringMaxLen1600 — the model does NOT enumerate the allowed values, so the valid-value set and the refusal text are recorded behavior. Real AWS refuses an unknown policy name with HTTP 400, error code `ValidationError`, and the message `[NotARealPolicy] is not a valid termination policy. Valid termination policies are: [AllocationStrategy, ClosestToNextInstanceHour, Default, Lambda, NewestInstance, OldestInstance, OldestLaunchConfiguration, OldestLaunchTemplate].` Template: the FIXED text is ` is not a valid termination policy. Valid termination policies are: [AllocationStrategy, ClosestToNextInstanceHour, Default, Lambda, NewestInstance, OldestInstance, OldestLaunchConfiguration, OldestLaunchTemplate].`; the only VARIABLE part is the leading `[...]`, which is the rejected policy value(s) taken verbatim from the request's TerminationPolicies member. Note the valid list is alphabetical and includes the literal `Lambda` (a Lambda function ARN is accepted in its place). The rejected update must leave the stored TerminationPolicies and the fleet untouched.
  Emulator: refuses with code ValidationError (correct) but the message `The TerminationPolicies value NotARealPolicy is not valid: it must be one of ["Default", "AllocationStrategy", "ClosestToNextInstanceHour", "NewestInstance", "OldestInstance", "OldestLaunchConfiguration", "OldestLaunchTemplate"] or a Lambda function ARN` — emulator-authored prose: different sentence shape, the value not bracketed, the list JSON-quoted, in a non-alphabetical order, missing the `Lambda` entry, and with the "or a Lambda function ARN" clause appended instead.
  Why:      clients and test suites that match on AWS's sentence (or parse the bracketed valid-value list out of it to report what the operator may use) see no match, so a correctly-detected bad configuration surfaces as an unrecognised error; conversely a fixer reading the emulator message would believe `Lambda` is not an accepted policy token.
  Fix:      the UpdateAutoScalingGroup (and the shared CreateAutoScalingGroup) TerminationPolicies validation: emit AWS's exact sentence with the rejected value(s) bracketed and the alphabetical valid-value list including `Lambda`.
  Traces:   autoscaling.termination-policy-selects-instance (OldestInstance/NewestInstance selection and the round-trip of the updated policy passed; only the refusal message diverges).

## Evidence
Cause 1 — check autoscaling.op-contract-attachLoadBalancerTargetGroups:
  expected: ClientError ValidationError, message "Provided Target Groups may not be valid. Please ensure they exist and try again.", attachment list unchanged
  actual:   AssertionError: real AWS rejects this call, but it SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'text/xml', 'content-length': '311', ...}, 'RetryAttempts': 0}}
  request:  AttachLoadBalancerTargetGroups(AutoScalingGroupName=<group>, TargetGroupARNs=[arn:aws:elasticloadbalancing:<region>:<account>:targetgroup/sim2real-nonexistent-<random>/0123456789abcdef])

Cause 2 — check autoscaling.termination-policy-selects-instance:
  request:  UpdateAutoScalingGroup(AutoScalingGroupName=<group>, TerminationPolicies=["NotARealPolicy"])
  expected: ValidationError / "[NotARealPolicy] is not a valid termination policy. Valid termination policies are: [AllocationStrategy, ClosestToNextInstanceHour, Default, Lambda, NewestInstance, OldestInstance, OldestLaunchConfiguration, OldestLaunchTemplate]."
  actual:   ValidationError / 'The TerminationPolicies value NotARealPolicy is not valid: it must be one of ["Default", "AllocationStrategy", "ClosestToNextInstanceHour", "NewestInstance", "OldestInstance", "OldestLaunchConfiguration", "OldestLaunchTemplate"] or a Lambda function ARN'
  (AssertionError: unexpected ValidationError message: ...)

## Steps to Reproduce
Cause 1:
1. Create a launch template and an Auto Scaling group (Min=0, Max=1, Desired=0) on it.
2. Create an ELBv2 target group in the group's VPC and attach it with AttachLoadBalancerTargetGroups; DescribeLoadBalancerTargetGroups shows it in state Added/InService (this part works).
3. Call AttachLoadBalancerTargetGroups on the same group with a well-formed but nonexistent target-group ARN (e.g. arn:aws:elasticloadbalancing:<region>:<account>:targetgroup/does-not-exist/0123456789abcdef).
4. Observe HTTP 200 instead of ValidationError "Provided Target Groups may not be valid. Please ensure they exist and try again."

Cause 2:
1. Create an Auto Scaling group with TerminationPolicies=["OldestInstance"].
2. Call UpdateAutoScalingGroup(AutoScalingGroupName=<group>, TerminationPolicies=["NotARealPolicy"]).
3. Observe the ValidationError message text and compare it to AWS's sentence above.

Replay: run_behavioral_suite and re-run the two check ids in trace_ids.
