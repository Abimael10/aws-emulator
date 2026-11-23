Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__s3__ssm.xsvc-ec2-3-imds-credentials-put-object-outside-role-policy-denied ec2__iam__s3__ssm.xsvc-ec2-3-imds-credentials-withdrawn-after-disassociate-iam-instance-profile
Issue-Key: ec2__iam__s3__ssm.xsvc-ec2-3-imds-credentials-put-object-outside-role-policy-denied|run#20|AssertionError|assert p1["code"] == "Success" and p1["akid_prefix"] == "ASIA", (
Issue-Key: ec2__iam__s3__ssm.xsvc-ec2-3-imds-credentials-withdrawn-after-disassociate-iam-instance-profile|run#17|AssertionError|assert p1["list_http"] == "200" and p1["listed_role"] == role, (
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# ec2__iam__s3__ssm: in-guest IMDS serves nothing for the instance-profile credential tree — the role listing answers with an empty status/body and the credential document has no Code or ASIA key

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateIamInstanceProfile

## Description
Cause 1 of 1: the guest's instance metadata service returns nothing for the `iam/security-credentials` tree — neither the role listing nor the credential document — so instance-profile credentials can never be used and neither their authorization scope nor their withdrawal on disassociation is observable. Two failing checks share this single cause.
  AWS: recorded behavior only — IMDS is an in-guest HTTP endpoint and is not Smithy-modelled. Smithy grounding for the control plane: EC2 `AssociateIamInstanceProfile` / `DisassociateIamInstanceProfile` bind and unbind an `IamInstanceProfileSpecification`. AWS documents (https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/iam-roles-for-amazon-ec2.html) that while a profile is associated, `GET /latest/meta-data/iam/security-credentials/` answers HTTP 200 with the associated role's name as its body, and `GET /latest/meta-data/iam/security-credentials/<role>` answers 200 with a JSON document whose `Code` is the literal `Success`, whose `Type` is `AWS-HMAC` and whose `AccessKeyId` is a temporary-credential key in the `ASIA` class; after DisassociateIamInstanceProfile the same listing stops naming the role. The role name and key/token values are taken from the instance's association and from state; only `Success`, `AWS-HMAC` and the `ASIA` class are fixed.
  Emulator: both reads come back empty. The role-listing read reports `http='' body=''` — not even an HTTP status — and the credential document reports `code='' prefix=''`. Because no credential is ever served, neither the positive arm (PutObject allowed exactly where the role's policy allows) nor the negative arms (AccessDenied for the other bucket, AccessDenied once the bucket policy denies the role, and credentials withdrawn after disassociation) is ever measured.
  Why: the SDK/CLI default credential chain inside an instance resolves to nothing, so no in-instance workload can call AWS as its role. An operator also gets no signal that removing an instance profile actually revokes the instance's credentials — the security-relevant half of the behavior.
  Fix: the EC2 instance guest IMDS handler for the `iam/security-credentials` tree (both the role listing and the credential document), and its binding to the live IamInstanceProfile association so that disassociation stops serving the role.
  Traces: ec2__iam__s3__ssm.xsvc-ec2-3-imds-credentials-put-object-outside-role-policy-denied, ec2__iam__s3__ssm.xsvc-ec2-3-imds-credentials-withdrawn-after-disassociate-iam-instance-profile.

Note: the open issues 20261114T203825Z-ec2-iam-s3-ssm-in-guest-imds-credential.md and 20261114T143448Z-ec2-iam-s3-ssm-imds-role-credential-probes-never-complete-the-ssm.md describe neighbouring in-guest IMDS defects; the replay report does not mark either of these checks as owned by them.

## Evidence
ec2__iam__s3__ssm.xsvc-ec2-3-imds-credentials-put-object-outside-role-policy-denied [mismatch]
  Expected: IMDS in the guest serves temporary credentials — credential document Code='Success' with an AccessKeyId in the ASIA class — and those credentials may PutObject only where the role allows; the other bucket is AccessDenied, and so is the permitted one once its bucket policy explicitly denies the role.
  Actual: AssertionError: "IMDS must serve temporary credentials (Code Success, ASIA key); got code='' prefix=''".

ec2__iam__s3__ssm.xsvc-ec2-3-imds-credentials-withdrawn-after-disassociate-iam-instance-profile [mismatch]
  Expected: before DisassociateIamInstanceProfile, GET /latest/meta-data/iam/security-credentials/ answers HTTP 200 with the associated role's name as the body; after disassociation the role is no longer served.
  Actual: AssertionError: "before disassociation IMDS must list '<role-name>'; got http='' body=''" — no HTTP status and no body at all, so the withdrawal arm is never reached.

## Steps to Reproduce
Create a VPC (10.93.0.0/16 or 10.94.0.0/16) with a subnet and internet gateway; create an IAM role with AmazonSSMManagedInstanceCore plus a prefix-scoped S3 policy and an instance profile; launch an AL2023 t3.micro with that profile and wait for DescribeInstanceInformation PingStatus 'Online'. In the guest, PUT /latest/api/token then (a) GET /latest/meta-data/iam/security-credentials/ — the emulator answers with no HTTP status and an empty body instead of 200 + the role name; (b) GET /latest/meta-data/iam/security-credentials/<role> — the emulator answers with no Code and no AccessKeyId instead of Code='Success' and an ASIA-class key. Then call DisassociateIamInstanceProfile and re-read the listing to see the withdrawal.
