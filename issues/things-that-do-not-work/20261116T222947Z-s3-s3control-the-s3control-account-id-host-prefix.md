Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__s3control.xsvc-s3-bucket-tags-visible-through-s3control-generic-tagging s3__s3control.xsvc-s3control-tag-resource-visible-through-s3-get-bucket-tagging
Issue-Key: s3__s3control.xsvc-s3-bucket-tags-visible-through-s3control-generic-tagging|_tags_dict#8|EgressRefused|resp = s3c.list_tags_for_resource(AccountId=account, ResourceArn=arn)
Issue-Key: s3__s3control.xsvc-s3control-tag-resource-visible-through-s3-get-bucket-tagging|run#11|EgressRefused|s3c.tag_resource(
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# s3__s3control: the s3control account-id host prefix ({AccountId}.<endpoint>) is unserved, so both S3<->S3 Control tagging cross-checks die before any call (1 cause)

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Both cross-service checks that compare S3 bucket tagging with s3control's generic tagging fail before any s3control call is made. This bundle has 1 cause; it must be closed.

Cause 1 of 1: the emulator serves no address for the account-id-prefixed s3control host the API model mandates.

AWS: Smithy (com.amazonaws.s3control#AWSS3ControlServiceV20180820, restXml): every account-scoped s3control operation — including TagResource/UntagResource/ListTagsForResource and GetAccessPoint/PutPublicAccessBlock — carries @endpoint(hostPrefix: "{AccountId}.") and binds AccountId as @httpHeader("x-amz-account-id") plus @hostLabel (pattern ^\d{12}$). An AWS SDK therefore addresses '<account id>.' + the endpoint authority (real AWS: <account-id>.s3-control.<region>.amazonaws.com), and AWS answers there; the account id is taken from the request, the dot-prefix placement is fixed by the model.

Emulator: only the bare authority is announced/served. When the SDK applies the modeled prefix, the address is refused before any HTTP request: 'CapabilityNotOffered: the target serves no address for <account id>.<endpoint host>' / simparity.egress.EgressRefused ('... is not the endpoint under test ...'). No status, no error code, no body. (The run's 000000000000 and 127.0.0.1 are per-environment values; the requirement is that <12-digit account id>.<served authority> is served.)

Why: a client cannot observe the shared tag store at all through s3control: bucket tags written with S3 PutBucketTagging cannot be read back through s3control ListTagsForResource, and tags written with s3control TagResource cannot be read back through S3 GetBucketTagging, because every s3control request fails at address resolution rather than returning the tag set. The S3 half of each check is fine; only the s3control hop is unreachable.

Fix: the emulator's HTTP front door — the hostnames/authorities it announces it serves and its host-based dispatch — must accept and route the modeled '{AccountId}.' prefix for the s3control endpoint (validating the label against x-amz-account-id and ^\d{12}$) onto the same s3control handlers that answer on the bare authority. Same root cause as the s3control issue filed in this sweep; no open issue covers host-prefix reachability.

Traces: s3__s3control.xsvc-s3-bucket-tags-visible-through-s3control-generic-tagging, s3__s3control.xsvc-s3control-tag-resource-visible-through-s3-get-bucket-tagging.

## Evidence
Behavioral suite: s3__s3control 0/2. Both checks report the identical pre-request failure:

  Error: CapabilityNotOffered: the target serves no address for 000000000000.127.0.0.1
    simparity.egress.EgressRefused: egress refused: '000000000000.127.0.0.1' is not the endpoint under test (127.0.0.1); an emulator run may reach only the emulator. If the host came from a URL the emulator returned, the emulator announces an address other than the endpoint it is reached on

Expected: the s3control TagResource / ListTagsForResource call is delivered to '<account id>.<endpoint authority>' (the model's @endpoint hostPrefix "{AccountId}.") and answers with the bucket's tag set, matching what S3 GetBucketTagging reports.
Actual: the call never leaves the client — the host carrying the modeled account-id prefix is served by nothing, so no HTTP status/error code/body exists at all.

## Steps to Reproduce
1. Boot the emulator. Create a bucket and put a tag set on it with S3 PutBucketTagging.
2. With boto3's s3control client pointed at the emulator, call list_tags_for_resource(AccountId=<account id>, ResourceArn=<bucket arn>) — or tag_resource(...) for the reverse direction.
3. Observe botocore address '<account id>.<emulator authority>' per the modeled @endpoint(hostPrefix "{AccountId}.") and the request fail at resolution ('the target serves no address for <account id>.<authority>').
4. After the fix, the host-prefixed request must route to the s3control tagging handlers and the tag set must agree in both directions.
5. Re-replay s3__s3control.xsvc-s3-bucket-tags-visible-through-s3control-generic-tagging and s3__s3control.xsvc-s3control-tag-resource-visible-through-s3-get-bucket-tagging.
