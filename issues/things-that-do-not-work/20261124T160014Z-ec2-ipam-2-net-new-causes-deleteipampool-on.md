Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.ipam-pool-with-provisioned-cidr-deletion-rejected ec2.op-contract-release-ipam-pool-allocation
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.ipam-pool-with-provisioned-cidr-deletion-rejected|run#34|AssertionError|assert second_delete_code == "InvalidIpamPoolId.NotFound", (
Issue-Key: ec2.op-contract-release-ipam-pool-allocation|run#30|AssertionError|assert (code2, message2) == ("InvalidParameterCombination", expected2), (
Issue-Commit: f7be059b7d8a500db20da2601816c40a629b62a4

# ec2 IPAM: 2 net-new causes — DeleteIpamPool on an already-deleted pool answers IncorrectState instead of InvalidIpamPoolId.NotFound, and a second ReleaseIpamPoolAllocation answers InvalidIpamPoolAllocationId.NotFound instead of InvalidParameterCombination with its exact message

> _Note: this issue was identified against the code base at commit `f7be059b7d8a500db20da2601816c40a629b62a4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteIpamPool, ReleaseIpamPoolAllocation

## Description
Two net-new root causes on the ec2 IPAM control plane, each grounded in one failing behavioural check. Both are repeat-call (idempotency) refusal paths where the emulator refuses with the WRONG error class, so a client cannot tell "this resource is already gone" from "this resource is in a state that forbids the action". Each cause is independent and must be closed on its own; this bundle contains 2 causes.

=== Cause 1 of 2: DeleteIpamPool on a pool that has already been deleted answers IncorrectState instead of the not-found refusal ===
AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json): DeleteIpamPool input is {IpamPoolId (required, target IpamPoolId), Cascade, DryRun}, output {IpamPool}; the operation declares NO modeled errors — ec2Query carries an untyped error envelope, so the code and message are recorded behaviour only. Recorded, for a DeleteIpamPool naming a pool id that this account/region did issue but that has since reached delete-complete and is gone: HTTP 400, Code `InvalidIpamPoolId.NotFound`, Message `The ipam-pool ID '<ipam-pool-id>' does not exist`. The fixed text is exactly `The ipam-pool ID '` … `' does not exist` (no trailing period); the only variable part is the IpamPoolId echoed from the request. Note the contrast with the live-dependency refusal on the SAME operation, which is a state conflict: a pool that still holds a provisioned CIDR refuses with `DependencyViolation`. AWS reserves the state-conflict class for a pool that still exists and the not-found class for one that does not.
Emulator: the first DeleteIpamPool (after the allocation is released and the CIDR deprovisioned) is accepted and the pool reaches delete-complete as it should, but repeating the identical call on the now-gone pool refuses with Code `IncorrectState` — the state-conflict class — instead of `InvalidIpamPoolId.NotFound`. The pool id is no longer resolvable, so there is no state to be "incorrect".
Why: deletion idempotency is the normal shape of every teardown path (CloudFormation rollback, Terraform destroy, a retried cleanup). Clients swallow `InvalidIpamPoolId.NotFound` as "already gone, done" and retry/back off on a state conflict, because `IncorrectState` legitimately means "wait and try again". Against this emulator a double delete looks retryable forever: teardown loops never converge, and a client cannot distinguish an id that was never issued from a pool that is merely busy.
Fix: the DeleteIpamPool handler's id-resolution step and the refusal it raises when the pool id does not resolve — resolution failure must raise the not-found refusal with AWS's code and message, before any state/dependency check that produces `IncorrectState`. The related open issue 20261116T044838Z-ec2-5-net-new-causes-createdefaultsubnet-and-deleteipampool.md carries this check's id for a DIFFERENT, earlier assertion in the same check (the DependencyViolation message's second sentence) and does not carry this replay key, so this cause is reported here; the fixed issue 20261029T200817Z-ec2-deleteipampool-stub.md stood the operation up but says nothing about the not-found class.
Traces: ec2.ipam-pool-with-provisioned-cidr-deletion-rejected

=== Cause 2 of 2: a second ReleaseIpamPoolAllocation of an already-released allocation answers InvalidIpamPoolAllocationId.NotFound with an emulator-authored sentence instead of InvalidParameterCombination with AWS's message ===
AWS: Smithy: ReleaseIpamPoolAllocation input is {IpamPoolId (required), Cidr (required), IpamPoolAllocationId (required), DryRun}, output {Success: Boolean}; the operation declares NO modeled errors (untyped ec2Query envelope), so the code and message are recorded behaviour only. Recorded: the first release returns HTTP 200 with Success=true and the allocation leaves GetIpamPoolAllocations; repeating the identical call refuses with HTTP 400, Code `InvalidParameterCombination`, Message `No allocation found with ID <ipam-pool-allocation-id> and CIDR <cidr>.` The fixed text is exactly `No allocation found with ID ` … ` and CIDR ` … `.` (trailing period included); the two variable parts are the IpamPoolAllocationId and the Cidr, both taken verbatim from the request. The class matters: AWS treats the (allocation-id, CIDR) pair as a parameter COMBINATION that no longer matches any allocation, not as a bad identifier — the same id is well formed and was genuinely issued.
Emulator: refuses with Code `InvalidIpamPoolAllocationId.NotFound` and the message `The ipam-pool-allocation ID '<ipam-pool-allocation-id>' does not exist` — an id-not-found class, an emulator-authored sentence, and it drops the Cidr the request named, so the refusal no longer states which pair failed to match.
Why: a client retrying a release (or releasing in a cleanup that may run twice) branches on the error code. `InvalidParameterCombination` tells it the id/CIDR pair is stale — recoverable by re-reading GetIpamPoolAllocations — while an id-not-found code reads as "this allocation id was never valid", which for IPAM allocation ids is a different diagnosis and sends the client down a wrong path. Any code that matches on AWS's message to confirm the allocation is already gone matches nothing.
Fix: the ReleaseIpamPoolAllocation handler's lookup-and-refusal path: when the (IpamPoolId, IpamPoolAllocationId, Cidr) triple matches no live allocation, raise `InvalidParameterCombination` with AWS's message interpolating the request's allocation id and CIDR, rather than the id-not-found refusal. The open issue 20261116T044838Z-ec2-releaseipampoolallocation-stub.md describes this operation as an unimplemented scaffold, but the operation now answers (the success path returns Success=true and the allocation does disappear), so that issue does not cover this refusal; the fixed issue 20261117T065015Z-ec2-ipam-pool-allocation-and-createvpc-provisioning-diverge.md covers the post-release pool view and carries none of this replay key.
Traces: ec2.op-contract-release-ipam-pool-allocation

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout. Both failures are verdict `mismatch`, service heading `### ec2`, and neither is marked ALREADY FILED or OWNED by an open issue.

Cause 1 — [mismatch] ec2.ipam-pool-with-provisioned-cidr-deletion-rejected:
  check_ec2_ipam_pool_with_provisioned_cidr_deletion_rejected.py, line 149, in run
  assert second_delete_code == "InvalidIpamPoolId.NotFound", (
  AssertionError: expected a double-delete on the now-gone pool to fail with InvalidIpamPoolId.NotFound, got 'IncorrectState'
  Sequence the check had already driven successfully before this point: DeleteIpamPool on a pool holding a provisioned CIDR + allocation refused with DependencyViolation; the pool survived in create-complete with its CIDR provisioned; ReleaseIpamPoolAllocation and DeprovisionIpamPoolCidr succeeded; the pool settled create-complete; the identical DeleteIpamPool then succeeded and the pool reached delete-complete. Only the repeat delete diverges.
  Expected: HTTP 400, Code InvalidIpamPoolId.NotFound, Message "The ipam-pool ID '<pool-id>' does not exist" (asserted at line 97745 of the report's check source).
  Observed: Code 'IncorrectState'.

Cause 2 — [mismatch] ec2.op-contract-release-ipam-pool-allocation:
  check_ec2_op_contract_release_ipam_pool_allocation.py, line 161, in run
  assert (code2, message2) == ("InvalidParameterCombination", expected2), (
  AssertionError: expected InvalidParameterCombination 'No allocation found with ID ipam-pool-alloc-00000000000000aec8d589e486e29eed and CIDR 10.102.1.0/24.' on double-release, got 'InvalidIpamPoolAllocationId.NotFound' "The ipam-pool-allocation ID 'ipam-pool-alloc-00000000000000aec8d589e486e29eed' does not exist"
  Steps that passed first: DryRun=True refused with DryRunOperation "Request would have succeeded, but DryRun flag is set."; the real release returned Success=True; the allocation left GetIpamPoolAllocations. Only the second release diverges.
  The allocation id and CIDR above are this run's values and fill the message template; they are not part of the requirement — the requirement is the code InvalidParameterCombination and the fixed text "No allocation found with ID " … " and CIDR " … "." with the request's IpamPoolAllocationId and Cidr interpolated.

Model grounding (smithy_query on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json): DeleteIpamPool input {DryRun, IpamPoolId (required), Cascade}, output {IpamPool}, errors []; ReleaseIpamPoolAllocation input {DryRun, IpamPoolId (required), Cidr (required), IpamPoolAllocationId (required)}, output {Success}, errors []. Neither operation models an error shape, so both codes and messages above are grounded in the recorded behaviour the checks assert.

## Steps to Reproduce
Cause 1 (DeleteIpamPool repeat delete):
1. CreateIpam(OperatingRegions=[<region>]); wait create-complete and read PrivateDefaultScopeId.
2. CreateIpamPool(IpamScopeId=<private scope>, AddressFamily=ipv4, Locale=<region>); wait create-complete.
3. ProvisionIpamPoolCidr(IpamPoolId=<pool>, Cidr=10.99.0.0/16); wait until GetIpamPoolCidrs shows it provisioned. AllocateIpamPoolCidr(IpamPoolId=<pool>, NetmaskLength=28) and note the allocation id and CIDR.
4. ReleaseIpamPoolAllocation(IpamPoolId=<pool>, IpamPoolAllocationId=<alloc>, Cidr=<alloc cidr>); wait until it leaves GetIpamPoolAllocations. DeprovisionIpamPoolCidr(IpamPoolId=<pool>, Cidr=10.99.0.0/16); wait deprovisioned; wait for the pool to settle in create-complete.
5. DeleteIpamPool(IpamPoolId=<pool>) — this must succeed; wait until the pool is gone or delete-complete.
6. Call DeleteIpamPool(IpamPoolId=<pool>) again with the identical input.
7. Expect HTTP 400, Code InvalidIpamPoolId.NotFound, Message "The ipam-pool ID '<pool>' does not exist". Observe Code 'IncorrectState'.

Cause 2 (ReleaseIpamPoolAllocation double release):
1. Reuse (or create) an IPAM and create a pool in its private default scope with Locale=<region>; wait create-complete.
2. ProvisionIpamPoolCidr on the pool and wait until the CIDR is provisioned; AllocateIpamPoolCidr(IpamPoolId=<pool>, NetmaskLength=24) and note IpamPoolAllocationId and Cidr; poll GetIpamPoolAllocations until the allocation is visible.
3. ReleaseIpamPoolAllocation(DryRun=True, IpamPoolId=<pool>, IpamPoolAllocationId=<alloc>, Cidr=<alloc cidr>) — expect DryRunOperation.
4. ReleaseIpamPoolAllocation without DryRun — expect Success=true; poll until the allocation leaves GetIpamPoolAllocations.
5. Repeat the exact call from step 4.
6. Expect HTTP 400, Code InvalidParameterCombination, Message "No allocation found with ID <alloc> and CIDR <alloc cidr>." Observe Code InvalidIpamPoolAllocationId.NotFound with "The ipam-pool-allocation ID '<alloc>' does not exist".
