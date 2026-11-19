Issue-Source: trace-sweep-divergence
Issue-Trace: braket-spending-limit-lifecycle
Issue-Step: task#5
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: braket-spending-limit-lifecycle
Issue-Shape-Id: com.amazonaws.braket#Braket
Issue-Key: braket|TagResource|2xx->2xx|$headers.content-length
Issue-Key: braket|UntagResource|2xx->2xx|$headers.content-length
Issue-Commit: 8158354380cf2730bfcf967251c7e659abcc621b

# Braket 204 No Content answers omit the content-length header on both tag mutations

> _Note: this issue was identified against the code base at commit `8158354380cf2730bfcf967251c7e659abcc621b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Two net-new braket divergences, both the SAME framing fault on the service's two tag mutations: a success that answers HTTP 204 No Content reaches the client with NO `content-length` header at all, where the recorded AWS answer carries `content-length: 0`. The status, the empty body and the error paths are all correct; only the framing header is missing. A fixer must close BOTH causes before submitting.

Neither is covered by an open braket issue: 20261103T044954Z-braket-read-paths-serialize-response-bodies-wrongly-spending.md covers SearchSpendingLimits body serialization and the GetDevice capability document, 20261111T122945Z-braket-cloudformation-braket-searchspendinglimits-createdat-is-served-as-an-epoch.md covers SearchSpendingLimits' createdAt form, and 20261112T060434Z-s3-braket-quantum-task-results-object-served-by-s3.md covers the S3 result object. None of them touches response framing on the tagging doors.

Cause 1 of 2 — TagResource's 204 answer carries no `content-length` (fixture braket-spending-limit-lifecycle, step task#5).
AWS: Smithy (services/service-com-amazonaws-braket-braket/smithy-model/com-amazonaws-braket-braket.json) models TagResource as POST /tags/{resourceArn} with output shape TagResourceResponse declaring NO members, and leaves the success code defaulted (codeDefaulted: true). Recorded behavior only for the status and the framing: the recording shows Amazon Braket answering a successful tag write HTTP 204 with an empty body and the header `content-length: 0`. A 204 has no body (RFC 9110 15.3.5), and AWS still states the zero length explicitly, so the required response is status 204, no body bytes, and `content-length: 0`.
Emulator: status 204 and an empty body are served correctly, but the replayed response has NO `content-length` header ($headers.content-length: missing — expected '0', got None). The service crate does compute it: TagResource declares SUCCESS_STATUS = service_common::no_content_responses::NO_CONTENT_STATUS and every response passes through NoContentDateTimeRestJson::serialize_output -> without_content, which inserts `content-length: 0`. The header is then dropped BELOW the service crate — services/common/src/no_content_responses.rs:34-35 states the condition outright: "An answer served over HTTP/1.1 reaches the client without the framing header — the HTTP writer the vendored transport uses drops `content-length` from a 204." So the correction is made and then discarded on the way out.
Why: a client, proxy or contract check that reads the response framing sees a 204 that declares no length where AWS declares zero. HTTP-level conformance checks and strict proxies treat a 204 without `content-length` differently from one with `content-length: 0` (connection framing/caching decisions), and any test asserting the recorded header set against the emulator fails on a response the emulator otherwise got right.
Fix: the emulator's HTTP response writer in the transport layer — the vendored HTTP/1.1 writer that strips `content-length` from a 204 — must preserve the header the serializer set, rather than the per-service seam. service_common::no_content_responses::without_content is already correct and already unit-tested (services/common/src/no_content_responses.rs:96-126); nothing in the braket operation handler or its state is at fault.
Traces: braket-spending-limit-lifecycle#task#5.

Cause 2 of 2 — UntagResource's 204 answer carries no `content-length` (fixture braket-spending-limit-lifecycle, step task#7).
AWS: Smithy models UntagResource as DELETE /tags/{resourceArn} with `tagKeys` bound @httpQuery, output shape UntagResourceResponse declaring NO members, success code defaulted, and the operation marked @idempotent. Recorded behavior only for the framing: the recording shows a successful untag answered HTTP 204 with an empty body and `content-length: 0`, exactly as TagResource.
Emulator: same divergence — status 204 and empty body correct, `$headers.content-length: missing (expected '0', got None)`. Same path: UntagResource's SUCCESS_STATUS is the shared NO_CONTENT_STATUS and the response is framed by the same without_content seam before the transport drops the header.
Why: same as cause 1 — the removal side of tagging is equally unusable as a framing reference, so a client that round-trips tag/untag sees neither answer state its length.
Fix: same single component as cause 1 — the transport's HTTP response writer. The two causes are listed separately only because they are two distinct operations and two distinct replay steps; one correction in the writer closes both.
Traces: braket-spending-limit-lifecycle#task#7.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator up; /__admin/health answered 200 and the :39051 data listener accepts): 39 steps total, 33 passed, 6 mismatched, 0 errored, 0 blocked, score 84.6%, 4 divergence clusters. Two clusters are owned by an open issue (GetDevice $.deviceCapabilities and SearchSpendingLimits amounts); the two below are net-new, neither stub-blocked nor owned.

Cause 1 — fixture braket-spending-limit-lifecycle, step task#5, operation TagResource, service braket, verdict mismatch, expected status 204 / actual status 204:
  divergence_key: service=braket|TagResource|status=204->204|keys=$headers.content-length
  detail: $headers.content-length: missing (expected '0', got None)
Expected: HTTP/1.1 204, empty body, header `content-length: 0`.
Actual:   HTTP/1.1 204, empty body, NO `content-length` header.

Cause 2 — fixture braket-spending-limit-lifecycle, step task#7, operation UntagResource, service braket, verdict mismatch, expected status 204 / actual status 204:
  divergence_key: service=braket|UntagResource|status=204->204|keys=$headers.content-length
  detail: $headers.content-length: missing (expected '0', got None)
Expected: HTTP/1.1 204, empty body, header `content-length: 0`.
Actual:   HTTP/1.1 204, empty body, NO `content-length` header.

The expected value '0' is a fixed literal for this response class, not a per-run value: it is the length of an absent body and is the same for every 204 either operation serves. Corroborating code evidence that the emulator sets it and then loses it: services/service-com-amazonaws-braket-braket/src/service.rs:175-186 (every serialized response handed through without_content), services/service-com-amazonaws-braket-braket/src/ops/tag_resource.rs:141-144 (SUCCESS_STATUS = NO_CONTENT_STATUS), services/common/src/no_content_responses.rs:62-79 (inserts content-length: 0) and services/common/src/no_content_responses.rs:34-35 (records that the vendored HTTP/1.1 writer drops it from a 204).

## Steps to Reproduce
1. git clone the repository and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Cause 1: fixture braket-spending-limit-lifecycle, step task#5 — POST /tags/{resourceArn} (TagResource) against a Braket resource ARN that exists in the store, with a valid tags map. Observe HTTP 204 with an empty body but no `content-length` response header, where the recorded AWS answer carries `content-length: 0`.
4. Cause 2: fixture braket-spending-limit-lifecycle, step task#7 — DELETE /tags/{resourceArn}?tagKeys=<key> (UntagResource) against the same resource. Observe the same HTTP 204 with an empty body and no `content-length` header.
5. Inspect the raw response headers (e.g. curl -i / -D -) rather than an SDK's parsed output: the SDK hides the framing, so the header set must be read off the wire.
