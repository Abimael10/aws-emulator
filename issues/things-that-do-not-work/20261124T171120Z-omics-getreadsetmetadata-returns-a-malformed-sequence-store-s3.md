Issue-Source: trace-sweep-divergence
Issue-Trace: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Issue-Shape-Id: com.amazonaws.omics#Omics
Issue-Key: omics|GetReadSetMetadata|2xx->2xx|$.files.source1.s3Access.s3Uri
Issue-Commit: 17ba6c4c127f3c3d0453e714b8e64fcb08b51df4

# omics GetReadSetMetadata returns a malformed sequence-store S3 access-point alias in files.*.s3Access.s3Uri

> _Note: this issue was identified against the code base at commit `17ba6c4c127f3c3d0453e714b8e64fcb08b51df4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetReadSetMetadata

## Description
Cause 1 of 1: the bucket (host) component of the read-set file S3 URI is built from the wrong identity and in the wrong shape.

AWS: GetReadSetMetadata (GET /sequencestore/{sequenceStoreId}/readset/{id}/metadata, 200) returns `files.source1`/`source2`/`index` as ReadSetFile members whose `s3Access` is a `ReadSetS3Access` with member `s3Uri` targeting `S3Uri`, constrained by @pattern `^s3://([a-z0-9][a-z0-9-.]{1,61}[a-z0-9])/(.{1,1024})$` (Smithy: com.amazonaws.omics#ReadSetS3Access$s3Uri; see also com.amazonaws.omics#SequenceStoreS3Access, whose `s3AccessPointArn` shows the store is fronted by an S3 access point). The bucket component is the sequence store's S3 access-point ALIAS, which AWS forms from the access point name plus S3's alias suffix: `<access-point-name>-<7 decimal digits>-<34 lowercase alphanumeric chars>-s3alias`, where for a HealthOmics sequence store the access-point-name part is the 12-digit AWS ACCOUNT ID that owns the store — the same account id that is the first path segment of the object key. The two random middle segments are per-environment values, but the segment COUNT, their lengths and the `-s3alias` suffix are fixed, and the whole alias is exactly 63 characters (S3's access-point alias maximum). The key part is `<accountId>/sequenceStore/<sequenceStoreId>/readSet/<readSetId>/<name>_<source>.<ext>`, which the emulator already gets right. Recorded response status 200, no error code. Hypothesis: the emulator reuses the sequence-store id as the alias prefix because the store id is what is in hand at serialization time.

Emulator: the replayed step answers 200 with the same key but a bucket of the form `<sequenceStoreId>-<12 hex chars>-s3alias` (recorded instance `s3://1000000000-cfbd39c95256-s3alias/...` where AWS produced `s3://000000000000-5492605-6dy3889y4ip1bp5hc4y19d36nrockusw2b-s3alias/...`). Two things are wrong independently of the random values: the alias is prefixed with the sequence store id instead of the owning 12-digit account id, and it has only one random segment (12 hex chars) instead of AWS's `-<7 digits>-<34 lowercase alnum>` pair, so the alias is 31 characters rather than 63.

Why: a client that parses `files.*.s3Access.s3Uri` to reach the read-set bytes through S3 gets a bucket name that does not identify the account-scoped access point. Anything that derives the owning account from the alias prefix (cross-account grants, access-point policy construction, log correlation), that validates the alias against S3's 63-character `-s3alias` form, or that compares the alias against the one returned by GetSequenceStore/CreateSequenceStore sees a different, shorter identifier than AWS ever emits, and S3-access flows built on it break while the emulator reports success.

Fix: the sequence-store S3 access-alias derivation used when serializing read-set file access metadata — in the omics crate's GetReadSetMetadata handler/serializer and the sequence-store state that records the store's access point (services/service-com-amazonaws-omics-omics/src/ops/, src/state.rs). The alias must be derived from the store's owning account id with two random segments of 7 digits and 34 lowercase alphanumerics plus the `-s3alias` suffix, and must be stored once per sequence store so every read-set file and GetSequenceStore report the same alias.

Traces: omics-multipart-read-set-upload-reads-back-uploaded-bytes#task#5 (GetReadSetMetadata, 200 vs 200, key $.files.source1.s3Access.s3Uri).

## Evidence
Fixture omics-multipart-read-set-upload-reads-back-uploaded-bytes, step task#5, operation GetReadSetMetadata, verdict mismatch, expected status 200 / actual status 200.

Diverging key $.files.source1.s3Access.s3Uri (value_mismatch):
  expected: s3://000000000000-5492605-6dy3889y4ip1bp5hc4y19d36nrockusw2b-s3alias/000000000000/sequenceStore/1000000000/readSet/3000000000/sim2real-probe-omics-readset-b8d02c63_source1.fastq.gz
  actual:   s3://1000000000-cfbd39c95256-s3alias/000000000000/sequenceStore/1000000000/readSet/3000000000/sim2real-probe-omics-readset-b8d02c63_source1.fastq.gz

Requirement vs per-run values in the expected bucket component: fixed text is the `-s3alias` suffix, the two-random-segment layout (7 decimal digits, then 34 lowercase alphanumerics) and the 63-character total; the value taken from state is the leading 12-digit owning account id (`000000000000` here, the same account that leads the object key). `5492605` and `6dy3889y4ip1bp5hc4y19d36nrockusw2b` are per-environment random values and are NOT requirements. The emulator's `1000000000` prefix is the sequence store id, not an account id, and its `cfbd39c95256` is a single 12-hex segment where AWS emits two segments. The object key itself matches byte for byte, so only the alias construction diverges.

Smithy evidence: com.amazonaws.omics#ReadSetS3Access$s3Uri @pattern ^s3://([a-z0-9][a-z0-9-.]{1,61}[a-z0-9])/(.{1,1024})$; com.amazonaws.omics#SequenceStoreS3Access$s3AccessPointArn @pattern ^arn:[^:]*:s3:[^:]*:[^:]*:accesspoint/.*$; GetReadSetMetadata http GET /sequencestore/{sequenceStoreId}/readset/{id}/metadata, code 200.

No open issue covers this: the open omics issues cover StartRun/CreateWorkflow behavior (20261029T184954Z-omics-4-net-new-root-causes-startrun-refuses.md), AWS::Omics::* in CloudFormation, and the GetReadSet / DeleteRunGroup / BatchDeleteReadSet / AbortMultipartReadSetUpload stubs plus GetReadSet's 501 — none of them touch GetReadSetMetadata's s3Access serialization.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay fixture omics-multipart-read-set-upload-reads-back-uploaded-bytes: it creates a sequence store, performs a multipart read-set upload, completes it, then reads the read set back.
3. At step task#5 the fixture calls GetReadSetMetadata with the created sequenceStoreId and read set id (GET /sequencestore/{sequenceStoreId}/readset/{id}/metadata).
4. Inspect the 200 response body at $.files.source1.s3Access.s3Uri (and source2/index where present).
5. Observed: the bucket component is `<sequenceStoreId>-<12 hex>-s3alias` (e.g. 1000000000-cfbd39c95256-s3alias). Expected: an S3 access-point alias of the form `<12-digit owning account id>-<7 digits>-<34 lowercase alnum>-s3alias`, 63 characters, whose leading account id equals the first path segment of the object key.
6. The same alias should also be consistent with the store's SequenceStoreS3Access.s3Uri / s3AccessPointArn returned by GetSequenceStore.
