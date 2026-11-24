Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__glue__s3.iam-role-trusts-s3-service-positive
Issue-Key: athena__glue__s3.iam-role-trusts-s3-service-positive|_create_and_run_job#24|EgressRefused|job_id = s3control.create_job(
Issue-Commit: c037406dab1b28830e94eb95f4ccf1fc05361c78

# athena__glue__s3: the S3 Control endpoint is unreachable — a CreateJob/DescribeJob call addressed to the account-id-prefixed host of the endpoint under test is served no address, so S3 Batch Operations is unobservable

> _Note: this issue was identified against the code base at commit `c037406dab1b28830e94eb95f4ccf1fc05361c78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateJob

## Description
One net-new cause. The check is listed under the report heading `### athena__glue__s3` and carries no ALREADY FILED or OWNED marker.

Cause 1 of 1: no address is served for the account-id-prefixed S3 Control host, so every s3control call in the run is refused before it is sent.
  AWS: recorded behavior only for the addressing — AWS S3 Control is addressed per account at the account-id-prefixed host of the service endpoint (<account-id>.s3-control.<region>.amazonaws.com), which is why an SDK configured with an endpoint resolves the account-prefixed form of THAT endpoint; the AccountId member of every S3 Control request is what the prefix is built from. The behavior the check exercises on top of that is documented by AWS (https://docs.aws.amazon.com/AmazonS3/latest/userguide/batch-ops-iam-role-policies.html): a role whose trust policy allows sts:AssumeRole for the service principal batchoperations.s3.amazonaws.com is assumable by S3 Batch Operations, so CreateJob (Operation=S3PutObjectTagging, a CSV manifest with its ETag, Report enabled, ConfirmationRequired=false, RoleArn=<that role>) is accepted, DescribeJob advances the job to Status 'Complete', and the tag lands on the manifest's object (GetObjectTagging returns it).
  Emulator: the request is never answered — the harness's egress layer refuses it with 'CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1' / 'egress refused: \'000000000000.127.0.0.1\' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator'. The host is exactly the account-id-prefixed form of the endpoint the emulator IS reached on, i.e. the emulator announces/serves only the bare host and no address for the per-account S3 Control host that addressing requires. (Hypothesis: the prefix is applied by the SDK's S3 Control endpoint rule from the request's AccountId; whichever side composes it, the emulator offers no address for that host, so the call cannot leave the client.) Nothing about CreateJob/DescribeJob/UpdateJobStatus or the batchoperations.s3.amazonaws.com trust path was exercised at all.
  Why: a client that uses S3 Batch Operations the normal way — a standard SDK with the emulator's endpoint configured — cannot reach the service at all; the call fails at address resolution rather than returning a modeled error, so an IAM service-principal trust that AWS honours is untestable and any batch-tagging workflow is dead.
  Fix: the service's endpoint/host handling for s3control — serve (and announce) the account-id-prefixed virtual host of the endpoint the emulator is reached on, in addition to the bare host, so a request carrying AccountId resolves to the emulator; then the CreateJob/DescribeJob job state machine and the batchoperations.s3.amazonaws.com assume-role path behind it can be graded.
  Traces: athena__glue__s3.iam-role-trusts-s3-service-positive.

## Evidence
Behavioral suite simparity-behavioral-aws-v1; check athena__glue__s3.iam-role-trusts-s3-service-positive [mismatch], no ALREADY FILED / OWNED marker.

  Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
    File "egress.py", line 371, in resolve / line 291, in refuse
    simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

The refused host is the account-id prefix joined to the endpoint under test; the account id and the 127.0.0.1 host are this run's values and are not part of the requirement — the requirement is that the account-prefixed form of whatever endpoint the emulator serves is reachable.

What the check never got to observe (all after the first s3control call): s3control.create_job(AccountId=<acct>, ConfirmationRequired=False, Operation={'S3PutObjectTagging': {'TagSet': [{'Key':'batch','Value':'true'}]}}, Manifest={Spec: S3BatchOperations_CSV_20180820 with Fields [Bucket, Key], Location: {ObjectArn: arn:aws:s3:::<bucket>/manifest/manifest.csv, ETag: <put ETag>}}, Report={Bucket: arn:aws:s3:::<bucket>, Prefix:'reports', Format: Report_CSV_20180820, Enabled: True, ReportScope: AllTasks}, Priority=1, RoleArn=<role trusting batchoperations.s3.amazonaws.com>) must be accepted; DescribeJob must reach Status 'Complete'; s3.get_object_tagging on the manifest's target object must then return batch=true. The S3 and IAM setup steps in the same check (CreateBucket, PutObject, PutBucketTagging, CreateRole, PutRolePolicy) all succeeded against the emulator, so the refusal is specific to the s3control addressing.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator, replay the behavioral suite simparity-behavioral-aws-v1 and look at athena__glue__s3.iam-role-trusts-s3-service-positive.
2. By hand: point a standard SDK's s3control client at the emulator's endpoint and call any S3 Control operation that carries AccountId (e.g. CreateJob or DescribeJob with AccountId=000000000000). The SDK addresses <AccountId>.<emulator-host>; observe that no address is served for that host and the request never reaches the emulator, where a bare-host request would.
3. Once the account-prefixed host resolves to the emulator, replay the check's body: create a bucket, PutObject an object and a CSV manifest '<bucket>,<key>' (keep the manifest's ETag), create a role whose trust policy allows sts:AssumeRole for Service 'batchoperations.s3.amazonaws.com' with an inline policy granting s3:GetObject/s3:PutObject/s3:PutObjectTagging on the bucket and s3:GetBucketLocation on it, then CreateJob (Operation S3PutObjectTagging, TagSet batch=true, ConfirmationRequired=false, Priority=1, Report enabled with ReportScope AllTasks, RoleArn=<role>). Poll DescribeJob until a terminal status: it must be 'Complete', and GetObjectTagging on the manifest's object must then return batch=true.
