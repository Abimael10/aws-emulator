Issue-Source: trace-sweep-divergence
Issue-Trace: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Issue-Shape-Id: com.amazonaws.omics#Omics
Issue-Key: omics|GetReadSet|2xx->2xx|$headers.content-type
Issue-Commit: e0fb8928b55d063818ff45281a7e96a54cacfd74

# omics GetReadSet serves the read-set part with Content-Type application/octet-stream instead of application/json

> _Note: this issue was identified against the code base at commit `e0fb8928b55d063818ff45281a7e96a54cacfd74`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetReadSet

## Description
Cause 1 of 1: the `Content-Type` response header on a successful `GetReadSet` read is wrong.

AWS: recorded behavior only. The Smithy model (`com.amazonaws.omics#GetReadSet`) binds the response to `GetReadSetResponse$payload` -> `ReadSetStreamingBlob`, a `@streaming` blob marked `@httpPayload`, and that blob carries NO `@mediaType` trait, so the model itself states no content type. The recorded AWS response for a 200 `GET /sequencestore/{sequenceStoreId}/readset/{id}?file=...&partNumber=...` carries `content-type: application/json` alongside the part's bytes — HealthOmics' storage endpoint labels the read-set part response `application/json` rather than deriving a content type from the blob payload. That recorded header is the requirement here; the status (200) and the body bytes are already correct.

Emulator: the same 200 response is served with `content-type: application/octet-stream`. That is the restJson1 default a serializer picks for an un-`@mediaType`'d `@httpPayload` streaming blob, i.e. the emulator is deriving the header from the shape instead of emitting the content type the service actually sends. Replay detail, verbatim: `$headers.content-type: value_mismatch (expected 'application/json', got 'application/octet-stream')`.

Why: a client that dispatches on the response content type sees a different media type from the one AWS sends. Anything that sniffs `Content-Type` to decide how to frame, buffer, or route the read-set bytes (SDK response handlers wired per media type, HTTP proxies and caches keyed on content type, genomics pipelines that assert the header before streaming a part to disk) takes the non-AWS branch against the emulator and the AWS branch against the real service, so the behavior under test is not the behavior in production. Header-only divergence: the payload bytes themselves round-trip correctly.

Fix: the `GetReadSet` response serialization path — the response content type must be pinned to `application/json` for this operation rather than defaulted from the `@httpPayload` streaming blob's (absent) `@mediaType`. The handler itself is correct and needs no change: it returns the stored part's bytes verbatim. Note that the open issue `20261121T101952Z-omics-getreadset-stub.md` ("Implement omics GetReadSet: unimplemented scaffold stub") does NOT cover this: that issue describes a 501 scaffold, whereas the operation now answers 200 with the right bytes and only the response header diverges.

Traces: fixture `omics-multipart-read-set-upload-reads-back-uploaded-bytes`, step `task#6` (`GetReadSet`, expected 200 / actual 200, header mismatch).

## Evidence
Fixture: omics-multipart-read-set-upload-reads-back-uploaded-bytes
Step: task#6
Operation: GetReadSet (GET /sequencestore/{sequenceStoreId}/readset/{id})
Verdict: mismatch (expected_status 200, actual_status 200)

Replay detail:
  $headers.content-type: value_mismatch (expected 'application/json', got 'application/octet-stream')

Expected (recorded AWS): HTTP 200, header `content-type: application/json`, body = the uploaded part's bytes.
Actual (emulator):        HTTP 200, header `content-type: application/octet-stream`, body = the uploaded part's bytes (correct).

Model evidence: smithy_query(operation="GetReadSet") -> output `{payload: ReadSetStreamingBlob, httpPayload: true, streaming: true}`; smithy_query(shape="ReadSetStreamingBlob") -> `{type: blob, streaming: true}` with no `mediaType` trait, so the content type is not model-derivable and must follow the recorded service behavior.

Handler: services/service-com-amazonaws-omics-omics/src/ops/get_read_set.rs returns `Ok(GetReadSetOutput { payload })` (line 276) with no content-type control; the header is emitted by the response serializer.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Create a sequence store, start a multipart read-set upload, upload a part, and complete the upload so the read set reaches ACTIVE (as fixture `omics-multipart-read-set-upload-reads-back-uploaded-bytes` does in its earlier steps).
3. Call GetReadSet for that read set with `file=SOURCE1` and `partNumber=1` (step `task#6` of the fixture).
4. Inspect the response headers: the emulator returns `content-type: application/octet-stream`; the recorded AWS response returns `content-type: application/json`. The status and body bytes match.
5. Or replay the corpus: replay_wire_corpus() reports this cluster as fixture `omics-multipart-read-set-upload-reads-back-uploaded-bytes`, step `task#6`.
