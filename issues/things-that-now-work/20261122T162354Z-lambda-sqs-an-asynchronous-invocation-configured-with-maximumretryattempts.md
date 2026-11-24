Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts
Issue-Key: lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts|run#23|AssertionError|assert len(two_attempts) == 3, (
Issue-Commit: e3483ea57c457ab7b31d6d09f2c005f800b70f84

# lambda__sqs: an asynchronous invocation configured with MaximumRetryAttempts=2 still runs only two attempts instead of three — the async retry driver spends at most one retry

> _Note: this issue was identified against the code base at commit `e3483ea57c457ab7b31d6d09f2c005f800b70f84`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
ONE root cause (Cause 1 of 1), grounded in 1 failing behavioral check of this replay. Service token copied verbatim from the suite heading `### lambda__sqs`. Per-service bootstrap gate: get_bootstrap_status() reports `pending: []` (every service crate, including the lambda and sqs crates, is bootstrapped and answering), so this is filed as a fidelity finding, not a bootstrap gap.

PRIOR ART / WHY THIS IS NOT COVERED. The RESOLVED issue 20261114T172248Z-lambda-sqs-an-asynchronous-invocation-with-maximumretryattempts-2.md carried exactly this check id (lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts) and claimed exactly this cause (an off-by-one attempt ladder: 1 + min(MaximumRetryAttempts, 1) instead of 1 + MaximumRetryAttempts); the check still fails with the same frame (run -> assert len(two_attempts) == 3) and the same observed count of 2, so that fix did not take and a resolved issue never covers a still-failing check. The earlier resolved issue 20261108T104640Z-lambda-sqs-an-asynchronous-invoke-with-maximumretryattempts-2.md described the async plane as wholly inert and no longer matches what the emulator does. This is NOT a refusal-class regression — Invoke with InvocationType='Event' is accepted (202) and attempts do run — so it is not opened with the REOPENS wording reserved for a fixed issue whose call came back refused. Still-open siblings that do NOT cover it: 20261110T232651Z-lambda-sqs-2-net-new-causes-asynchronous-invocations.md (carries async-event-that-fails-once-then-succeeds-on-retry..., async-retry-budget-of-ONE..., and three ReportBatchItemFailures checks — not this one, and an all-or-nothing 'never retried' cause, not an off-by-one), 20261114T003845Z-lambda-sqs-4-net-new-causes-in-the.md (async record/dead-letter METADATA shape) and 20261118T150346Z-lambda-sqs-reservedconcurrentexecutions-1-does-not-throttle-a.md (concurrency throttling). This replay marks the present check neither ALREADY FILED nor OWNED, so its replay key is filed here.

Cause 1 of 1 — Lambda's asynchronous retry driver re-drives a failing Event invocation at most ONCE regardless of the configured MaximumRetryAttempts, so a function configured with MaximumRetryAttempts=2 runs two handler entries where AWS runs three. Failing check: lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts.

AWS: Smithy (com.amazonaws.lambda#AWSGirApiService): PutFunctionEventInvokeConfig (PUT /2019-09-25/functions/{FunctionName}/event-invoke-config, 200) carries MaximumRetryAttempts as an integer with @range min 0 / max 2 — documented as the maximum number of times to retry when the function returns an error, i.e. retries IN ADDITION to the original invocation — alongside MaximumEventAgeInSeconds (@range 60..21600) and DestinationConfig (OnSuccess/OnFailure). Invoke (POST /2015-03-31/functions/{FunctionName}/invocations) with InvocationType='Event' answers 202 and queues the event. A handler that always raises therefore runs 1 + MaximumRetryAttempts times, so MaximumRetryAttempts=2 yields exactly THREE handler entries for one event, after which exactly one invocation record is written to the OnFailure destination with requestContext.condition == 'RetriesExhausted' and requestContext.approximateInvokeCount == 3, using the function's execution role. Retries reuse the same event payload, so every attempt carries the same event body. The inter-attempt backoff (documented as roughly 1 minute then 2 minutes) is recorded behavior only and is NOT the requirement — the attempt COUNT and the record's requestContext fields are.

Emulator: the MaximumRetryAttempts=0 twin on identical wiring is correct (exactly one attempt, and its OnFailure record reports condition 'RetriesExhausted' with approximateInvokeCount 1 — those assertions pass), but the MaximumRetryAttempts=2 function produces only TWO counting messages for one Event invoke, both carrying the same event marker and the same aws_request_id. The configured budget of 2 is therefore honoured as a single retry: the observed ladder is 1 + min(MaximumRetryAttempts, 1) rather than 1 + MaximumRetryAttempts. The marker string and request-id values are per-run and are not part of the requirement; the attempt count is.

Why: a client that relies on Lambda's documented asynchronous retry budget to ride out a transient downstream failure gets one fewer chance than it configured, so a class of transient errors becomes permanent event loss. The dead-letter record is wrong in a way automation reads: requestContext.approximateInvokeCount is the only place the attempt count is reported, so a consumer that counts attempts, decides whether to replay, or alerts on retry exhaustion is handed a number that matches neither the configuration nor what actually ran.

Fix: the asynchronous invocation retry driver in the lambda crate — the Event-invocation re-drive loop behind the Invoke handler and the event-invoke configuration state it reads (PutFunctionEventInvokeConfig/UpdateFunctionEventInvokeConfig): re-drive a failing async event up to MaximumRetryAttempts times rather than once, accumulate the attempt count across the whole ladder into the invocation record's requestContext.approximateInvokeCount, and only write the OnFailure record with condition 'RetriesExhausted' once the budget is actually exhausted.

Traces: lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout (emulator serving; bootstrap_signal false — 1339/1572 checks passed, 85.2%; this check is the only lambda__sqs failure not marked ALREADY FILED or OWNED).

[mismatch] lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts
  File "run.py", line 456, in <lambda> -> body = calls.in_phase(calls.BODY, lambda: fn(ctx))
  File "check_lambda__sqs_async_retry_budget_of_two_yields_exactly_three_attempts.py", line 125, in run -> assert len(two_attempts) == 3, (...)
  AssertionError: MaximumRetryAttempts=2 must produce exactly THREE handler entries (the original plus two retries); the counting queue received 2 for 'marker-two-<per-run>': ['<request id>', '<request id>'] (the same request id twice)

Assertions that PASS before it (the control half): the MaximumRetryAttempts=0 twin produced exactly ONE handler entry, and its OnFailure record reported requestContext.condition == 'RetriesExhausted' with requestContext.approximateInvokeCount == 1.

Expected (AWS): three counting messages for the one Event invoke, all carrying the same event marker, then exactly one OnFailure record with requestContext.condition == 'RetriesExhausted' and requestContext.approximateInvokeCount == 3, and no fourth attempt during the quiet window.
Actual (emulator): two counting messages (original + exactly one retry) for the 2-retry function.

Smithy grounding: PutFunctionEventInvokeConfig.MaximumRetryAttempts is an integer with @range {min: 0, max: 2}; Invoke's InvocationType='Event' path returns 202. The marker and request-id values above are per-run values; the attempt count is the requirement.

Note on prior art: the RESOLVED issue 20261114T172248Z-lambda-sqs-an-asynchronous-invocation-with-maximumretryattempts-2.md carried this same check id and this same off-by-one cause; the failure is the same shape, so the cause is still open.

## Steps to Reproduce
1. git_clone the repo, then run_behavioral_suite (it builds and boots the emulator).
2. Re-replay the check id in trace_ids: lambda__sqs.async-retry-budget-of-two-yields-exactly-three-attempts.
3. Or directly: create three SQS queues — one counting queue the handler writes to on every entry, and one OnFailure destination queue per function — and an execution role granted sqs:SendMessage on all three.
4. CreateFunction twice with a handler that sends a message carrying event['marker'] and its aws_request_id to the counting queue and then raises, so every attempt fails.
5. PutFunctionEventInvokeConfig on the first function with MaximumRetryAttempts=2 and DestinationConfig.OnFailure=<its destination queue ARN>; on the second with MaximumRetryAttempts=0 and its own OnFailure queue.
6. Invoke each once with InvocationType='Event' (expect HTTP 202), with a distinct marker per function.
7. Drain the counting queue.
   Expected: exactly THREE messages carrying the 2-retry marker and exactly ONE carrying the 0-retry marker.
   Actual: TWO for the 2-retry marker (the 0-retry marker is correct at one).
8. Drain the OnFailure queues: the 2-retry record must carry requestContext.condition == 'RetriesExhausted' with requestContext.approximateInvokeCount == 3, matching the three attempts counted in step 7.
9. Wait a quiet window equal to the span between the first and last observed attempt and confirm no fourth attempt arrives.
