Issue-Source: trace-sweep-divergence
Issue-Trace: xray-put-segment-is-found-by-trace-summaries-and-batch-get
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: xray-put-segment-is-found-by-trace-summaries-and-batch-get
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray|BatchGetTraces|2xx->2xx|$.Traces[*].Segments[*].Document
Issue-Commit: d320d15df1e8a4ddf926cc8f8c86b65a79910efa

# xray BatchGetTraces re-serializes the stored segment document instead of returning it verbatim

> _Note: this issue was identified against the code base at commit `d320d15df1e8a4ddf926cc8f8c86b65a79910efa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetTraces

## Description
Cause 1 of 1: BatchGetTraces returns Traces[].Segments[].Document as a re-encoded JSON rendering of the ingested segment rather than the exact document string that was submitted to PutTraceSegments.

AWS: Smithy: BatchGetTraces -> BatchGetTracesResult.Traces (TraceList of Trace); Trace.Segments (SegmentList of Segment); Segment.Document targets SegmentDocument with @length min 1, documented as "The segment document." and "The segment can be compiled from documents uploaded with PutTraceSegments". Segment.Document is an opaque string, not a modeled structure: X-Ray stores the segment document the client uploaded in PutTraceSegmentsRequest.TraceSegmentDocuments and hands that same document text back. Recorded behavior confirms it: the recorded Document is character-for-character the document the fixture PUT - same member order (id, name, start_time, trace_id, end_time, http, annotations), the same compact separators with no space after ':' or ',', and the same numeric lexical forms the client emitted (the epoch seconds appear as the JSON number literal 1.793915525E9, i.e. the producer's double rendering, not an integer). HTTP status for the step is 200 on both sides; no error code is involved.

Emulator: the emulator answers 200 but serves a Document it rebuilt itself. On the same step it returned a document whose members are ordered trace_id, id, name, start_time, end_time, http, annotations, formatted with a space after every ':' and ',', and whose start_time/end_time are emitted as bare integers rather than the double literal the client submitted. The nested objects are likewise reordered (http.request comes back as {"method", "url"} where the submitted document had {"url", "method"}). This is the signature of parsing the uploaded document into a map and re-encoding it on read instead of storing the original bytes. (The absolute epoch numbers differ between the recording and the replay because the fixture derives them from the run clock - that per-run value difference is NOT the finding; the ordering, whitespace and numeric-form changes are.)

Why: a client relying on AWS's behavior treats Segment.Document as the opaque payload it uploaded. Anything that hashes, diffs, signs or byte-compares a retrieved segment document against what it sent, or that depends on member order or on float-valued start_time/end_time surviving the round trip, sees a mismatch that never occurs against real X-Ray. Consumers that parse the document with a schema expecting fractional second timestamps (the X-Ray segment-document schema types start_time/end_time as numbers with sub-second precision) get integer-truncated values back, so sub-second timing information submitted by an instrumented client is silently lost on readback.

Fix: belongs in the X-Ray segment-ingest state and the BatchGetTraces serializer - PutTraceSegments should retain each TraceSegmentDocuments entry as the original string alongside whatever indexed fields it extracts for trace summaries/service graph, and BatchGetTraces should emit that retained string as Segment.Document rather than re-encoding a parsed representation.

No existing open xray issue covers this: the open BatchGetTraces/GetTraceSummaries issues (20261116T082341Z-xray-gettracesummaries-nexttoken-is-not-an-opaque-aws.md, 20261119T180833Z-xray-gettracesummaries-response-metadata-diverges-approximatetime-is-a.md, 20261022T142349Z-xray-x-ray-group-and-trace-segment-destination-responses.md) concern pagination tokens, ApproximateTime and group responses, not segment-document fidelity.

Traces: xray-put-segment-is-found-by-trace-summaries-and-batch-get#task#3.

## Evidence
Replay cluster service=xray|BatchGetTraces|status=200->200|keys=$.Traces[0].Segments[0].Document, verdict=mismatch, expected_status 200, actual_status 200, affected_count 1.

Detail: $.Traces[0].Segments[0].Document: value_mismatch
  expected (verbatim uploaded document): {"id":"2f44de08d6fd8715","name":"sim2real-probe-xray-svc-df3a98ab","start_time":1.793915525E9,"trace_id":"1-ebf50f9d-f602d3ed401e1fe5a393f953","end_time":1.793915527E9,"http":{"request":{"url":"https://example.com/orders/o-1001","method":"GET"},"response":{"status":200}},"annotations":{"order_id":"o-1001"}}
  actual (emulator re-encoding): {"trace_id": "1-ebf50f9d-f602d3ed401e1fe5a393f953", "id": "2f44de08d6fd8715", "name": "sim2real-probe-xray-svc-df3a98ab", "start_time": 1795416050, "end_time": 1795416052, "http": {"request": {"method": "GET", "url": "https://example.com/orders/o-1001"}, "response": {"status": 200}}, "annotations": {"order_id": "o-1001"}}

Stable differences (environment-independent): member order of the top-level object and of http.request; whitespace after ':' and ','; start_time/end_time emitted as integers instead of the submitted double literals. Environment-dependent and NOT asserted as a requirement: the absolute epoch values and the generated trace/segment ids and service name.

## Steps to Reproduce
1. Replay fixture xray-put-segment-is-found-by-trace-summaries-and-batch-get against the emulator.
2. Step task#1/2 call PutTraceSegments with one segment document whose members are in the order id, name, start_time, trace_id, end_time, http, annotations, serialized compactly (no spaces) with start_time/end_time as double literals.
3. Step task#3 calls BatchGetTraces with that TraceId.
4. Compare Traces[0].Segments[0].Document to the string passed in TraceSegmentDocuments: AWS returns it unchanged; the emulator returns a reordered, re-spaced document with integer-truncated start_time/end_time.
