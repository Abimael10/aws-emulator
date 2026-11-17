Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3control.cfn-s3-access-point-tags-update-in-place-then-name-replacement-retains-old cloudformation__s3control.cfn-s3-access-point-update-to-nonexistent-bucket-rolls-back-unchanged
Issue-Key: cloudformation__s3control.cfn-s3-access-point-tags-update-in-place-then-name-replacement-retains-old|run#18|EgressRefused|got = s3c.get_access_point(AccountId=account, Name=first)
Issue-Key: cloudformation__s3control.cfn-s3-access-point-update-to-nonexistent-bucket-rolls-back-unchanged|run#13|EgressRefused|before = s3c.get_access_point(AccountId=account, Name=ap_name)
Issue-Commit: cea05121548b3a16dc11942b4ef340d67b81162b

# cloudformation__s3control: the s3control {AccountId}. host prefix is unserved, so both CFN AWS::S3::AccessPoint verification checks never reach s3control (1 cause)

> _Note: this issue was identified against the code base at commit `cea05121548b3a16dc11942b4ef340d67b81162b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAccessPoint

## Description
Two CloudFormation AWS::S3::AccessPoint checks fail before any s3control verification call can be delivered. This bundle has 1 cause; it must be closed.

Cause 1 of 1: the emulator serves no address for the account-id-prefixed s3control host the API model mandates, so every s3control read a CloudFormation check makes against its deployed access point is refused at address resolution.
Failing checks for this cause: cloudformation__s3control.cfn-s3-access-point-tags-update-in-place-then-name-replacement-retains-old, cloudformation__s3control.cfn-s3-access-point-update-to-nonexistent-bucket-rolls-back-unchanged.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml): the operations these checks use — GetAccessPoint (GET /v20180820/accesspoint/{Name}, 200), ListTagsForResource (GET /v20180820/tags/{ResourceArn+}, 200) and DeleteAccessPoint (DELETE /v20180820/accesspoint/{Name}, 200) — each carry @endpoint(hostPrefix: "{AccountId}."), with AccountId bound as @httpHeader("x-amz-account-id") + @hostLabel (pattern ^\d{12}$, required). An SDK therefore addresses '<account id>.' + the endpoint authority (real AWS: <account-id>.s3-control.<region>.amazonaws.com), and AWS answers there. The prefixing is fixed by the model; the account id is a request value.

Emulator: only the bare authority is announced/served, so the modeled host is refused before a request is sent: 'CapabilityNotOffered: the target serves no address for <account id>.<endpoint host>' / simparity.egress.EgressRefused: "egress refused: '<account id>.<endpoint host>' is not the endpoint under test (<endpoint host>) ...". No HTTP status, no error code and no body are produced at all. (The '000000000000' account and '127.0.0.1' host in this run are per-environment values, not part of the requirement.)

Why: a CloudFormation user can create an AWS::S3::AccessPoint but cannot observe it. Neither check can read back GetAccessPoint.Bucket/NetworkOrigin after CREATE_COMPLETE, nor the access point's Tags via ListTagsForResource after a Tags-only in-place update, nor confirm that the access point retained by UpdateReplacePolicy/DeletionPolicy outlives the stack delete, nor confirm that a rolled-back update to a nonexistent bucket left the access point pointing at the original bucket, nor observe the modeled NoSuchAccessPoint ('The specified accesspoint does not exist') refusal after the stack's access point is deleted. Every one of those assertions fails at host resolution, so a deployed access point is indistinguishable from one that was never created, and a rollback is indistinguishable from a destructive update.

Fix: the emulator's HTTP front door — the set of authorities it announces it serves plus its host-based request dispatch — must accept and route the modeled '{AccountId}.' host prefix for the s3control endpoint onto the same s3control handlers that answer on the bare authority (the host label must be parsed off, validated against ^\d{12}$ and reconciled with the x-amz-account-id header, not treated as an unknown vhost). Same root cause as the already-open issues 20261116T222947Z-cloudformation-s3control-the-access-grants-stack-lifecycle-check.md and 20261116T222947Z-s3control-the-account-id-host-prefix-every-operation.md, which carry only their own checks' replay keys and therefore do not cover these two checks. Note 20261109T211344Z-cloudformation-s3control-5-net-new-causes-three-aws.md covers unregistered AWS::S3::* resource types and stack-Outputs defects, not host-prefix reachability.

Traces: cloudformation__s3control.cfn-s3-access-point-tags-update-in-place-then-name-replacement-retains-old, cloudformation__s3control.cfn-s3-access-point-update-to-nonexistent-bucket-rolls-back-unchanged.

## Evidence
Behavioral suite: cloudformation__s3control 0/7. Five of the seven are owned by other issues; the two unowned checks both report, identically:

  Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
    File "egress.py", line 371, in resolve / refuse(
    simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

Expected (AWS):
- s3c.get_access_point(AccountId=<account>, Name=<ap>) is delivered to '<account>.<endpoint authority>' (model: @endpoint hostPrefix "{AccountId}.") and returns 200 with Bucket=<the stack's bucket> and NetworkOrigin='Internet'.
- s3c.list_tags_for_resource(AccountId=<account>, ResourceArn='arn:<partition>:s3:<region>:<account>:accesspoint/<ap>') returns 200 with the access point's env tag ('first', then 'second' after the Tags-only in-place update).
- After the stack delete removes the stack-owned access point, get_access_point on that name refuses with Code='NoSuchAccessPoint', Message='The specified accesspoint does not exist' (fixed text; no variable part), while the access point retained by UpdateReplacePolicy still answers 200.
- After an update whose AP Bucket names a nonexistent bucket rolls back (UPDATE_ROLLBACK_COMPLETE), get_access_point still returns 200 with the original Bucket.

Actual (emulator): none of these calls leaves the client. The host carrying the modeled account-id prefix resolves to nothing, so there is no status, no error code and no body — the checks die at the first s3control call after CREATE_COMPLETE.

## Steps to Reproduce
1. Boot the emulator.
2. Create a stack whose template contains an AWS::S3::Bucket and an AWS::S3::AccessPoint referencing it; wait for CREATE_COMPLETE.
3. With boto3's s3control client pointed at the emulator, call get_access_point(AccountId=<12-digit account>, Name=<the AP's Name>).
4. Observe botocore address '<account>.<emulator authority>' (per the modeled @endpoint(hostPrefix "{AccountId}.")) and the call fail at resolution: 'the target serves no address for <account>.<authority>' — nothing reaches the emulator.
5. Same for list_tags_for_resource(AccountId=..., ResourceArn='arn:...:accesspoint/<name>') and delete_access_point(AccountId=..., Name=...).
6. After the fix, those calls must reach the s3control handlers: GetAccessPoint 200 with the stack's Bucket and NetworkOrigin 'Internet'; ListTagsForResource 200 reflecting the template's Tags both before and after a Tags-only in-place update; GetAccessPoint on a deleted access point refusing NoSuchAccessPoint / 'The specified accesspoint does not exist'; and the access point unchanged after a rolled-back update to a nonexistent bucket.
7. Re-replay cloudformation__s3control.cfn-s3-access-point-tags-update-in-place-then-name-replacement-retains-old and cloudformation__s3control.cfn-s3-access-point-update-to-nonexistent-bucket-rolls-back-unchanged.
