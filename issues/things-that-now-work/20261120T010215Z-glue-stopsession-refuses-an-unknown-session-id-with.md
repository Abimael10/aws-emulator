Issue-Source: trace-sweep-divergence
Issue-Trace: glue-stop-session-not-found
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-stop-session-not-found
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|StopSession|2xx->4xx|$.Id
Issue-Key: glue|StopSession|2xx->4xx|status
Issue-Commit: 2a1cdc6b2a14525bb7c98862b0695a5a1cd91d66

# Glue StopSession refuses an unknown session id with 400 instead of AWS's 200 Id echo

> _Note: this issue was identified against the code base at commit `2a1cdc6b2a14525bb7c98862b0695a5a1cd91d66`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopSession

## Description
One cause.

Cause 1 of 1: StopSession on a session id that does not exist is answered with an HTTP 400 refusal instead of AWS's success response.

AWS: Smithy (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json) models StopSession with input StopSessionRequest{Id: NameString @required @length(min 1, max 255), RequestOrigin} and output StopSessionResponse{Id: NameString}. Its modeled error set is AccessDeniedException, ConcurrentModificationException, IllegalSessionStateException, InternalServiceException, InvalidInputException and OperationTimeoutException — EntityNotFoundException is NOT bound to StopSession, so the operation has no modeled 'no such session' refusal at all. Recorded behavior only: for a well-formed Id naming no existing session, AWS answers HTTP 200 with a body whose single member Id is the Id from the request, echoed verbatim; i.e. the stop request is terminal/idempotent and does not probe for the session's existence. The Id in the recording ('sim2real-probe-glue-session-missing-853eef2c') is the request's value filling that template, not a fixed string.

Emulator: answers HTTP 400 and the body carries no Id member at all (replay: 'status: expected 200, got 400' and '$.Id: missing (expected <the requested session id>, got None)'). The emulator is resolving the session in state first and converting the miss into a 4xx refusal, a status and error class AWS does not use for this operation.

Why: a client that stops an interactive session it no longer holds state for — the ordinary shutdown/cleanup path, and the retry path after a timed-out StopSession — sees a modeled-exception 400 where AWS returns success. The AWS SDKs surface that as a terminal GlueException, so teardown code that is correct against AWS aborts, leaves sessions un-stopped, and cannot be made idempotent. A caller also cannot read back the Id it stopped, because the response body is empty.

Fix: the StopSession handler in the glue service crate (and specifically its session lookup, not its input validation — the Id is well formed here): a lookup miss must not short-circuit to an error. StopSession should serialize StopSessionResponse{Id: <Id from the request>} with HTTP 200 whether or not the session exists, reserving its 400s for the modeled cases (invalid input, illegal session state, concurrent modification).

Traces: glue-stop-session-not-found#task#1 (StopSession, expected 200 + $.Id, got 400 with no $.Id).

## Evidence
Fixture glue-stop-session-not-found, step task#1, operation StopSession (service glue).
Expected status: 200, actual status: 400.
Replay detail: "status: expected 200, got 400" and "$.Id: missing (expected 'sim2real-probe-glue-session-missing-853eef2c', got None)".
Expected body: StopSessionResponse with Id equal to the Id sent in the request.
Actual body: a 400 refusal with no Id member.
Smithy evidence: StopSession output shape StopSessionResponse has member Id (NameString); the operation's error list contains AccessDeniedException, ConcurrentModificationException, IllegalSessionStateException, InternalServiceException, InvalidInputException, OperationTimeoutException and does NOT contain EntityNotFoundException, so no modeled error covers an unknown session id.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a Glue client at its data listener.
2. Call StopSession with Id set to a well-formed session name (1-255 chars matching NameString) that was never created, e.g. the id used by fixture glue-stop-session-not-found.
3. Observe HTTP 400 with no Id in the body.
4. Expected (per the recording and the Smithy model's error set): HTTP 200 with body {"Id": "<the Id sent in the request>"}.
5. Re-replay glue-stop-session-not-found#task#1 to confirm the fix.
