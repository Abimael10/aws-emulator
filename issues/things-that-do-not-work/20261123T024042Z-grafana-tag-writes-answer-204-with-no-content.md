Issue-Source: trace-sweep-divergence
Issue-Trace: grafana-tag-resource-roundtrip
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: grafana-tag-resource-roundtrip
Issue-Shape-Id: com.amazonaws.grafana#AWSGrafanaControlPlane
Issue-Key: grafana|TagResource|2xx->2xx|$headers.content-length
Issue-Key: grafana|UntagResource|2xx->2xx|$headers.content-length
Issue-Commit: 481a4a04672d6cfe412cdf6a905c6af373954dc8

# Grafana tag writes answer 204 with no Content-Length header

> _Note: this issue was identified against the code base at commit `481a4a04672d6cfe412cdf6a905c6af373954dc8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
The emulator's empty-body success responses for the Grafana tagging operations are framed without a Content-Length header. Both tagging operations share one serializer path for an empty output structure answered with 204, so both diverge the same way. There are 2 causes in this issue.

Cause 1 of 2: TagResource (fixture grafana-tag-resource-roundtrip, step task#4).
AWS: Smithy: TagResource input {resourceArn @httpLabel required, tags TagMap required @length max 50}, output TagResourceResponse is EMPTY (no members, no @httpHeader/@httpPayload bindings), @http(method: POST, uri: "/tags/{resourceArn}") with code DEFAULTED to 200; the service itself answers a successful tag write with 204 No Content. The response header set is not described by the model, so: recorded behavior only — the recorded AWS response for a successful tag write is HTTP 204 and carries a Content-Length response header (the fixture asserts the header is PRESENT and does not assert its value; for a 204 with no entity body AWS frames it as the zero length of that empty body). No error code or error message is involved: this is a success response.
Emulator: answers HTTP 204 (status matches) but emits NO Content-Length header at all — replay reports "$headers.content-length: missing (expected '<shaped-not-asserted>', got None)".
Why: a client or proxy that frames a response by its declared length sees a response with neither Content-Length nor any other length signal. HTTP clients and intermediaries that require an explicit length on a bodyless response (or that reuse the connection based on it) cannot tell a zero-length body from a response whose body is still coming, so a caller that works against AWS can stall, close the connection, or report a malformed response against the emulator even though the status is right.
Fix: the empty-output success serializer / response-framing layer for this operation — the component that turns an empty output structure plus SUCCESS_STATUS into the HTTP response must set the Content-Length of the (empty) body it writes, rather than omitting the header when there is nothing to serialize. Not a handler-logic change: the handler already returns the right status and an empty output.
Traces: grafana-tag-resource-roundtrip#task#4.

Cause 2 of 2: UntagResource (fixture grafana-tag-resource-roundtrip, step task#6).
AWS: Smithy: UntagResource input {resourceArn @httpLabel required, tagKeys @httpQuery "tagKeys" required}, output UntagResourceResponse is EMPTY, @http(method: DELETE, uri: "/tags/{resourceArn}") with code DEFAULTED to 200, @idempotent; the service answers a successful tag removal with 204 No Content. Recorded behavior only — the recorded AWS response is HTTP 204 carrying a Content-Length header (presence asserted, value not).
Emulator: answers HTTP 204 but emits no Content-Length header — "$headers.content-length: missing (expected '<shaped-not-asserted>', got None)".
Why: same client-visible consequence as cause 1 — an unframed bodyless response on the untag path.
Fix: same component as cause 1 (the shared empty-output response serializer), so both causes are expected to close together; re-replay BOTH steps before submitting, because they are separate steps of the same fixture and a fix that only covers one leaves the other diverging.
Traces: grafana-tag-resource-roundtrip#task#6.

No open issue covers either cause (list_existing_issues reports no grafana issues at all).

## Evidence
Replay of fixture grafana-tag-resource-roundtrip (trace set simparity-wire-aws-v1), 2 mismatch clusters:

Step task#4, TagResource: expected status 204, actual status 204 — detail "$headers.content-length: missing (expected '<shaped-not-asserted>', got None)". divergence_key service=grafana|TagResource|status=204->204|keys=$headers.content-length.

Step task#6, UntagResource: expected status 204, actual status 204 — detail "$headers.content-length: missing (expected '<shaped-not-asserted>', got None)". divergence_key service=grafana|UntagResource|status=204->204|keys=$headers.content-length.

Expected: a 204 success response for both operations that carries a Content-Length header describing its empty body.
Actual: a 204 success response with no Content-Length header present.
Status and body content otherwise match the recording; the whole divergence is the missing response header.

Source: services/service-com-amazonaws-grafana-awsgrafanacontrolplane/src/ops/tag_resource.rs:147 (SUCCESS_STATUS = 204) and the equivalent line in ops/untag_resource.rs — the status is deliberate and correct; the header is contributed by the shared response serializer, not by these handlers.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture grafana-tag-resource-roundtrip from trace set simparity-wire-aws-v1.
2. Step task#4 issues POST /tags/{resourceArn} (TagResource) against an existing workspace ARN with a valid tag map.
3. Inspect the raw HTTP response: status is 204, but no Content-Length header is present (expected: present).
4. Step task#6 issues DELETE /tags/{resourceArn}?tagKeys=... (UntagResource) for the same ARN.
5. Inspect the raw HTTP response: status is 204, again with no Content-Length header.
6. Both steps are reported as mismatches keyed on $headers.content-length.
