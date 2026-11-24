Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.create-network-interface-permission ec2.op-contract-delete-network-interface-permission ec2.subnet-cidr-reservation-and-eni-permission ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves ec2.op-contract-delete-ipam-prefix-list-resolver-target
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.create-network-interface-permission|_client_error#8|AssertionError|raise AssertionError(f"real AWS rejects this call, but it SUCCEEDED and returned {result!r}")
Issue-Key: ec2.op-contract-delete-network-interface-permission|_error_of#6|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED and returned {result!r}")
Issue-Key: ec2.subnet-cidr-reservation-and-eni-permission|run#27|AssertionError|raise AssertionError(
Issue-Key: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves|run#44|AssertionError|assert promoted_instance["State"]["Name"] == "running", (
Issue-Key: ec2.op-contract-delete-ipam-prefix-list-resolver-target|run#28|AssertionError|assert code == "InvalidIpamPrefixListResolverTargetId.NotFound", f"unexpected code re-deleting {code!r}"
Issue-Commit: 51580daa099a6a502e147b76b77645918dba2576

# ec2: 3 net-new causes — CreateNetworkInterfacePermission grants a cross-account ENI permission AWS refuses, a warm-pool-promoted instance never leaves 'pending', and DeleteIpamPrefixListResolverTarget re-delete answers IncorrectState

> _Note: this issue was identified against the code base at commit `51580daa099a6a502e147b76b77645918dba2576`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNetworkInterfacePermission

## Description
Three independent net-new root causes in the ec2 emulator, found by the behavioral replay. They are bundled because they share one service token; each must be closed on its own before this issue is submitted.

=== Cause 1 of 4 is not present; this bundle carries 3 causes. ===

--- Cause 1 of 3: CreateNetworkInterfacePermission accepts a grantee account AWS refuses, and records the grant ---
Failing checks: ec2.create-network-interface-permission, ec2.op-contract-delete-network-interface-permission, ec2.subnet-cidr-reservation-and-eni-permission.
AWS: recorded behavior only. Smithy (services/service-com-amazonaws-ec2-amazonec2) models CreateNetworkInterfacePermission with input NetworkInterfaceId (required), AwsAccountId (String), AwsService (String), Permission (InterfacePermissionType enum, required), DryRun, and output InterfacePermission -> NetworkInterfacePermission; `errors: []`, because the EC2 Query protocol models no error shapes — so the status, code and message are taken from the recording and not from the model. The recordings show AWS refusing a grant whose grantee is an ordinary account that is not enabled for cross-account ENI permissions: a non-2xx ClientError with error code `OperationNotPermitted` and the message `The account <AwsAccountId> is not permitted to receive cross account permissions.` The fixed text of that message is `The account ` and ` is not permitted to receive cross account permissions.`; the only variable part is the AwsAccountId copied from the request. The refusal leaves no state: DescribeNetworkInterfacePermissions filtered on `network-interface-permission.network-interface-id` = the ENI returns an empty NetworkInterfacePermissions list afterwards. The same recordings also pin the two id boundaries that sit behind this call: a well-formed but unknown NetworkInterfaceId is refused `InvalidNetworkInterfaceID.NotFound` with `The interface ID '<NetworkInterfaceId>' does not exist` (fixed text `The interface ID '` / `' does not exist`, variable part the requested id), and DeleteNetworkInterfacePermission on an unknown permission id is refused `InvalidPermissionID.NotFound` with `The ENI permission ID '<NetworkInterfacePermissionId>' does not exist`.
Emulator: the call SUCCEEDS with HTTP 200 and mints the grant. The replay output shows `InterfacePermission` carrying a freshly minted NetworkInterfacePermissionId, the requested NetworkInterfaceId, the requested AwsAccountId, the requested Permission, and `PermissionState.State = GRANTED`. It does so for Permission `INSTANCE-ATTACH` (ec2.create-network-interface-permission, ec2.subnet-cidr-reservation-and-eni-permission) and for `EIP-ASSOCIATE` (ec2.op-contract-delete-network-interface-permission) alike — i.e. the grantee account is never evaluated at all. Note the knock-on: because the grant is accepted, all three checks stop at that first assertion, so the `InvalidNetworkInterfaceID.NotFound` and `InvalidPermissionID.NotFound` boundaries named above were never exercised and remain unverified behind this cause; a fix should bring them under test too.
Why: a client that relies on AWS refusing cross-account ENI grants to accounts that are not enabled for them sees the opposite — an authorization boundary that AWS enforces is silently open. Code written against the emulator will believe a grant exists (and that DescribeNetworkInterfacePermissions will list it), and negative-path code that expects to catch `OperationNotPermitted` never runs; against real AWS the same call raises, so the two environments take different branches.
Fix: the CreateNetworkInterfacePermission handler's request validation in the ec2 service crate (services/service-com-amazonaws-ec2-amazonec2) — the grantee check must run before any permission is minted, and must emit the `OperationNotPermitted` refusal with the message template above while leaving the permission store untouched.

--- Cause 2 of 3: an Auto Scaling warm-pool instance promoted to InService never leaves 'pending' in DescribeInstances ---
Failing check: ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves.
AWS: recorded behavior only (EC2 models InstanceState as a structure of Code/Name and does not model lifecycle timing). The recording shows that once a warm-pool instance has been promoted by a scale-out — the group lists exactly that instance id as the serving one, the group's Instances entry for it reports HealthStatus `Healthy`, and no other instance ever reached InService — DescribeInstances on that same instance id reports `State.Name = running` (and State.Code 16), with its root volume unchanged from the volume that was pre-initialised while the instance sat in the pool. That is the defining property of a warm pool: the promoted host is an already-booted host being started, not a cold launch.
Emulator: DescribeInstances on the promoted instance reports `State = {'Code': 0, 'Name': 'pending'}` instead, at the point where the Auto Scaling group has already declared it InService and Healthy. The two planes disagree about the same instance: the scaling plane says the host is serving, the EC2 plane says it has not finished starting.
Why: a client cannot tell when a scaled-out instance is usable. Any caller that gates work on DescribeInstances reporting `running` — a Run Command probe, an SSM registration wait, a health gate, an ELB registration — blocks forever or times out against the emulator while the identical code proceeds against AWS. It also makes the warm-pool contract unobservable: 'pending' is what a cold launch reports, so a client cannot distinguish a promotion from a fresh launch.
Fix: the instance-state transition in the ec2 service's instance state machine / its warm-pool promotion path — promoting a pooled (stopped) instance must drive its EC2 instance state to `running` and keep its root volume, rather than leaving it parked at `pending`. (Related but distinct from the open issue 20261123T150355Z-ec2-the-instance-guest-has-no-date-program.md, which is about what the guest on a promoted instance can execute; that issue does not cover the instance state never advancing.)

--- Cause 3 of 3: DeleteIpamPrefixListResolverTarget on an already-deleted target answers IncorrectState instead of the not-found refusal ---
Failing check: ec2.op-contract-delete-ipam-prefix-list-resolver-target.
AWS: recorded behavior only. Smithy models DeleteIpamPrefixListResolverTarget with input DryRun + IpamPrefixListResolverTargetId (required) and output IpamPrefixListResolverTarget; `errors: []` (EC2 Query models no error shapes), so the code and message come from the recording. The recording shows that after the target has been deleted and is gone, deleting it again is refused with error code `InvalidIpamPrefixListResolverTargetId.NotFound` and the message `The ipam-prefix-list-resolver-target ID '<IpamPrefixListResolverTargetId>' does not exist` — fixed text `The ipam-prefix-list-resolver-target ID '` and `' does not exist`, with the requested id as the only variable part. The same recording pins the neighbouring behaviour that the emulator does get right and that must not regress: DryRun short-circuits with `DryRunOperation` / `Request would have succeeded, but DryRun flag is set.` and leaves the target in state `create-complete`.
Emulator: the second delete is refused with error code `IncorrectState` — the wrong refusal class entirely. A vanished resource is reported as a resource in the wrong state.
Why: a client cannot make delete idempotent. The standard pattern — call delete, swallow the resource's `*.NotFound` code, re-raise anything else — re-raises against the emulator, so cleanup and teardown paths that are correct against AWS fail. `IncorrectState` also reads as retryable (wait and try again), so a caller may loop on a target that will never come back.
Fix: the DeleteIpamPrefixListResolverTarget handler's lookup/validation in the ec2 service crate — an id that resolves to no target must take the not-found branch with the code and message template above, ahead of any state check.

## Evidence
Cause 1 of 3 — CreateNetworkInterfacePermission.
Expected (AWS, from the recording): a non-2xx refusal, error code `OperationNotPermitted`, message `The account 111122223333 is not permitted to receive cross account permissions.` (the account number is the AwsAccountId from the request; the rest is fixed text), and DescribeNetworkInterfacePermissions filtered on network-interface-permission.network-interface-id returning [].
Actual (emulator): HTTP 200. ec2.create-network-interface-permission, Permission=INSTANCE-ATTACH: AssertionError: real AWS rejects this call, but it SUCCEEDED and returned {'InterfacePermission': {'NetworkInterfacePermissionId': 'eni-perm-...', 'NetworkInterfaceId': 'eni-...', 'AwsAccountId': '111122223333', 'Permission': 'INSTANCE-ATTACH', 'PermissionState': {'State': 'GRANTED'}}, ... 'HTTPStatusCode': 200 ...}.
ec2.op-contract-delete-network-interface-permission, Permission=EIP-ASSOCIATE: AssertionError: expected a rejection, but the call SUCCEEDED and returned {'InterfacePermission': {..., 'Permission': 'EIP-ASSOCIATE', 'PermissionState': {'State': 'GRANTED'}}, ... 'HTTPStatusCode': 200 ...}.
ec2.subnet-cidr-reservation-and-eni-permission: AssertionError: expected the grant to an unauthorized account to be refused, but it SUCCEEDED and returned {'InterfacePermission': {..., 'Permission': 'INSTANCE-ATTACH', 'PermissionState': {'State': 'GRANTED'}}, ... 'HTTPStatusCode': 200 ...}. (Resource ids, request ids and dates in these payloads are per-run values, not requirements.)
Unreached behind this cause: CreateNetworkInterfacePermission with an unknown NetworkInterfaceId must answer `InvalidNetworkInterfaceID.NotFound` / `The interface ID '<id>' does not exist`; DeleteNetworkInterfacePermission with an unknown permission id must answer `InvalidPermissionID.NotFound` / `The ENI permission ID '<id>' does not exist`.

Cause 2 of 3 — warm-pool promotion.
Expected (AWS, from the recording): with the group already reporting the warm instance as the only serving/InService instance and its group record HealthStatus=Healthy, DescribeInstances on that instance reports State.Name == 'running' and its root volume is the volume pre-initialised in the pool.
Actual (emulator): AssertionError: the promoted instance must be running, got {'Code': 0, 'Name': 'pending'}.

Cause 3 of 3 — DeleteIpamPrefixListResolverTarget.
Expected (AWS, from the recording): re-deleting the already-deleted target is refused with code `InvalidIpamPrefixListResolverTargetId.NotFound` and message `The ipam-prefix-list-resolver-target ID '<IpamPrefixListResolverTargetId>' does not exist`.
Actual (emulator): AssertionError: unexpected code re-deleting 'IncorrectState'. (The DryRun leg of the same check — `DryRunOperation` / `Request would have succeeded, but DryRun flag is set.` with the target left at state `create-complete` — passed and must keep passing.)

## Steps to Reproduce
Run the behavioral suite (suite simparity-behavioral-aws-v1) against the emulator and replay the five checks listed in trace_ids.

Cause 1 of 3 (ec2.create-network-interface-permission, ec2.op-contract-delete-network-interface-permission, ec2.subnet-cidr-reservation-and-eni-permission):
1. CreateNetworkInterface in a subnet of the run's network; note the ENI id.
2. CreateNetworkInterfacePermission(NetworkInterfaceId=<that ENI>, AwsAccountId="111122223333", Permission="INSTANCE-ATTACH") — and separately with Permission="EIP-ASSOCIATE". AWS refuses OperationNotPermitted with the message template above; the emulator answers 200 with PermissionState.State=GRANTED.
3. DescribeNetworkInterfacePermissions(Filters=[{Name:"network-interface-permission.network-interface-id", Values:[<ENI>]}]) must be empty after the refusal.
4. Then CreateNetworkInterfacePermission with a well-formed unknown ENI id (e.g. eni-00000000000000000) -> InvalidNetworkInterfaceID.NotFound / "The interface ID '<id>' does not exist"; and DeleteNetworkInterfacePermission with an unknown permission id (e.g. eni-perm-00000000000000000) -> InvalidPermissionID.NotFound / "The ENI permission ID '<id>' does not exist".

Cause 2 of 3 (ec2.dasgrefresh-run-command-to-a-stopped-warm-pool-instance-rejected-then-that-same-guest-serves):
1. Create an Auto Scaling group with a warm pool and let one instance pre-initialise and stop in the pool.
2. Scale the group out so that pooled instance is promoted; wait until the group reports exactly that instance id as InService with HealthStatus Healthy.
3. DescribeInstances on that same instance id. AWS reports State.Name == 'running'; the emulator reports {'Code': 0, 'Name': 'pending'}.

Cause 3 of 3 (ec2.op-contract-delete-ipam-prefix-list-resolver-target):
1. Create an IPAM prefix-list resolver target and wait for state create-complete.
2. DeleteIpamPrefixListResolverTarget(..., DryRun=True) -> DryRunOperation, target still create-complete (passes today).
3. DeleteIpamPrefixListResolverTarget(...) for real; poll until the target is gone.
4. DeleteIpamPrefixListResolverTarget(...) again. AWS refuses InvalidIpamPrefixListResolverTargetId.NotFound with "The ipam-prefix-list-resolver-target ID '<id>' does not exist"; the emulator refuses IncorrectState.
