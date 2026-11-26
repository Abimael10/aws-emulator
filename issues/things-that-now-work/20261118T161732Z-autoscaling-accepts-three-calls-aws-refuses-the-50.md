Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.put-scaling-policy-limit-exceeded-rejected autoscaling.update-asg-launch-template-missing-image-rejected autoscaling.set-instance-health-inside-the-grace-period-rejected-and-the-instance-is-not-replaced
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.put-scaling-policy-limit-exceeded-rejected|_error#11|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: autoscaling.update-asg-launch-template-missing-image-rejected|_client_error#17|AssertionError|raise AssertionError(f"expected the call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Key: autoscaling.set-instance-health-inside-the-grace-period-rejected-and-the-instance-is-not-replaced|_error_code_and_message#11|AssertionError|raise AssertionError("the call was expected to be rejected but it succeeded")
Issue-Commit: c9344e2779494e078eb181c7552bbea5ec625bd0

# autoscaling accepts three calls AWS refuses: the 50-policy limit, a launch template with no ImageId, and SetInstanceHealth inside the grace period

> _Note: this issue was identified against the code base at commit `c9344e2779494e078eb181c7552bbea5ec625bd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
Three net-new causes where the emulator answers HTTP 200 to a call AWS refuses with HTTP 400. Each must be closed separately; they are bundled only because all three are missing refusals in one service.

Cause 1 of 3 — PutScalingPolicy does not enforce the 50-policies-per-group limit.
AWS: Smithy: PutScalingPolicy models LimitExceededFault (errorKind client, awsQueryError code LimitExceeded, httpResponseCode 400). Recorded behavior: once a group already holds the maximum of 50 scaling policies, a further PutScalingPolicy is refused 400 with code LimitExceeded and the fixed sentence 'The maximum limit of 50 policies has been reached.' (no interpolated values); the group's policy set stays at 50, and freeing one slot lets the identical call succeed.
Emulator: the 51st PutScalingPolicy returns HTTP 200 with a PolicyARN and an empty Alarms list, so the policy is created and the per-group ceiling does not exist.
Why: a client that sizes its policy set against the documented quota (or catches LimitExceededFault to prune and retry) is told the write succeeded, so its view of the group diverges from AWS's and the code path AWS exercises is never tested; nothing bounds policy growth.
Fix: PutScalingPolicy's handler/validation — count the group's existing policies and raise the modeled LimitExceededFault with AWS's fixed sentence before the policy is stored.
Traces: autoscaling.put-scaling-policy-limit-exceeded-rejected.

Cause 2 of 3 — UpdateAutoScalingGroup accepts a launch template that carries no ImageId.
AWS: Smithy: UpdateAutoScalingGroup.LaunchTemplate -> LaunchTemplateSpecification; the modeled errors are ResourceContentionFault, ScalingActivityInProgressFault and ServiceLinkedRoleFailure, so the completeness check on the referenced template is a server-side validation whose refusal is recorded behavior only: 400 ValidationError with the fixed sentence 'You must use a valid fully-formed launch template. The request must contain the parameter ImageId', and the group stays pointed at its previous template and version.
Emulator: the update returns HTTP 200 and rebases the group onto the incomplete template, so a group can end up pointing at a template that can never launch an instance.
Why: the failure is deferred from the control-plane call (where the caller can react) to every subsequent launch, which then fails or silently never happens; a client that treats the 200 as 'the group is now valid' ships a group that cannot scale.
Fix: UpdateAutoScalingGroup's handler — resolve the referenced launch template version and validate that the required launch parameters (ImageId) are present before accepting the update.
Traces: autoscaling.update-asg-launch-template-missing-image-rejected.

Cause 3 of 3 — SetInstanceHealth with ShouldRespectGracePeriod=true is accepted inside the health-check grace period.
AWS: Smithy: SetInstanceHealth.InstanceId and .HealthStatus are required, .ShouldRespectGracePeriod is a boolean; only ResourceContentionFault is modeled, so this refusal is recorded behavior only: marking an instance Unhealthy with ShouldRespectGracePeriod=true while the instance is still inside its group's HealthCheckGracePeriod is refused outright with HTTP 400 and code ValidationError (AWS's message embeds the instance's server-measured age in seconds, so only the code/status and the fact of the refusal are pinned here), the instance keeps running and is not replaced; the same call with the flag off is accepted and replaces it.
Emulator: the call returns HTTP 200 — the grace period is not consulted at all, so the flag has no effect.
Why: a client using ShouldRespectGracePeriod=true as the guard that protects a still-booting instance gets the opposite of the contract: the instance is marked unhealthy and replaced during boot, producing a replacement loop AWS prevents, and the refusal it branches on never arrives.
Fix: SetInstanceHealth's handler — when ShouldRespectGracePeriod is true, compare the instance's in-service age against the group's HealthCheckGracePeriod and refuse with ValidationError instead of applying the status.
Traces: autoscaling.set-instance-health-inside-the-grace-period-rejected-and-the-instance-is-not-replaced.

The policy names, group names, template ids, instance ids and grace-period value quoted in the evidence are one run's request/state values; the requirement is the status, the code, the fixed message text and the absent state change.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (service heading '### autoscaling'):
Cause 1 — autoscaling.put-scaling-policy-limit-exceeded-rejected: after creating 50 SimpleScaling policies on one group, the 51st PutScalingPolicy expected code LimitExceeded with 'The maximum limit of 50 policies has been reached.'; AssertionError 'expected the call to be rejected, but it SUCCEEDED and returned {PolicyARN: arn:aws:autoscaling:us-west-2:000000000000:scalingPolicy:...:policyName/sim2real-pol-overflow-..., Alarms: [], HTTPStatusCode: 200}'.
Cause 2 — autoscaling.update-asg-launch-template-missing-image-rejected: a launch template created with only InstanceType (no ImageId); UpdateAutoScalingGroup pointing the group at it expected code ValidationError with 'You must use a valid fully-formed launch template. The request must contain the parameter ImageId'; AssertionError 'expected the call to be rejected, but it SUCCEEDED and returned {... HTTPStatusCode: 200}'.
Cause 3 — autoscaling.set-instance-health-inside-the-grace-period-rejected-and-the-instance-is-not-replaced: group created with HealthCheckType=EC2 and HealthCheckGracePeriod=900; SetInstanceHealth(HealthStatus='Unhealthy', ShouldRespectGracePeriod=True) on the freshly InService instance expected code ValidationError/HTTP 400; AssertionError 'the call was expected to be rejected but it succeeded'.

## Steps to Reproduce
1. Replay the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout.
2. Cause 1: create a group, PutScalingPolicy 50 distinct SimpleScaling policies on it, then call PutScalingPolicy once more and read the HTTP status / Error.Code; then DescribePolicies and count.
3. Cause 2: create a launch template whose LaunchTemplateData has InstanceType but no ImageId, then UpdateAutoScalingGroup an existing group onto it and read the HTTP status; then DescribeAutoScalingGroups and check which template the group points at.
4. Cause 3: create a group with HealthCheckType=EC2 and a long HealthCheckGracePeriod (e.g. 900 s), wait for one InService instance, then call SetInstanceHealth(InstanceId=<it>, HealthStatus='Unhealthy', ShouldRespectGracePeriod=True) and read the HTTP status; then watch whether the instance is replaced.
