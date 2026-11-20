Issue-Source: trace-sweep-divergence
Issue-Trace: xray-put-segment-is-found-by-trace-summaries-and-batch-get
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: xray-put-segment-is-found-by-trace-summaries-and-batch-get
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray|BatchGetTraces|2xx->2xx|$.Traces[*].Segments[*].Document
Issue-Commit: 98d9083befffa5463a97d216ae9f7eb2c391175c

# xray BatchGetTraces re-serializes the stored segment document instead of returning it verbatim, overwriting submitted start_time/end_time

> _Note: this issue was identified against the code base at commit `98d9083befffa5463a97d216ae9f7eb2c391175c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetTraces

## Description
One cause.

Cause 1 of 1: BatchGetTraces does not round-trip the segment document that PutTraceSegments ingested.

AWS: Smithy (services/service-com-amazonaws-xray-awsxray) models PutTraceSegments.TraceSegmentDocuments as a list of opaque strings (TraceSegmentDocumentList) and BatchGetTraces -> Traces[].Segments[].Document as a String whose documentation is only "The segment document." The document is an opaque blob to X-Ray's API surface: AWS stores the JSON text the caller submitted and hands that same text back on BatchGetTraces. Nothing in the model gives the service permission to re-encode it, re-order its keys, or recompute any field inside it — in particular `start_time` and `end_time`, which are caller-supplied epoch seconds describing when the traced work happened, not when the segment was ingested. Recorded behavior confirms this: the fixture submits a segment document and BatchGetTraces returns that exact string, including its original member order and its numeric formatting (`"start_time":1.793915525E9`).

Emulator: BatchGetTraces returns a document the emulator re-serialized from a parsed internal representation rather than the submitted text. Status is correct (200 -> 200) and the surrounding Trace/Segment shape is correct, but $.Traces[0].Segments[0].Document differs in three ways: (a) member order is rewritten (`trace_id` hoisted ahead of `id`/`name`, `http.request.method` ahead of `url`), (b) the JSON is pretty-spaced (`": "` separators) where the submitted text had none, and (c) the caller-supplied epoch values in `start_time`/`end_time` are replaced with the emulator's own ingest clock — the recorded step shows the submitted pair (start 1.793915525E9 / end 1.793915527E9, scientific-notation form) coming back as two unrelated integers taken from the replay run's wall clock, with the 2-second duration happening to survive only because both ends were restamped together.

Why: a client that reads a segment back is reading a document it must be able to trust byte-for-byte. Anything that checksums, signs, diffs or caches the document by its text (and any test asserting a submitted segment round-trips) fails on the re-encoding alone. The timestamp rewrite is worse than cosmetic: segment start/end times are the only source of a span's wall-clock position and latency, so every consumer that computes service latency, builds a timeline, correlates a segment with a parent/subsegment by time, or back-fills historical segments sees the data relocated to ingest time. Back-filled or delayed segments — the normal case for batch and agent-buffered submission — are silently dated wrong, and any annotation-driven analysis joined on time windows no longer matches.

Fix: the BatchGetTraces path's segment state/serializer. The segment document must be persisted as the exact string received by PutTraceSegments and emitted unchanged — not parsed into a map and re-encoded on read, and with no field inside the document (least of all start_time/end_time) derived from the server clock. Parsing for indexing (trace id, service name, annotations, time window used by GetTraceSummaries/GetServiceGraph) is fine, but it must be kept alongside the original text, not replace it.

Traces: xray-put-segment-is-found-by-trace-summaries-and-batch-get#task#3 (the preceding PutTraceSegments step in the same fixture supplies the document).

No open xray issue covers this: the nearest ones concern group responses, GetTraceSummaries pagination/metadata and CreateSamplingRule wording, none of which touch the BatchGetTraces segment document.

## Evidence
Fixture xray-put-segment-is-found-by-trace-summaries-and-batch-get, step task#3, operation BatchGetTraces. Expected status 200, actual status 200; the divergence is confined to $.Traces[0].Segments[0].Document (verdict: mismatch).

Expected (verbatim, as submitted by the fixture's PutTraceSegments step):
{"id":"5d3543ca08de7483","name":"sim2real-probe-xray-svc-779b4368","start_time":1.793915525E9,"trace_id":"1-a094db89-9f96a3da0ec16a5d54b48487","end_time":1.793915527E9,"http":{"request":{"url":"https://example.com/orders/o-1001","method":"GET"},"response":{"status":200}},"annotations":{"order_id":"o-1001"}}

Actual (emulator):
{"trace_id": "1-a094db89-9f96a3da0ec16a5d54b48487", "id": "5d3543ca08de7483", "name": "sim2real-probe-xray-svc-779b4368", "start_time": 1795147186, "end_time": 1795147188, "http": {"request": {"method": "GET", "url": "https://example.com/orders/o-1001"}, "response": {"status": 200}}, "annotations": {"order_id": "o-1001"}}

What is a requirement vs. what is a per-run value: the requirement is that the returned Document string is IDENTICAL to the string submitted to PutTraceSegments for that segment — same member order, same separators/whitespace, same numeric lexical form, and the same start_time/end_time the caller sent. The specific trace id, segment id, service name and epoch numbers above are this recording's values filling that template; the defect is that the emulator's output is a re-encoding at all, and that its start_time/end_time come from the server's ingest clock instead of the submitted document (here 1795147186/1795147188 in place of the submitted 1.793915525E9/1.793915527E9).

## Steps to Reproduce
1. Replay fixture xray-put-segment-is-found-by-trace-summaries-and-batch-get against the emulator (trace set simparity-wire-aws-v1).
2. The fixture's earlier step calls PutTraceSegments with one segment document whose text is {"id":...,"name":...,"start_time":1.793915525E9,"trace_id":"1-a094db89-9f96a3da0ec16a5d54b48487","end_time":1.793915527E9,"http":{...},"annotations":{...}} (compact, no spaces, members in that order).
3. At step task#3, call BatchGetTraces with TraceIds=["1-a094db89-9f96a3da0ec16a5d54b48487"].
4. Compare $.Traces[0].Segments[0].Document in the 200 response against the exact string submitted in step 2.
5. Observe it is not the same string: members are re-ordered, ": " spacing is introduced, and start_time/end_time hold the emulator's ingest-time epoch seconds instead of the submitted 1.793915525E9/1.793915527E9.
