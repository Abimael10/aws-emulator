Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__s3control.xsvc-s3-3-access-point-policy-denied-lambda-role-still-reads-bucket-directly
Issue-Key: lambda__s3__s3control.xsvc-s3-3-access-point-policy-denied-lambda-role-still-reads-bucket-directly|run#7|EgressRefused|ap_arn = s3control.create_access_point(AccountId=account, Name=ap_name, Bucket=bucket)[
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# lambda__s3__s3control: the access-point permission-chain check cannot create its access point — the modeled s3control {AccountId}. host prefix is unserved (1 cause)

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAccessPoint

## Description
The cross-service check that proves a Lambda role denied through an access-point policy can still read the bucket directly fails before any s3control call. This bundle has 1 cause; it must be closed.

Cause 1 of 1: the emulator serves no address for the account-id-prefixed s3control host the API model mandates, so the access point the check needs can neither be created nor policed.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml): every account-scoped s3control operation — CreateAccessPoint, PutAccessPointPolicy, GetAccessPoint, DeleteAccessPoint — carries @endpoint(hostPrefix: "{AccountId}.") with AccountId bound as @httpHeader("x-amz-account-id") and @hostLabel (pattern ^\d{12}$). An SDK therefore addresses '<account id>.' + the endpoint authority (real AWS: <account-id>.s3-control.<region>.amazonaws.com), and AWS answers there. The account id is a value from the request; the prefix placement is fixed by the model.

Emulator: only the bare authority is announced/served, so the modeled host is refused at resolution: 'CapabilityNotOffered: the target serves no address for <account id>.<endpoint host>' / simparity.egress.EgressRefused ('... is not the endpoint under test ...'). No HTTP status, error code or body is produced. (000000000000 and 127.0.0.1 here are per-environment values.)

Why: no client can build the access-point half of an S3 permission chain against the emulator: CreateAccessPoint and PutAccessPointPolicy fail at address resolution, so the access-point ARN data path a Lambda role is meant to be denied on never exists and the distinction between an access-point denial and direct bucket access is unobservable.

Fix: the emulator's HTTP front door — the hostnames/authorities it announces it serves and its host-based dispatch — must accept and route the modeled '{AccountId}.' prefix for the s3control endpoint (validated against x-amz-account-id and the ^\d{12}$ label pattern) onto the same s3control handlers that answer on the bare authority. Same root cause as the s3control issue filed in this sweep. The open issues 20261106T133147Z-lambda-s3-s3control-a-lambda-role-s-read.md and 20261105T105042Z-lambda-s3-s3control-the-python3-12-invoke-runtime.md describe the AP-ARN data path and the Lambda runtime's missing SDK, not host-prefix reachability.

Traces: lambda__s3__s3control.xsvc-s3-3-access-point-policy-denied-lambda-role-still-reads-bucket-directly.

## Evidence
Behavioral suite: lambda__s3__s3control 0/1. The check reports:

  Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
    simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

Expected: s3control CreateAccessPoint / PutAccessPointPolicy are delivered to '<account id>.<endpoint authority>' (model: @endpoint hostPrefix "{AccountId}."), returning the access point and its policy so the Lambda role's read through the AP ARN can be denied while its direct bucket read succeeds.
Actual: the s3control calls never leave the client — the host carrying the modeled account-id prefix is served by nothing, so no status, error code or body is ever produced.

## Steps to Reproduce
1. Boot the emulator. Create a bucket and a Lambda function with a role that can read it.
2. With boto3's s3control client pointed at the emulator, call create_access_point(AccountId=<account id>, Name=<name>, Bucket=<bucket>).
3. Observe botocore address '<account id>.<emulator authority>' per the modeled @endpoint(hostPrefix "{AccountId}.") and the request fail at resolution ('the target serves no address for <account id>.<authority>').
4. After the fix, the access point and its policy must be creatable over the host-prefixed address, and the Lambda role's read through the AP ARN must be denied while its direct bucket read succeeds.
5. Re-replay lambda__s3__s3control.xsvc-s3-3-access-point-policy-denied-lambda-role-still-reads-bucket-directly.
