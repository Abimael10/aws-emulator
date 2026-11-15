Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3tables.put-table-bucket-encryption-with-unsupported-kms-key-rejected s3tables.put-table-bucket-encryption-kms-missing-key-rejected
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables.put-table-bucket-encryption-with-unsupported-kms-key-rejected|_error#7|AssertionError|raise AssertionError(f"real AWS rejects this call, but it SUCCEEDED and returned {result!r}")
Issue-Key: s3tables.put-table-bucket-encryption-kms-missing-key-rejected|run#6|AssertionError|assert message == (
Issue-Commit: 2c9e097b66ada646a313dc09bbc30cfe7dbc4d7b

# s3tables PutTableBucketEncryption KMS handling diverges: an inaccessible kmsKeyArn is stored with 200 instead of ForbiddenException, and the missing-kmsKeyArn refusal message is emulator-authored

> _Note: this issue was identified against the code base at commit `2c9e097b66ada646a313dc09bbc30cfe7dbc4d7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutTableBucketEncryption

## Description
Two net-new root causes in PutTableBucketEncryption's SSE-KMS handling. The handler no longer answers 501 (open issue 20261103T220427Z-s3tables-encryption-configuration-3-net-new-root-causes.md stated these semantics while it was a stub, and open issue 20261102T065133Z-s3tables-encryption-metrics-replication-put-operations-answer-501.md stated the kmsKeyArn-required rule for the 501 era) — it now serves, and these two behaviors are wrong in the serving handler.

Cause 1 of 2: no access check against the referenced KMS key, so an unusable encryption configuration is accepted and stored.
AWS: Smithy (com.amazonaws.s3tables#S3TableBuckets) PutTableBucketEncryption = PUT /buckets/{tableBucketARN}/encryption, @idempotent, success 200, Unit output; modeled errors BadRequestException 400, ConflictException 409, ForbiddenException 403, NotFoundException 404, InternalServerErrorException 500, TooManyRequestsException 429. EncryptionConfiguration.sseAlgorithm is @required (enum AES256 | aws:kms) and documents that SSE-KMS requires granting the S3 Tables maintenance principal access to the key; kmsKeyArn has @length 1..2048 and pattern ^(arn:aws[-a-z0-9]*:kms:[-a-z0-9]*:[0-9]{12}:key/.+)$. Recorded behavior: a put of {sseAlgorithm: aws:kms, kmsKeyArn: <a syntactically valid key ARN naming a key the caller cannot access — e.g. one in a foreign account or absent from the request's Region>} is refused HTTP 403, code ForbiddenException, message `User: <caller principal ARN> is not authorized to perform: kms:DescribeKey on this resource because the resource does not exist in this Region, no resource-based policies allow access, or a resource-based policy explicitly denies access`. Fixed text: `User: ` and ` is not authorized to perform: kms:DescribeKey on this resource because the resource does not exist in this Region, no resource-based policies allow access, or a resource-based policy explicitly denies access`; the single variable part is the caller's own principal ARN from the signing identity (what sts:GetCallerIdentity reports), never a literal from one run. The refusal is effect-free: GetTableBucketEncryption still reports sseAlgorithm=AES256 with no kmsKeyArn, and a following {sseAlgorithm: AES256} put succeeds.
Emulator: the put returns HTTP 200 with an empty body and the configuration is accepted; the referenced key is never resolved.
Why: a client that mis-types or mis-scopes its CMK gets silent success instead of the 403 naming kms:DescribeKey as the missing permission, and the bucket is left carrying an encryption configuration pointing at a key S3 Tables can never use — later reads report it as valid while every write under it would fail.
Fix: PutTableBucketEncryption's handler — before any state write, resolve the supplied kmsKeyArn against the KMS key state for the request's account/Region and refuse an unresolvable or inaccessible key with ForbiddenException carrying the kms:DescribeKey message above, leaving the stored configuration untouched.
Traces: s3tables.put-table-bucket-encryption-with-unsupported-kms-key-rejected.

Cause 2 of 2: the refusal for sseAlgorithm=aws:kms with no kmsKeyArn carries emulator-authored prose instead of AWS's invalid-KMS-key-ARN message.
AWS: the same operation; with sseAlgorithm=aws:kms and kmsKeyArn omitted the call is refused HTTP 400, code BadRequestException (modeled), message `The specified KMS key ARN is invalid. Only key ARN format 'arn:<partition>:kms:<region>:<account-id>:key/<key-id>' is supported.` — that message is entirely fixed text, including the literal placeholder-style format template; nothing is interpolated from the request or from state. The refusal writes nothing (the bucket's prior encryption state is unchanged) and a later {sseAlgorithm: AES256} put on the same bucket succeeds.
Emulator: the status and error code are right (400 BadRequestException) but the message is `kmsKeyArn is required when sseAlgorithm is aws:kms.` — wording AWS never emits.
Why: clients and tests that match on the refusal text (or surface it to operators) see prose no AWS SDK or log would ever carry, and the message loses the ARN-format guidance that tells the caller what shape of value is accepted.
Fix: PutTableBucketEncryption's validation in its handler — emit AWS's fixed invalid-KMS-key-ARN message for the missing/ill-formed kmsKeyArn class instead of the hand-written requirement sentence.
Traces: s3tables.put-table-bucket-encryption-kms-missing-key-rejected.

## Evidence
Behavioral replay (102/156 passed):

1. s3tables.put-table-bucket-encryption-with-unsupported-kms-key-rejected — the check puts {sseAlgorithm: aws:kms, kmsKeyArn: arn:aws:kms:<request region>:111122223333:key/00000000-0000-0000-0000-000000000000} on a bucket whose baseline reads AES256, and expects code ForbiddenException with message `User: <sts:GetCallerIdentity Arn> is not authorized to perform: kms:DescribeKey on this resource because the resource does not exist in this Region, no resource-based policies allow access, or a resource-based policy explicitly denies access`, then that the bucket still reads AES256 with no kmsKeyArn. Actual: `AssertionError: real AWS rejects this call, but it SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-length': '0', ...}}}`.

2. s3tables.put-table-bucket-encryption-kms-missing-key-rejected — the check puts {sseAlgorithm: aws:kms} with no kmsKeyArn; code BadRequestException matched, and the message assertion failed: `AssertionError: expected the exact invalid-KMS-key message, got 'kmsKeyArn is required when sseAlgorithm is aws:kms.'` Expected message: `The specified KMS key ARN is invalid. Only key ARN format 'arn:<partition>:kms:<region>:<account-id>:key/<key-id>' is supported.`

## Steps to Reproduce
1. Boot the emulator and point an s3tables client (and an sts client) at it.
2. CreateTableBucket(name=<unique lowercase name>) -> keep `arn`; confirm GetTableBucketEncryption(tableBucketARN=arn) reports sseAlgorithm=AES256 with no kmsKeyArn.
3. Cause 1: PutTableBucketEncryption(tableBucketARN=arn, encryptionConfiguration={"sseAlgorithm":"aws:kms","kmsKeyArn":"arn:aws:kms:<request region>:111122223333:key/00000000-0000-0000-0000-000000000000"}). Expect HTTP 403 ForbiddenException with `User: <caller principal ARN> is not authorized to perform: kms:DescribeKey on this resource because the resource does not exist in this Region, no resource-based policies allow access, or a resource-based policy explicitly denies access`, and GetTableBucketEncryption still reporting AES256 with no kmsKeyArn; observe HTTP 200 and the configuration accepted.
4. Cause 2: PutTableBucketEncryption(tableBucketARN=arn, encryptionConfiguration={"sseAlgorithm":"aws:kms"}). Expect HTTP 400 BadRequestException with message `The specified KMS key ARN is invalid. Only key ARN format 'arn:<partition>:kms:<region>:<account-id>:key/<key-id>' is supported.`; observe the message `kmsKeyArn is required when sseAlgorithm is aws:kms.`
5. Positive control for both: PutTableBucketEncryption(tableBucketARN=arn, encryptionConfiguration={"sseAlgorithm":"AES256"}) must still succeed.
6. Re-replay s3tables.put-table-bucket-encryption-with-unsupported-kms-key-rejected and s3tables.put-table-bucket-encryption-kms-missing-key-rejected.
