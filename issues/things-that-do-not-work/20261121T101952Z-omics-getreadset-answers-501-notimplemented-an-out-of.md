Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: omics.get-read-set-range-not-satisfiable-rejected
Issue-Shape-Id: com.amazonaws.omics#Omics
Issue-Key: omics.get-read-set-range-not-satisfiable-rejected|run#5|AssertionError|assert code == "ResourceNotFoundException", f"expected ResourceNotFoundException, got {code!r}"
Issue-Commit: 41dd8320c6bf5491a3e0066243f821165de79b27

# omics GetReadSet answers 501 NotImplemented — an out-of-range partNumber must be refused 404 ResourceNotFoundException and partNumber=1 must stream the stored part verbatim

> _Note: this issue was identified against the code base at commit `41dd8320c6bf5491a3e0066243f821165de79b27`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetReadSet

## Description
Cause 1 of 1: `GetReadSet` is still an unimplemented scaffold stub, so every call — both the refusal path and the positive path — comes back as HTTP 501 `NotImplemented` instead of the modeled behavior.

Related open/auto-filed work item: the suite also marks `omics.multipart-read-set-upload-round-trip` as ALREADY FILED, blocked by the UNIMPLEMENTED `GetReadSet` scaffold stub. That stub issue names the operation and its handler file; THIS issue records the response-fidelity requirements the stub issue does not state, and both close together once `GetReadSet` is implemented. Filed rather than skipped because the suite did not mark this check as covered.

AWS:      Smithy (services/service-com-amazonaws-omics-omics/smithy-model/com-amazonaws-omics-omics.json): `GetReadSet` is `GET /sequencestore/{sequenceStoreId}/readset/{id}` with host prefix `storage-`, success 200, required httpQuery member `partNumber` (Integer, @range min 1 max 10000), required httpLabel members `sequenceStoreId` and `id` (both @length 10..36, @pattern ^[0-9]+$), optional httpQuery `file` (ReadSetFile: INDEX|SOURCE1|SOURCE2), and an @httpPayload streaming blob output member `payload`. Modeled errors include `ResourceNotFoundException` (HTTP 404, "The target resource was not found in the current Region") and `RangeNotSatisfiableException` (HTTP 416). The model declares no `NotImplemented` error for this operation.
              Recorded behavior: for a read set whose upload completed with a single part, a `partNumber` greater than the read set's actual part count is refused with HTTP 404, error code `ResourceNotFoundException`, and — for this refusal class — a message that is exactly the error code string `ResourceNotFoundException` (fixed text; it carries no variable part). AWS does NOT use 416 `RangeNotSatisfiableException` here: the part simply does not exist on this read set. In the same recording, `partNumber=1` succeeds with 200 and streams the stored part byte-for-byte as uploaded — the service does not decompress it, so a gzip-compressed FASTQ part comes back still gzip-compressed.

Emulator: `GetReadSet::handle` is the generated stub and unconditionally returns `crate::scaffolded::Scaffolded::not_implemented("GetReadSet")`, i.e. HTTP 501 with error code `NotImplemented`. The replay asserts on the refusal and gets `AssertionError: expected ResourceNotFoundException, got 'NotImplemented'`. The positive control (`partNumber=1` streaming the stored bytes) is never reached, and `file`/`partNumber` are not consulted at all.

Why:      A client cannot read any read-set data from the emulator. Error handling diverges in both directions: code that catches `ResourceNotFoundException` to probe how many parts a read set has (a common pattern for iterating parts) instead sees an unmodeled `NotImplemented`, which botocore surfaces as a non-retryable unknown-code client error rather than the modeled 404, so part-walking loops and "does this part exist" checks break. Anything that downloads a read set and then gunzips the payload itself gets no payload at all, and a 501 also hides the distinction AWS draws between a missing part (404) and a genuinely bad byte range (416).

Fix:      Implement the `GetReadSet` handler: resolve `sequenceStoreId`/`id` against the sequence-store and read-set state (404 `ResourceNotFoundException` when either is absent), then resolve the requested part — when `partNumber` exceeds the part count stored for the read set's selected `file`/part source, return the modeled `ResourceNotFoundException` with the message AWS uses for this class, and serialize the stored part bytes unchanged through the streaming `payload` httpPayload member on 200. Remove the SCAFFOLDED marker line from the handler when done.

Traces:   omics.get-read-set-range-not-satisfiable-rejected (mismatch). Handler: services/service-com-amazonaws-omics-omics/src/ops/get_read_set.rs:175 (the `not_implemented` return); modeled errors enumerated at lines 99-110 of the same file.

## Evidence
Failing check: omics.get-read-set-range-not-satisfiable-rejected [mismatch]

  File "check_omics_get_read_set_range_not_satisfiable_rejected.py", line 25, in run
    assert code == "ResourceNotFoundException", f"expected ResourceNotFoundException, got {code!r}"
  AssertionError: expected ResourceNotFoundException, got 'NotImplemented'

Expected (real AWS, per the recording and the Smithy model):
  GetReadSet(sequenceStoreId=<store>, id=<readSet>, partNumber=2) on a one-part read set
    -> HTTP 404, Error.Code = "ResourceNotFoundException", Error.Message = "ResourceNotFoundException"
       (the message for this refusal class is exactly the code string; no request/state values are interpolated)
  GetReadSet(sequenceStoreId=<store>, id=<readSet>, partNumber=1)
    -> HTTP 200, payload streams the stored part verbatim (still gzip-compressed as uploaded)

Actual (emulator):
  GetReadSet(..., partNumber=2) -> HTTP 501, Error.Code = "NotImplemented"
  GetReadSet(..., partNumber=1) -> HTTP 501, Error.Code = "NotImplemented" (never reached by the check,
       which aborts on the first assertion)

Source: services/service-com-amazonaws-omics-omics/src/ops/get_read_set.rs
  line 4:   //! SCAFFOLDED, NOT IMPLEMENTED: this operation answers HTTP 501 `NotImplemented`
  line 175: Err(crate::scaffolded::Scaffolded::not_implemented("GetReadSet"))

## Steps to Reproduce
1. CreateSequenceStore(name=<unique>, tags={"sim2real":"true"}) -> note `id` as <store>.
2. CreateMultipartReadSetUpload(sequenceStoreId=<store>, sourceFileType="FASTQ", subjectId=..., sampleId=..., name=<unique>) -> <uploadId>.
3. UploadReadSetPart(sequenceStoreId=<store>, uploadId=<uploadId>, partSource="SOURCE1", partNumber=1, payload=gzip(b"@sim2real-read1\nACGTACGTAC\n+\nFFFFFFFFFF\n")) -> note `checksum`.
4. CompleteMultipartReadSetUpload(sequenceStoreId=<store>, uploadId=<uploadId>, parts=[{partNumber:1, partSource:"SOURCE1", checksum:<checksum>}]) -> <readSetId>.
5. Poll GetReadSetMetadata(sequenceStoreId=<store>, id=<readSetId>) until status == "ACTIVE".
6. Call GetReadSet(sequenceStoreId=<store>, id=<readSetId>, partNumber=2).
   Expected: ClientError with HTTP 404, code "ResourceNotFoundException", message "ResourceNotFoundException".
   Actual:   ClientError with HTTP 501, code "NotImplemented".
7. Call GetReadSet(sequenceStoreId=<store>, id=<readSetId>, partNumber=1).
   Expected: HTTP 200; gzip.decompress(payload.read()) == b"@sim2real-read1\nACGTACGTAC\n+\nFFFFFFFFFF\n".
   Actual:   HTTP 501 "NotImplemented".

Replay: run_behavioral_suite, check id omics.get-read-set-range-not-satisfiable-rejected.
