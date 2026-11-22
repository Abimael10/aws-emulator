Issue-Source: trace-sweep-divergence
Issue-Trace: pipes-pipe-tag-management
Issue-Step: task#10
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: pipes-pipe-tag-management pipes-tag-empty-value-vs-absent-preserved pipes-tag-value-unicode-preserved pipes-tagging-workflow
Issue-Shape-Id: com.amazonaws.pipes#Pipes
Issue-Key: pipes|TagResource|2xx->2xx|$headers.content-length
Issue-Key: pipes|UntagResource|2xx->2xx|$headers.content-length
Issue-Commit: adacb752e5960473e35bd697b5a66409b62437b5

# pipes: empty-body tagging responses omit the Content-Length header

> _Note: this issue was identified against the code base at commit `adacb752e5960473e35bd697b5a66409b62437b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
The EventBridge Pipes tagging operations whose modeled output is an empty structure (TagResourceResponse / UntagResourceResponse) are served by the emulator as a bodyless 204 with NO Content-Length header at all. AWS frames these empty-body responses explicitly, so the response carries a Content-Length entity header. Two causes, one shared fault in how the pipes tagging responses are framed.

Cause 1 of 2: TagResource (POST /tags/{resourceArn}) empty-body response omits Content-Length.
  AWS: recorded behavior only. Smithy (com-amazonaws-pipes-pipes): TagResource http {method: POST, uri: "/tags/{resourceArn}"}, output shape TagResourceResponse with no members, so the body is empty; the model defaults the success code (codeDefaulted) and does not model response headers. The recording shows the success response status 204 WITH a Content-Length header present (its value is a framing value, not an asserted constant).
  Emulator: same status (204) but the Content-Length header is absent entirely ($headers.content-length: missing — expected a header to be present, got None).
  Why: a client that frames the response from its entity headers (an HTTP/1.1 proxy, a hand-rolled client, or any middleware that requires either Content-Length or Transfer-Encoding to decide the message has a zero-length body) cannot tell a framed empty body from a response whose body is still to come. Against AWS the tag call completes immediately; against the emulator such a client can block on the read or treat the response as malformed, so a tagging step that AWS completes silently becomes a hang or a protocol error.
  Fix: the response serializer for the pipes tagging operations — the path that builds the empty-output HTTP response must emit the entity header that frames the zero-length body, for every pipes operation whose output structure has no members, not just the two exercised here.
  Traces: pipes-pipe-tag-management#task#10 (representative); also pipes-tag-empty-value-vs-absent-preserved#task#9, pipes-tag-value-unicode-preserved#task#9, pipes-tagging-workflow#task#10 — 4 steps in the cluster.

Cause 2 of 2: UntagResource (DELETE /tags/{resourceArn}) empty-body response omits Content-Length.
  AWS: recorded behavior only. Smithy (com-amazonaws-pipes-pipes): UntagResource http {method: DELETE, uri: "/tags/{resourceArn}"}, output shape UntagResourceResponse with no members; success code defaulted, no modeled response headers. The recording shows status 204 WITH Content-Length present.
  Emulator: status 204 served with no Content-Length header ($headers.content-length: missing, got None).
  Why: identical client consequence as Cause 1 — the un-tagging call is unframed, so a Content-Length-dependent client cannot conclude the response is complete.
  Fix: same component as Cause 1 (the pipes empty-output response serializer); fixing it in one place should close both causes, but both must be re-replayed.
  Traces: pipes-pipe-tag-management#task#12 (representative); also pipes-tag-empty-value-vs-absent-preserved#task#11, pipes-tagging-workflow#task#12 — 3 steps in the cluster.

No open issue covers this. The nearest open pipes tagging issues — 20261117T090851Z-pipes-the-badrequestexception-message-for-tag-key-length.md and 20261118T212031Z-pipes-tagresource-tag-key-length-refusal-nests-a.md — are about the BadRequestException error-document shape and message for tag-key-length REFUSALS; this is the SUCCESS response's header framing and is independent of them.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 97.8%, 7 mismatched steps of 314), 2 divergence clusters, both service=pipes, both verdict=mismatch, both with empty stub_path and empty owned_by:

1) divergence_key service=pipes|TagResource|status=204->204|keys=$headers.content-length
   fixture_id pipes-pipe-tag-management, step_key task#10
   expected_status 204, actual_status 204
   detail: "$headers.content-length: missing (expected '<shaped-not-asserted>', got None)"
   affected (4): pipes-pipe-tag-management#task#10, pipes-tag-empty-value-vs-absent-preserved#task#9, pipes-tag-value-unicode-preserved#task#9, pipes-tagging-workflow#task#10

2) divergence_key service=pipes|UntagResource|status=204->204|keys=$headers.content-length
   fixture_id pipes-pipe-tag-management, step_key task#12
   expected_status 204, actual_status 204
   detail: "$headers.content-length: missing (expected '<shaped-not-asserted>', got None)"
   affected (3): pipes-pipe-tag-management#task#12, pipes-tag-empty-value-vs-absent-preserved#task#11, pipes-tagging-workflow#task#12

Expected (AWS, per recording): 204 response carrying a Content-Length entity header that frames the zero-length body.
Actual (emulator): 204 response with no Content-Length header present. Status and body content agree; only the framing header is absent. The header's recorded numeric value is a framing value and is not asserted — the requirement is that the header is emitted, not any one run's value.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay fixture pipes-pipe-tag-management against it.
3. At step task#10 the fixture tags a pipe: POST /tags/{pipeArn} with a tags map. Inspect the raw response headers (e.g. curl -i / a client that does not normalize headers): the status is 204 and no Content-Length header is returned, whereas the recording for the same step shows Content-Length present.
4. At step task#12 the same fixture untags the pipe: DELETE /tags/{pipeArn}?tagKeys=... . The 204 response likewise carries no Content-Length header.
5. The same omission reproduces on pipes-tagging-workflow#task#10/#task#12, pipes-tag-empty-value-vs-absent-preserved#task#9/#task#11 and pipes-tag-value-unicode-preserved#task#9.
