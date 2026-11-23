Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__efs.deploy-cfn-efs-access-point-and-mount-target-round-trip-and-delete
Issue-Key: cloudformation__efs.deploy-cfn-efs-access-point-and-mount-target-round-trip-and-delete|run#21|AssertionError|assert ap["PosixUser"] == {"Uid": 1500, "Gid": 1500, "SecondaryGids": []}, (
Issue-Commit: 74fb065dbfcdabb80a136d7542fc7487ef053384

# cloudformation__efs: an access point created by AWS::EFS::AccessPoint reports PosixUser without SecondaryGids, where AWS reports it as an empty list

> _Note: this issue was identified against the code base at commit `74fb065dbfcdabb80a136d7542fc7487ef053384`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAccessPoints

## Description
One net-new cause (Cause 1 of 1) in the EFS access-point representation reached through a CloudFormation stack.

Cause 1 of 1: DescribeAccessPoints (and the shared AccessPointDescription projection CreateAccessPoint returns) drops PosixUser$SecondaryGids for an access point whose creating request named no secondary group, where the recorded AWS response carries the member as an empty list. Failing check: cloudformation__efs.deploy-cfn-efs-access-point-and-mount-target-round-trip-and-delete.

AWS: Smithy (services/service-com-amazonaws-efs-magnolioapiservice-v20150201/smithy-model/com-amazonaws-efs-magnolioapiservice-v20150201.json): AccessPointDescription$PosixUser targets com.amazonaws.efs#PosixUser with members Uid (@required, long, @range 0..4294967295), Gid (@required, long, @range 0..4294967295) and SecondaryGids (NOT required, target com.amazonaws.efs#SecondaryGids, @length min 0 max 16, no @default); AccessPointDescription is the element type of DescribeAccessPointsResponse$AccessPoints and is also what CreateAccessPoint returns. The model therefore does not settle whether an unset SecondaryGids is omitted or reported empty — this is recorded behavior only: the recording this check encodes shows that for an access point created with a PosixUser that carries Uid and Gid only (here through a CloudFormation AWS::EFS::AccessPoint whose PosixUser names Uid and Gid and no SecondaryGids), DescribeAccessPoints answers HTTP 200 with PosixUser = {Uid: <the Uid the request named>, Gid: <the Gid the request named>, SecondaryGids: []} — the member PRESENT and empty, with the Uid/Gid values taken from the request and the empty list being the fixed part. No error class is involved: the call succeeds on both sides.

Emulator: HTTP 200, but PosixUser omits the member entirely. Replay reports `AssertionError: expected PosixUser {'Uid': 1500, 'Gid': 1500, 'SecondaryGids': []}, got {'Uid': 1500, 'Gid': 1500}` (the 1500 values are the template's Uid/Gid for this run and fill the template; the requirement is that SecondaryGids is present as an empty list when the access point has no secondary groups). The omission is deliberate in the current code: services/service-com-amazonaws-efs-magnolioapiservice-v20150201/src/ops/create_access_point.rs builds the stored identity with crate::state::posix_identity and its comment states that a request naming no secondary group "stored no such list, and the member is left out of every read of the access point rather than reported as an empty array".

Why: a client that reads the described POSIX identity back — an SDK that models SecondaryGids as a list and indexes or iterates it, a drift/idempotency comparison that diffs the described PosixUser against the desired one, or a CloudFormation/Terraform-style reconciler that round-trips the description into an update — sees a PosixUser shaped differently from AWS's. Comparisons against the AWS-shaped value report a spurious difference and never converge, and code that reads SecondaryGids unconditionally fails on an absent key.

NOTE for the fixer — direct conflict with an open issue: open issue 20261123T010712Z-elasticfilesystem-efs-access-points-emit-posixuser-secondarygids-as-an.md ("EFS access points emit PosixUser.SecondaryGids as an empty array instead of omitting the unset member", wire corpus, fixture efs-access-point-lifecycle) asserts the OPPOSITE requirement for the same member on CreateAccessPoint/DescribeAccessPoints. That issue does not cover this check (the behavioral replay reports this check as unowned), and the emulator today already omits the member, i.e. it already behaves the way that issue asks for while diverging from the behavior this behavioral recording captured. The two cannot both be satisfied, so reconcile them against the recordings before changing the serializer: the recording behind this check shows SecondaryGids present and empty for an access point created without secondary groups.

Fix: the EFS access-point state and the shared AccessPointDescription projection in the EFS service crate — crate::state::posix_identity / AccessPoint::describe, used by the CreateAccessPoint handler and by the DescribeAccessPoints read path — must report PosixUser$SecondaryGids as an empty list for an access point that has no secondary groups, and must still report the requested list when one was supplied. Nothing in the CloudFormation AWS::EFS::AccessPoint handler is implicated beyond its call into EFS: every other assertion of this check (stack CREATE_COMPLETE, Ref/GetAtt against DescribeAccessPoints, RootDirectory and CreationInfo) passed before this one.

Traces: cloudformation__efs.deploy-cfn-efs-access-point-and-mount-target-round-trip-and-delete (behavioral suite simparity-behavioral-aws-v1, verdict mismatch).

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 1252/2378 passed; the group cloudformation__efs shows 3 failed, of which two are owned by 20261123T000847Z-cloudformation-efs-a-filesystempolicy-statement-that-omits-resource.md and this one is unowned.

Failing check: [mismatch] cloudformation__efs.deploy-cfn-efs-access-point-and-mount-target-round-trip-and-delete

Reported detail (tail):
  File "check_cloudformation__efs_deploy_cfn_efs_access_point_and_mount_target_round_trip_and_delete.py", line 242, in run
    assert ap["PosixUser"] == {"Uid": 1500, "Gid": 1500, "SecondaryGids": []}
  AssertionError: expected PosixUser {'Uid': 1500, 'Gid': 1500, 'SecondaryGids': []}, got {'Uid': 1500, 'Gid': 1500}

Expected (AWS, as recorded): DescribeAccessPoints -> HTTP 200, AccessPoints[0].PosixUser = {"Uid": <request Uid>, "Gid": <request Gid>, "SecondaryGids": []}.
Actual (emulator): DescribeAccessPoints -> HTTP 200, AccessPoints[0].PosixUser = {"Uid": <request Uid>, "Gid": <request Gid>} — the SecondaryGids key is absent.

Stack template fragment the check deploys (values are per-run; the shape is the requirement):
  Ap:
    Type: AWS::EFS::AccessPoint
    Properties:
      FileSystemId: !Ref Fs
      PosixUser: { Uid: "1500", Gid: "1500" }        # no SecondaryGids member
      RootDirectory: { Path: /sim2real-ap, CreationInfo: { OwnerUid: "1500", OwnerGid: "1500", Permissions: "0750" } }

Model evidence (smithy_query, services/service-com-amazonaws-efs-magnolioapiservice-v20150201/smithy-model/com-amazonaws-efs-magnolioapiservice-v20150201.json):
  com.amazonaws.efs#PosixUser members = Uid (@required), Gid (@required), SecondaryGids (optional list, @length 0..16, no @default).
  com.amazonaws.efs#AccessPointDescription$PosixUser is optional and is the element shape of DescribeAccessPoints' AccessPoints list.

Emulator code evidence: services/service-com-amazonaws-efs-magnolioapiservice-v20150201/src/ops/create_access_point.rs lines 358-363 store the identity through crate::state::posix_identity and document that an unnamed SecondaryGids "is left out of every read of the access point rather than reported as an empty array".

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, and run the behavioral suite simparity-behavioral-aws-v1.
2. Replay the single check cloudformation__efs.deploy-cfn-efs-access-point-and-mount-target-round-trip-and-delete. It CreateStacks a template with an AWS::EFS::FileSystem (Encrypted: true), an AWS::EFS::AccessPoint whose PosixUser names only Uid and Gid, a RootDirectory with Path and CreationInfo, and an AWS::EFS::MountTarget, and waits for CREATE_COMPLETE.
3. Call efs DescribeAccessPoints(FileSystemId=<the stack's file system id>) and inspect AccessPoints[0].PosixUser in the HTTP 200 body: the emulator returns {"Uid": <Uid>, "Gid": <Gid>} with no SecondaryGids key; AWS returns {"Uid": <Uid>, "Gid": <Gid>, "SecondaryGids": []}.
4. The same projection is returned by CreateAccessPoint, so a direct efs CreateAccessPoint(FileSystemId=..., ClientToken=..., PosixUser={Uid, Gid}) reproduces it without CloudFormation.
5. Fixed when the described (and created) access point reports SecondaryGids as an empty list for an access point with no secondary groups, and still reports the supplied list when the request names one — and when that choice is reconciled with open issue 20261123T010712Z-elasticfilesystem-efs-access-points-emit-posixuser-secondarygids-as-an.md, which demands the member be omitted.
