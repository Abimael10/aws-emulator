Issue-Source: trace-sweep-divergence
Issue-Trace: sagemakertrainingsessionruntime-resource-not-found
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemakertrainingsessionruntime-resource-not-found sagemakertrainingsessionruntime-validation-errors sagemakertrainingsessionruntime-execute-sampling-operation-generates-text sagemakertrainingsessionruntime-identifier-pattern-not-enforced-on-read-and-stream
Issue-Shape-Id: com.amazonaws.sagemakertrainingsessionruntime#LegoRouterService
Issue-Key: sagemakertrainingsessionruntime|ExecuteSamplingOperation|4xx->2xx|$.message
Issue-Key: sagemakertrainingsessionruntime|ExecuteSamplingOperation|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: sagemakertrainingsessionruntime|ExecuteSamplingOperation|4xx->2xx|status
Issue-Key: sagemakertrainingsessionruntime|ExecuteSamplingOperation|4xx->4xx|$.message
Issue-Key: sagemakertrainingsessionruntime|ExecuteSamplingOperation|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: sagemakertrainingsessionruntime|ExecuteSamplingOperation|4xx->4xx|status
Issue-Key: sagemakertrainingsessionruntime|GetSamplingOperationResult|4xx->4xx|$.message
Issue-Key: sagemakertrainingsessionruntime|GetTrainingOperationResult|4xx->4xx|$.message
Issue-Key: sagemakertrainingsessionruntime|GetTrainingOperationResult|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: sagemakertrainingsessionruntime|GetTrainingOperationResult|4xx->4xx|status
Issue-Key: sagemakertrainingsessionruntime|GetSamplingOperationResult|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: sagemakertrainingsessionruntime|GetSamplingOperationResult|4xx->4xx|status
Issue-Key: sagemakertrainingsessionruntime|SampleTrainingSessionWithResponseStream|4xx->4xx|$.message
Issue-Key: sagemakertrainingsessionruntime|SampleTrainingSessionWithResponseStream|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: sagemakertrainingsessionruntime|SampleTrainingSessionWithResponseStream|4xx->4xx|status
Issue-Commit: c57dff367a65e304f9274fadba3272377fe8966a

# sagemakertrainingsessionruntime resource-resolution path diverges: unknown session accepted, pattern validation preempts the 404, and not-found messages are emulator-authored

> _Note: this issue was identified against the code base at commit `c57dff367a65e304f9274fadba3272377fe8966a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteSamplingOperation

## Description
Seven replayed steps of the SageMakerTrainingSessionRuntime (LegoRouterService) corpus diverge on the one shared fault: the emulator does not resolve the named training session / operation the way AWS does, so the 404 ResourceNotFoundException path is reached at the wrong time, with the wrong status, or with emulator-authored prose. Seven causes, each with its own fixture step.

Cause 1 of 7: ExecuteSamplingOperation materialises a training session that does not exist instead of refusing it.
AWS: ExecuteSamplingOperation is a data-plane call against a session the control plane already created. Smithy: ExecuteSamplingOperation models ResourceNotFoundException (httpError 404), documented as "The specified training session or operation does not exist, or the operation does not belong to the specified training session." The recording shows a submit naming a session that was never created answered 404 with message "Session not found: <trainingSessionName>-<accountId>-<region>" — fixed text "Session not found: " followed by the session identity assembled from the request's trainingSessionName label plus the caller's account id and the signing region (recorded behavior for the identity's exact composition).
Emulator: answers HTTP 200 with a freshly minted operationId; no $.message and no x-amzn-errortype header are emitted at all. src/state.rs documents this explicitly — session records are "materialised on first use of a session name" — and ops/execute_sampling_operation.rs only falls out of session_for_submit with a ThrottlingException when the store is full, never with ResourceNotFoundException.
Why: a client that checks whether a session exists by submitting to it (or that typos the session name) gets a success and an operationId that resolves, so a missing or torn-down training session is never surfaced; error handling for a deleted session is dead code against the emulator.
Fix: ExecuteSamplingOperation's handler plus the session lookup it uses in state (session_for_submit) — a submit must resolve an existing session and return ResourceNotFoundException when there is none, rather than creating one on first use.
Traces: sagemakertrainingsessionruntime-resource-not-found#task#1.

Cause 2 of 7: because the session is never resolved, a submit to a nonexistent session is refused on the request body instead of with the 404.
AWS: for a submit that both names a nonexistent session and carries an operationConfig union with more than one member set, the recording shows HTTP 404 with "Session not found: <trainingSessionName>-<accountId>-<region>" — i.e. resource resolution is reported ahead of the union-arity refusal. Smithy: SampleOperationConfig is a union ("Exactly one member must be set") and ValidationException is modelled at httpError 400, so the arity refusal exists, but it is not what AWS answers here.
Emulator: HTTP 400 with x-amzn-errortype ValidationException and message "union 'SampleOperationConfig' had more than one member set" — emulator-authored text (AWS does not word a union-arity refusal that way), and the wrong status/error class for this request.
Why: a client cannot tell a missing session from a malformed body; retry logic keyed on ResourceNotFoundException never fires, and the message is not AWS's.
Fix: ExecuteSamplingOperation's handler ordering — resolve the training session before the operationConfig arity/validation gate runs; the boundary validator's union message also needs AWS's wording for the cases where AWS does refuse on arity.
Traces: sagemakertrainingsessionruntime-validation-errors#task#4.

Cause 3 of 7: GetSamplingOperationResult's 404 message is emulator prose, not AWS's.
AWS: for an operationId that does not resolve on the sampling route, the recorded 404 body is message "Sample request not found: <operationId>" — fixed text "Sample request not found: ", variable part the operationId taken verbatim from the request URI label. Smithy: ResourceNotFoundException{message} required, httpError 404.
Emulator: status 404 is right, but the message is "Sampling operation <operationId> does not exist, or does not belong to training session <trainingSessionName>." (ops/get_sampling_operation_result.rs) — different fixed text and an extra variable (the session name) AWS does not include.
Why: clients and recorded-fixture comparisons that match on the refusal text see a different error string for the same condition; the message is part of the error contract.
Fix: the ResourceNotFoundException message construction in GetSamplingOperationResult's handler.
Traces: sagemakertrainingsessionruntime-execute-sampling-operation-generates-text#task#9 (same cluster also covers sagemakertrainingsessionruntime-resource-not-found#task#2).

Cause 4 of 7: GetTrainingOperationResult's 404 message is emulator prose, not AWS's.
AWS: for an operationId that does not resolve on the training route, the recorded 404 body is message "Operation not found: <operationId>" — fixed text "Operation not found: ", variable part the request's operationId.
Emulator: status 404 with "Training operation <operationId> does not exist, or does not belong to training session <trainingSessionName>." — emulator wording plus a session-name variable AWS omits.
Why: same as cause 3 — the refusal text for a missing training operation does not match AWS's.
Fix: the ResourceNotFoundException message construction in GetTrainingOperationResult's handler.
Traces: sagemakertrainingsessionruntime-resource-not-found#task#3.

Cause 5 of 7: GetTrainingOperationResult enforces the identifier @pattern on its URI labels where AWS resolves the resource and 404s.
AWS: Smithy declares trainingSessionName/operationId with length 1..64 and pattern ^[a-zA-Z0-9-]+$, but the recording shows AWS does NOT refuse a label that violates it on this read path: a request with trainingSessionName "bad name!" was answered HTTP 404 ResourceNotFoundException with "Operation not found: <operationId>" (recorded behavior; the pattern is modelled but not server-enforced here).
Emulator: HTTP 400 with x-amzn-errortype ValidationException and message "1 validation error detected: Value 'bad name!' at 'trainingSessionName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9-]+".
Why: a client that passes an identifier containing an unexpected character sees a 400 validation failure where AWS reports the resource simply does not exist, so not-found handling (create-on-miss, polling give-up) takes the wrong branch.
Fix: the boundary/constraint validation applied to this operation's httpLabel members — the modelled pattern must not be enforced on this read path; resolution must proceed to the ResourceNotFoundException with AWS's "Operation not found: <operationId>" text (cause 4).
Traces: sagemakertrainingsessionruntime-identifier-pattern-not-enforced-on-read-and-stream#task#1.

Cause 6 of 7: GetSamplingOperationResult enforces the same patterns instead of answering the 404.
AWS: with both labels pattern-violating (trainingSessionName "bad name!", operationId "bad id!") the recording shows HTTP 404 ResourceNotFoundException with "Sample request not found: <operationId>" — the operationId echoed verbatim from the request, even though it violates the modelled pattern.
Emulator: HTTP 400 ValidationException "2 validation errors detected: Value 'bad name!' at 'trainingSessionName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9-]+; Value 'bad id!' at 'operationId' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9-]+".
Why: as cause 5 — wrong status and wrong error class for an unresolvable sampling operation.
Fix: the same httpLabel constraint gate for GetSamplingOperationResult, then the 404 path with AWS's "Sample request not found: <operationId>" text.
Traces: sagemakertrainingsessionruntime-identifier-pattern-not-enforced-on-read-and-stream#task#2.

Cause 7 of 7: SampleTrainingSessionWithResponseStream enforces the session-name pattern instead of answering the session 404.
AWS: a streaming sample against a pattern-violating session name is answered HTTP 404 ResourceNotFoundException with "Session not found: <trainingSessionName>-<accountId>-<region>" — the session label echoed verbatim into the session identity (recorded behavior).
Emulator: HTTP 400 ValidationException "1 validation error detected: Value 'bad name!' at 'trainingSessionName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9-]+".
Why: a streaming client sees a validation refusal rather than the not-found it must handle, and never reaches the event-stream error path AWS exposes.
Fix: the httpLabel constraint gate for SampleTrainingSessionWithResponseStream plus its session resolution, which must emit ResourceNotFoundException with AWS's "Session not found: " text.
Traces: sagemakertrainingsessionruntime-identifier-pattern-not-enforced-on-read-and-stream#task#3.

No open issue covers this service; the only existing issue naming it (20261021T231759Z-bootstrap-service-com-amazonaws-sagemakertrainingsessionruntime-legorouterservice.md) is the resolved bootstrap issue.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 37.9%, 17 mismatched steps), all seven clusters on service sagemakertrainingsessionruntime, none carrying stub_path or owned_by:

1. ExecuteSamplingOperation — fixture sagemakertrainingsessionruntime-resource-not-found step task#1: expected 404, got 200; $.message missing (expected 'Session not found: sim2real-probe-sagemakertrainingsessionruntime-48dfe674-000000000000-us-west-2', got None); $headers.x-amzn-errortype missing.
2. ExecuteSamplingOperation — fixture sagemakertrainingsessionruntime-validation-errors step task#4: expected 404, got 400; $.message expected 'Session not found: sim2real-probe-sagemakertrainingsessionruntime-5a347b1b-000000000000-us-west-2', got "union 'SampleOperationConfig' had more than one member set"; x-amzn-errortype got 'ValidationException'.
3. GetSamplingOperationResult — fixture sagemakertrainingsessionruntime-execute-sampling-operation-generates-text step task#9 (cluster also: sagemakertrainingsessionruntime-resource-not-found#task#2): 404 both sides; $.message expected 'Sample request not found: sim2real-probe-tsr-fake-op-de21cc41', got 'Sampling operation sim2real-probe-tsr-fake-op-de21cc41 does not exist, or does not belong to training session sim2real-probe-tsr-sess-58624d12.'
4. GetTrainingOperationResult — fixture sagemakertrainingsessionruntime-resource-not-found step task#3: 404 both sides; $.message expected 'Operation not found: sim2real-probe-sagemakertrainingsessionruntime-op-ed6b6284', got 'Training operation sim2real-probe-sagemakertrainingsessionruntime-op-ed6b6284 does not exist, or does not belong to training session sim2real-probe-sagemakertrainingsessionruntime-48dfe674.'
5. GetTrainingOperationResult — fixture sagemakertrainingsessionruntime-identifier-pattern-not-enforced-on-read-and-stream step task#1: expected 404, got 400; expected $.message 'Operation not found: sim2real-probe-sagemakertrainingsessionruntime-op-e2b14d06', got "1 validation error detected: Value 'bad name!' at 'trainingSessionName' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z0-9-]+"; x-amzn-errortype got 'ValidationException'.
6. GetSamplingOperationResult — same fixture step task#2: expected 404, got 400; expected $.message 'Sample request not found: bad id!', got "2 validation errors detected: ...'trainingSessionName'...; ...'operationId'... pattern: [a-zA-Z0-9-]+"; x-amzn-errortype got 'ValidationException'.
7. SampleTrainingSessionWithResponseStream — same fixture step task#3: expected 404, got 400; expected $.message 'Session not found: bad name!-000000000000-us-west-2', got "1 validation error detected: Value 'bad name!' at 'trainingSessionName' ... pattern: [a-zA-Z0-9-]+"; x-amzn-errortype got 'ValidationException'.

Source corroboration: services/service-com-amazonaws-sagemakertrainingsessionruntime-legorouterservice/src/state.rs:10 ("session records (sessions), materialised on first use of a session name"); src/ops/execute_sampling_operation.rs:166-192 (config validation runs before session lookup; session_for_submit never yields ResourceNotFoundException); src/ops/get_sampling_operation_result.rs:171-197 (emulator-authored 404 messages).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay trace set simparity-wire-aws-v1 (replay_wire_corpus) and read the sagemakertrainingsessionruntime clusters.
3. Cause 1: POST /v1/training-sessions/<never-created-session>/sampling-operations with a single-member operationConfig -> emulator 200 + operationId; AWS 404 ResourceNotFoundException "Session not found: <session>-<account>-<region>" (fixture sagemakertrainingsessionruntime-resource-not-found#task#1).
4. Cause 2: same POST with two members set in operationConfig against a nonexistent session -> emulator 400 ValidationException "union 'SampleOperationConfig' had more than one member set"; AWS 404 "Session not found: ..." (sagemakertrainingsessionruntime-validation-errors#task#4).
5. Causes 3-4: GET /v1/training-sessions/<existing-session>/sampling-operations/<unknown-op> and .../operations/<unknown-op> -> compare the 404 message against AWS's "Sample request not found: <operationId>" / "Operation not found: <operationId>".
6. Causes 5-7: GET .../operations/<op> and .../sampling-operations/<op> and POST /training-sessions/<name>/sample-with-response-stream using a session name 'bad name!' (and operationId 'bad id!') -> emulator 400 ValidationException; AWS 404 ResourceNotFoundException (fixture sagemakertrainingsessionruntime-identifier-pattern-not-enforced-on-read-and-stream#task#1..3).
