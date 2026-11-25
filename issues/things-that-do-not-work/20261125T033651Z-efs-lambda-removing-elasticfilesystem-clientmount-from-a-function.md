Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: efs__lambda.deep-lambda-2-efs-mount-shared-between-two-functions-and-clientmount-denied
Issue-Key: efs__lambda.deep-lambda-2-efs-mount-shared-between-two-functions-and-clientmount-denied|_poll#10|TimeoutError|raise TimeoutError(f"{what}: the run budget ended first; not graded")
Issue-Commit: 7ce46e2ec7daea03f5b998d8aff6c247aac00dc8

# efs__lambda: removing elasticfilesystem:ClientMount from a function's execution role never makes its cold Invoke fail with EFSMountFailureException (403), so the mount-authorization boundary reaches no verdict and the check burns its whole budget

> _Note: this issue was identified against the code base at commit `7ce46e2ec7daea03f5b998d8aff6c247aac00dc8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout left one surviving, unowned root cause in the cross-service efs+lambda group (service token `efs__lambda`). THIS ISSUE HAS 1 CAUSE.

This is net-new work relative to the resolved issue 20261110T174333Z-efs-lambda-2-net-new-causes-an-efs.md, whose replay key for this same check was `_invoke#10` (the handler could not touch the mount at all). That part is now fixed and the check demonstrably gets much further: every phase-1 assertion passes (function A's Invoke returns 200 with no FunctionError, ismount true, uid/gid = the access point's PosixUser, and a DIFFERENT function B reads back the identical bytes with tmp_has_it false). The check now dies in phase 2, at the IAM-authorization leg of the mount. Two older open issues, 20261023T153329Z-efs-lambda-the-shared-efs-mount-check-never.md and 20261024T012650Z-efs-lambda-the-shared-efs-access-point-lambda.md, describe this scenario only as "a poll loop never converges" from a time when the mount itself did not exist; neither carries this check's current failure point, and the replay report does not mark this failure as owned by either.

Cause 1 of 1: the EFS mount an execution environment performs is not authorized against the function's CURRENT execution-role permissions, so dropping elasticfilesystem:ClientMount from one of the two roles produces no refusal and the scenario can never observe its negative case.

AWS: Smithy (services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json) models Invoke error EFSMountFailureException with errorKind client and httpError 403, documented "The Lambda function couldn't mount the configured file system due to a permission or configuration issue." FileSystemConfig{Arn required, @pattern ^arn:aws[a-zA-Z-]*:elasticfilesystem:<region>:<account>:access-point/fsap-[a-f0-9]{17}$; LocalMountPath required, @pattern ^/mnt/[a-zA-Z0-9-_.]+$}. Behaviourally the recording shows: with the file system policy delegating ClientMount/ClientWrite to the account root, the function's own identity policy decides the mount. When elasticfilesystem:ClientMount is removed from role B (ClientWrite kept) and B's next execution environment is built cold, B's Invoke is REFUSED with HTTP 403, Error.Code `EFSMountFailureException` and Error.Message `The function couldn't mount the Amazon EFS file system with access point <access-point-arn>.` — that sentence is AWS's fixed text and the only variable part is the access point ARN taken from the function's FileSystemConfigs[].Arn, not a value from any one run. Function A, cold in the same window with ClientMount intact, still mounts and reads; restoring the grant to B heals it. A cold environment is forced by UpdateFunctionConfiguration (Environment change), after which GetFunctionConfiguration must leave LastUpdateStatus `InProgress` for a terminal `Successful` or `Failed` (Smithy enum LastUpdateStatus = Successful | Failed | InProgress).

Emulator: the refusal never arrives. The check's refusal loop (`_poll(_b_cold_read, lambda r: r[0] != "ok", timeout_s=min(300, remaining))`) spins until the run budget is exhausted and the check is reported as `[error] ... TimeoutError: <fn-b> update to settle: the run budget ended first; not graded`, surfacing from the `get_function_configuration` poll inside `_cold`. Because `_invoke` converts any ClientError into a non-ok tuple and the loop only exits when B is NOT ok, a B that keeps mounting and reading successfully after ClientMount was revoked is indistinguishable from "not yet" and the loop cannot terminate. Hypothesis: the stall is the missing authorization — the emulator builds B's mount from the access point alone and never evaluates the execution role's elasticfilesystem:ClientMount, so B is still ok on every retry. The one alternative consistent with the same traceback is that UpdateFunctionConfiguration/GetFunctionConfiguration never moves LastUpdateStatus off a non-terminal value (or omits the member), in which case `_cold` itself never returns; both are emulator-side and both must be closed for this check to reach a verdict, so verify both when fixing.

Why: the EFS mount permission boundary is unenforced and unobservable. A client that relies on Lambda to refuse a function whose role lacks elasticfilesystem:ClientMount gets a function that mounts and reads the file system anyway — over-permissive, and exactly the opposite of the security outcome AWS gives. There is also no error code to branch on: instead of a classifiable 403 EFSMountFailureException, the caller gets a successful invoke, so negative tests of the mount-authorization rule pass where they must fail. Any caller polling for a configuration update to settle also hangs rather than succeeding or failing.

Fix: the Invoke handler's execution-environment build, in the component that materialises a FileSystemConfigs entry. The mount must be authorized at environment-build time against the function's execution role (combined with the EFS file-system policy), and refused with EFSMountFailureException at HTTP 403 and the message above when elasticfilesystem:ClientMount is not granted; a role change must take effect on the next cold environment and not on a reused warm one. Alongside it, the UpdateFunctionConfiguration handler and function state must drive LastUpdateStatus from InProgress to a terminal Successful/Failed so a cold start can be forced and awaited.

Traces: efs__lambda.deep-lambda-2-efs-mount-shared-between-two-functions-and-clientmount-denied.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 34/40 passed (85.0%), bootstrap_signal=false — the emulator was serving (efs 27/28, and the ec2/efs/iam/lambda calls on this check's whole setup path were answered). Per-service: efs__lambda 3/4 (75%), 1 failed.

The failure, verbatim from the report:

  [error] efs__lambda.deep-lambda-2-efs-mount-shared-between-two-functions-and-clientmount-denied
  Error: File "check_efs__lambda_deep_lambda_2_efs_mount_shared_between_two_functions_and_clientmount_denied.py", line 347, in _cold
  _poll(lambda: lam.get_function_configuration(FunctionName=fn_name),
  File "check_efs__lambda_deep_lambda_2_efs_mount_shared_between_two_functions_and_clientmount_denied.py", line 126, in _poll
  raise TimeoutError(f"{what}: the run budget ended first; not graded")
  TimeoutError: sim2real-xsvc-efsmnt-fn-b-<id> update to settle: the run budget ended first; not graded

What that locates: there is no AssertionError, so no phase-2 assertion was ever reached — but there is no phase-1 AssertionError either, which means every phase-1 assertion PASSED (A's write returned ok/ismount/uid=gid=1001 with the right sha256, B read the identical bytes, B did not see A's /tmp). Execution therefore reached `_b_cold_read`, which is only called from the two phase-2/3 refusal loops. `_cold` is reached only after `update_function_configuration` on fn-b returned without a ClientError (the ClientError branch would have returned an ('update-refused', ...) tuple instead).

Expected (AWS, as the check encodes the recording): after role B's inline policy is replaced with elasticfilesystem:ClientWrite only, B's next cold Invoke raises a ClientError with Error.Code 'EFSMountFailureException', HTTPStatusCode 403 and Error.Message 'The function couldn't mount the Amazon EFS file system with access point <access-point-arn>.'; a cold function A still reads A's bytes in the same window; restoring ClientMount to role B makes B read those bytes again.

Actual (emulator): none of those three outcomes is produced. The refusal loop `_poll(_b_cold_read, lambda r: r[0] != "ok", ctx=ctx, timeout_s=_budget_s(ctx, 300), interval_s=2)` never satisfies its predicate and runs until ctx.time_left() <= 6 s, at which point `_budget_s(ctx, 300)` collapses to 1.0 s and the nested `_cold` poll raises the budget TimeoutError shown above. The two emulator behaviours that can produce exactly this are (a) B's cold Invoke keeps returning ('ok', payload) because the mount is not authorized against the execution role, and (b) GetFunctionConfiguration never reports LastUpdateStatus in ('Successful','Failed') after UpdateFunctionConfiguration.

Models consulted: services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json — Invoke errors include EFSMountFailureException (errorKind client, httpError 403, doc "The Lambda function couldn't mount the configured file system due to a permission or configuration issue."); FileSystemConfig{Arn required @pattern access-point/fsap-[a-f0-9]{17}, LocalMountPath required @pattern ^/mnt/[a-zA-Z0-9-_.]+$}; enum LastUpdateStatus = Successful | Failed | InProgress on the FunctionConfiguration output of both UpdateFunctionConfiguration and GetFunctionConfiguration.

Generated VPC/subnet/security-group ids, the file-system, mount-target and access-point ids, function and role names, the random 4096-byte payload and its sha256, hosts, ports and timestamps are environment-specific and are NOT requirements. The requirements are: HTTP 403, Error.Code 'EFSMountFailureException', the message sentence with the request's own access point ARN substituted, the differential outcome between role A and role B in the same window, and a terminal LastUpdateStatus after a configuration update.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout.
2. Replay simparity-behavioral-aws-v1 (run_behavioral_suite) and look at service group efs__lambda: deep-lambda-2-efs-mount-shared-between-two-functions-and-clientmount-denied reports the budget TimeoutError from inside _cold with no assertion verdict.
3. Reproduce by hand (this is the same order the check uses, and every step up to 8 currently succeeds):
   a. ec2.create_vpc(CidrBlock='10.77.0.0/16'); wait for the vpc_available waiter; modify_vpc_attribute(EnableDnsHostnames=True); create two subnets in two AZs; create a security group and authorize TCP 2049 ingress from itself.
   b. efs.create_file_system(Encrypted=True) and poll DescribeFileSystems until LifeCycleState='available'.
   c. efs.create_mount_target in each subnet with that security group; poll until BOTH report 'available'.
   d. efs.put_file_system_policy granting elasticfilesystem:ClientMount+ClientWrite to arn:aws:iam::<account>:root on the file-system ARN (so the identity policy alone decides the mount).
   e. efs.create_access_point(PosixUser={'Uid':1001,'Gid':1001}, RootDirectory={'Path':'/export/lambda','CreationInfo':{'OwnerUid':1001,'OwnerGid':1001,'Permissions':'750'}}); poll until 'available'.
   f. iam.create_role twice (role A, role B) with the lambda.amazonaws.com trust policy plus the three ec2 network-interface actions, and put_role_policy on each granting elasticfilesystem:ClientMount AND ClientWrite on the file-system ARN.
   g. lambda.create_function twice with VpcConfig={'SubnetIds':[both],'SecurityGroupIds':[sg]} and FileSystemConfigs=[{'Arn': <access point arn>, 'LocalMountPath': '/mnt/shared'}]; wait for State='Active'.
   h. Invoke A to write a file under /mnt/shared, then Invoke B to read it: both succeed today.
   i. iam.put_role_policy on role B with elasticfilesystem:ClientWrite ONLY (ClientMount removed).
   j. lambda.update_function_configuration(FunctionName=B, Environment={'Variables':{'NONCE':'1'}}) to force a new execution environment, then poll lambda.get_function_configuration(FunctionName=B) for LastUpdateStatus in ('Successful','Failed').
      -> Check here whether LastUpdateStatus ever reaches a terminal value; if it does not, that alone is one of the two defects.
   k. lambda.invoke(FunctionName=B, InvocationType='RequestResponse') with a read event.
      -> Observe that it still returns StatusCode 200 with the file's bytes, instead of raising a ClientError with Error.Code 'EFSMountFailureException', HTTPStatusCode 403 and Error.Message "The function couldn't mount the Amazon EFS file system with access point <access point arn>."
   l. Force a cold start on A the same way and invoke A: it must still read the bytes (the differential control).
   m. Restore ClientMount on role B, force a cold start, and invoke B: it must read A's bytes again.
