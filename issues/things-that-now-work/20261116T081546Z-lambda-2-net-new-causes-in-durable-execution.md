Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.durable-execution-history-ordering lambda.durable-execution-server-managed-append-only-history
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.durable-execution-history-ordering|run#15|AssertionError|assert wait_started_idx != -1, f"expected a WaitStartedDetails event in history, got events={events}"
Issue-Key: lambda.durable-execution-server-managed-append-only-history|run#22|AssertionError|assert code == "InvalidParameterValueException", (
Issue-Commit: 5636e5ddac188567344946aa5c15e82054f1c023

# lambda: 2 net-new causes in durable execution — GetDurableExecutionHistory records no WaitStarted/WaitSucceeded events, and CheckpointDurableExecution answers NotImplemented for an invalid checkpoint token

> _Note: this issue was identified against the code base at commit `5636e5ddac188567344946aa5c15e82054f1c023`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDurableExecutionHistory

## Description
Two net-new root causes in this service group (every other failure under this heading is marked ALREADY FILED or OWNED and is excluded).

Cause 1 of 2: a durable execution that calls context.wait(...) has no WaitStarted/WaitSucceeded pair in its history — the wait is performed but never checkpointed as history events.
Failing check id: lambda.durable-execution-history-ordering.
AWS: Smithy (lambda model): GetDurableExecutionHistory (GET /2025-12-01/durable-executions/{DurableExecutionArn}/history, output Events required, paginated by Marker) and the EventType enum, whose members include WaitStarted, WaitSucceeded and WaitCancelled alongside ExecutionStarted/ExecutionSucceeded and StepStarted/StepSucceeded/StepFailed; each event carries the per-type details member (WaitStartedDetails, WaitSucceededDetails, StepStartedDetails, ...). Recorded behavior: a handler that calls context.wait(Duration) inside a durable execution produces a WaitStarted event followed by a WaitSucceeded event, strictly between ExecutionStarted (first) and ExecutionSucceeded (last), ordered consistently by EventTimestamp and EventId. The event ids and timestamps are per-run values; the presence and relative order of the pair is the requirement.
Emulator: the history is returned 200 but contains only ExecutionStarted, three StepStarted/StepSucceeded pairs and ExecutionSucceeded — no event with WaitStartedDetails (nor WaitSucceededDetails) exists, so the check reports "AssertionError: expected a WaitStartedDetails event in history, got events=[{'EventType': 'ExecutionStarted', 'EventId': 1, ...}, {'EventType': 'StepStarted', 'EventId': 2, 'Id': 'step-1', ...}, {'EventType': 'StepSucceeded', 'EventId': 3, ...}, {'EventType': 'StepStarted', 'EventId': 4, 'Id': 'step-2', ...}, ...]". The recorded timestamps show the wait itself did elapse (step-1 succeeded at 20.939 and step-2 started at 22.942, the 2 s the handler asked for), so the emulator performs the wait and omits its checkpoints. EventId numbering is contiguous across the missing pair, i.e. the events are not merely hidden from the read.

Cause 2 of 2: CheckpointDurableExecution with a stale/invalid CheckpointToken is answered with the error code NotImplemented instead of the modelled client refusal.
Failing check id: lambda.durable-execution-server-managed-append-only-history.
AWS: Smithy (lambda model): CheckpointDurableExecution (POST /2025-12-01/durable-executions/{DurableExecutionArn}/checkpoint; required members DurableExecutionArn and CheckpointToken, pattern ^[A-Za-z0-9+/]+={0,2}$, length 1-2048) models exactly these errors: InvalidParameterValueException (400), KMSAccessDeniedException/KMSDisabledException/KMSInvalidStateException/KMSNotFoundException (502), ServiceException (500), TooManyRequestsException (429) — NotImplemented is not among them. Recorded behavior: a well-formed but not-current checkpoint token is refused with HTTP 400, error code InvalidParameterValueException and the message "Invalid checkpoint token" (fixed text, no per-run parts), and the refusal must leave the execution's history exactly as it was.
Emulator: the call is answered with error code NotImplemented — "AssertionError: expected InvalidParameterValueException for a stale/invalid checkpoint token, got NotImplemented" — so the operation refuses every caller with an unmodelled code instead of validating the token, and the append-only-history guarantee behind it is unobservable.

Why: Cause 1 — GetDurableExecutionHistory is the only record of what a durable execution did. A client that reconstructs progress, bills or audits per phase, or waits for a WaitSucceeded checkpoint before resuming, sees an execution that apparently never waited: a wait that is not in the history cannot be replayed, resumed or reasoned about, and a consumer that indexes the wait pair fails outright. Cause 2 — a client that handles InvalidParameterValueException to refresh a stale token (the normal response to a lost race on a checkpoint) instead sees an unmodelled NotImplemented, which its SDK surfaces as an unknown/retryable server condition rather than the "get a fresh token" signal, and no caller can checkpoint at all.

Fix: Cause 1 — the durable-execution engine's history/checkpoint writer: a context.wait must append a WaitStarted event when the wait begins and a WaitSucceeded event when it elapses (WaitCancelled when it is cancelled), with the same EventId/EventTimestamp ordering as the step events, and GetDurableExecutionHistory must serialize them with their details members. Cause 2 — the CheckpointDurableExecution handler and its validation: validate CheckpointToken against the execution's current token and refuse a non-current one with HTTP 400 InvalidParameterValueException / "Invalid checkpoint token" without mutating history, rather than answering NotImplemented.

Traces: lambda.durable-execution-history-ordering (cause 1), lambda.durable-execution-server-managed-append-only-history (cause 2).

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the booted emulator: 1945/2850 passed (68.2%); 725 mismatched, 180 errored; bootstrap_signal false. Under the 'lambda' heading all other failures are marked ALREADY FILED or OWNED by open issues; these two are not.

Cause 1 — [mismatch] lambda.durable-execution-history-ordering
  File "check_lambda_durable_execution_history_ordering.py", line 93, in run
    assert wait_started_idx != -1, f"expected a WaitStartedDetails event in history, got events={events}"
  AssertionError: expected a WaitStartedDetails event in history, got events=[{'EventType': 'ExecutionStarted', 'EventId': 1, 'ExecutionStartedDetails': {'Input': {'Payload': '{"n": 3}', 'Truncated': False}, 'ExecutionTimeout': 300}}, {'EventType': 'StepStarted', 'EventId': 2, 'Id': 'step-1', 'Name': 'step_one', ...}, {'EventType': 'StepSucceeded', 'EventId': 3, 'Id': 'step-1', 'StepSucceededDetails': {'Result': {'Payload': '{"step": 1, "value": 3}', ...}, 'RetryDetails': {'CurrentAttempt': 1}}}, {'EventType': 'StepStarted', 'EventId': 4, 'Id': 'step-2', 'Name': 'step_two', ...}, {'EventType': 'StepSucceeded', 'EventId': 5, ...}, {'EventType': 'StepStarted', 'EventId': 6, 'Id': 'step-3', ...}, {'EventType': 'StepSucceeded', 'EventId': 7, ...}]
The handler is @durable_execution with context.step(step_one), context.wait(Duration.from_seconds(2)), context.step(step_two), context.step(step_three). The execution reached SUCCEEDED, the first event carried ExecutionStartedDetails and the last ExecutionSucceededDetails, so only the wait pair is missing. The step-1 StepSucceeded timestamp (…:20.939) and the step-2 StepStarted timestamp (…:22.942) are ~2 s apart — the wait happened, it was simply never checkpointed.
Expected (AWS): exactly one event with WaitStartedDetails and one with WaitSucceededDetails, both strictly between the first and last events and in that relative order.

Cause 2 — [mismatch] lambda.durable-execution-server-managed-append-only-history
  File "check_lambda_durable_execution_server_managed_append_only_history.py", line 176, in run
    assert code == "InvalidParameterValueException", (
  AssertionError: expected InvalidParameterValueException for a stale/invalid checkpoint token, got NotImplemented
The graded call is lam.checkpoint_durable_execution(DurableExecutionArn=<arn of a completed durable execution>, CheckpointToken=base64(b"sim2real-behav-not-the-real-token")) — a token matching the modelled pattern but not the execution's current one. GetDurableExecutionHistory before the call returned at least one event, and the check then re-reads the history to prove the refusal rewrote nothing; that leg is ungraded.
Expected (AWS): error code InvalidParameterValueException (HTTP 400) with message exactly "Invalid checkpoint token", and an unchanged Events list afterwards.
Actual (emulator): error code NotImplemented.

Smithy consulted: lambda CheckpointDurableExecution errors = InvalidParameterValueException(400), KMSAccessDenied/Disabled/InvalidState/NotFound(502), ServiceException(500), TooManyRequestsException(429) — NotImplemented is unmodelled; lambda EventType enum includes WaitStarted, WaitSucceeded, WaitCancelled; GetDurableExecutionHistory output Events is required.

Bootstrap gate: get_bootstrap_status() reports pending == [] — lambda is stood up in this checkout, so these are fidelity findings, not bootstrap gaps.

## Steps to Reproduce
Boot the emulator and point lambda, iam and kms clients at its data listener.
Cause 1:
1. Create a role trusting lambda.amazonaws.com with the basic execution and basic durable-execution policies; CreateFunction (python3.12) with a DurableConfig and a handler decorated @durable_execution that does context.step(step_one), context.wait(Duration.from_seconds(2)), context.step(step_two), context.step(step_three); wait for State == Active.
2. Invoke it with InvocationType='Event', DurableExecutionName=<unique>, Payload={"n": 3}; resolve the DurableExecutionArn (from the invoke response or ListDurableExecutionsByFunction) and poll GetDurableExecution until Status == 'SUCCEEDED'.
3. THE GRADED READ: GetDurableExecutionHistory(DurableExecutionArn=<arn>).
   EXPECTED (AWS): events ordered by EventTimestamp/EventId from ExecutionStarted to ExecutionSucceeded, containing one WaitStarted event (WaitStartedDetails) and one WaitSucceeded event (WaitSucceededDetails) strictly between them, in that order.
   OBSERVED: ExecutionStarted, three StepStarted/StepSucceeded pairs and ExecutionSucceeded only — no WaitStartedDetails or WaitSucceededDetails event.
Cause 2:
4. Create a durable function the same way, invoke it once and let it complete; read GetDurableExecutionHistory and keep the Events list.
5. THE GRADED CALL: CheckpointDurableExecution(DurableExecutionArn=<arn>, CheckpointToken=base64 of arbitrary bytes, e.g. base64(b"not-the-real-token")).
   EXPECTED (AWS): HTTP 400, Error.Code 'InvalidParameterValueException', Error.Message 'Invalid checkpoint token'; GetDurableExecutionHistory afterwards returns the identical Events list.
   OBSERVED: Error.Code 'NotImplemented'.
6. Re-replay both ids: lambda.durable-execution-history-ordering and lambda.durable-execution-server-managed-append-only-history.
