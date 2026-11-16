Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.copy-object-into-directory-bucket-rejects-source-tags-and-destination-ssec s3.create-session-express-zone s3.create-session-no-such-bucket s3.create-session-roundtrip s3.create-session-token-ttl s3.directory-bucket-multipart-complete-etag-is-not-an-md5-composite s3.directory-bucket-rejects-object-tagging-versioning-notification-and-replication s3.directory-buckets-listing s3.list-directory-buckets-isolation s3.list-directory-buckets-rejects-regular-bucket-ops s3.put-object-append-conditional-offset-contention s3.put-object-append-write-offset-rejected s3.put-object-encryption-type-mismatch-rejected s3.put-object-invalid-write-offset s3.put-object-invalid-write-offset-rejected s3.rename-object-concurrent-idempotency s3.rename-object-destination-exists-rejected s3.rename-object-idempotency-mismatch s3.rename-object-idempotency-token s3.rename-object-idempotency-token-mismatch s3.rename-object-lifecycle s3.rename-object-source-if-none-match-precondition-rejected s3.rename-object-state-transition s3.s3-express-append-produces-the-exact-concatenation-of-three-offset-writes s3.s3x-s3express-session-and-listing-cross-bucket-token-denied s3.s3x-s3express-session-and-listing-expired-token-denied s3.s3x-s3express-session-and-listing-inflight-mpu-prefix-listed s3.s3x-s3express-session-and-listing-non-slash-delimiter-rejected s3.s3x-s3express-session-and-listing-readonly-session-put-denied s3.s3x-s3express-session-and-listing-readwrite-session-round-trip s3.s3x-s3express-session-and-listing-start-after-rejected s3.s3x-s3express-session-and-listing-unterminated-prefix-rejected
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.copy-object-into-directory-bucket-rejects-source-tags-and-destination-ssec|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.create-session-express-zone|_create_directory_bucket#3|AssertionError|raise AssertionError(f"could not create an S3 Express directory bucket in {ctx.region}: {last_err}")
Issue-Key: s3.create-session-no-such-bucket|_create_directory_bucket#4|AssertionError|raise AssertionError(f"could not create an S3 Express directory bucket in {ctx.region}: {last_err}")
Issue-Key: s3|CreateSession|NoSuchBucket
Issue-Key: s3.directory-bucket-multipart-complete-etag-is-not-an-md5-composite|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.directory-bucket-rejects-object-tagging-versioning-notification-and-replication|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.directory-buckets-listing|_create_directory_bucket#3|AssertionError|raise AssertionError(
Issue-Key: s3.list-directory-buckets-isolation|_create_directory_bucket#5|AssertionError|raise AssertionError(
Issue-Key: s3.list-directory-buckets-rejects-regular-bucket-ops|_create_directory_bucket#3|AssertionError|raise AssertionError(
Issue-Key: s3.put-object-append-conditional-offset-contention|_create_directory_bucket_or_skip#2|AssertionError|raise AssertionError(
Issue-Key: s3.put-object-encryption-type-mismatch-rejected|_create_directory_bucket#3|AssertionError|assert bucket is not None, (
Issue-Key: s3.put-object-invalid-write-offset|_create_directory_bucket#2|AssertionError|raise AssertionError(
Issue-Key: s3.rename-object-concurrent-idempotency|_create_directory_bucket_or_skip#2|AssertionError|raise AssertionError(
Issue-Key: s3.rename-object-destination-exists-rejected|_create_directory_bucket#2|AssertionError|raise AssertionError(f"directory buckets unavailable in region {ctx.region}: {last_err}")
Issue-Key: s3.rename-object-idempotency-mismatch|_create_directory_bucket#2|AssertionError|raise AssertionError(
Issue-Key: s3.rename-object-idempotency-token-mismatch|run#9|AssertionError|raise AssertionError(
Issue-Key: s3.rename-object-source-if-none-match-precondition-rejected|_create_directory_bucket#1|AssertionError|raise AssertionError(f"S3 Express directory buckets unavailable in this environment: {last_err}")
Issue-Key: s3.rename-object-state-transition|_create_directory_bucket#1|AssertionError|raise AssertionError(f"S3 Express directory buckets unavailable in {ctx.region}: {last_err}")
Issue-Key: s3.s3-express-append-produces-the-exact-concatenation-of-three-offset-writes|run#8|AssertionError|assert bucket is not None, (
Issue-Key: s3.s3x-s3express-session-and-listing-cross-bucket-token-denied|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.s3x-s3express-session-and-listing-expired-token-denied|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.s3x-s3express-session-and-listing-inflight-mpu-prefix-listed|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.s3x-s3express-session-and-listing-non-slash-delimiter-rejected|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.s3x-s3express-session-and-listing-readonly-session-put-denied|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.s3x-s3express-session-and-listing-readwrite-session-round-trip|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.s3x-s3express-session-and-listing-start-after-rejected|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Key: s3.s3x-s3express-session-and-listing-unterminated-prefix-rejected|_directory_bucket#2|AssertionError|raise AssertionError(f"could not create a directory bucket in any of {azs!r}: {last}")
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# REOPENS: S3 Express One Zone is unreachable again — CreateSession answers NoSuchBucket for a directory bucket that was just created, stopping the whole directory-bucket surface

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSession

## Description
REOPENS 20261029T211510Z-s3-express-one-zone-is-unreachable-createsession-answers.md: its fix left the call refused.

Cause 1 of 1: a directory bucket (S3 Express One Zone) created in any availability zone cannot be sessioned — CreateSession answers NoSuchBucket, so every check that needs a directory bucket dies at setup.

AWS: CreateBucket with a directory-bucket name (<base>--<az-id>--x-s3) and CreateBucketConfiguration{Location:{Type:'AvailabilityZone',Name:<az-id>}, Bucket:{Type:'Directory',DataRedundancy:'SingleAvailabilityZone'}} creates the bucket, and CreateSession on that name then returns HTTP 200 with the required Credentials member (AccessKeyId, SecretAccessKey, SessionToken, Expiration) scoped to that bucket. Smithy: CreateSession, GET /{Bucket}?session, output member Credentials required, single modeled error NoSuchBucket with httpError 404 — i.e. NoSuchBucket is reserved for a bucket that does not exist. The session token returned here is what the SDK signs every subsequent Express data-plane call with (ListObjectsV2 on the directory bucket, PutObject with x-amz-write-offset-bytes, RenameObject, CreateMultipartUpload/CompleteMultipartUpload, CopyObject into the directory bucket), so no Express behaviour is observable without it. The error message for a genuinely absent bucket is the fixed text 'The specified bucket does not exist.' — it interpolates nothing from the request.

Emulator: for every documented Express availability zone of the region under test the bucket cannot be used: the SDK's directory-bucket flow is refused with HTTP 404, code NoSuchBucket, message 'The specified bucket does not exist.' naming the bucket the checks had just created, reported by the harness as 'An error occurred (NoSuchBucket) when calling the CreateSession operation: The specified bucket does not exist.' One check sees only a bare 404 ('tried 3 zones): 404'). The refusal is uniform across every zone tried, so it is not a per-zone gap: the name CreateBucket accepted is not the name CreateSession resolves. This is exactly the state 20261029T211510Z-...-createsession-answers.md was closed as fixed for; the later issue 20261105T184920Z-s3-the-directory-bucket-s3-express-one-zone.md recorded that directory buckets COULD be created and sessioned at that commit (its two causes are about listing partitioning and a dropped period), so this is a regression back to 'unreachable', not that issue's subject. Because the refusal now happens during bucket setup, the four checks named in that open issue's trace ids no longer reach their own assertions either and are included below.

Why: the entire S3 Express One Zone surface is unusable for a client. It cannot obtain session credentials for a bucket it just created, cannot list directory buckets, cannot append with x-amz-write-offset-bytes, cannot RenameObject, cannot run the Express multipart or copy paths, and cannot observe the Express-specific refusals (non-'/' delimiter, unterminated prefix, StartAfter, cross-bucket or expired session token, read-only session). Worse than a wrong answer: the bucket the client created is reported not to exist, so retry and fallback logic keyed on NoSuchBucket treats a healthy bucket as absent.

Fix: the CreateSession handler and the directory-bucket state CreateBucket writes — a bucket created with CreateBucketConfiguration.Bucket.Type='Directory' must be registered under the exact name/zone key CreateSession looks up, and CreateSession must return 200 with Credentials for it (reserving NoSuchBucket/404 for a name that really is absent). The Express data-plane operations that consume the session token depend on that registration.

Traces: the 32 failing check ids in trace_ids, all of which fail in their directory-bucket setup helper on this one refusal.

## Evidence
Representative replay details (generated bucket names, zone ids and request ids differ per run; the refusal is what matters):
- s3.create-session-express-zone -> AssertionError: could not create an S3 Express directory bucket in us-west-2: An error occurred (NoSuchBucket) when calling the CreateSession operation: The specified bucket does not exist.
- s3.create-session-roundtrip / s3.create-session-token-ttl -> AssertionError: directory bucket creation not supported here: An error occurred (NoSuchBucket) when calling the CreateSession operation: The specified bucket does not exist.
- s3.list-directory-buckets-isolation / s3.list-directory-buckets-rejects-regular-bucket-ops -> AssertionError: S3 Express directory buckets unavailable in this environment across all 3 discovered availability zone(s): usw2-az1: ... NoSuchBucket ...; usw2-az2: ... NoSuchBucket ...; usw2-az3: ... NoSuchBucket ...
- s3.put-object-invalid-write-offset / s3.put-object-encryption-type-mismatch-rejected / s3.s3-express-append-produces-the-exact-concatenation-of-three-offset-writes / ec2-side siblings -> AssertionError: no Availability Zone accepted an S3 Express directory bucket; refusals were {'usw2-az1': 'NoSuchBucket', 'usw2-az2': 'NoSuchBucket', 'usw2-az3': 'NoSuchBucket'}
- s3.s3x-s3express-session-and-listing-* (8 checks) -> AssertionError: could not create a directory bucket in any of ['usw2-az1', 'usw2-az3', 'usw2-az4']: An error occurred (NoSuchBucket) when calling the CreateSession operation: The specified bucket does not exist.
- s3.rename-object-* (8 checks) -> AssertionError: directory buckets unavailable in region us-west-2 / 'directory bucket creation not supported here' / 'tried 3 zones): 404', all with the same CreateSession NoSuchBucket underneath.
- s3.copy-object-into-directory-bucket-rejects-source-tags-and-destination-ssec, s3.directory-bucket-multipart-complete-etag-is-not-an-md5-composite, s3.directory-bucket-rejects-object-tagging-versioning-notification-and-replication, s3.directory-buckets-listing -> same AssertionError in their _directory_bucket helpers.
Expected everywhere: CreateBucket 200 for the directory-bucket name, then CreateSession 200 carrying Credentials, and the bucket visible in ListDirectoryBuckets.

## Steps to Reproduce
1. Replay the behavioral suite and run the s3 checks listed in trace_ids.
2. Minimal reproduction: in a region with documented S3 Express availability zones (e.g. us-west-2: usw2-az1/az3/az4), call CreateBucket(Bucket='<base>--<az-id>--x-s3', CreateBucketConfiguration={Location:{Type:'AvailabilityZone',Name:'<az-id>'}, Bucket:{Type:'Directory',DataRedundancy:'SingleAvailabilityZone'}}).
3. Call CreateSession(Bucket='<that exact name>') (GET /<bucket>?session). Observe HTTP 404 / NoSuchBucket / 'The specified bucket does not exist.' for every zone, where AWS answers 200 with Credentials.
4. Confirm the knock-on: with no session token, ListDirectoryBuckets, PutObject with x-amz-write-offset-bytes, RenameObject and the Express multipart/copy paths cannot be exercised at all.
