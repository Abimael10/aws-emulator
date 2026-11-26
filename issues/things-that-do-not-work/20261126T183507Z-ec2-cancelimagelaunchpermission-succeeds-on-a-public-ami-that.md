Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-cancel-image-launch-permission
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-cancel-image-launch-permission|run#11|AssertionError|raise AssertionError(
Issue-Commit: 4c2ddda98f023f570764c4b2cc687bfb0de98717

# ec2 CancelImageLaunchPermission succeeds on a public AMI that is not shared directly with the caller instead of refusing InvalidAMIID.NotFound

> _Note: this issue was identified against the code base at commit `4c2ddda98f023f570764c4b2cc687bfb0de98717`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CancelImageLaunchPermission

## Description
One net-new root cause.

Cause 1 of 1: CancelImageLaunchPermission resolves its ImageId with the ordinary "can this account see this image" predicate, so it accepts any publicly-visible AMI and answers 200 Return=true. AWS scopes this operation to AMIs the caller OWNS or that are shared DIRECTLY with the caller's account, and refuses everything else as not found. Failing check: ec2.op-contract-cancel-image-launch-permission.

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) — CancelImageLaunchPermission takes CancelImageLaunchPermissionRequest{ImageId (required, @ImageId), DryRun} and returns CancelImageLaunchPermissionResult{Return: Boolean}; the ec2Query protocol models no errors on the operation, so the refusal classification is recorded behavior only. The operation's own documentation scopes it to a share held by the caller: "Removes your Amazon Web Services account from the launch permissions for the specified AMI", and ImageId is documented as "The ID of the AMI that was shared with your Amazon Web Services account". Recorded behavior: for an AMI the caller neither owns nor holds a per-account launch permission on — a PUBLIC AMI, whose visibility comes from the `all` group and not from any per-account grant — AWS refuses the call. Recorded refusal: HTTP 400, error code InvalidAMIID.NotFound, message `The image id "<ImageId>" belongs to an AMI which does not exist or is not shared directly with your account.` In that message the fixed text is `The image id "` … `" belongs to an AMI which does not exist or is not shared directly with your account.`; the only variable part is the ImageId copied verbatim from the request (the recording's concrete ami-… value is one run's fill of that slot, not part of the requirement). The same check's positive control — grant the caller's own account a launch permission on an AMI it registered, cancel it, read the attribute back empty — passes against the emulator, so the ownership/direct-share condition is the whole divergence.

Emulator: the call is accepted. Replay shows HTTP 200 with body {'Return': True} for an ImageId resolved from the public AL2023 AMI parameter (/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64), which the calling account does not own and holds no direct launch-permission grant on. The handler resolves the image through the generic attribute-read predicate (require_resolvable_image), which admits any image the account can see, including a public one, and then returns Return=true because "an account that holds no grant on the image has nothing to take off". No ownership check and no "is there a per-account grant for the caller" check is performed, so the operation never produces InvalidAMIID.NotFound for a visible-but-not-directly-shared AMI. (Note: the resolved issue 20261023T102725Z-ec2-cancelimagelaunchpermission-stub.md stood this operation up; the call is no longer a stub and is not refused, so this is not a reopen of that issue — the implementation it delivered simply lacks this narrower resolution rule.)

Why: a client relying on AWS's behavior reads Return=true as proof that the AMI was one shared with its account and that the share has now been revoked. Against the emulator any public AMI id — including one from a different owner, or a typo'd id that happens to name a public image — is silently accepted, so (a) tooling that probes "is this AMI shared directly with me?" by calling CancelImageLaunchPermission and catching InvalidAMIID.NotFound concludes the opposite of the truth, (b) cleanup/un-share reconciliation loops believe they revoked a share that never existed and stop retrying or stop reporting the AMI as still-shared, and (c) negative-path tests and error handlers for InvalidAMIID.NotFound are never exercised, so a real deployment meets a 400 the client has no branch for.

Fix: the ImageId resolution in the CancelImageLaunchPermission handler (services/service-com-amazonaws-ec2-amazonec2/src/ops/cancel_image_launch_permission.rs). The operation needs its own resolution predicate rather than the shared attribute-read one: after the malformed-id check, the image must be one the calling account owns OR one carrying an explicit per-account launch permission for the calling account; a readable image that is merely public (visible through the `all` group only) must be refused with InvalidAMIID.NotFound and the message above, before DryRun is answered and before any revoke is written. The owner/grant lookup belongs against the image record and launch-permission set in crate::state (the same set ModifyImageAttribute writes and DescribeImageAttribute reports), so the refusal and the attribute cannot disagree.

Traces: ec2.op-contract-cancel-image-launch-permission (the only failing check in this suite not already owned by an open issue or blocked by an unimplemented-stub issue).

## Evidence
Check: ec2.op-contract-cancel-image-launch-permission [mismatch]

Expected (AWS): CancelImageLaunchPermission(ImageId=<public AL2023 AMI>) → HTTP 400, Error.Code = "InvalidAMIID.NotFound", Error.Message = 'The image id "<ImageId>" belongs to an AMI which does not exist or is not shared directly with your account.'

Actual (emulator): HTTP 200, body {'Return': True}. Replay assertion:
  AssertionError: expected CancelImageLaunchPermission on a public AMI never shared with the caller to be rejected as InvalidAMIID.NotFound, but it succeeded: {'Return': True, 'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'text/xml;charset=UTF-8', 'content-length': '203'}}}

Same check's positive control PASSES against the emulator: ModifyImageAttribute adds the caller's account to LaunchPermission on a self-registered AMI, DescribeImageAttribute(launchPermission) reports it, CancelImageLaunchPermission removes it, DescribeImageAttribute reports it gone. So only the not-owned / not-directly-shared refusal path diverges.

Source of the over-permissiveness, services/service-com-amazonaws-ec2-amazonec2/src/ops/cancel_image_launch_permission.rs:181 — `require_resolvable_image(&store, &input.image_id)?;` is the generic attribute-read resolver (admits any image the account can see, public included); lines 191-225 then revoke nothing and return Ok(Return=true) by design ("an account that holds no grant on the image has nothing to take off"). No owner-or-direct-grant gate exists between the two.

## Steps to Reproduce
1. Clone the repo and boot the emulator; replay the behavioral suite.
2. Resolve a public AMI id the calling account does not own:
   aws ssm get-parameter --name /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 --query Parameter.Value --output text
3. Confirm the account holds no per-account launch permission on it:
   aws ec2 describe-image-attribute --image-id <ami-id> --attribute launchPermission
4. Call the operation:
   aws ec2 cancel-image-launch-permission --image-id <ami-id>
   Emulator: HTTP 200, {"Return": true}.
   AWS: HTTP 400 InvalidAMIID.NotFound — 'The image id "<ami-id>" belongs to an AMI which does not exist or is not shared directly with your account.'
5. Control (must keep passing): register an AMI the caller owns, grant its own account a launch permission with modify-image-attribute, then cancel-image-launch-permission on it — this must still return Return=true and clear the grant.
6. Re-replay check ec2.op-contract-cancel-image-launch-permission to confirm the fix.
