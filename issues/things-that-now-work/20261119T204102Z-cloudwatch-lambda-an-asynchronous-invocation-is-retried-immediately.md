Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__lambda.obs-lambda-1-deleted-on-failure-queue-meters-delivery-failure-and-event-age
Issue-Key: cloudwatch__lambda.obs-lambda-1-deleted-on-failure-queue-meters-delivery-failure-and-event-age|_poll_until#40|AssertionError|raise AssertionError(f"{description} did not happen within {timeout_s}s; last read {last!r}")
Issue-Commit: 0d5fdb6d7a78988c971aec85849026eed9fa5958

# cloudwatch__lambda: an asynchronous invocation is retried immediately, so AsyncEventAge never reflects Lambda's retry backoff (reported ~4.5 s where AWS reports >= 60 s)

> _Note: this issue was identified against the code base at commit `0d5fdb6d7a78988c971aec85849026eed9fa5958`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
Cause 1 of 1 — the async-invoke retry has no backoff, so the AsyncEventAge metric is far below the value AWS's retry delay guarantees.

AWS: an asynchronous Invoke (Smithy: Invoke.InvocationType = 'Event', httpHeader X-Amz-Invocation-Type; the operation answers 202 and queues the event) whose handler raises is retried after a documented wait of roughly one minute for the first retry, with jitter. The Lambda metric AsyncEventAge (Maximum, over the FunctionName dimension) measures the time an event spent queued between Lambda receiving it and the invocation, so with MaximumRetryAttempts=1 the Maximum over the two attempts necessarily exceeds the first-retry delay — tens of seconds, not single-digit seconds. The Smithy model carries no metric semantics, so the AsyncEventAge/backoff requirement is documented behavior only (https://docs.aws.amazon.com/lambda/latest/dg/monitoring-metrics-types.html).

Emulator: the Invocations (2), Errors (2), AsyncEventsReceived (1) and DestinationDeliveryFailures (1) counts for the subject are all correct, and the live-queue twin's RetriesExhausted record is delivered — but AsyncEventAge Maximum is reported as ~4.48e3 ms for the subject and ~4.48e3 ms for the twin, i.e. both retries happened ~4.5 seconds after the original invoke. The emulator re-invokes a failed asynchronous event almost immediately instead of waiting out the retry interval, and AsyncEventAge faithfully reports that absent delay.

Why: a client that reasons about asynchronous retry timing — an alarm on AsyncEventAge, a MaximumEventAgeInSeconds configuration that is supposed to discard an event before its retry, or any test that depends on the retry window — sees behaviour that cannot occur on AWS. MaximumEventAgeInSeconds in particular becomes unenforceable, because no event ever ages far enough to be discarded.

Fix: the asynchronous-invocation queue/retry scheduler in the Lambda async invoke path (apply the per-attempt retry delay before re-invoking) and the AsyncEventAge metric emission that derives from it.

Traces: cloudwatch__lambda.obs-lambda-1-deleted-on-failure-queue-meters-delivery-failure-and-event-age.

## Evidence
AssertionError: the subject's DestinationDeliveryFailures and both retry ages did not happen within 300.0s; last read {'bddf': [1.0], 'binv': [2.0], 'berr': [2.0], 'brcv': [1.0], 'bage': [4480.9491530000005], 'gddf': [], 'ginv': [2.0], 'gage': [4482.147967000001]}

The only unmet conditions are the two AsyncEventAge Maximum reads ('bage' for the function with the deleted OnFailure queue, 'gage' for the live-queue twin): both ~4481 ms, against a floor of 30000 ms chosen as half of AWS's ~60 s first-retry delay. Every count in the same read (Invocations 2, Errors 2, AsyncEventsReceived 1, DestinationDeliveryFailures 1 on the subject and 0 on the twin) matched.

## Steps to Reproduce
1. Create two SQS queues (one to keep, one to delete) and an execution role allowing sqs:SendMessage on both.
2. Create two python functions whose handler raises, and PutFunctionEventInvokeConfig on each with MaximumRetryAttempts=1 and OnFailure pointing at its own queue.
3. DeleteQueue on the second function's destination queue and wait for GetQueueAttributes to report AWS.SimpleQueueService.NonExistentQueue.
4. Invoke both functions with InvocationType='Event' (both answer 202).
5. GetMetricData for AWS/Lambda AsyncEventAge, statistic Maximum, dimension FunctionName=<each function>. The Maximum is a few thousand milliseconds, showing the retry fired seconds after the first attempt instead of after the ~60 s retry delay.
