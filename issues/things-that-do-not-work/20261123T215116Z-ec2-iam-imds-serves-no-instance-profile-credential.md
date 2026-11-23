Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam.instance-profile-role-swap-changes-the-credentials-imds-serves
Issue-Key: ec2__iam.instance-profile-role-swap-changes-the-credentials-imds-serves|run#13|AssertionError|assert first["code"] == "Success", (
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# ec2__iam: IMDS serves no instance-profile credential document — the credentials endpoint returns an empty body, so Code is '' instead of 'Success' and an instance-profile role swap is unobservable

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateIamInstanceProfile

## Description
Cause 1 of 1: the instance metadata service inside a launched guest serves nothing for the instance-profile credential document, so the guest never obtains role credentials and a role swap on the profile can never be observed.
  AWS: recorded behavior only — IMDS is an in-guest HTTP endpoint (169.254.169.254) and is not Smithy-modelled. Smithy grounding for the control plane: EC2 `AssociateIamInstanceProfile` / `ReplaceIamInstanceProfileAssociation` / `DisassociateIamInstanceProfile` bind an `IamInstanceProfileSpecification` to an instance, and IAM `AddRoleToInstanceProfile` / `RemoveRoleFromInstanceProfile` change which role a profile carries. AWS documents (https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/iam-roles-for-amazon-ec2.html) that `GET /latest/meta-data/iam/security-credentials/<role-name>` returns a JSON document whose fixed fields are `Code`, `LastUpdated`, `Type`, `AccessKeyId`, `SecretAccessKey`, `Token`, `Expiration`; `Code` is the literal string `Success` on a successful fetch, `Type` is `AWS-HMAC`, and `AccessKeyId` is a temporary-credential key in the `ASIA` class. The role name in the path and the key/token values come from the instance's current profile and from state — they are not fixed text.
  Emulator: the document the guest reads back is empty. The check reports: "the credentials document must report Code 'Success', got ''" — i.e. no `Code`, no `AccessKeyId`, no `Token`. Because no credential is ever served, the second half of the behavior (swapping the profile's role must change which credentials IMDS serves, and therefore which S3 prefix the guest may write) is never reached.
  Why: any client that relies on instance-profile credentials — the AWS CLI/SDK default credential chain inside an EC2 instance — gets no credentials at all. Nothing on an instance can call AWS as its role, and an operator who swaps the role on a profile has no way to confirm the instance picked up the new role's permissions.
  Fix: the EC2 instance guest's IMDS handler for the `iam/security-credentials` tree, and its binding to the instance's current IamInstanceProfile association (so a role swap re-mints the served document).
  Traces: ec2__iam.instance-profile-role-swap-changes-the-credentials-imds-serves.

Note: the open issues 20261114T203825Z-ec2-iam-an-aws-runshellscript-imds-probe-never.md and 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md describe neighbouring in-guest/IMDS defects; the replay report does not mark this check as owned by either.

## Evidence
ec2__iam.instance-profile-role-swap-changes-the-credentials-imds-serves [mismatch]
  Expected: after the instance is Online, an in-guest IMDSv2 read of /latest/meta-data/iam/security-credentials/<role> returns a JSON document with Code='Success', Type='AWS-HMAC' and an AccessKeyId in the ASIA class; swapping the role on the instance profile then changes which role's credentials are served, inverting which S3 prefix the guest may PutObject (the other prefix being refused AccessDenied with STS session principal arn:aws:sts::<account>:assumed-role/<role>/<instance-id>).
  Actual: AssertionError: "the credentials document must report Code 'Success', got ''" — the credentials document read from IMDS is empty, so no credential is served and the role-swap arm is never exercised.

## Steps to Reproduce
Create a VPC 10.85.0.0/16 with a subnet and internet gateway; create an IAM role with AmazonSSMManagedInstanceCore plus a prefix-scoped S3 write policy and an instance profile for it; launch an AL2023 t3.micro with that instance profile; wait for DescribeInstanceInformation PingStatus 'Online'. In the guest, fetch an IMDSv2 token (PUT /latest/api/token with X-aws-ec2-metadata-token-ttl-seconds) and GET /latest/meta-data/iam/security-credentials/<role-name>. The emulator returns an empty body; AWS returns the credential JSON with Code='Success' and an ASIA-class AccessKeyId.
