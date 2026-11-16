Issue-Source: trace-sweep-divergence
Issue-Trace: ivs-stream-key-management
Issue-Step: task#6
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ivs-stream-key-management ivs-tagging-workflow
Issue-Shape-Id: com.amazonaws.ivs#AmazonInteractiveVideoService
Issue-Key: ivs|DeleteStreamKey|2xx->2xx|$headers.content-length
Issue-Key: ivs|TagResource|2xx->2xx|$headers.content-length
Issue-Key: ivs|UntagResource|2xx->2xx|$headers.content-length
Issue-Commit: 82088200bb48db9041b40c00c264266c5bbb9a19

# IVS empty-bodied 204 responses omit the content-length: 0 framing header

> _Note: this issue was identified against the code base at commit `82088200bb48db9041b40c00c264266c5bbb9a19`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteStreamKey

## Description
Replaying the wire corpus (simparity-wire-aws-v1) against the IVS emulator (service token `ivs`, crate services/service-com-amazonaws-ivs-amazoninteractivevideoservice) shows that EVERY operation whose success response has no body answers 204 with NO `content-length` header at all, where AWS sends the explicit `content-length: 0` framing header. Status, error-free body and all other asserted keys are correct in all three; the only divergence is the missing framing header. THREE causes, one per operation, bundled because they share one root cause in the shared empty-body response path and one service. A fixer must close ALL THREE before submitting (re-replay ivs-stream-key-management#task#6, ivs-tagging-workflow#task#2 and ivs-tagging-workflow#task#4).

=== Cause 1 of 3: DeleteStreamKey 204 has no content-length ===
AWS: Smithy (com.amazonaws.ivs, restJson1) DeleteStreamKey -> @http{method POST, uri /DeleteStreamKey, code 204} with output shape Unit, i.e. a modelled empty body. Recorded behavior for the header itself: the recorded AWS response to a successful DeleteStreamKey is `HTTP/1.1 204` carrying `content-length: 0` and no body bytes. The header's VALUE is fixed (`0`), not a per-run value.
Emulator: answers 204 (correct status) but emits no `content-length` header; the replay reports `$headers.content-length: missing (expected '0', got None)`.
Why: a client or proxy that reads response framing from `content-length` sees an unframed response. HTTP intermediaries and strict clients that distinguish "no body declared" from "zero-length body declared" (connection reuse / keep-alive accounting, HTTP-cache storage rules, SDK response-length assertions) behave differently against the emulator than against AWS, and a recorded-response differ flags every empty success.
Fix: the service's response serialization / HTTP response layer for empty-output operations — the component that turns a Unit/empty output into the HTTP response must set `content-length: 0` rather than omitting the header.
Traces: ivs-stream-key-management#task#6 (representative).

=== Cause 2 of 3: TagResource 204 has no content-length ===
AWS: Smithy TagResource(resourceArn @httpLabel required, tags required) -> empty output, @http{POST /tags/{resourceArn}} (modelled code defaulted to 200). Recorded behavior: AWS answers a successful IVS TagResource with `204` and `content-length: 0`, no body. The emulator already matches the recorded 204 status; only the framing header is absent.
Emulator: 204 with no `content-length` header — replay reports `$headers.content-length: missing (expected '0', got None)`.
Why: same as Cause 1 — the tagging calls are the ones a client issues most often in a loop, so the unframed response is hit on every tag write.
Fix: same shared empty-body response path (the tagging operations' response construction inherits it).
Traces: ivs-tagging-workflow#task#2.

=== Cause 3 of 3: UntagResource 204 has no content-length ===
AWS: Smithy UntagResource(resourceArn @httpLabel required, tagKeys @httpQuery required) -> empty output, @http{DELETE /tags/{resourceArn}}, @idempotent. Recorded behavior: AWS answers a successful IVS UntagResource with `204` and `content-length: 0`, no body.
Emulator: 204 with no `content-length` header — replay reports `$headers.content-length: missing (expected '0', got None)`.
Why: same as above.
Fix: same shared empty-body response path.
Traces: ivs-tagging-workflow#task#4.

Related open issues, NONE of which owns these clusters (the replay reports owned_by [] and stub_path "" for all three): 20261030T164443Z-ivschat-empty-bodied-204-tag-responses-omit-the.md is the SAME class of defect in a DIFFERENT service (ivschat) and carries ivschat replay keys only, so it does not cover any ivs step; 20261023T004057Z-ivs-7-behavioural-divergences-error-messages-dropped-on.md is a behavioural bundle about error bodies, authorization and tag-ARN resolution and says nothing about empty-body framing.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true ("/__admin/health answered 200 and the :39603 data listener accepts"), steps_total 84, passed 67, mismatched 12, blocked 5, errored 0, score 79.8%, divergence_count 5, bootstrap_signal false.

Cluster 1 (representative, Cause 1):
  divergence_key: service=ivs|DeleteStreamKey|status=204->204|keys=$headers.content-length
  fixture ivs-stream-key-management, step task#6, verdict mismatch, owned_by [], stub_path ""
  expected_status 204, actual_status 204 (status CORRECT)
  detail: $headers.content-length: missing (expected '0', got None)

Cluster 2 (Cause 2):
  divergence_key: service=ivs|TagResource|status=204->204|keys=$headers.content-length
  fixture ivs-tagging-workflow, step task#2, affected ['ivs-tagging-workflow#task#2'], owned_by []
  detail: $headers.content-length: missing (expected '0', got None)

Cluster 3 (Cause 3):
  divergence_key: service=ivs|UntagResource|status=204->204|keys=$headers.content-length
  fixture ivs-tagging-workflow, step task#4, affected ['ivs-tagging-workflow#task#4'], owned_by []
  detail: $headers.content-length: missing (expected '0', got None)

Expected vs actual, in all three: expected response headers include `content-length: 0`; actual response carries no `content-length` header. The value `0` is fixed text for an empty body, not a per-run value, and the bodies themselves match (empty) with the correct 204 status.

Smithy (services/service-com-amazonaws-ivs-amazoninteractivevideoservice/smithy-model/com-amazonaws-ivs-amazoninteractivevideoservice.json): DeleteStreamKey http {method POST, uri /DeleteStreamKey, code 204}, output shape Unit; TagResource http {POST /tags/{resourceArn}}, output TagResourceResponse with no members; UntagResource http {DELETE /tags/{resourceArn}}, output UntagResourceResponse with no members, idempotent.

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus), or exercise it by hand against the data listener in region us-west-2 with a client that shows raw response headers (e.g. curl -i):
1. POST /CreateChannel, then POST /CreateStreamKey for that channel; POST /DeleteStreamKey with {"arn": "<stream key arn>"} (fixture ivs-stream-key-management#task#6) -> observe `HTTP/1.1 204` with no `content-length` header.
2. POST /tags/<url-encoded channel arn> with {"tags": {"k": "v"}} (fixture ivs-tagging-workflow#task#2) -> 204, no `content-length`.
3. DELETE /tags/<url-encoded channel arn>?tagKeys=k (fixture ivs-tagging-workflow#task#4) -> 204, no `content-length`.
In each case AWS's recorded response for the same call carries `content-length: 0`.
