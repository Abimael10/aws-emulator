Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3control.deploy-s3-access-grants-instance-location-grant-lifecycle
Issue-Key: cloudformation__s3control.deploy-s3-access-grants-instance-location-grant-lifecycle|run#8|EgressRefused|prior = s3c.get_access_grants_instance(AccountId=account)
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# cloudformation__s3control: the Access Grants stack lifecycle check never reaches s3control — the modeled {AccountId}. host prefix is unserved (1 cause)

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAccessGrantsInstance

## Description
The CloudFormation Access Grants deploy/verify/delete check fails before any s3control verification call. This bundle has 1 cause; it must be closed.

Cause 1 of 1: the emulator serves no address for the account-id-prefixed s3control host the API model mandates, so the check's s3control reads of the deployed resources cannot be delivered.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml): every account-scoped s3control operation — GetAccessGrantsInstance, GetAccessGrantsLocation, GetAccessGrant and the rest — carries @endpoint(hostPrefix: "{AccountId}.") with AccountId bound as @httpHeader("x-amz-account-id") and @hostLabel (pattern ^\d{12}$). An SDK therefore addresses '<account id>.' + the endpoint authority (real AWS: <account-id>.s3-control.<region>.amazonaws.com), which AWS answers. The account id comes from the request; the prefixing is fixed by the model.

Emulator: only the bare authority is announced/served, so the modeled host is refused at resolution time: 'CapabilityNotOffered: the target serves no address for <account id>.<endpoint host>' / simparity.egress.EgressRefused ('... is not the endpoint under test ...'). No status, no error code, no body is produced. (000000000000 and 127.0.0.1 in this run are per-environment values.)

Why: a CloudFormation user cannot verify or operate AWS::S3::AccessGrantsInstance / Location / Grant resources at all: after the stack converges, every s3control read of the created instance, location and grant — and every post-delete not-found assertion — fails at address resolution instead of answering, so the stack's resources are indistinguishable from never having existed.

Fix: the emulator's HTTP front door — the set of hostnames/authorities it announces it serves and its host-based request dispatch — must accept and route the modeled '{AccountId}.' host prefix for the s3control endpoint onto the same handlers that answer on the bare authority. Same root cause as the s3control issue filed in this sweep. Note that 20261109T211344Z-cloudformation-s3control-5-net-new-causes-three-aws.md (which owns the four other cloudformation__s3control failures) covers unregistered AWS::S3::* resource types and stack-Outputs defects, not host-prefix reachability.

Traces: cloudformation__s3control.deploy-s3-access-grants-instance-location-grant-lifecycle.

## Evidence
Behavioral suite: cloudformation__s3control 0/5 (the other four are owned by 20261109T211344Z-cloudformation-s3control-5-net-new-causes-three-aws.md). The unowned check reports:

  Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
    simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

Expected: after the stack reaches CREATE_COMPLETE, s3control GetAccessGrantsInstance/GetAccessGrantsLocation/GetAccessGrant are delivered to '<account id>.<endpoint authority>' (model: @endpoint hostPrefix "{AccountId}.") and describe the deployed resources; after DeleteStack they refuse with the modeled not-found errors.
Actual: the s3control calls never leave the client — the host carrying the modeled account-id prefix is served by nothing.

## Steps to Reproduce
1. Boot the emulator. Deploy a stack containing AWS::S3::AccessGrantsInstance, AccessGrantsLocation and AccessGrant.
2. With boto3's s3control client pointed at the emulator, call get_access_grants_instance(AccountId=<account id>).
3. Observe botocore address '<account id>.<emulator authority>' per the modeled @endpoint(hostPrefix "{AccountId}.") and the request fail at resolution ('the target serves no address for <account id>.<authority>').
4. After the fix, those reads must reach the s3control handlers and describe the stack's resources, and after DeleteStack must answer the modeled not-found refusals.
5. Re-replay cloudformation__s3control.deploy-s3-access-grants-instance-location-grant-lifecycle.
