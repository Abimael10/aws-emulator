Issue-Source: trace-sweep-divergence
Issue-Trace: signer-data-revoked-job-listed-in-revocation-status
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: signer-data-revoked-job-listed-in-revocation-status
Issue-Shape-Id: com.amazonaws.signerdata#SignerDataPlane
Issue-Key: signer|RevokeSignature|2xx->2xx|$headers.content-length
Issue-Commit: 87ad42ce826020c3c25d7c413e65e0908692b03d

# Signer RevokeSignature empty 204 reply omits Content-Length

> _Note: this issue was identified against the code base at commit `87ad42ce826020c3c25d7c413e65e0908692b03d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RevokeSignature

## Description
One net-new wire divergence in the signer crate's response-envelope construction: the bodyless success reply of RevokeSignature is sent without the Content-Length entity header AWS emits on an empty reply. signer is bootstrapped (service-com-amazonaws-signer-wallabyservice; nothing pending), so this is a fidelity defect, not a bootstrap gap.

Not covered by an open issue. The neighbouring open issue 20261031T194210Z-signer-response-envelopes-diverge-empty-204-replies-omit.md ('Signer response envelopes diverge: empty 204 replies omit Content-Length and ListProfilePermissions miscomputes policySizeBytes') describes the SAME framing fault, but its replay keys and traces cover only TagResource, UntagResource, RevokeSigningProfile and ListProfilePermissions (Issue-Key lines: signer|TagResource, signer|UntagResource, signer|RevokeSigningProfile, signer|ListProfilePermissions). RevokeSignature and the fixture signer-data-revoked-job-listed-in-revocation-status appear in neither, so the step below is un-gated by that issue and is filed here; a fixer closing that issue should also re-replay this fixture.

Cause 1 of 1: RevokeSignature's empty success reply omits Content-Length.
AWS: Smithy signer#RevokeSignature is PUT /signing-jobs/{jobId}/revoke with output shape Unit (no members) — input jobId (@httpLabel, required), reason (required, @length 1..500), optional jobOwner (@length 12..12, pattern ^[0-9]{12}$); modeled errors AccessDeniedException 403, ResourceNotFoundException 404, ValidationException 400, TooManyRequestsException 429, InternalServiceErrorException 500. AWS answers the successful revocation with a bodyless response that still carries the entity header Content-Length (its recorded value is shape-asserted, not value-asserted, so the requirement is that the header is emitted on a bodyless reply, not any one number). Recorded behavior: status 204 with a content-length header present.
Emulator: answers status 204 (status matches) but emits no content-length header at all — $headers.content-length: missing (expected '<shaped-not-asserted>', got None).
Why: the empty reply IS the only success signal for RevokeSignature, so a client or intermediary that frames a response by Content-Length rather than by the status code has no message terminator and must fall back on connection close or treat the reply as indeterminate-length. Strict HTTP proxies and SDK transports that assert the header see a malformed empty reply, so a signature revocation that AWS reports as cleanly complete is reported as a transport error or hangs, and the caller cannot confirm the revocation landed.
Fix: the signer crate's response serializer for empty/Unit output shapes — the same component that selects the bodyless success status must also set Content-Length: 0 on it; RevokeSignature shares that path with the other Unit-output signer operations.
Traces: signer-data-revoked-job-listed-in-revocation-status#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200, data listener accepts). 25 steps: 24 passed, 1 mismatched, 0 errored, 0 blocked; score 96.0%; 1 divergence cluster; bootstrap_signal false.

Cause 1 — fixture signer-data-revoked-job-listed-in-revocation-status, step task#4, service signer, operation RevokeSignature, verdict mismatch:
  expected status 204, actual status 204
  detail: $headers.content-length: missing (expected '<shaped-not-asserted>', got None)
  divergence_key: service=signer|RevokeSignature|status=204->204|keys=$headers.content-length
  affected (1): signer-data-revoked-job-listed-in-revocation-status#task#4
  stub_path: empty (operation IS implemented); owned_by: empty (no open issue carries this replay key).

Smithy consulted: services/service-com-amazonaws-signer-wallabyservice/smithy-model/com-amazonaws-signer-wallabyservice.json — signer#RevokeSignature: http PUT /signing-jobs/{jobId}/revoke, output_shape Unit (no members), input RevokeSignatureRequest {jobId @httpLabel required, reason required @length 1..500, jobOwner @length 12..12 pattern ^[0-9]{12}$}, errors AccessDeniedException/403, InternalServiceErrorException/500, ResourceNotFoundException/404, TooManyRequestsException/429, ValidationException/400.

No status code, error code or error message diverges here — the sole divergence is the missing Content-Length header on the bodyless 204.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Cause 1: fixture signer-data-revoked-job-listed-in-revocation-status, step task#4 — create/sign a signing job as the fixture does, then call RevokeSignature on it: PUT /signing-jobs/{jobId}/revoke with a non-empty `reason` (1..500 chars) for an existing job.
4. Inspect the response headers. The emulator answers 204 with no content-length header; AWS answers the same bodyless 204 with Content-Length present (0 for an empty body).
5. Confirm the body assertion itself passes — only $headers.content-length diverges.
