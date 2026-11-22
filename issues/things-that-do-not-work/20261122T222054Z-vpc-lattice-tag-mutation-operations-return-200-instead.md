Issue-Source: trace-sweep-divergence
Issue-Trace: vpclattice-service-network-lifecycle
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: vpclattice-service-network-lifecycle
Issue-Shape-Id: com.amazonaws.vpclattice#MercuryControlPlane
Issue-Key: vpc-lattice|TagResource|2xx->2xx|status
Issue-Key: vpc-lattice|UntagResource|2xx->2xx|status
Issue-Commit: 2c016b961c559bd5da9e009513c7e0fc712e2b9d

# vpc-lattice tag-mutation operations return 200 instead of 204 No Content

> _Note: this issue was identified against the code base at commit `2c016b961c559bd5da9e009513c7e0fc712e2b9d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
The emulator's VPC Lattice tag-mutation operations answer the empty-output tagging calls with HTTP 200 where AWS answers 204 No Content. Both operations share one root cause: the tagging handlers' response serializer emits the generic success status for an empty output shape instead of the no-content status AWS uses for these two routes. 2 causes are bundled here.

Cause 1 of 2: TagResource (POST /tags/{resourceArn}) — fixture vpclattice-service-network-lifecycle, step task#8.
AWS: TagResource has an empty output shape (Smithy: TagResource, input_shape TagResourceRequest / output_shape TagResourceResponse with no members; @http POST /tags/{resourceArn}, code 200 as declared in the model). The recorded AWS response for this route is HTTP 204 with an empty body — recorded behavior only, and it is the recording, not the model's declared code, that describes the wire. No error code or message is involved: this is a success response.
Emulator: returns HTTP 200 for the same request.
Why: a client that branches on the response status — any SDK or tool that distinguishes 204 No Content from 200 OK, or a strict HTTP assertion/proxy in front of the call — sees a different success status than AWS returns, so a tagging step that is a no-content success against AWS looks like a body-bearing 200 against the emulator and response-status assertions in client test suites and in generated SDK protocol tests fail.
Fix: the TagResource handler's response serializer in the vpc-lattice service crate — the status chosen for the empty tagging output, not the body (the body is already correctly empty).
Traces: vpclattice-service-network-lifecycle#task#8 (expected 204, got 200).

Cause 2 of 2: UntagResource (DELETE /tags/{resourceArn}?tagKeys=...) — fixture vpclattice-service-network-lifecycle, step task#10.
AWS: UntagResource likewise has an empty output shape (Smithy: UntagResource, output_shape UntagResourceResponse with no members; @http DELETE /tags/{resourceArn}, code 200 but flagged codeDefaulted, i.e. the model never states a code for this route; operation is @idempotent). The recorded AWS response is HTTP 204 with an empty body — recorded behavior only. Again a success response, with no error code or message.
Emulator: returns HTTP 200 for the same request.
Why: same client consequence as Cause 1 — the untag call's success status does not match AWS, so status-sensitive clients and protocol-conformance assertions diverge on a call that otherwise succeeded.
Fix: the UntagResource handler's response serializer in the vpc-lattice service crate, the same empty-output status path as Cause 1; fixing one without the other leaves the sibling route diverging.
Traces: vpclattice-service-network-lifecycle#task#10 (expected 204, got 200).

Note: the other vpc-lattice divergences in this replay (DnsEntry.domainName in the loopback zone, DeregisterTargets failureMessage) are already owned by open issues 20261114T190024Z-vpc-lattice-announces-dnsentry-domainname-in-the-loopback.md and 20261116T112029Z-vpc-lattice-deregistertargets-reports-a-composed-per-target.md and are NOT part of this bundle. No open issue covers the tagging response status.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, 110 steps, 100 passed, 10 mismatched):

Cause 1 — divergence_key service=vpc-lattice|TagResource|status=204->200|keys=status
  fixture vpclattice-service-network-lifecycle, step task#8, operation TagResource
  expected_status 204, actual_status 200
  detail: "status: expected 204, got 200"
  verdict mismatch; stub_path empty; owned_by empty (not covered by any open issue)

Cause 2 — divergence_key service=vpc-lattice|UntagResource|status=204->200|keys=status
  fixture vpclattice-service-network-lifecycle, step task#10, operation UntagResource
  expected_status 204, actual_status 200
  detail: "status: expected 204, got 200"
  verdict mismatch; stub_path empty; owned_by empty

Only the status diverges in both clusters; the response bodies are empty as expected, and no other asserted key diverged on either step. Model evidence: Smithy TagResource/UntagResource both have empty output shapes; UntagResource's @http code is codeDefaulted, so the model asserts no status for it.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire fixture corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture vpclattice-service-network-lifecycle up to step task#8 — create a service network, then call TagResource with POST /tags/{resourceArn} for that service network's ARN and a tag map. Observe the response status is 200; AWS's recording for the step is 204 with an empty body.
3. Cause 2: continue the same fixture to step task#10 — call UntagResource with DELETE /tags/{resourceArn}?tagKeys=<key> for the same ARN. Observe the response status is 200; AWS's recording for the step is 204 with an empty body.
4. Both steps are reported as mismatch clusters with detail "status: expected 204, got 200".
