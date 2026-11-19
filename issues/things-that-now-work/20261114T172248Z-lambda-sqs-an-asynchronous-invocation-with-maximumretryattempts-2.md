Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts
Issue-Key: lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts|run#23|AssertionError|assert len(two_attempts) == 3, (
Issue-Commit: 225b72df8882575de41dd997e8ca538d39a59320

# lambda__sqs: an asynchronous invocation with MaximumRetryAttempts=2 runs only two attempts instead of three, so one retry of the budget is never spent

> _Note: this issue was identified against the code base at commit `225b72df8882575de41dd997e8ca538d39a59320`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1: Lambda's asynchronous retry driver spends at most ONE retry regardless of the configured MaximumRetryAttempts, so a function configured with MaximumRetryAttempts=2 produces two handler entries (the original plus one retry) where AWS produces three.

This check was previously grounded in the open issue 20261108T104640Z-lambda-sqs-an-asynchronous-invoke-with-maximumretryattempts-2.md, which describes the async plane as wholly inert (no attempt and no OnFailure record arrived at all, the check timed out); that is no longer what the emulator does and that issue does not carry this check's current replay key, so the residual cause is grounded here. The sibling issues 20261107T014900Z-lambda-sqs-the-asynchronous-invocation-plane-is-inert.md and 20261110T232651Z-lambda-sqs-2-net-new-causes-asynchronous-invocations.md own other checks in this group and do not cover an off-by-one attempt count.

AWS: Smithy: PutFunctionEventInvokeConfig.MaximumRetryAttempts is an integer with @range min 0 / max 2, documented as "The maximum number of times to retry when the function returns an error" — i.e. retries IN ADDITION to the original invocation. Invoke with InvocationType='Event' returns 202 and queues the event; a handler that always raises therefore runs 1 + MaximumRetryAttempts times, so MaximumRetryAttempts=2 yields exactly THREE entries for one event, and the OnFailure destination then receives one invocation record with requestContext.condition == 'RetriesExhausted' and requestContext.approximateInvokeCount == 3, written with the function's execution role. Retries reuse the same event payload, so every attempt carries the same marker. The inter-attempt backoff (documented as roughly 1 minute then 2 minutes) is recorded behavior only and is NOT the requirement — the attempt COUNT and the record's requestContext fields are.

Emulator: the 0-retry twin on identical wiring is correct (exactly one attempt, one OnFailure record with condition 'RetriesExhausted' and approximateInvokeCount 1 — those assertions pass), but the MaximumRetryAttempts=2 function produces only TWO counting messages for one Event invoke, both carrying the same marker and the same aws_request_id. The configured budget of 2 is therefore honoured as a single retry: observed attempts behave as 1 + min(MaximumRetryAttempts, 1) rather than 1 + MaximumRetryAttempts.

Why: a client that relies on Lambda's documented async retry budget to ride out a transient downstream failure gets one fewer chance than it configured — a class of transient errors becomes permanent loss. The dead-letter record is also wrong in a way automation reads: approximateInvokeCount is the only place the attempt count is reported, so a consumer counting attempts, deciding whether to replay, or alerting on retry exhaustion is given a number that does not match either the configuration or what actually ran.

Fix: the asynchronous invocation retry driver in the lambda crate — the Event-invocation re-drive loop behind the Invoke handler and the event-invoke configuration it reads (PutFunctionEventInvokeConfig/UpdateFunctionEventInvokeConfig state): re-drive a failing async event up to MaximumRetryAttempts times (not once), count attempts across the whole ladder into approximateInvokeCount, and only then write the OnFailure record with condition 'RetriesExhausted'.

Traces: lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts.

## Evidence
Failing check: [mismatch] lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts — not marked ALREADY FILED and not OWNED by any open issue in this replay.

AssertionError at check_lambda__sqs_async_retry_budget_of_two_yields_exactly_three_attempts.py:125:
  MaximumRetryAttempts=2 must produce exactly THREE handler entries (the original plus two retries); the counting queue received 2 for 'marker-two-<per-run>': ['<rid>', '<rid>'] (the same request id twice)

Assertions that pass before it (the control half): the MaximumRetryAttempts=0 twin produced exactly ONE handler entry, and its OnFailure record reported requestContext.condition == 'RetriesExhausted' with requestContext.approximateInvokeCount == 1.

Expected (AWS): three counting messages for the one Event invoke, all carrying the same event marker, then exactly one OnFailure record with condition 'RetriesExhausted' and approximateInvokeCount 3, and no fourth attempt during a quiet window.
Actual (emulator): two counting messages (original + one retry) for the 2-retry function.

(The marker and request-id values above are per-run; the attempt count is the requirement.)

## Steps to Reproduce
1. Build and boot the emulator; replay lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts from simparity-behavioral-aws-v1 (report heading `lambda__sqs`).
2. Or directly: create three SQS queues — one counting queue the handler writes to on every entry, and one OnFailure destination per function — and an execution role granted sqs:SendMessage on all three.
3. CreateFunction twice with a handler that sends a message carrying event["marker"] and its aws_request_id to the counting queue and then raises, so every attempt fails.
4. PutFunctionEventInvokeConfig on one function with MaximumRetryAttempts=2 and DestinationConfig.OnFailure=<its queue ARN>; on the other with MaximumRetryAttempts=0 and its own OnFailure queue.
5. Invoke each once with InvocationType='Event' (expect 202), with a distinct marker each.
6. Drain the counting queue.
   Expected: exactly THREE messages for the 2-retry marker and exactly ONE for the 0-retry marker.
   Actual: TWO for the 2-retry marker (one for the 0-retry marker, which is correct).
7. Drain the OnFailure queues: expect the 2-retry record to carry requestContext.condition='RetriesExhausted' with approximateInvokeCount=3 matching the three attempts counted in step 6.
