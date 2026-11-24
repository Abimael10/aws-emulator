Issue-Source: trace-sweep-divergence
Issue-Trace: resiliencehubv2-policy-tagging
Issue-Step: task#5
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resiliencehubv2-policy-tagging
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2|UntagResource|2xx->2xx|$headers.content-length
Issue-Commit: 8a50ecdab6898d293b5c485cedad6f5ed64820e8

# resiliencehubv2 UntagResource empty response omits the Content-Length header

> _Note: this issue was identified against the code base at commit `8a50ecdab6898d293b5c485cedad6f5ed64820e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UntagResource

## Description
Wire replay of the simparity-wire-aws-v1 corpus against the emulator built from this checkout found ONE net-new cause on the resiliencehubv2 tagging surface. Its cluster reported stub_path="" and owned_by=[]: no unimplemented operation blocks it and no open issue carries its replay key. (The open issue "resiliencehubv2 TagResource empty response omits the Content-Length header", 20261113T163326Z-resiliencehubv2-tagresource-empty-response-omits-the-content-length.md, describes the same class of fault on TagResource and its Issue-Key is scoped to `resiliencehubv2|TagResource|2xx->2xx|$headers.content-length`; it does NOT carry this step's replay key, and the replay confirms this UntagResource cluster is unowned, so it is filed separately here. The other three resiliencehubv2 clusters in this replay - CreateSystem task#1, CreateUserJourney task#2, GetUserJourney task#8 - all reported owned_by and were skipped.)

=== Cause 1 of 1: UntagResource's successful, body-less response carries no Content-Length header ===
AWS: Smithy UntagResource (service com.amazonaws.resiliencehubv2#NGRHServiceCore, sdkId resiliencehubv2, protocol restJson1; http DELETE /v2/tags/{resourceArn}, modeled code 200, @idempotent). Input UntagResourceRequest { resourceArn -> Arn @httpLabel @required @length min 31 @pattern ^arn:(aws|aws-cn|aws-iso|...):...$, tagKeys -> TagKeyList @httpQuery "tagKeys" @required @length 1..50 @sensitive }. Output shape UntagResourceResponse has NO members, so under restJson1 there is no payload to serialize. Errors: AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400. The HTTP framing of that empty success is recorded behavior only: the recording shows AWS answering the untag call with a body-less success response that still carries a Content-Length header (value 0 for the empty body). The status is NOT part of this finding - the emulator returns the same status the recording does.
Emulator: the status agrees, but the response is emitted with NO Content-Length header at all - absent rather than present-with-value-0. The replay reports `$headers.content-length: missing (expected '<shaped-not-asserted>', got None)`, i.e. the recording asserts only that the header EXISTS (its value is not asserted) and the emulator sends no such header.
Why: a client that frames a body-less response by its declared length gets a response whose body length is undeclared. HTTP clients, proxies and connection-reuse layers that require an explicit length on a response with no transfer encoding either fall back to read-until-close (defeating keep-alive for every untag call) or report the response as malformed, so an UntagResource that AWS makes cheap and connection-reusable becomes a connection-terminating call against the emulator. Anything asserting on response headers (SDK response-metadata checks, HTTP contract tests) sees a header AWS always sends go missing.
Fix: the resiliencehubv2 response serializer's empty-output path - the restJson1 serializer branch that builds the body-less success for operations whose output structure has no members (UntagResourceResponse, and the same branch used by TagResource) - must emit Content-Length: 0 for the empty body instead of omitting the header. This is response framing, not handler logic: the UntagResource handler's state mutation is already correct, since the step's status and the untagging effect both match the recording.
Traces: resiliencehubv2-policy-tagging#task#5.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout (trace_set_id=simparity-wire-aws-v1, emulator_up=true, bootstrap_signal=false, 13 fixtures replayed, 124 steps total: 99 passed, 18 mismatched, 7 blocked, 0 errored, score_pct 79.8, divergence_count 5).

Cause 1 - fixture resiliencehubv2-policy-tagging, step task#5, operation UntagResource, verdict mismatch, divergence_key `service=resiliencehubv2|UntagResource|status=204->204|keys=$headers.content-length`:
  expected status 204 / actual status 204 (the status AGREES - it is not part of this finding)
  $headers.content-length: missing
    expected: '<shaped-not-asserted>'  (the recording asserts the header is PRESENT; its value is not asserted, and for an empty body it is 0)
    actual:   None                     (the emulator sends no Content-Length header)
  stub_path="", stub_service="", owned_by=[], affected=["resiliencehubv2-policy-tagging#task#5"], affected_count=1.

Model evidence (services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/smithy-model/com-amazonaws-resiliencehubv2-ngrhservicecore.json):
- UntagResource: http DELETE /v2/tags/{resourceArn} code 200, @idempotent; input UntagResourceRequest { resourceArn: Arn @httpLabel @required @length min 31 @pattern ^arn:...$, tagKeys: TagKeyList @httpQuery tagKeys @required @length 1..50 @sensitive }; output_shape UntagResourceResponse with no members; errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400).
- Because the output structure is empty, restJson1 serializes no payload; the only wire requirement left for the success is its framing, and the recording shows that framing includes Content-Length.

No per-run value is part of this requirement: the resource ARN, tag keys, request id and timestamps in the recording differ in every environment. The requirement is only that the body-less success declares its body length.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from this checkout.
2. Replay the wire corpus simparity-wire-aws-v1 and inspect fixture resiliencehubv2-policy-tagging, step task#5 (operation UntagResource).
3. Directly: POST /v2/create-policy to create a policy, then POST /v2/tags/{policyArn} with body {"tags": {"k": "v"}} to attach a tag.
4. Call DELETE /v2/tags/{policyArn}?tagKeys=k and inspect the RAW response headers (use a client that prints framing headers, not an SDK that hides them).
5. Observe: the status and the empty body match the recording, but no Content-Length header is present. AWS's body-less success for this operation carries Content-Length: 0.
