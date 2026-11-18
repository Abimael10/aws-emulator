Issue-Source: trace-sweep-divergence
Issue-Trace: s3-bucket-configuration-crud
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3-bucket-configuration-crud s3-bucket-default-encryption-config s3-bucket-default-encryption-setup s3-bucket-encryption-and-public-access s3-bucket-encryption-lifecycle s3-cfg-sse-bucket-key-with-aes256 s3-cfg-us-east-1-reference-path s3-sseinv-dsse-kms-is-not-aws-kms s3-sseinv-get-encryption-bucket-reference-mistakes s3-sseinv-kms-key-reference-forms-echo-verbatim-and-dangling-key s3-sseinv-put-encryption-algorithm-and-key-mistakes s3-sseinv-us-east-1-location-constraint-is-empty s3-sseinv-walk-partitions-kms-from-s3-managed s3w-bucket-metadata-agent-shapes
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3|GetBucketEncryption|2xx->2xx|$.ServerSideEncryptionConfiguration.Rule.BlockedEncryptionTypes
Issue-Commit: a84a0a47137d3eec61135ddc1c28c473b0a18687

# S3 GetBucketEncryption rule omits BlockedEncryptionTypes on the wire across every bucket-encryption read

> _Note: this issue was identified against the code base at commit `a84a0a47137d3eec61135ddc1c28c473b0a18687`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBucketEncryption

## Description
1 net-new root cause in the s3 wire surface, from the one GetBucketEncryption divergence cluster the replay report marks neither stubbed nor OWNED. It is the single largest cluster in the sweep (27 affected steps across 25 fixtures).

Cause 1 of 1: the ServerSideEncryptionRule element the emulator serialises for GetBucketEncryption carries no BlockedEncryptionTypes child, so a general purpose bucket never reports SSE-C as blocked.

AWS: Smithy (com.amazonaws.s3) models ServerSideEncryptionRule with three members - ApplyServerSideEncryptionByDefault (ServerSideEncryptionByDefault), BucketKeyEnabled (boolean) and BlockedEncryptionTypes (structure com.amazonaws.s3#BlockedEncryptionTypes { EncryptionType: EncryptionTypeList, @xmlFlattened }). The BlockedEncryptionTypes doc: "A bucket-level setting for Amazon S3 general purpose buckets used to prevent the upload of new objects encrypted with the specified server-side encryption type", and "This data type is used with the following actions: PutBucketEncryption, GetBucketEncryption, DeleteBucketEncryption"; the model notes it "only supports blocking or unblocking server-side encryption with customer-provided keys (SSE-C)". Recorded behavior: GetBucketEncryption answers HTTP 200 and the rule carries BlockedEncryptionTypes with a flattened EncryptionType element whose value is SSE-C, i.e. $.ServerSideEncryptionConfiguration.Rule.BlockedEncryptionTypes == {'EncryptionType': 'SSE-C'}. The recordings show this on every shape of general purpose bucket: one never configured (implicit AES256 / BucketKeyEnabled=false default rule), one that PUT a plain AES256 rule, one that PUT AES256 with BucketKeyEnabled, and aws:kms / DSSE-KMS rules - the member is derived bucket state, not an echo of the request, and it is present whatever the SSEAlgorithm is. No error is involved: the status matches (200 -> 200) and only this element differs.

Emulator: the same GetBucketEncryption answers 200 but the serialised rule has only ApplyServerSideEncryptionByDefault and BucketKeyEnabled - BlockedEncryptionTypes is absent from the XML entirely, so a parsing client reads it as None. The replay reports exactly one diff key for all 27 steps: "$.ServerSideEncryptionConfiguration.Rule.BlockedEncryptionTypes: missing (expected {'EncryptionType': 'SSE-C'}, got None)".

Why: a client that reads GetBucketEncryption to confirm its buckets refuse SSE-C uploads - a compliance gate, a config rule, a deploy-time hardening assertion, or any strict round-trip comparison of the rule set against what was PUT - cannot distinguish "SSE-C is blocked" from "the setting is unset". It concludes the bucket is unhardened on an emulator where AWS reports it hardened, and any exact-match assertion on the rule fails because the emulator's rule is a strict subset of AWS's.

Fix: the s3 GetBucketEncryption handler's ServerSideEncryptionRule serializer and the bucket default-encryption state behind it - the stored encryption state must carry the bucket's blocked-encryption-type set (SSE-C blocked by default on a general purpose bucket, including a bucket that was never explicitly configured or whose configuration was deleted), and the serializer must emit the BlockedEncryptionTypes wrapper with its flattened EncryptionType element. Note the OPEN behavioral issue 20261114T150051Z-s3-getbucketencryption-omits-blockedencryptiontypes-so-a-general-purpose.md, which describes the same missing member from three behavioral checks: it carries NO wire fixture/step key, so this wire cluster is not owned by it and the fixer must close both pointers. Also note the OPEN CloudFormation/CDK issues 20261111T003921Z-cloudformation-s3-2-net-new-causes-the-legacy.md, 20261110T232635Z-cloudformation-ecr-iam-s3-ssm-sts-toolkit-staging-bucket-reports-aes256-default-encryption-and.md and 20261110T232635Z-cloudformation-ecr-iam-s3-sqs-ssm-sts-cdk-bootstrap-staging-bucket-reports-aes256-default-encryption.md, which report the member being emitted on the stack-provisioned bucket path where the template asked for an aws:kms rule; a fix must reconcile the two paths rather than delete the member.

Traces: s3-bucket-configuration-crud#task#7 (representative) plus 26 further steps in the same cluster, including s3-bucket-configuration-crud#task#9, s3-bucket-default-encryption-config#task#3, s3-bucket-default-encryption-config#teardown#10, s3-bucket-default-encryption-setup#task#3/#5/#7, s3-bucket-encryption-and-public-access#task#4/#6, s3-bucket-encryption-lifecycle#task#3/#5, s3-cfg-sse-bucket-key-with-aes256#task#3/#5, s3-cfg-us-east-1-reference-path#task#10, s3-sseinv-dsse-kms-is-not-aws-kms#task#3/#5, s3-sseinv-get-encryption-bucket-reference-mistakes#task#7, s3-sseinv-kms-key-reference-forms-echo-verbatim-and-dangling-key#task#3/#5/#7, s3-sseinv-put-encryption-algorithm-and-key-mistakes#task#6/#8, s3-sseinv-us-east-1-location-constraint-is-empty#task#5, s3-sseinv-walk-partitions-kms-from-s3-managed#task#8/#9.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, score 94.5%).

Divergence cluster (verdict=mismatch, stub_path empty, owned_by empty, affected_count=27):
  service=s3 | GetBucketEncryption | status=200->200 | keys=$.ServerSideEncryptionConfiguration.Rule.BlockedEncryptionTypes
  fixture_id: s3-bucket-configuration-crud, step_key: task#7
  detail: $.ServerSideEncryptionConfiguration.Rule.BlockedEncryptionTypes: missing (expected {'EncryptionType': 'SSE-C'}, got None)

Expected (AWS, recorded): HTTP 200, <ServerSideEncryptionConfiguration><Rule>...<BlockedEncryptionTypes><EncryptionType>SSE-C</EncryptionType></BlockedEncryptionTypes></Rule></ServerSideEncryptionConfiguration>.
Actual (emulator): HTTP 200, the same <Rule> with <ApplyServerSideEncryptionByDefault> and <BucketKeyEnabled> only; no <BlockedEncryptionTypes> element.

The requirement is the presence of the BlockedEncryptionTypes wrapper and its flattened EncryptionType list value SSE-C. Bucket names in the fixtures (e.g. sim2real-probe-*) are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Build and boot the emulator from this checkout (s3 is bootstrapped and serving; /__admin/health answered 200).
2. CreateBucket a general purpose bucket (with LocationConstraint when the region is not us-east-1) and do NOT call PutBucketEncryption.
3. Call GetBucketEncryption. Expect 200 with Rule = {ApplyServerSideEncryptionByDefault: {SSEAlgorithm: AES256}, BucketKeyEnabled: false, BlockedEncryptionTypes: {EncryptionType: [SSE-C]}}; observe the rule with no BlockedEncryptionTypes element.
4. PutBucketEncryption with a single rule {ApplyServerSideEncryptionByDefault: {SSEAlgorithm: AES256}} (and separately with SSEAlgorithm aws:kms + KMSMasterKeyID, and with BucketKeyEnabled=true), then GetBucketEncryption again. Expect the same three-member rule each time; observe the same two-member rule.
5. Equivalently: re-replay s3-bucket-configuration-crud step task#7 and the other 26 steps of the cluster listed under Traces.
