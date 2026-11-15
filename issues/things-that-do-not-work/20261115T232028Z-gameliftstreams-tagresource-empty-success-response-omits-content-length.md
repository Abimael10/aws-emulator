Issue-Source: trace-sweep-divergence
Issue-Trace: gameliftstreams-stream-group-application-association-lifecycle
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: gameliftstreams-stream-group-application-association-lifecycle
Issue-Shape-Id: com.amazonaws.gameliftstreams#GameLiftStreams
Issue-Key: gameliftstreams|TagResource|2xx->2xx|$headers.content-length
Issue-Commit: ec5dcb8875904219d541ea60b44e9fd361ab5da2

# gameliftstreams TagResource empty success response omits Content-Length header

> _Note: this issue was identified against the code base at commit `ec5dcb8875904219d541ea60b44e9fd361ab5da2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
One net-new root cause in the gameliftstreams tagging path, found by replaying wire fixture gameliftstreams-stream-group-application-association-lifecycle against the emulator built from this checkout. The replay report flagged this cluster with neither stub_path nor owned_by.

Ownership check: the only issue that ever carried this replay key (TagResource, step task#11) is 20261103T145438Z-gameliftstreams-stream-group-read-path-drops-capacity-and.md, and that issue is RESOLVED (it appears under the fixed issues, not the open ones), so it does not cover this failure — the framing defect is still present on replay and is re-filed here. Related-but-not-covering open issue: 20261114T150215Z-gameliftstreams-untagresource-empty-success-response-omits-content-length.md carries the SAME framing fault for UntagResource (step task#13) and its replay pointer covers UntagResource/task#13 only; it does not name TagResource or step task#11, so this cause would be closed unrun by that issue's replay. A single fix in the shared empty-output serializer closes both, but each must be re-replayed on its own step.

=== Cause 1 of 1: TagResource's empty success response carries no Content-Length header (step task#11) ===
AWS: Smithy (services/service-com-amazonaws-gameliftstreams-gameliftstreams/smithy-model/com-amazonaws-gameliftstreams-gameliftstreams.json): TagResource is POST /tags/{ResourceArn}, code 200 (codeDefaulted), @idempotent, input members ResourceArn (@httpLabel, required, @length 1..128, @pattern ^arn:aws:gameliftstreams:([^:\n]*):([0-9]{12}):([^:\n]*)$) and Tags (required map, @length 1..50), output shape TagResourceResponse — a structure with NO members. Modeled errors: AccessDeniedException(403), ValidationException(400), ThrottlingException(429), InternalServerException(500) — none is at issue here, this is a 2xx framing requirement, so no error code or message is involved. AWS frames such an empty REST-JSON success with a Content-Length header describing the body it actually sent (0 for a bodiless response, 2 for the `{}` document). The recording asserts only that the header is PRESENT and shaped (`<shaped-not-asserted>`), never a particular numeric value, so the requirement is the PRESENCE of Content-Length on an empty-body success for an operation whose output shape has no members — not a specific length, and not any per-run value.
Emulator: the status (204) matched the recording and the body was empty as expected, but the response carries no Content-Length header at all — replay reports `$headers.content-length: missing (expected '<shaped-not-asserted>', got None)`.
Why: an HTTP client that frames a response body by Content-Length and receives neither Content-Length nor chunked framing must read until the connection closes, or treat the message as unterminated. On a keep-alive connection (the default for every AWS SDK) that either stalls the read or desynchronizes the connection so the NEXT response on it is misparsed; strict clients and intermediary proxies reject the response outright. A tagging call the emulator considers successful therefore surfaces to the caller as a transport error or a hang rather than a success.
Fix: the response serializer for operations whose output shape has no members — it must emit Content-Length for the empty body it writes. This is shared framing rather than TagResource-specific, so the same change covers UntagResource and every other empty-output operation in this service; the defect is not in the TagResource handler's logic (the tag write itself and the status matched).
Traces: gameliftstreams-stream-group-application-association-lifecycle#task#11.

## Evidence
Replay of wire trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, detail "/__admin/health answered 200 and the :42355 data listener accepts", bootstrap_signal=false, 48 steps total, 41 passed, 7 mismatched, 0 errored, 0 blocked, score_pct 85.4, divergence_count 2.

Divergence cluster (representative): fixture gameliftstreams-stream-group-application-association-lifecycle, step task#11, service gameliftstreams, operation TagResource, verdict mismatch, expected status 204 / actual status 204, affected_count 1 (affected: gameliftstreams-stream-group-application-association-lifecycle#task#11), stub_path empty, owned_by empty, divergence_key "service=gameliftstreams|TagResource|status=204->204|keys=$headers.content-length".

Detail reported by the replay:
  $headers.content-length: missing (expected '<shaped-not-asserted>', got None)

Expected: an empty-body 2xx success that carries a Content-Length header (value shaped, not asserted — AWS sends the length of the body it wrote: 0 bodiless, 2 for `{}`).
Actual: empty-body response with the matching status but NO Content-Length header.

Model evidence (smithy_query on services/service-com-amazonaws-gameliftstreams-gameliftstreams/smithy-model/com-amazonaws-gameliftstreams-gameliftstreams.json): TagResource http { method POST, uri /tags/{ResourceArn}, code 200 (codeDefaulted) }, idempotent=true, output_shape TagResourceResponse with no members; input ResourceArn @httpLabel required @length 1..128 @pattern ^arn:aws:gameliftstreams:([^:\n]*):([0-9]{12}):([^:\n]*)$, Tags required map @length 1..50; errors AccessDeniedException(403)/ValidationException(400)/ThrottlingException(429)/InternalServerException(500).

The only divergence is the missing framing header. No per-run value (request id, timestamp, generated ARN, clock time) is part of this finding, and the asserted header value is explicitly not compared.

## Steps to Reproduce
Boot the emulator built from this checkout and replay fixture gameliftstreams-stream-group-application-association-lifecycle, step task#11. By hand:
1. CreateApplication over S3 content and poll GetApplication until Status == READY.
2. CreateStreamGroup(... LocationConfigurations=[{LocationName=<region>, AlwaysOnCapacity=<n>}]) and poll GetStreamGroup until the group is ACTIVE.
3. TagResource(ResourceArn=<group ARN>, Tags={<k>: <v>}) and inspect the RAW response headers (curl -i or an HTTP-level trace rather than an SDK call — SDKs hide framing).
4. Observe the success response has no Content-Length header at all, where AWS frames the empty-body success with Content-Length (0 for a bodiless response, 2 for `{}`). The status and the empty body themselves match the recording.
5. Re-replay step task#11 (and, for the sibling framing cause, step task#13 under its own open issue) after fixing the shared empty-output serializer.
