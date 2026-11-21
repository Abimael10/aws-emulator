Issue-Source: trace-sweep-divergence
Issue-Trace: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Issue-Shape-Id: com.amazonaws.omics#Omics
Issue-Key: omics|GetReadSetMetadata|2xx->2xx|$.creationJobId
Issue-Key: omics|GetReadSetMetadata|2xx->2xx|$.etag
Issue-Key: omics|GetReadSetMetadata|2xx->2xx|$.files.source1.md5
Issue-Key: omics|GetReadSetMetadata|2xx->2xx|$.files.source1.partSize
Issue-Key: omics|GetReadSetMetadata|2xx->2xx|$.files.source1.s3Access
Issue-Key: omics|GetReadSetMetadata|2xx->2xx|$.sequenceInformation
Issue-Key: omics|DeleteSequenceStore|2xx->4xx|status
Issue-Commit: b61d91764c5b64113b88eda248364bbad90afe06

# omics direct read-set upload lifecycle diverges: GetReadSetMetadata omits ingest-derived metadata and invents creationJobId, and the store can never be deleted

> _Note: this issue was identified against the code base at commit `b61d91764c5b64113b88eda248364bbad90afe06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetReadSetMetadata

## Description
Two net-new root causes in the HealthOmics direct-upload lifecycle (CreateMultipartReadSetUpload -> UploadReadSetPart -> CompleteMultipartReadSetUpload -> GetReadSetMetadata -> teardown). This issue bundles 2 causes; both must be closed before submitting.

=== Cause 1 of 2: GetReadSetMetadata answers 200 but the read-set metadata document is incomplete and carries a creationJobId no upload-created read set has ===

AWS: GetReadSetMetadata (GET /sequencestore/{sequenceStoreId}/readset/{id}/metadata, 200) reports the metadata the service derived when it INGESTED the uploaded read set, not merely the fields the upload request carried. Smithy (com.amazonaws.omics#GetReadSetMetadata output): `sequenceInformation` -> SequenceInformation{totalReadCount:Long, totalBaseCount:Long, generatedFrom, alignment:String}; `files` -> ReadSetFiles{source1,source2,index} of FileInformation{totalParts range 1..10000, partSize range 1..5368709120, contentLength, s3Access:ReadSetS3Access}; `etag` -> ETag{algorithm:ETagAlgorithm (FASTQ_MD5up|BAM_MD5up|CRAM_MD5up|FASTQ_SHA256up|...), source1, source2}; `creationJobId` (CreationJobId, pattern ^[a-zA-Z0-9]+$); SequenceStoreS3Access.s3Uri pattern ^s3://([a-z0-9][a-z0-9-.]{1,61}[a-z0-9])/(.{1,1024})$. For a read set formed by a DIRECT multipart upload the recording shows, for the whole success class: `etag` present with `algorithm` the <FILETYPE>_MD5up family matching the read set's fileType (FASTQ_MD5up for a FASTQ) and `source1` the MD5 of that source's ingested content (`source2` absent when the upload had one source); `sequenceInformation` present with `alignment` "UNALIGNED" for an unaligned FASTQ and `totalReadCount`/`totalBaseCount` the read and base counts parsed out of the uploaded records (1 read / 10 bases for the fixture's 46-byte single-record FASTQ — those numbers are the fixture's values, the requirement is that the counts are the ones the uploaded bytes contain), `generatedFrom` absent when the upload named no source; `files.source1.partSize` the part size the multipart upload is conducted with (104857600 = 100 MiB in the recording), NOT the byte length of the part that happened to be uploaded; `files.source1.s3Access.s3Uri` the s3:// location of the ingested file under the store's access-point alias bucket, shaped s3://<store-bucket-alias>/<accountId>/sequenceStore/<sequenceStoreId>/readSet/<readSetId>/<readSetName>_source1.<ext> (the bucket alias is environment-specific and is not itself a requirement; the account/sequenceStore/readSet path segments come from the request and state); `files.source1.md5` the MD5 of that file's content (recorded behavior only — this service's on-disk Smithy FileInformation declares only totalParts/partSize/contentLength/s3Access, so the member is not modeled but IS on the recorded wire); and `creationJobId` ABSENT, because a read set whose creationType is UPLOAD was produced by no import job.

Emulator: the step answers 200 but the body diverges on six keys — `$.etag` missing (expected {algorithm: FASTQ_MD5up, source1: <md5 of source1>}), `$.sequenceInformation` missing (expected {alignment: UNALIGNED, totalReadCount, totalBaseCount}), `$.files.source1.md5` missing, `$.files.source1.s3Access` missing, `$.files.source1.partSize` value_mismatch (expected the upload's 100 MiB part size, got 46 — the length of the uploaded part), and `$.creationJobId` type_mismatch (expected absent, got the multipart upload's id, e.g. "2000000000"). The handler hard-codes `etag: None` (services/service-com-amazonaws-omics-omics/src/ops/get_read_set_metadata.rs:250) and `creation_job_id: Some(read_set.creation_job_id.clone())` (:251), which CompleteMultipartReadSetUpload populated from `upload.upload_id` (src/ops/complete_multipart_read_set_upload.rs:432); `ReadSet::sequence_information_output` returns None unless the upload named `generatedFrom` and never fills the counts or alignment (src/state.rs:946-954); `file_information` sets `s3_access: None` and `part_size` to the first part's byte length (src/state.rs:992-999).

Why: a client that uploads a read set and reads it back cannot verify the upload (no etag, no per-file md5), cannot learn where the file lives (no s3Access.s3Uri, so any S3-based download path is impossible), sees a part size equal to one part's payload instead of the upload's part size, gets no read/base counts or alignment for QC, and sees a creationJobId that invites it to call GetReadSetImportJob for a job that does not exist. The read set is reported as if the service had never ingested it.

Fix: GetReadSetMetadata's response assembly plus the read-set state it reads — compute the ingest-derived members when CompleteMultipartReadSetUpload forms the read set (per-source MD5 and the <fileType>_MD5up ETag, FASTQ record/base counts and UNALIGNED alignment, the file's s3Uri under the store's access, the upload's part size) and stop reporting `creationJobId` for a read set whose creationType is UPLOAD.

Traces: omics-multipart-read-set-upload-reads-back-uploaded-bytes#task#5 (GetReadSetMetadata, status 200 -> 200, 6 diverging keys).

=== Cause 2 of 2: DeleteSequenceStore answers 409 ConflictException where AWS deletes the emptied store 200 ===

AWS: DeleteSequenceStore (DELETE /sequencestore/{id}) answers 200 with an empty body; Smithy models ConflictException (409, "The request cannot be applied to the target resource in its current state") for the case where the store still holds resources. The recorded teardown deletes the store's read sets and then deletes the store, and the store delete answers 200.

Emulator: the teardown's DeleteSequenceStore answers 409 ConflictException with the fixed text "The specified resource is associated with other resources and cannot be deleted." (src/state.rs:299, no variable parts) instead of 200, because the store still holds the uploaded read set: the refusal gate is `registry.holds_read_sets(&input.id)` (src/ops/delete_sequence_store.rs:184) and `ReadSet::is_held` only stops counting a read set once its status is DELETED (src/state.rs:914). Hypothesis: nothing in the emulator can ever reach that DELETED status, because BatchDeleteReadSet is an unimplemented scaffold stub (the preceding teardown step is blocked) — see open issue 20261121T101952Z-omics-batchdeletereadset-stub.md, which covers only the stub's 501 and does NOT carry this step's replay key. Either way a sequence store that has ever held a read set is undeletable in the emulator.

Why: every lifecycle test and every teardown leaks a sequence store: the resource can be created but never reclaimed, so repeated runs accumulate stores and any client that cleans up after itself fails with a 409 it cannot clear by any call.

Fix: the DeleteSequenceStore handler's conflict gate together with the read-set deletion path that is supposed to clear it — a read set deleted through BatchDeleteReadSet must stop being held by its store so the store's delete succeeds with a 200 empty body, and the 409 must remain only for a store that genuinely still holds a non-deleted read set.

Traces: omics-multipart-read-set-upload-reads-back-uploaded-bytes#teardown#8 (DeleteSequenceStore, expected 200, got 409).

## Evidence
Replay of trace set simparity-wire-aws-v1 (8 fixtures, 41 steps, 21 passed, score 51.2%), fixture omics-multipart-read-set-upload-reads-back-uploaded-bytes:

Cause 1 — step task#5, GetReadSetMetadata, verdict mismatch, expected_status 200, actual_status 200:
  $.creationJobId: type_mismatch (expected None, got '2000000000')
  $.etag: missing (expected {'algorithm': 'FASTQ_MD5up', 'source1': 'ee53031e5dbb0215f4c751fa488a6bf1', 'source2': None}, got None)
  $.files.source1.md5: missing (expected 'dd6d1869e4898bc0355e5c96b67090c4', got None)
  $.files.source1.partSize: value_mismatch (expected 104857600, got 46)
  $.files.source1.s3Access: missing (expected {'s3Uri': 's3://<store-alias>/<account>/sequenceStore/<storeId>/readSet/<readSetId>/<name>_source1.fastq.gz'}, got None)
  $.sequenceInformation: missing (expected {'alignment': 'UNALIGNED', 'generatedFrom': None, 'totalBaseCount': 10, 'totalReadCount': 1}, got None)
  (the literal md5/etag digests, ids and bucket alias above are the single recording's values and fill the template; the requirement is the members' presence and derivation)

Cause 2 — step teardown#8, DeleteSequenceStore, verdict mismatch: status: expected 200, got 409 (ConflictException, message "The specified resource is associated with other resources and cannot be deleted.").

Emulator source: src/ops/get_read_set_metadata.rs:228-252; src/ops/complete_multipart_read_set_upload.rs:410-444; src/state.rs:930-999 (files_output/sequence_information_output/file_information); src/ops/delete_sequence_store.rs:184-197; src/state.rs:299-300, 914.

## Steps to Reproduce
1. Replay fixture omics-multipart-read-set-upload-reads-back-uploaded-bytes from trace set simparity-wire-aws-v1 against the emulator.
2. Cause 1: CreateSequenceStore, CreateMultipartReadSetUpload (FASTQ, source1), UploadReadSetPart with a single small FASTQ part, CompleteMultipartReadSetUpload, then GET /sequencestore/{sequenceStoreId}/readset/{readSetId}/metadata (step task#5). Observe 200 with no `etag`, no `sequenceInformation`, no `files.source1.s3Access`, no `files.source1.md5`, `files.source1.partSize` equal to the uploaded part's byte length, and a `creationJobId` equal to the multipart upload id.
3. Cause 2: with that read set in the store, run the fixture teardown — delete the read set, then DELETE /sequencestore/{id} (step teardown#8). Observe 409 ConflictException "The specified resource is associated with other resources and cannot be deleted." instead of 200 with an empty body.
