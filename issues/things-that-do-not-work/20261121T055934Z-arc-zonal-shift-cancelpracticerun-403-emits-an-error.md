Issue-Source: trace-sweep-divergence
Issue-Trace: arc-zonal-shift-practice-run-error-paths
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: arc-zonal-shift-practice-run-error-paths
Issue-Shape-Id: com.amazonaws.arczonalshift#PercDataPlane
Issue-Key: arc-zonal-shift|CancelPracticeRun|4xx->4xx|$
Issue-Commit: 7cb8f835e6eb48db46e23acdee391f97cacfb45c

# arc-zonal-shift CancelPracticeRun 403 emits an error document where AWS returns an empty body

> _Note: this issue was identified against the code base at commit `7cb8f835e6eb48db46e23acdee391f97cacfb45c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CancelPracticeRun

## Description
One net-new divergence on the arc-zonal-shift practice-run access-denied path. This bundle has 1 cause; it must be closed.

Cause 1 of 1: CancelPracticeRun 403 response body (fixture arc-zonal-shift-practice-run-error-paths, step task#2).
AWS: Smithy: CancelPracticeRun (DELETE /practiceruns/{zonalShiftId}, success code 200) models AccessDeniedException with errorKind client and httpError 403 (alongside ConflictException 409, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400, InternalServerException 500). For the error-class wording the model states only that AccessDeniedException carries a single `message` member; the recorded response for this path is 403 with an EMPTY JSON body ({}) - recorded behavior only - i.e. the error class is conveyed by the HTTP status and the error-type header and no message document is serialized at all. There is no fixed text and no request- or state-derived variable part in the recorded body: the body is the empty object.
Emulator: answers 403 (status is correct) but serializes the body {"__type": "AccessDeniedException"} where the recording has {}. It adds a `__type` discriminator member the service does not send on this path.
Why: a client or contract test that compares the access-denied response body against what the service actually returns sees a member that never appears in production; an SDK or proxy that keys error classification off the presence of a body document (rather than the status/error-type header) takes a different branch against the emulator than against AWS, and any strict body equality check on this error path fails.
Fix: the arc-zonal-shift error-response serializer for AccessDeniedException as reached from the CancelPracticeRun handler (DELETE /practiceruns/{zonalShiftId}) - the 403 must be emitted with an empty JSON body for this operation rather than a synthesized error document.
Traces: arc-zonal-shift-practice-run-error-paths#task#2.

Related but NOT covering this: open issue 20261029T170702Z-arc-zonal-shift-accessdeniedexception-403-body-does-not.md ("arc-zonal-shift AccessDeniedException 403 body does not match the recorded wire shape") describes the same fault class for StartZonalShift, CancelZonalShift and UpdateZonalShift and carries replay keys only for fixture arc-zonal-shift-zonal-shift-error-paths steps task#1-3. It does not carry the CancelPracticeRun step, so closing it would leave this divergence unexercised; this issue carries the practice-run replay key.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 34 steps, 24 passed, 9 divergence clusters).

CancelPracticeRun - fixture arc-zonal-shift-practice-run-error-paths, step task#2:
  expected status 403, actual status 403 (status matches)
  $ : value_mismatch (expected {}, got {'__type': 'AccessDeniedException'})
  divergence_key: service=arc-zonal-shift|CancelPracticeRun|status=403->403|keys=$
  verdict: mismatch; stub_path: empty (operation is implemented); owned_by: empty (no open issue carries this replay key).

The diverging element is the response body only: the recorded body is the empty JSON object, and the emulator emits an error document whose sole member is the `__type` discriminator with the value "AccessDeniedException". No part of the recorded body is request- or state-derived - it is the empty object for this error class on this operation.

Model evidence (services/service-com-amazonaws-arczonalshift-percdataplane/smithy-model/com-amazonaws-arczonalshift-percdataplane.json, via smithy_query query=operation operation=CancelPracticeRun): http {method: DELETE, uri: /practiceruns/{zonalShiftId}, code: 200}; errors include AccessDeniedException (errorKind client, httpError 403).

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it.
2. Replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
3. Drive the CancelPracticeRun access-denied path: DELETE /practiceruns/{zonalShiftId} for a zonal shift id the caller is not permitted to cancel - fixture arc-zonal-shift-practice-run-error-paths, step task#2.
4. Observe the response: status 403 is correct, but the body is {"__type": "AccessDeniedException"} while the recorded service response body is {}.
5. The step passes only when the 403 for this operation is returned with an empty JSON body (no __type, no message member).
