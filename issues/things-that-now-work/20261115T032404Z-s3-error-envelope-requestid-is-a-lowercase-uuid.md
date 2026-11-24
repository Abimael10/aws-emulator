Issue-Source: trace-sweep-divergence
Issue-Trace: s3-aaxe-configuration-id-mistakes
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3-aaxe-configuration-id-mistakes s3-aaxe-data-export-schema-mistakes s3-aaxe-filter-shapes-beside-the-foreign-export s3-b2b-summary-metadata-is-not-tagging s3-itc-put-tiering-day-and-enum-mistakes s3-mbes-traj-delete-objects-batch-mistakes s3-mbes-traj-reference-version-purge s3-mbes-traj-versioning-status-and-null-version s3-oc-put-document-mistakes-then-corrected
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3|PutBucketAnalyticsConfiguration|4xx->4xx|$.Error.RequestId
Issue-Key: s3|DeleteBucketAnalyticsConfiguration|4xx->4xx|$.Error.RequestId
Issue-Key: s3|PutObjectTagging|4xx->4xx|$.Error.RequestId
Issue-Key: s3|PutBucketIntelligentTieringConfiguration|4xx->4xx|$.Error.RequestId
Issue-Key: s3|DeleteObjects|4xx->4xx|$.Error.RequestId
Issue-Key: s3|PutBucketVersioning|4xx->4xx|$.Error.RequestId
Issue-Key: s3|PutBucketOwnershipControls|4xx->4xx|$.Error.RequestId
Issue-Commit: a4e33e928c3dea0394d6eb4179d27b5f997e5341

# s3 error-envelope RequestId is a lowercase UUID, not an S3-shaped request id (7 causes)

> _Note: this issue was identified against the code base at commit `a4e33e928c3dea0394d6eb4179d27b5f997e5341`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketAnalyticsConfiguration

## Description
Every S3 refusal body is an <Error> element whose <RequestId> element carries the service's request identifier, and S3's request identifier has a fixed shape: a 16-character token of uppercase letters and digits (recorded examples in this corpus include 'X92SNP8YKTXS6CAT', 'WN143ET27G6T4F9W' and 'YKZBBA83DAX70G9V'). The emulator instead serves a lowercase hyphenated RFC-4122 UUID (36 characters, 8-4-4-4-12 with hyphens) in that element. The identifier VALUE is per-run and is never a requirement; its SHAPE is, and the replay asserts the shape, not a literal. The same generator feeds every operation below, so this is one defect observed on seven operations.

Smithy: the S3 model does not model the <Error> envelope's RequestId member at all (PutBucketAnalyticsConfiguration, PutBucketOwnershipControls, PutBucketVersioning etc. declare `errors: []`), so for every cause here the requirement is recorded behavior only: the element name is fixed text of the S3 error envelope and its value must be an S3-shaped request id minted by the service.

Cause 1 of 7: PutBucketAnalyticsConfiguration. AWS: a 400 refusal of an analytics-configuration write carries <RequestId> as a 16-character uppercase alphanumeric S3 request id. Recorded behavior only. Emulator: HTTP 400 is right, but $.Error.RequestId is a lowercase UUID (observed '776ec827-330e-45a0-abbc-ba73a41e1d78'). Why: a client or CLI that logs the request id to raise it with support, or that correlates the body's RequestId with the x-amz-request-id response header, records an identifier of the wrong form; anything that validates or parses it as an S3 request id rejects it. Fix: the shared request-id generator used by the S3 error serializer. Traces: s3-aaxe-configuration-id-mistakes#task#2 (and s3-aaxe-data-export-schema-mistakes#task#2, s3-aaxe-filter-shapes-beside-the-foreign-export#task#8).

Cause 2 of 7: DeleteBucketAnalyticsConfiguration. AWS: same envelope, same request-id shape, on the 400 refusal of a delete addressed by a malformed configuration id. Recorded behavior only. Emulator: $.Error.RequestId is a lowercase UUID (observed '9a58cf1f-ae4f-4505-8da2-f37ceb3ade7e'). Why: as Cause 1. Fix: the same generator, reached from the analytics sub-resource handler's error path. Traces: s3-aaxe-configuration-id-mistakes#task#6.

Cause 3 of 7: PutObjectTagging. AWS: the 400 tagging refusal carries an S3-shaped <RequestId>; the recording shows a 16-character uppercase token. Recorded behavior only. Emulator: a lowercase UUID (observed '7e4e7cbb-5f07-4da2-aac5-704b8ccc0fc5'). Why: as Cause 1, on the object write path where batch tooling logs one id per failed key. Fix: the same generator, reached from the object-tagging error path. Traces: s3-b2b-summary-metadata-is-not-tagging#task#5.

Cause 4 of 7: PutBucketIntelligentTieringConfiguration. AWS: the 400 refusals of tiering-day and access-tier enum mistakes carry an S3-shaped <RequestId>. Recorded behavior only. Emulator: a lowercase UUID (observed '22b2ea65-b8ff-4ae8-85d2-c46adb61d644'). Why: as Cause 1. Fix: the same generator, reached from the intelligent-tiering sub-resource handler's error path. Traces: s3-itc-put-tiering-day-and-enum-mistakes#task#7 (and task#8, task#9 of the same fixture).

Cause 5 of 7: DeleteObjects. AWS: the 400 refusal of a malformed batch-delete document carries an S3-shaped <RequestId>. Recorded behavior only. Emulator: a lowercase UUID (observed '06eba409-7fab-4ae5-a44e-49f2b65990fd'). Why: as Cause 1, and batch deletes are exactly where callers keep the request id per call. Fix: the same generator, reached from the DeleteObjects error path. Traces: s3-mbes-traj-delete-objects-batch-mistakes#task#5 (and s3-mbes-traj-reference-version-purge#task#9).

Cause 6 of 7: PutBucketVersioning. AWS: the 400 refusal of an invalid versioning status / MFA-delete document carries an S3-shaped <RequestId>. Recorded behavior only. Emulator: a lowercase UUID (observed 'd77ff238-3e2b-4752-b6a7-db91c1d170a1'). Why: as Cause 1. Fix: the same generator, reached from the versioning sub-resource handler's error path. Traces: s3-mbes-traj-versioning-status-and-null-version#task#5 (and task#6 of the same fixture).

Cause 7 of 7: PutBucketOwnershipControls. AWS: the 400 refusal of an ownership-controls document carries an S3-shaped <RequestId>. Recorded behavior only. Emulator: a lowercase UUID (observed '62838a2e-de2a-4dd9-a461-3043daf408b4'). Why: as Cause 1. Fix: the same generator, reached from the ownership-controls handler's error path. Traces: s3-oc-put-document-mistakes-then-corrected#task#2 (and task#4 of the same fixture).

Related, not covering: open issue 20261102T111455Z-s3-refusal-envelopes-omit-aws-contextual-error-members.md carries RequestId replay keys for a DIFFERENT set of operations (PutObject, ListBuckets, GetObjectAttributes, PutBucketLifecycleConfiguration, DeleteObject, CreateBucket, PutBucketEncryption, GetObjectTagging, GetBucketEncryption, PutBucketNotificationConfiguration) and is scoped to MISSING context members; none of the seven causes above is owned by it, which is why they replay as net-new.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator (296 fixtures, 3264 steps, 159 mismatched). Representative s3-aaxe-configuration-id-mistakes#task#2 (PutBucketAnalyticsConfiguration, expected 400 / actual 400): $.Error.RequestId value_mismatch — expected an S3-shaped request id (asserted by shape, reported as '<shaped-not-asserted>'), got '776ec827-330e-45a0-abbc-ba73a41e1d78'. Other causes, identical shape of failure, status matching in every one: s3-aaxe-configuration-id-mistakes#task#6 (DeleteBucketAnalyticsConfiguration, 400) got '9a58cf1f-ae4f-4505-8da2-f37ceb3ade7e'; s3-b2b-summary-metadata-is-not-tagging#task#5 (PutObjectTagging, 400) expected a 16-character uppercase token ('X92SNP8YKTXS6CAT' in the recording), got '7e4e7cbb-5f07-4da2-aac5-704b8ccc0fc5'; s3-itc-put-tiering-day-and-enum-mistakes#task#7 (PutBucketIntelligentTieringConfiguration, 400) got '22b2ea65-b8ff-4ae8-85d2-c46adb61d644'; s3-mbes-traj-delete-objects-batch-mistakes#task#5 (DeleteObjects, 400) got '06eba409-7fab-4ae5-a44e-49f2b65990fd'; s3-mbes-traj-versioning-status-and-null-version#task#5 (PutBucketVersioning, 400) got 'd77ff238-3e2b-4752-b6a7-db91c1d170a1'; s3-oc-put-document-mistakes-then-corrected#task#2 (PutBucketOwnershipControls, 400) got '62838a2e-de2a-4dd9-a461-3043daf408b4'. In all seven the HTTP status, Code and Message matched; only the request-id FORM diverged. Corroborating recordings elsewhere in the corpus show the AWS form directly: s3-lctf-traj-tag-filter-document-mistakes#task#2 expected RequestId 'WN143ET27G6T4F9W', s3-refarch-s3ev-bucket-addressing-and-filter-name-mistakes#task#7 expected 'YKZBBA83DAX70G9V' — 16 characters, uppercase letters and digits only, no hyphens.

## Steps to Reproduce
1. Replay fixture s3-aaxe-configuration-id-mistakes and inspect step task#2's 400 <Error> body. 2. Read $.Error.RequestId: it is a lowercase hyphenated 36-character UUID; it must be a 16-character token of uppercase letters and digits only (value per-run, shape fixed), and it must equal the x-amz-request-id response header. 3. Repeat for each fixture#step in trace_ids: s3-aaxe-configuration-id-mistakes#task#6, s3-b2b-summary-metadata-is-not-tagging#task#5, s3-itc-put-tiering-day-and-enum-mistakes#task#7, s3-mbes-traj-delete-objects-batch-mistakes#task#5, s3-mbes-traj-versioning-status-and-null-version#task#5, s3-oc-put-document-mistakes-then-corrected#task#2. 4. Each refusal's RequestId element should be an S3-shaped request id.
