Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__kms__s3.directory-bucket-sse-kms-customer-managed-key-round-trips
Issue-Key: ec2__kms__s3.directory-bucket-sse-kms-customer-managed-key-round-trips|_create_directory_bucket#3|AssertionError|assert bucket is not None, (
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# ec2__kms__s3: REOPENS — a directory bucket cannot be sessioned (CreateSession NoSuchBucket), so SSE-KMS on S3 Express is unobservable

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSession

## Description
REOPENS 20261029T211510Z-s3-express-one-zone-is-unreachable-createsession-answers.md: its fix left the call refused.

Cause 1 of 1: CreateSession answers NoSuchBucket for a directory bucket that was just created, so the SSE-KMS-on-directory-bucket check never reaches its subject.

AWS: a directory bucket named <base>--<az-id>--x-s3 created with CreateBucketConfiguration{Location:{Type:'AvailabilityZone',Name:<az-id>}, Bucket:{Type:'Directory',DataRedundancy:'SingleAvailabilityZone'}} can immediately be sessioned: Smithy CreateSession, GET /{Bucket}?session, output member Credentials required, single modeled error NoSuchBucket with httpError 404 (reserved for a bucket that does not exist). With those credentials the Express data plane serves PutObject/GetObject/HeadObject, and a directory bucket configured with a customer managed KMS key echoes x-amz-server-side-encryption: aws:kms plus the key ARN on every response.

Emulator: every documented Express availability zone of the region refuses the directory-bucket flow with HTTP 404, code NoSuchBucket, message 'The specified bucket does not exist.' — for the name the check had just created. Refusals were identical for usw2-az1, usw2-az2 and usw2-az3, so no SSE-KMS behaviour on a directory bucket can be reached.

Why: a client cannot use S3 Express One Zone with a customer managed key at all — the bucket it created is reported not to exist, so key-scoped encryption, the response echo and the KMS grant path are all unreachable.

Fix: the CreateSession handler and the directory-bucket state CreateBucket writes (the bucket must be registered under the name/zone key CreateSession resolves), as in the s3-service issue filed for the same refusal in this run.

Traces: ec2__kms__s3.directory-bucket-sse-kms-customer-managed-key-round-trips.

## Evidence
ec2__kms__s3.directory-bucket-sse-kms-customer-managed-key-round-trips ->
AssertionError: unreachable: no Availability Zone accepted an S3 Express directory bucket; refusals were {'usw2-az1': 'NoSuchBucket', 'usw2-az2': 'NoSuchBucket', 'usw2-az3': 'NoSuchBucket'} over zones ['usw2-az1', 'usw2-az2', 'usw2-az3']
(raised in the check's _create_directory_bucket helper, before any KMS assertion)
Expected: CreateBucket 200 for the directory-bucket name and CreateSession 200 with Credentials, then SSE-KMS responses echoing aws:kms and the customer managed key ARN. Actual: 404/NoSuchBucket/'The specified bucket does not exist.' in every zone.

## Steps to Reproduce
1. Replay the behavioral suite and run ec2__kms__s3.directory-bucket-sse-kms-customer-managed-key-round-trips.
2. Minimal reproduction: CreateBucket a directory bucket ('<base>--<az-id>--x-s3' with Location{Type:'AvailabilityZone'} + Bucket{Type:'Directory',DataRedundancy:'SingleAvailabilityZone'}), then CreateSession(Bucket=<that name>) and observe NoSuchBucket/404 instead of 200 with Credentials.
