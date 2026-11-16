Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__s3.directory-bucket-sse-s3-aes256-exact-on-every-response
Issue-Key: ec2__s3.directory-bucket-sse-s3-aes256-exact-on-every-response|_create_directory_bucket#2|AssertionError|assert bucket is not None, (
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# ec2__s3: REOPENS — a directory bucket cannot be sessioned (CreateSession NoSuchBucket), so the SSE-S3 AES256 echo on S3 Express is unobservable

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSession

## Description
REOPENS 20261029T211510Z-s3-express-one-zone-is-unreachable-createsession-answers.md: its fix left the call refused.

Cause 1 of 1: CreateSession answers NoSuchBucket for a directory bucket that was just created, so the SSE-S3/AES256 response-echo check never reaches its subject.

AWS: a directory bucket named <base>--<az-id>--x-s3 created with CreateBucketConfiguration{Location:{Type:'AvailabilityZone',Name:<az-id>}, Bucket:{Type:'Directory',DataRedundancy:'SingleAvailabilityZone'}} can immediately be sessioned: Smithy CreateSession, GET /{Bucket}?session, output member Credentials required, single modeled error NoSuchBucket with httpError 404 (reserved for a bucket that does not exist). With those credentials the Express data plane serves the object operations whose every response carries x-amz-server-side-encryption: AES256.

Emulator: every documented Express availability zone of the region refuses the directory-bucket flow with HTTP 404, code NoSuchBucket, message 'The specified bucket does not exist.' for the name the check had just created (usw2-az1, usw2-az2, usw2-az3 all identical), so no Express SSE-S3 response can be observed.

Why: a client cannot use S3 Express One Zone at all — the bucket it created is reported not to exist, so neither the default AES256 encryption nor its echo on each response is reachable.

Fix: the CreateSession handler and the directory-bucket state CreateBucket writes (the bucket must be registered under the name/zone key CreateSession resolves), as in the s3-service issue filed for the same refusal in this run.

Traces: ec2__s3.directory-bucket-sse-s3-aes256-exact-on-every-response.

## Evidence
ec2__s3.directory-bucket-sse-s3-aes256-exact-on-every-response ->
AssertionError: unreachable: no Availability Zone accepted an S3 Express directory bucket; refusals were {'usw2-az1': 'NoSuchBucket', 'usw2-az2': 'NoSuchBucket', 'usw2-az3': 'NoSuchBucket'} over zones ['usw2-az1', 'usw2-az2', 'usw2-az3']
(raised in the check's _create_directory_bucket helper, before any encryption assertion)
Expected: CreateBucket 200 for the directory-bucket name and CreateSession 200 with Credentials, then AES256 echoed on every object response. Actual: 404/NoSuchBucket/'The specified bucket does not exist.' in every zone.

## Steps to Reproduce
1. Replay the behavioral suite and run ec2__s3.directory-bucket-sse-s3-aes256-exact-on-every-response.
2. Minimal reproduction: CreateBucket a directory bucket ('<base>--<az-id>--x-s3' with Location{Type:'AvailabilityZone'} + Bucket{Type:'Directory',DataRedundancy:'SingleAvailabilityZone'}), then CreateSession(Bucket=<that name>) and observe NoSuchBucket/404 instead of 200 with Credentials.
