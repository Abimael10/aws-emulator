Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs.maximum-event-age-discards-the-throttled-async-invocation-to-the-on-failure-destination
Issue-Key: lambda__sqs.maximum-event-age-discards-the-throttled-async-invocation-to-the-on-failure-destination|run#27|AssertionError|assert len(ran_short) == 1, (
Issue-Commit: 8e5675a008353ca996c02d5e79f804eabbe67887

# lambda__sqs: ReservedConcurrentExecutions=1 does not throttle a second asynchronous invocation — both reach the handler, so nothing ever ages out to the OnFailure destination

> _Note: this issue was identified against the code base at commit `8e5675a008353ca996c02d5e79f804eabbe67887`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
1 net-new root cause.

Cause 1 of 4 ... (single cause) — Cause 1 of 1: a function capped at ReservedConcurrentExecutions=1 whose handler holds its slot for longer than the whole MaximumEventAgeInSeconds window does NOT throttle a second asynchronous (InvocationType=Event) invocation: both invocations execute the handler code. Because nothing is ever held off the function, no event ever exceeds its maximum age, and the age-out discard to the OnFailure destination is unreachable. Failing check: lambda__sqs.maximum-event-age-discards-the-throttled-async-invocation-to-the-on-failure-destination.

AWS: Smithy (service-com-amazonaws-lambda-awsgirapiservice) PutFunctionConcurrency PUT /2017-10-31/functions/{FunctionName}/concurrency, input member ReservedConcurrentExecutions @required with @range min 0, output shape Concurrency; PutFunctionEventInvokeConfig PUT /2019-09-25/functions/{FunctionName}/event-invoke-config, members MaximumRetryAttempts @range 0..2 and MaximumEventAgeInSeconds @range 60..21600 plus DestinationConfig (OnSuccess/OnFailure). The reserved value is the function's hard concurrency ceiling. With exactly one slot and a handler that occupies it beyond the event's whole age window, only ONE of two back-to-back async invokes can be dispatched; the other is throttled, held on Lambda's internal asynchronous queue and retried, and once its age exceeds MaximumEventAgeInSeconds Lambda stops retrying and discards it, writing an asynchronous invocation record to the OnFailure destination (documented at invocation-async-error-handling: an event is discarded when it exceeds the maximum event age, and the discard is reported to the on-failure destination). So exactly one nonce reaches the handler code and exactly one OnSuccess record is written; the other nonce never runs and appears only on OnFailure. Both twins in the recording are configured with the IDENTICAL MaximumRetryAttempts=2, so retries cannot explain any difference — the only differing field is the event age.

Emulator: both asynchronous invocations reach the handler. The handler's side-channel queue recorded two entries where exactly one is admissible, and the check fails with: AssertionError: the short twin's handler side channel recorded ['first-<nonce>', 'second-<nonce>'], expected exactly one entry — this is the B4 count of how many invocations actually REACHED the code. (The nonces themselves are per-run values; the requirement is the COUNT of one.) The preceding beat passed, so exactly one OnSuccess record was delivered while two invocations ran — i.e. the single reserved slot is not consulted when dispatching an Event invocation, and the asynchronous queue has no event-age expiry that could discard an undispatched event to OnFailure.

Why: a client that caps a function with reserved concurrency relies on that ceiling both as a blast-radius limit (downstream systems the handler writes to see twice the configured parallelism) and as the precondition for the age-out contract. With no throttling and no age expiry, the only signal AWS gives that a queued event was dropped — the OnFailure record whose requestContext condition names the age-out — never appears, so a silently discarded event and a successfully processed one are indistinguishable.

Fix: the Lambda asynchronous-invocation path — (a) the admission check that must compare the function's in-flight execution count against its reserved concurrency before dispatching an InvocationType=Event invoke, and queue rather than run the excess; and (b) the asynchronous queue's event-age evaluation, which must stop retrying an event older than the function's MaximumEventAgeInSeconds and hand it to the OnFailure destination instead of eventually running it. The PutFunctionConcurrency/GetFunctionConcurrency handlers already store and echo the value (the check's own assertion that it settled at 1 passed), so the defect is in the dispatcher, not in the configuration surface.

Related open issues, named for the fixer: 20261108T104640Z-lambda-sqs-an-asynchronous-invoke-with-maximumretryattempts-2.md and 20261114T172248Z-lambda-sqs-an-asynchronous-invocation-with-maximumretryattempts-2.md both concern the RETRY COUNT of async invocations, and 20261114T003845Z-lambda-sqs-4-net-new-causes-in-the.md concerns the async record metadata. None of them describes reserved-concurrency admission or event-age discard, and the suite reported this replay key unowned, so this is filed as its own work item.

Traces: lambda__sqs.maximum-event-age-discards-the-throttled-async-invocation-to-the-on-failure-destination (assertion at the check's B4 side-channel count).

## Evidence
Expected (AWS): with ReservedConcurrentExecutions=1 on a function whose handler occupies its slot for 70 s, MaximumRetryAttempts=2 and MaximumEventAgeInSeconds=60, two back-to-back Invoke calls with InvocationType=Event (both answered 202) result in: exactly ONE handler execution (the handler's side-channel queue holds exactly one nonce), exactly ONE OnSuccess record naming that same nonce in requestPayload, and exactly ONE OnFailure record whose requestPayload is {"nonce": <the other nonce>} because that event exceeded its maximum age while throttled. The identical twin configured with MaximumEventAgeInSeconds=21600 discards neither event.

Actual (emulator): both async invokes ran the handler. Replay detail:
  File "check_lambda__sqs_maximum_event_age_discards_the_throttled_async_invocation_to_the_on_failure_destination.py", line 333, in run
    assert len(ran_short) == 1, (
  AssertionError: the short twin's handler side channel recorded ['first-0e4e450d31', 'second-d26e9f98ef'], expected exactly one entry — this is the B4 count of how many invocations actually REACHED the code
The check's earlier assertions passed: put_function_concurrency settled at ReservedConcurrentExecutions == 1, get_function_event_invoke_config echoed MaximumEventAgeInSeconds == 60 and MaximumRetryAttempts == 2 with the OnFailure destination stored, and both invokes answered StatusCode 202 — so the configuration surface is correct and the divergence is purely in dispatch. Function names, queue URLs, the role ARN and the two nonces are per-run generated values and are not requirements; the requirements are the counts (one execution, one OnSuccess, one OnFailure) and the age-out discard itself.

## Steps to Reproduce
1. CreateQueue six times (per twin: a handler side-channel 'ran' queue, an OnSuccess queue and an OnFailure queue).
2. CreateRole trusting lambda.amazonaws.com with sqs:SendMessage on those queues.
3. CreateFunction twice (python3.12, Timeout=100) with a handler that writes its event's nonce to RAN_QUEUE_URL and then sleeps OCCUPY_SECONDS=70; wait for State=Active.
4. PutFunctionEventInvokeConfig on the first function with MaximumRetryAttempts=2, MaximumEventAgeInSeconds=60 and DestinationConfig OnSuccess/OnFailure; on the second function the same but MaximumEventAgeInSeconds=21600.
5. PutFunctionConcurrency(ReservedConcurrentExecutions=1) on both; confirm GetFunctionConcurrency answers 1.
6. Invoke each function twice back to back with InvocationType=Event and payloads {"nonce": "first-..."} / {"nonce": "second-..."} (both answered 202).
7. Drain the short twin's side-channel queue.
   -> EXPECT exactly one nonce: only one invocation can hold the single slot, the other is throttled and then discarded at 60 s.
   -> OBSERVE both nonces, i.e. two handler executions; consequently no OnFailure record for an aged-out event is ever written.
8. Or replay the check id in trace_ids.
