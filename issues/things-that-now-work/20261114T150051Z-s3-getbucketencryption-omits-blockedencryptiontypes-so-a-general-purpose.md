Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.sseinv-new-bucket-default-is-sse-s3-with-bucket-key-off-and-sse-c-blocked s3.versioned-sse-s3-bucket-configuration-round-trips s3.s3-objver-versioned-hardened-bucket-config-round-trips
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.sseinv-new-bucket-default-is-sse-s3-with-bucket-key-off-and-sse-c-blocked|run#7|AssertionError|assert blocked == ["SSE-C"], f"a new bucket blocks SSE-C by default, got {blocked!r}"
Issue-Key: s3.versioned-sse-s3-bucket-configuration-round-trips|run#7|AssertionError|assert rules == [{
Issue-Key: s3.s3-objver-versioned-hardened-bucket-config-round-trips|run#12|AssertionError|assert rules[0].get("BlockedEncryptionTypes") == {"EncryptionType": ["SSE-C"]}, (
Issue-Commit: 242873f3341a0220639d323ce35a2b91fad7ef25

# S3 GetBucketEncryption omits BlockedEncryptionTypes, so a general purpose bucket never reports SSE-C as blocked

> _Note: this issue was identified against the code base at commit `242873f3341a0220639d323ce35a2b91fad7ef25`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBucketEncryption

## Description
1 net-new root cause in the s3 surface, grounded in three failing checks the replay report marks neither ALREADY FILED nor OWNED.

Cause 1 of 1: the ServerSideEncryptionRule the emulator serialises carries no BlockedEncryptionTypes member, so a general purpose bucket's SSE-C blocking state is unreportable.

AWS: Smithy (s3) models ServerSideEncryptionRule with three members — ApplyServerSideEncryptionByDefault (ServerSideEncryptionByDefault), BucketKeyEnabled (boolean) and BlockedEncryptionTypes (structure BlockedEncryptionTypes { EncryptionType: EncryptionTypeList, @xmlFlattened }). The model documents BlockedEncryptionTypes as "A bucket-level setting for Amazon S3 general purpose buckets used to prevent the upload of new objects encrypted with the specified server-side encryption type" and that it "is used with the following actions: PutBucketEncryption, GetBucketEncryption, DeleteBucketEncryption". The recordings show that on a general purpose bucket GetBucketEncryption answers the rule WITH that member populated as EncryptionType = ["SSE-C"], both (a) for a bucket that was never configured, where the implicit default rule is SSEAlgorithm=AES256 / BucketKeyEnabled=false / BlockedEncryptionTypes={EncryptionType:["SSE-C"]}, and (b) for a bucket configured by PutBucketEncryption with a single plain {"SSEAlgorithm": "AES256"} rule, where the read-back adds the same two derived members. HTTP 200 in both cases; no error is involved.

Emulator: the same GetBucketEncryption answers 200 with the rule {'ApplyServerSideEncryptionByDefault': {'SSEAlgorithm': 'AES256'}, 'BucketKeyEnabled': False} — BlockedEncryptionTypes is absent entirely, so the parsed member is None. This holds for an unconfigured bucket, for a bucket that explicitly PUT an AES256 rule, and on a versioned/public-access-blocked ("hardened") bucket.

Why: a client that reads GetBucketEncryption to confirm its buckets refuse SSE-C uploads (compliance gates, config rules, a deploy-time hardening assertion) sees no BlockedEncryptionTypes at all and cannot distinguish "SSE-C is blocked" from "the setting is unset" — it concludes the bucket is unhardened on an emulator where AWS would report it hardened. Strict response comparisons and round-trip assertions over the rule set also diverge, because the rule the emulator echoes is a strict subset of the one AWS echoes.

Fix: the s3 GetBucketEncryption handler and its ServerSideEncryptionRule serializer, plus the bucket-encryption state that backs them — the stored default-encryption state must carry the bucket's blocked-encryption-type set (defaulting to SSE-C blocked for a general purpose bucket, including a bucket that was never explicitly configured), and the serializer must emit the BlockedEncryptionTypes/EncryptionType element for it. Note the OPEN issues 20261111T003921Z-cloudformation-s3-2-net-new-causes-the-legacy.md, 20261110T232635Z-cloudformation-ecr-iam-s3-ssm-sts-toolkit-staging-bucket-reports-aes256-default-encryption-and.md and 20261110T232635Z-cloudformation-ecr-iam-s3-sqs-ssm-sts-cdk-bootstrap-staging-bucket-reports-aes256-default-encryption.md: they report the CFN/CDK-provisioned staging bucket emitting BlockedEncryptionTypes where the template asked for an aws:kms rule, i.e. the member IS produced on that path. None of them carries any of the three check keys below, and none of them covers the direct CreateBucket / PutBucketEncryption read path this issue is about; a fix must reconcile the two paths rather than simply deleting the member.

Traces: s3.sseinv-new-bucket-default-is-sse-s3-with-bucket-key-off-and-sse-c-blocked (unconfigured bucket), s3.versioned-sse-s3-bucket-configuration-round-trips (explicit AES256 rule round-trip), s3.s3-objver-versioned-hardened-bucket-config-round-trips (hardened versioned bucket).

## Evidence
Replay output (behavioral suite, section ### s3) — three [mismatch] checks, same missing member:

[mismatch] s3.sseinv-new-bucket-default-is-sse-s3-with-bucket-key-off-and-sse-c-blocked
  File "check_s3_sseinv_new_bucket_default_is_sse_s3_with_bucket_key_off_and_sse_c_blocked.py", line 31, in run
    assert blocked == ["SSE-C"], f"a new bucket blocks SSE-C by default, got {blocked!r}"
  AssertionError: a new bucket blocks SSE-C by default, got None

[mismatch] s3.versioned-sse-s3-bucket-configuration-round-trips
  File "check_s3_versioned_sse_s3_bucket_configuration_round_trips.py", line 30, in run
    assert rules == [{...}]
  AssertionError: a plain AES256 rule must read back with no KMS key, BucketKeyEnabled=False and SSE-C blocked; got [{'ApplyServerSideEncryptionByDefault': {'SSEAlgorithm': 'AES256'}, 'BucketKeyEnabled': False}]

[mismatch] s3.s3-objver-versioned-hardened-bucket-config-round-trips
  File "check_s3_s3_objver_versioned_hardened_bucket_config_round_trips.py", line 37, in run
    assert rules[0].get("BlockedEncryptionTypes") == {"EncryptionType": ["SSE-C"]}
  AssertionError: a new bucket's encryption rule reports SSE-C as blocked by default; got {'ApplyServerSideEncryptionByDefault': {'SSEAlgorithm': 'AES256'}, 'BucketKeyEnabled': False}

Expected (AWS, recorded): GetBucketEncryption -> 200, Rules == [{'ApplyServerSideEncryptionByDefault': {'SSEAlgorithm': 'AES256'}, 'BucketKeyEnabled': False, 'BlockedEncryptionTypes': {'EncryptionType': ['SSE-C']}}].
Actual (emulator): 200, Rules == [{'ApplyServerSideEncryptionByDefault': {'SSEAlgorithm': 'AES256'}, 'BucketKeyEnabled': False}] — BlockedEncryptionTypes absent. Bucket names in the recordings are per-run values and are not part of the requirement; the requirement is the member's presence and its EncryptionType list.

## Steps to Reproduce
1. Build and boot the emulator from this checkout (s3 is bootstrapped and serving).
2. CreateBucket a general purpose bucket (LocationConstraint set when the region is not us-east-1) and do NOT call PutBucketEncryption.
3. Call GetBucketEncryption on it. Expect Rules == [{'ApplyServerSideEncryptionByDefault': {'SSEAlgorithm': 'AES256'}, 'BucketKeyEnabled': False, 'BlockedEncryptionTypes': {'EncryptionType': ['SSE-C']}}]; observe the rule without any BlockedEncryptionTypes member.
4. Call PutBucketEncryption with ServerSideEncryptionConfiguration={'Rules': [{'ApplyServerSideEncryptionByDefault': {'SSEAlgorithm': 'AES256'}}]} and GetBucketEncryption again. Expect the same three-member rule; observe the same two-member rule.
5. Repeat on a bucket with versioning Enabled and all four PublicAccessBlock flags true — the read is unchanged.
6. Equivalently: re-replay s3.sseinv-new-bucket-default-is-sse-s3-with-bucket-key-off-and-sse-c-blocked, s3.versioned-sse-s3-bucket-configuration-round-trips and s3.s3-objver-versioned-hardened-bucket-config-round-trips.
