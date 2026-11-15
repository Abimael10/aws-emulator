Issue-Source: trace-sweep-divergence
Issue-Trace: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Issue-Shape-Id: com.amazonaws.omics#Omics
Issue-Key: omics|UploadReadSetPart|2xx->2xx|$.checksum
Issue-Commit: 903fa8a7d793ae0c33731172726d955a24eb2439

# omics UploadReadSetPart returns the part checksum as lowercase hex instead of base64 SHA-256

> _Note: this issue was identified against the code base at commit `903fa8a7d793ae0c33731172726d955a24eb2439`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UploadReadSetPart

## Description
One net-new root cause in the omics direct-upload (multipart read set) flow: the encoding of the part checksum the service hands back.

Cause 1 of 1: UploadReadSetPart's required `checksum` output member is serialized as a lowercase-hex rendering of the SHA-256 digest of the part payload, where AWS HealthOmics returns that digest base64-encoded.

AWS: Smithy (services/service-com-amazonaws-omics-omics/smithy-model/com-amazonaws-omics-omics.json) models UploadReadSetPart -> UploadReadSetPartResponse with a single REQUIRED member `checksum` (target String), documented as "An identifier used to confirm that parts are being added to the intended upload"; the operation doc adds "This operation returns a JSON formatted response containing a string identifier that is used to confirm that parts are being added to the intended upload." The model does not state the encoding, so the requirement here is recorded behavior: the recorded 200 response for a part carries checksum 'fLmPEt5hs+MYSzjCg9nzTr6rsv9YR4q+cCAks+0iQKY=' — a 44-character, '='-padded base64 string, i.e. the 32-byte SHA-256 digest of the part's bytes in standard base64 (the same encoding S3/HealthOmics use for SHA-256 checksums). The digest value itself is NOT the requirement (it is a function of whatever bytes the fixture uploaded); the ENCODING is: base64 of SHA-256 over the part payload, same value echoed by ListReadSetUploadParts for that part and accepted in CompleteMultipartReadSetUpload's `parts[].checksum`.

Emulator: the same request is answered HTTP 200 (status matches) but with $.checksum = '7cb98f12de61b3e3184b38c283d9f34ebeabb2ff58478abe702024b3ed2240a6' — the identical digest rendered as 64 lowercase hex characters. Decoding the recorded base64 gives bytes 7c b9 8f ... , so the hash is computed correctly and only the rendering diverges. The handler calls crate::state::read_set_part_checksum, which is documented as "rendered as lowercase hex" and implemented as hex::encode(Sha256::digest(payload)).

Why: the checksum is the caller's integrity handle on a direct sequence-store upload. A client that digests the bytes it sent and base64-encodes them (what the AWS SDKs and the AWS-documented upload flow do) compares its value against a hex string and concludes every part was corrupted; a client that stores the returned string and later feeds it back as CompleteMultipartReadSetUpload parts[].checksum, or diffs it against ListReadSetUploadParts, carries a value in a shape no real HealthOmics caller or recording produces. The field is required, so every successful part upload is wrong on the wire — nothing about this is per-run or environment-dependent.

Fix: the checksum computation feeding this operation's output — crate::state::read_set_part_checksum (and therefore both UploadReadSetPart's handler and the ListReadSetUploadParts/CompleteMultipartReadSetUpload paths that read the stored ReadSetUploadPart::checksum) must render the SHA-256 digest as standard, padded base64 rather than lowercase hex. Note this is a different function from the workflow definition digest (definition_digest), which is a separate value and must not be changed along with it.

Traces: omics-multipart-read-set-upload-reads-back-uploaded-bytes#task#3 (UploadReadSetPart, verdict mismatch, expected 200 / actual 200, $.checksum value_mismatch). Not covered by any open omics issue: the open omics issues concern StartRun's storageCapacity refusal, DeleteShare's ResourceNotFoundException wording, and the TagResource / UpdateRunGroup / ListMultipartReadSetUploads scaffold stubs.

## Evidence
Fixture omics-multipart-read-set-upload-reads-back-uploaded-bytes, step task#3, operation UploadReadSetPart.
Expected (recorded AWS): HTTP 200, body member checksum = base64 SHA-256 of the uploaded part bytes, e.g. 'fLmPEt5hs+MYSzjCg9nzTr6rsv9YR4q+cCAks+0iQKY=' (44 chars, '='-padded).
Actual (emulator): HTTP 200, checksum = '7cb98f12de61b3e3184b38c283d9f34ebeabb2ff58478abe702024b3ed2240a6' (64 lowercase hex chars).
Replay detail: "$.checksum: value_mismatch (expected 'fLmPEt5hs+MYSzjCg9nzTr6rsv9YR4q+cCAks+0iQKY=', got '7cb98f12de61b3e3184b38c283d9f34ebeabb2ff58478abe702024b3ed2240a6')"; divergence_key service=omics|UploadReadSetPart|status=200->200|keys=$.checksum.
The two strings are the SAME 32 bytes: base64 'fLmP' decodes to 0x7c 0xb9 0x8f, matching the hex prefix '7cb98f'. So only the encoding diverges.
Source: services/service-com-amazonaws-omics-omics/src/ops/upload_read_set_part.rs:268 (let checksum = crate::state::read_set_part_checksum(&input.payload);) and services/service-com-amazonaws-omics-omics/src/state.rs:1557-1560 (hex::encode(sha2::Sha256::digest(payload)), doc comment "rendered as lowercase hex").
Model: smithy_query(operation="UploadReadSetPart") — output { checksum: String, required }, http PUT /sequencestore/{sequenceStoreId}/upload/{uploadId}/part, success 200.

## Steps to Reproduce
1. Replay fixture omics-multipart-read-set-upload-reads-back-uploaded-bytes against the emulator (steps task#1..task#3): CreateSequenceStore, CreateMultipartReadSetUpload, then UploadReadSetPart.
2. At step task#3 issue PUT /sequencestore/{sequenceStoreId}/upload/{uploadId}/part?partSource=SOURCE1&partNumber=1 with the part bytes as the payload.
3. Observe the 200 response body: checksum is 64 lowercase hex characters.
4. Base64-encode the SHA-256 digest of the bytes you sent and compare: AWS (and the recording) return that base64 string; the emulator returns the hex rendering of the same digest, so the comparison fails.
