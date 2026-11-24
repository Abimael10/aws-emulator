Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative
Issue-Key: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative|run#30|Timeout|time.sleep(2)
Issue-Commit: d50969a6e6efc58b2521aa7f9ba160b669c146a6

# lambda__pipes__sqs: an ENABLED SQS event-source mapping never invokes its function, so the disabled-mapping negative scenario never reaches its first graded assertion

> _Note: this issue was identified against the code base at commit `d50969a6e6efc58b2521aa7f9ba160b669c146a6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
One net-new root cause, grounded in one failing check the suite lists under `lambda__pipes__sqs`. The check is not marked ALREADY FILED or OWNED in this run.

Cause 1 of 1: an SQS event-source mapping that Lambda reports as State 'Enabled' never polls its source queue, so a message sent to that queue is never delivered to the function.
AWS: CreateEventSourceMapping against an SQS queue ARN produces a mapping that settles in State 'Enabled' (Smithy: lambda#EventSourceMappingConfiguration$State, the shape UpdateEventSourceMapping/CreateEventSourceMapping return, with UpdateEventSourceMapping modelling Enabled as a boolean toggle), and an Enabled mapping continuously receives from the queue and invokes the function with a Records batch of at most BatchSize entries, each record's `body` carrying the message body verbatim. The invocation is observable: the handler's own SendMessage to a second queue arrives, and the source message is deleted from the queue when the handler returns an empty batchItemFailures list. Delivery latency is not a requirement, but delivery itself is: the queue drains and the function runs. The subsequent legs of this scenario (UpdateEventSourceMapping Enabled=false → State 'Disabled' → a message sent then must stay on the queue and never invoke → Enabled=true → that same message is delivered) all depend on the Enabled mapping working first.
Emulator: the warm-up leg never completes. With a python3.12 function (Timeout 10) wired by CreateEventSourceMapping(BatchSize 1, Enabled true) to a source queue (VisibilityTimeout 30), one SendMessage to the source queue produces no invocation — nothing ever arrives on the results queue — so the check polls its calibration predicate until the harness cuts it off at 60 s with no verdict on any of the disabled/re-enabled legs. The mapping's own State reads 'Enabled' (the pre-conditions, the function's configured Timeout and the queue's configured VisibilityTimeout all matched), so the control plane reports a healthy poller that the data plane never runs.
Why: an SQS event-source mapping is the standard way to drive a Lambda consumer from a queue. A client that provisions one gets a mapping reported Enabled that silently consumes nothing — queues grow without bound, no handler ever runs, and the whole overly-permissive class this scenario grades (that a Disabled mapping must NOT invoke) cannot be distinguished from the emulator's permanent non-delivery: the negative result is vacuous because the positive control never passes.
NOTE for the fixer: open issue 20261121T105340Z-lambda-s3-sqs-an-enabled-sqs-event-source.md describes the same non-delivery for the identically named check under the `lambda__s3__sqs` heading. It does not carry this check's replay key, so this check will not be re-replayed by it; if that issue is fixed first, re-replay this check to close this one.
Fix: the SQS event-source-mapping poller in the Lambda event-source-mapping subsystem — an ESM in State 'Enabled' whose EventSourceArn is an SQS queue must actually receive from that queue and invoke the target function with the Records batch, and honour the Enabled toggle (and BatchSize) rather than only recording the state.
Traces: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative.

## Evidence
lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative:
  Error: Check timed out after 60.0s

The check is cut off inside its first graded leg, `_wait_for_effect(..., description="warm-up delivery while enabled", timeout_s=60)`, which polls the results queue for a record whose `bodies` list contains the warm-up message body sent to the source queue.
Expected at that point: with the mapping in State 'Enabled', the message sent to the source queue is delivered as a Records batch (BatchSize 1), the handler's SendMessage lands on the results queue as {"bodies": [<warm-up body>], "count": 1, "ts": <float>}, and the source message is removed from the queue.
Actual: nothing ever arrives on the results queue, so the predicate never becomes true and the check reaches no verdict.
Everything before that matched: CreateQueue (source VisibilityTimeout 30, results), the IAM role with AWSLambdaBasicExecutionRole + AWSLambdaSQSQueueExecutionRole and an sqs:SendMessage grant on the results queue, CreateFunction (python3.12, Timeout 10) reaching Active, CreateEventSourceMapping settling in State 'Enabled', GetFunctionConfiguration Timeout == 10 and GetQueueAttributes VisibilityTimeout == 30.
The three unreached legs are therefore also unverified: UpdateEventSourceMapping Enabled=false → State 'Disabled'; a message sent while Disabled must never invoke and must stay receivable on the source queue; UpdateEventSourceMapping Enabled=true → State 'Enabled' and that same message delivered.

## Steps to Reproduce
1. CreateQueue <source> with Attributes {VisibilityTimeout: '30'} and CreateQueue <results>.
2. CreateRole for lambda.amazonaws.com, attach AWSLambdaBasicExecutionRole and AWSLambdaSQSQueueExecutionRole, and put an inline policy allowing sqs:SendMessage on <results>'s ARN.
3. CreateFunction (Runtime python3.12, Handler index.handler, Timeout 10, Environment RESULTS_QUEUE_URL=<results url>) whose handler forwards {"bodies": [r["body"] for r in event["Records"]], "count": len(Records)} to RESULTS_QUEUE_URL and returns {"batchItemFailures": []}; wait for the function to become Active.
4. CreateEventSourceMapping(FunctionName=<fn arn>, EventSourceArn=<source arn>, BatchSize=1, Enabled=true) and poll GetEventSourceMapping until State == 'Enabled'.
5. SendMessage(QueueUrl=<source url>, MessageBody=<warm body>) and poll ReceiveMessage on <results> for up to 60 s.
   Expected: a message whose JSON body has bodies == [<warm body>] and count == 1, and <source> drained.
   Emulator: <results> stays empty indefinitely; the function is never invoked.
6. (Unreached today) UpdateEventSourceMapping(UUID, Enabled=false), poll State == 'Disabled', SendMessage a second body, observe for 60 s that it is NOT forwarded and that ReceiveMessage on <source> still returns it; then UpdateEventSourceMapping(UUID, Enabled=true), poll State == 'Enabled', and that same message must be forwarded.
