Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.callback-heartbeat-extends-deadline lambda.callback-no-heartbeat-times-out lambda.heartbeat-extends-callback-window lambda.durable-execution-checkpoint-ttl-bound
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.callback-heartbeat-extends-deadline|run#9|AssertionError|raise _never_ready(
Issue-Key: lambda.callback-no-heartbeat-times-out|run#6|AssertionError|raise AssertionError(
Issue-Key: lambda.heartbeat-extends-callback-window|_poll_until#9|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Key: lambda.durable-execution-checkpoint-ttl-bound|run#12|AssertionError|assert 15 <= elapsed < 100, (
Issue-Commit: bc4720188099afd2bb4f421d1e83533814225f44

# The durable-execution callback plane never fires, and a checkpoint's TTL is not bound by the execution's own ExecutionTimeout

> _Split out of `issues/things-that-now-work/20261109T210043Z-lambda-10-net-new-causes-lambda-publishes-unreachable-data.md`
> (causes 3 and 4 of that bundle), which closed its other causes. The two kept here are one subsystem —
> the durable-execution runtime's checkpoint machinery — and carry that bundle's replay keys for them.
> They were identified against commit `bc4720188099afd2bb4f421d1e83533814225f44`, so the code base you
> are working in can have drifted._

## Operation
CheckpointDurableExecution

## Description
Two root causes in the durable-execution runtime.

Cause 1 of 2 — the durable-execution callback plane never fires.
AWS: a durable function that calls wait_for_callback reaches a callback checkpoint, which appears in the
execution history as a CallbackStartedDetails event and delivers a CallbackId through the configured
notify callback; a heartbeat extends the callback deadline and SendCallbackSuccess resolves it, after
which the execution's destination receives the record.
Emulator: the checkpoint is never reached — no CallbackStartedDetails event appears, no CallbackId is
ever delivered, and the destination queue stays empty for the whole poll window.
Why: every callback-based durable workflow hangs; a client waiting for a CallbackId waits forever with
nothing in the history to explain it.
Fix: the durable-execution runtime's callback checkpoint handling (history writer, CallbackId minting,
notify delivery and heartbeat deadline accounting).
Traces: lambda.callback-heartbeat-extends-deadline, lambda.callback-no-heartbeat-times-out,
lambda.heartbeat-extends-callback-window.

Cause 2 of 2 — a durable execution with a 30 s ExecutionTimeout fails after ~9 s.
AWS: a durable execution runs until its work completes or its own ExecutionTimeout elapses; a 30 s
timeout forces the execution out at ~30 s, not materially sooner, and the terminal status reflects that
timeout.
Emulator: the execution reaches FAILED in ~9 s — well inside its own 30 s ExecutionTimeout and far
short of the in-flight work it was given.
Why: a client cannot rely on ExecutionTimeout at all: work that AWS would let run is cut short, and the
failure is attributed to the execution rather than to a timeout the client configured.
Fix: the durable-execution state machine's timeout/TTL accounting for checkpoints (the bound must come
from the execution's configured ExecutionTimeout).
Traces: lambda.durable-execution-checkpoint-ttl-bound.

## Evidence
Cause 1: AssertionError: durable execution <arn>/durable-execution/<name>/<id> never delivered a
CallbackId over <queue url> (the notify callback passed to wait_for_callback never fired within the
poll window); ... never reached the wait_for_callback checkpoint (no CallbackStartedDetails history
event appeared); a message to arrive on <queue url> not met within 60s. Last result: None.
Cause 2: AssertionError: execution reached FAILED in 9.0s; its own 30s ExecutionTimeout should force it
out well before the 120s in-flight wait would complete naturally, and not implausibly fast.
(arns, queue URLs, names and ids in these lines are per-run values, never requirements.)

## Steps to Reproduce
Cause 1: Create a durable function that calls wait_for_callback with a notify destination; Invoke it
and poll the execution history for CallbackStartedDetails and the destination queue for the CallbackId.
Cause 2: Start a durable execution with ExecutionTimeout=30 whose work would run longer; observe it
ends FAILED after ~9 s.
