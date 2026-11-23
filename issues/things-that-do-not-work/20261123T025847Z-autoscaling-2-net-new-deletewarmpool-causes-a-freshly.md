Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.warm-pool-lifecycle autoscaling.op-contract-deleteWarmPool
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: auto-scaling|DeleteWarmPool|ResourceInUse
Issue-Commit: 26b85ab073b263b4ee174e2a21b18d29a50ca66d

# autoscaling: 2 net-new DeleteWarmPool causes — a freshly configured warm pool is refused ResourceInUse without ForceDelete, and deleting an absent warm pool answers 200 instead of ValidationError

> _Note: this issue was identified against the code base at commit `26b85ab073b263b4ee174e2a21b18d29a50ca66d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteWarmPool

## Description
Two net-new root causes on autoscaling DeleteWarmPool, both grounded in failing behavioral checks that the replay report marked neither ALREADY FILED nor OWNED. Both must be closed before this issue is submitted.

Related OPEN filings that do NOT cover these failures (they describe DeleteWarmPool answering HTTP 501 NotImplemented, which is no longer what happens — the operation is now implemented and answers a modeled refusal, and neither carries these checks' replay keys): 20261113T130916Z-autoscaling-deletewarmpool-stub.md and 20261113T201108Z-autoscaling-deletewarmpool-answers-501-notimplemented-instead-of-refusing.md. 20261113T064920Z-autoscaling-warm-pool-operations-diverge-phantom-warmed-instances.md is a WIRE issue about DescribeWarmPool inventing a warmed instance and about a delete that never takes effect; it carries wire replay keys only and does not cover the two refusal causes below.

Cause 1 of 2: DeleteWarmPool refuses a warm pool the caller has only just configured with HTTP 400 / ResourceInUse, where AWS accepts the delete.
Failing checks: autoscaling.warm-pool-lifecycle, autoscaling.op-contract-deleteWarmPool
AWS: Smithy (com.amazonaws.autoscaling#DeleteWarmPool): input {AutoScalingGroupName (XmlStringMaxLen255, required, length 1-255), ForceDelete (boolean) — "Specifies that the warm pool is to be deleted along with all of its associated instances, without waiting for all instances to be terminated"}, empty output, SUCCESS 200; modeled errors LimitExceededFault (400), ResourceContentionFault (500), ResourceInUseFault (400, awsQuery code ResourceInUse, doc "The operation can't be performed because the resource is in use"), ScalingActivityInProgressFault (400). Recorded behavior: on a group created with MinSize 0 / DesiredCapacity 0, PutWarmPool (MinSize=0, or MaxGroupPreparedCapacity=1) followed by DescribeWarmPool returning a WarmPoolConfiguration, a DeleteWarmPool WITHOUT ForceDelete is ACCEPTED — HTTP 200 with an empty DeleteWarmPoolResult — and DescribeWarmPool afterwards reports no WarmPoolConfiguration and no Instances. The ResourceInUse refusal with the fixed text "You cannot delete a warm pool while there are instances still in it unless you use the force delete option." (that message is entirely fixed text, it has no variable parts) belongs only to a pool that actually HOLDS warmed instances; a pool a client configured on a zero-capacity group and never let prepare anything is not in use.
Emulator: every such DeleteWarmPool is refused with HTTP 400, error code ResourceInUse and the message "You cannot delete a warm pool while there are instances still in it unless you use the force delete option." In warm-pool-lifecycle the pool was configured with MinSize=0 on a group at DesiredCapacity 0 (the check's own comment: "MinSize 0 keeps the pool empty"), so there is nothing in it at all, yet the refusal fires; the botocore failure is ResourceInUseFault on the DeleteWarmPool call. Hypothesis: the guard reads a pool population the emulator synthesizes the instant PutWarmPool returns (the same instant/phantom warmed-instance behavior the wire issue above records for DescribeWarmPool), so live_size() is non-zero for a pool that never prepared an instance.
Why: warm-pool teardown is impossible through the non-forced path — a client that configures a pool and then removes it (the ordinary create/update/delete lifecycle, and every cleanup path that must clear the pool before deleting the group) gets a 400 it cannot clear without changing its request to ForceDelete, and the safety signal AWS reserves for a pool holding live instances now fires for an empty one, so it no longer distinguishes the two.
Fix: the DeleteWarmPool handler's in-use guard and the warm-pool state it reads — the guard must only fire for instances the pool actually prepared and still holds, not for a pool configured on a group with no capacity.
Traces: autoscaling.warm-pool-lifecycle (ResourceInUseFault raised at the as_.delete_warm_pool call after PutWarmPool MinSize=0/PoolState=Running); autoscaling.op-contract-deleteWarmPool (same refusal at the asg.delete_warm_pool call after PutWarmPool MaxGroupPreparedCapacity=1, so the check's "expected DeleteWarmPool to return HTTP 200" assertion is never reached). Handler: services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/delete_warm_pool.rs:202-215 (`if !forced && group.warm_pool().is_some_and(|pool| pool.live_size() > 0)`).

Cause 2 of 2: DeleteWarmPool on a group that has NO warm pool answers 200 as a no-op, where AWS refuses it with ValidationError.
Failing check: autoscaling.op-contract-deleteWarmPool
AWS: recorded behavior only (the model declares no not-found error for this operation; the awsQuery common ValidationError carries it). After a successful DeleteWarmPool, calling DeleteWarmPool again for the same group is REFUSED — HTTP 400, Error.Code 'ValidationError', message "No warm pool found for Auto Scaling group '<AutoScalingGroupName>'." — the delete is NOT idempotent. Fixed text: "No warm pool found for Auto Scaling group '" and "'."; the variable part is the AutoScalingGroupName taken verbatim from the request. DescribeWarmPool afterwards still reports no WarmPoolConfiguration.
Emulator: the handler treats a group with no warm pool as already being in the requested state and returns HTTP 200 with an empty DeleteWarmPoolOutput — no refusal and no error code at all. This cause sits behind Cause 1 in the replay (the first delete never succeeds, so the check cannot reach its second delete), but the handler is unambiguous: the absent-pool branch falls straight through to `state.delete_warm_pool(...)` and `Ok(DeleteWarmPoolOutput {})`.
Why: a client cannot tell "the pool was there and I removed it" from "there was never a pool" — retry/idempotency logic, and any teardown that asserts the pool existed, read a success where AWS reports a validation failure, so a wrong group name or a double delete is silently swallowed.
Fix: the DeleteWarmPool handler's validation — after the group is resolved, refuse with ValidationError and AWS's sentence when the group carries no warm pool, instead of no-opping to 200.
Traces: autoscaling.op-contract-deleteWarmPool (the second delete asserts code == 'ValidationError' and message == "No warm pool found for Auto Scaling group '<group>'."). Handler: services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops/delete_warm_pool.rs:217-227 (the "A group with no warm pool is already in the state this delete asks for" branch).

## Evidence
Check [error] autoscaling.warm-pool-lifecycle — Cause 1
  Expected (AWS): CreateAutoScalingGroup(MinSize=0, MaxSize=2, DesiredCapacity=0) -> PutWarmPool(MinSize=0) -> DescribeWarmPool {MinSize 0, PoolState 'Stopped'} -> PutWarmPool(MinSize=0, PoolState='Running') -> DescribeWarmPool {PoolState 'Running'} -> DeleteWarmPool(AutoScalingGroupName=<group>) accepted with HTTP 200, then DescribeWarmPool with no WarmPoolConfiguration.
  Actual (emulator): botocore.errorfactory.ResourceInUseFault: An error occurred (ResourceInUse) when calling the DeleteWarmPool operation: You cannot delete a warm pool while there are instances still in it unless you use the force delete option. (HTTP 400). The PutWarmPool/DescribeWarmPool assertions before it all passed, so the pool was configured with MinSize 0 on a group at DesiredCapacity 0.

Check [error] autoscaling.op-contract-deleteWarmPool — Causes 1 and 2
  Expected (AWS): PutWarmPool(MaxGroupPreparedCapacity=1) -> DescribeWarmPool returns a WarmPoolConfiguration -> DeleteWarmPool -> HTTP 200 ('expected DeleteWarmPool to return HTTP 200'), DescribeWarmPool then reports WarmPoolConfiguration absent and Instances [] -> a SECOND DeleteWarmPool refused with code 'ValidationError' and message "No warm pool found for Auto Scaling group '<group>'.", DescribeWarmPool still showing no configuration.
  Actual (emulator): the FIRST DeleteWarmPool raises ResourceInUseFault (ResourceInUse, HTTP 400) with the in-use sentence, so the 200 assertion fails and the ValidationError assertion is never exercised. The handler source shows the absent-pool path would have answered 200 rather than the ValidationError the check asserts.

Status/code/message quoted from the emulator: HTTP 400, code 'ResourceInUse', message "You cannot delete a warm pool while there are instances still in it unless you use the force delete option." — all fixed text, no per-run values. Requirement for Cause 2's message: fixed text "No warm pool found for Auto Scaling group '" + the request's AutoScalingGroupName + "'."

## Steps to Reproduce
1. ec2 CreateLaunchTemplate with LaunchTemplateData {ImageId: <AL2023 AMI>, InstanceType: t3.micro}.
2. autoscaling CreateAutoScalingGroup(AutoScalingGroupName=<group>, LaunchTemplate={LaunchTemplateName/Id, Version '$Latest'}, MinSize=0, MaxSize=2, DesiredCapacity=0, VPCZoneIdentifier=<subnets>).
3. autoscaling PutWarmPool(AutoScalingGroupName=<group>, MinSize=0) (or MaxGroupPreparedCapacity=1), then DescribeWarmPool until WarmPoolConfiguration is present.
4. autoscaling DeleteWarmPool(AutoScalingGroupName=<group>) with NO ForceDelete.
   Observed: HTTP 400 / ResourceInUse / "You cannot delete a warm pool while there are instances still in it unless you use the force delete option."
   Expected: HTTP 200 with an empty DeleteWarmPoolResult, and DescribeWarmPool afterwards with no WarmPoolConfiguration and no Instances.
5. Once step 4 succeeds, call DeleteWarmPool(AutoScalingGroupName=<group>) again.
   Observed (from the handler): HTTP 200, a silent no-op.
   Expected: HTTP 400 / ValidationError / "No warm pool found for Auto Scaling group '<group>'.", with DescribeWarmPool still reporting no WarmPoolConfiguration.
6. Re-replay autoscaling.warm-pool-lifecycle and autoscaling.op-contract-deleteWarmPool to confirm both causes are closed.
