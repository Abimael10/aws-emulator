Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__sts.instance-profile-session-userid-is-the-role-id-and-instance-id-composite
Issue-Key: ec2__sts.instance-profile-session-userid-is-the-role-id-and-instance-id-composite|run#11|AssertionError|assert probe["userid"] == expected_userid, (
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# ec2__sts: a guest's own GetCallerIdentity returns nothing, so an instance-profile session's aws:userid (<role-id>:<instance-id>) is never presented

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetCallerIdentity

## Description
Cause 1 of 1: an instance-profile session's identity cannot be read from inside the guest — the guest's own GetCallerIdentity produces no result at all — so the composite aws:userid that AWS presents for such a session is never served, and any policy keyed on it is unenforceable.
  AWS: Smithy: STS `GetCallerIdentity` returns `UserId`, `Account` and `Arn`. For a session obtained from an EC2 instance profile, AWS sets `UserId` to the composite `<role-id>:<ec2-instance-id>` — the role's `RoleId` (the AROA-class id IAM `GetRole`/`CreateRole` returns) joined by a single colon to the instance id of the instance the credentials were issued to — and `Arn` to `arn:aws:sts::<account>:assumed-role/<role-name>/<ec2-instance-id>`. That `<role-id>:<instance-id>` composite is also the value `aws:userid` takes in an IAM/resource policy condition, which is what lets a policy pin an action to one specific instance. The role id and instance id are values from state; the colon-joined composite shape is the requirement.
  Emulator: the guest's own GetCallerIdentity reports '' — no UserId at all. The check reports: "an instance-profile session must present aws:userid as <role-id>:<ec2-instance-id> = 'AROA…:i-…'; the guest's own GetCallerIdentity reported ''" (the AROA id and instance id there are that run's values, not fixed text). Because no identity is returned, the twin-statement arm — one statement conditioned on the real instance id and its decoy conditioned on a different one, which must give opposite verdicts — is never measured.
  Why: a client cannot discover what identity its instance is acting as, and the aws:userid condition key that scopes a permission to exactly one instance is not honoured. A policy written to admit only one instance either admits everything or nothing, with no way to tell which.
  Fix: the instance-profile session's identity projection — STS GetCallerIdentity's UserId/Arn composition for a session minted from an EC2 instance profile — and the guest-side credential/CLI path that lets the instance make that call at all.
  Traces: ec2__sts.instance-profile-session-userid-is-the-role-id-and-instance-id-composite.

Note: the open issue 20261121T213740Z-ec2-sts-the-instance-profile-guest-has-no.md describes a neighbouring cause on this path (no `aws` CLI in the guest); the replay report does not mark this check as owned by it.

## Evidence
ec2__sts.instance-profile-session-userid-is-the-role-id-and-instance-id-composite [mismatch]
  Expected: GetCallerIdentity called from inside the instance returns UserId equal to <role-id>:<ec2-instance-id> (the role's AROA-class RoleId, a colon, then the launched instance's id), and a resource policy whose Condition pins aws:userid to that composite admits the instance while its twin statement naming a decoy instance id refuses it.
  Actual: AssertionError: "an instance-profile session must present aws:userid as <role-id>:<ec2-instance-id> = 'AROABGXEQES5NRCCJJWAN:i-00000000000000001'; the guest's own GetCallerIdentity reported ''" — the call from the guest yields no UserId, so the aws:userid condition arms are never measured. (The AROA id and instance id quoted are that run's state values; the requirement is the colon-joined composite.)

## Steps to Reproduce
Create a VPC 10.83.0.0/16 with a subnet and internet gateway; create an IAM role (note its RoleId from GetRole) with AmazonSSMManagedInstanceCore and an instance profile for it; launch an AL2023 t3.micro with that profile and wait for DescribeInstanceInformation PingStatus 'Online'. From inside the guest, call STS GetCallerIdentity using the instance-profile credentials and read UserId: the emulator returns nothing, where AWS returns <RoleId>:<instance-id>. Then grant with a policy whose Condition is StringLike on aws:userid = '<RoleId>:<instance-id>' and a twin naming a decoy instance id, and confirm the two give opposite verdicts.
