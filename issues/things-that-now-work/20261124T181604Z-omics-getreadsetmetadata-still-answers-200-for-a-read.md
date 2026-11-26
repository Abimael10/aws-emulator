Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: omics.batch-delete-read-set-partial-failure
Issue-Shape-Id: com.amazonaws.omics#Omics
Issue-Key: omics.batch-delete-read-set-partial-failure|_client_error#15|AssertionError|raise AssertionError(f"expected this call to be rejected, but it SUCCEEDED and returned {result!r}")
Issue-Commit: c350efbe81f21d0544162e94560fe01dc4519b26

# omics GetReadSetMetadata still answers 200 for a read set deleted by BatchDeleteReadSet instead of 404 ResourceNotFoundException "The specified readset does not exist."

> _Note: this issue was identified against the code base at commit `c350efbe81f21d0544162e94560fe01dc4519b26`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetReadSetMetadata

## Description
Cause 1 of 1: a read set that BatchDeleteReadSet removed is left readable through GetReadSetMetadata as a tombstone with status DELETED, instead of being gone.

Failing check ids for this cause: omics.batch-delete-read-set-partial-failure.

AWS: GetReadSetMetadata is GET /sequencestore/{sequenceStoreId}/readset/{id}/metadata (Smithy: com.amazonaws.omics#GetReadSetMetadata, success code 200, modeled errors AccessDeniedException 403, InternalServerException 500, RequestTimeoutException 408, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400). The model does not state the post-delete lifecycle, so for that part this is recorded behavior only: after BatchDeleteReadSet (POST /sequencestore/{sequenceStoreId}/readset/batch/delete, 200) reports NO per-id error for a read set, that read set's metadata is gone outright — it does not linger in a DELETING/DELETED state that GetReadSetMetadata will still serve. The recording shows the subsequent GetReadSetMetadata on the deleted id refused with HTTP 404, error code `ResourceNotFoundException` and message `The specified readset does not exist.` That message is entirely fixed text — it carries no id, name or other value from the request or from state, so the emulator must emit it verbatim with no interpolation.

Emulator: the same call succeeds. BatchDeleteReadSet does remove the id from ListReadSets (the check observes the read set disappear from ListReadSets within its poll window), but GetReadSetMetadata for that id then returns HTTP 200 with a full metadata body whose `status` is `DELETED` (replay shows `{'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'id': <readSetId>, 'arn': 'arn:aws:omics:<region>:<account>:sequenceStore/<storeId>/readSet/<readSetId>', 'sequenceStoreId': <storeId>, 'status': 'DELETED', 'name': ..., 'fileType': 'FASTQ', 'creationTime': ..., 'sequenceInformation': {...}, 'creationType': 'UPLOAD', 'etag': {...}}` — the ids, names and timestamps there are per-run values, the divergence is the 200-with-DELETED-body where AWS refuses 404). So the emulator's delete marks the read-set record DELETED and hides it from the list projection rather than making the per-read-set metadata lookup not-found.

Why: a client that confirms a delete, or that distinguishes "gone" from "present" by catching ResourceNotFoundException on GetReadSetMetadata, concludes against the emulator that the read set still exists. Delete-then-verify loops never terminate (or report the resource as undeleted), cleanup code that swallows ResourceNotFoundException instead sees a success and may re-delete or misreport, and any code branching on `status` now has to handle a DELETED value AWS never serves from this operation. Conversely, code written against the emulator that reads deleted read-set metadata breaks against real AWS with a 404.

Fix: the omics read-set state's delete path and the GetReadSetMetadata lookup in the omics crate — BatchDeleteReadSet must remove the read set from the store's read-set state (or mark it such that every per-id lookup, not just the list projection, treats it as absent), and GetReadSetMetadata must answer 404 ResourceNotFoundException with the fixed message `The specified readset does not exist.` for an id that is syntactically valid but not present in the named sequence store. Relevant components are the BatchDeleteReadSet and GetReadSetMetadata handlers and the sequence-store/read-set state in services/service-com-amazonaws-omics-omics/src/ops/ and src/state.rs.

Traces: omics.batch-delete-read-set-partial-failure (creates a sequence store, uploads one read set to ACTIVE, BatchDeleteReadSet([real_id, bogus_id]), then GetReadSetMetadata(real_id)).

No open issue covers this: 20261124T171120Z-omics-getreadsetmetadata-returns-a-malformed-sequence-store-s3.md is about the s3Access.s3Uri access-point alias in a 200 body, not the post-delete not-found behavior; 20261121T101952Z-omics-batchdeletereadset-stub.md is the stub-implementation issue for BatchDeleteReadSet (which now answers and reports the per-id error correctly in this check), and the remaining open omics issues cover GetReadSet, DeleteRunGroup, AbortMultipartReadSetUpload, StartRun/CreateWorkflow and AWS::Omics::* in CloudFormation.

## Evidence
Failing check: omics.batch-delete-read-set-partial-failure (verdict: mismatch).

The check gets as far as asserting, successfully, that BatchDeleteReadSet reports a per-id error for the never-created id and none for the real one, and that the real id disappears from ListReadSets. It then fails at:

  code, message = _client_error(lambda: omics.get_read_set_metadata(sequenceStoreId=store_id, id=real_id))
  AssertionError: expected this call to be rejected, but it SUCCEEDED and returned {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', ...}}, 'id': '3000000000', 'arn': 'arn:aws:omics:us-west-2:000000000000:sequenceStore/1000000000/readSet/3000000000', 'sequenceStoreId': '1000000000', 'subjectId': 'sim2real-subject', 'sampleId': 'sim2real-sample', 'status': 'DELETED', 'name': 'sim2real-readset-eeee0fcf7f4a', 'fileType': 'FASTQ', 'creationTime': datetime(...), 'sequenceInformation': {'totalReadCount': 1, 'totalBaseCount': 100, 'alignment': 'UNALIGNED'}, 'creationType': 'UPLOAD', 'etag': {'algorithm': 'FASTQ_MD5up'}}

Expected (what the check asserts AWS does, from the recording):
  HTTP 404, Error.Code == "ResourceNotFoundException", Error.Message == "The specified readset does not exist."

Actual (emulator):
  HTTP 200 with a metadata body, status == "DELETED", no error.

Message requirement breakdown: "The specified readset does not exist." is 100% fixed text; no part of it is taken from the request or from state (the read set id, store id and name do NOT appear in it). The ids/names/timestamps quoted above (3000000000, 1000000000, sim2real-readset-eeee0fcf7f4a, the creationTime, the account and region in the arn) are per-run values and are not requirements.

Smithy evidence: com.amazonaws.omics#GetReadSetMetadata — http GET /sequencestore/{sequenceStoreId}/readset/{id}/metadata, code 200; errors include ResourceNotFoundException (errorKind client, httpError 404). com.amazonaws.omics#BatchDeleteReadSet — http POST /sequencestore/{sequenceStoreId}/readset/batch/delete, code 200, output member `errors` (ReadSetBatchErrorList), i.e. per-id failures are reported in the 200 body, so an id reported WITHOUT an error must actually be deleted.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. CreateSequenceStore(name=<unique>) and note the returned store id.
3. Create one ACTIVE read set in that store: CreateMultipartReadSetUpload(sourceFileType=FASTQ, subjectId, sampleId, name), UploadReadSetPart(partSource=SOURCE1, partNumber=1, payload=<gzipped FASTQ>), CompleteMultipartReadSetUpload(parts=[{partNumber:1, partSource:SOURCE1, checksum:<from UploadReadSetPart>}]); poll GetReadSetMetadata until status == ACTIVE.
4. Call BatchDeleteReadSet(sequenceStoreId=<store id>, ids=[<real read set id>, '99999999999999999999']). Observe the 200 body reports an error entry for the bogus id only.
5. Poll ListReadSets(sequenceStoreId=<store id>) until the real id is absent — it is.
6. Call GetReadSetMetadata(sequenceStoreId=<store id>, id=<real read set id>).
7. Observed: HTTP 200 with a full metadata body whose status is DELETED. Expected: HTTP 404 ResourceNotFoundException with the fixed message "The specified readset does not exist."
