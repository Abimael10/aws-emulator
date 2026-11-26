Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative
Issue-Key: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative|_drain_json_messages#30|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=max_messages, WaitTimeSeconds=wait_s)
Issue-Commit: 6234e6a4a0cc0c3d0cf2da5570bb140770bc92d9

# lambda__pipes__sqs: an SQS event-source mapping reporting State 'Enabled' never polls or invokes, so the disabled-mapping negative scenario reaches no verdict

> _Note: this issue was identified against the code base at commit `6234e6a4a0cc0c3d0cf2da5570bb140770bc92d9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
One net-new root cause, grounded in the one failing check the suite lists under `### lambda__pipes__sqs` that this run marks neither ALREADY FILED nor OWNED.

Cause 1 of 1: an SQS event-source mapping that Lambda reports in State 'Enabled' never receives from its source queue and never invokes the target function, so every leg of the Enabled/Disabled/re-Enabled delivery contract is unobservable.

AWS: Smithy (services/service-com-amazonaws-lambda-awsgirapiservice) models CreateEventSourceMapping (POST /2015-03-31/event-source-mappings, 202) with EventSourceArn, FunctionName (required), Enabled:boolean and BatchSize (range 1..10000), returning EventSourceMappingConfiguration with UUID, State, StateTransitionReason, LastProcessingResult and Queues; UpdateEventSourceMapping (PUT /2015-03-31/event-source-mappings/{UUID}, 202) carries the same Enabled boolean toggle and returns the same shape. The model names the state and the toggle but not the poller semantics, so for delivery this is documented/recorded behavior only: a mapping whose EventSourceArn is an SQS queue and whose State is 'Enabled' continuously receives from that queue and invokes the function with an event whose Records list holds at most BatchSize entries, each record's `body` carrying the message body verbatim; when the handler returns an empty batchItemFailures list the source message is deleted from the queue. Delivery latency is not a requirement; delivery itself is. With Enabled=false the mapping settles in State 'Disabled' and must NOT invoke — the message stays receivable on the source queue — and with Enabled=true again the very same parked message is delivered.

Emulator: the scenario never gets past its calibration leg. A python3.12 function (Timeout 10) wired by CreateEventSourceMapping(BatchSize=1, Enabled=true) to a source queue (VisibilityTimeout 30) is never invoked: one SendMessage to the source queue produces nothing on the results queue the handler forwards to, so the check is killed at the 60 s harness cap with 'Check timed out after 60.0s' and no verdict. Everything in the control plane matched first — CreateQueue for both queues, the execution role with AWSLambdaBasicExecutionRole + AWSLambdaSQSQueueExecutionRole plus an sqs:SendMessage grant on the results queue, CreateFunction reaching Active, GetEventSourceMapping State == 'Enabled', GetFunctionConfiguration Timeout == 10 and GetQueueAttributes VisibilityTimeout == 30. The control plane therefore advertises a healthy poller the data plane never runs.

Why: an SQS event-source mapping is the standard way to drive a Lambda consumer from a queue. A client that provisions one is told State 'Enabled' while nothing is consumed — queues grow without bound and no handler ever runs. It also makes the overly-permissive guarantee this check grades vacuous: 'a Disabled mapping must not invoke' is indistinguishable from the emulator's permanent non-delivery, because the positive control (delivery while Enabled, and redelivery of the parked message once re-Enabled) never passes.

Fix: the SQS poller of the Lambda event-source-mapping subsystem (the ESM state/worker component behind CreateEventSourceMapping / UpdateEventSourceMapping), not the handlers' request validation: an ESM in State 'Enabled' whose EventSourceArn is an SQS queue ARN must actually ReceiveMessage from that queue, invoke the target function with a Records batch of at most BatchSize, delete the consumed messages on an empty batchItemFailures return, and stop/resume polling when the Enabled toggle flips (State 'Disabled' / 'Enabled').

NOTE for the fixer: open issue 20261124T195017Z-lambda-pipes-sqs-an-enabled-sqs-event-source-ebc281.md describes this same non-delivery for this same check, and 20261121T105340Z-lambda-s3-sqs-an-enabled-sqs-event-source.md describes it for the identically named check under the `lambda__s3__sqs` heading. Neither was reported as OWNING this run's check, so neither carries its replay key; if either is fixed first, re-replay the check named below to close this one.

Traces: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative.

## Evidence
lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative — verdict: mismatch; detail: 'Check timed out after 60.0s'.

The check is cut off inside its first graded leg, _wait_for_effect(..., description="warm-up delivery while enabled", timeout_s=60), which polls the results queue for a record whose `bodies` list contains the warm-up body sent to the source queue.

Expected at that point: with the mapping in State 'Enabled' and BatchSize 1, the message sent to the source queue is delivered as a one-entry Records batch; the handler's SendMessage lands on the results queue as {"bodies": [<warm-up body>], "count": 1, "ts": <float>}; the source message is removed from the source queue.
Actual: the results queue stays empty for the whole budget — the function is never invoked — so the predicate never becomes true and the run produces no status, error code or graded assertion at all.

The three downstream legs are consequently also unverified:
- UpdateEventSourceMapping(UUID, Enabled=false) -> GetEventSourceMapping State == 'Disabled';
- a message sent while Disabled must never be forwarded and must still be returned by ReceiveMessage on the source queue;
- UpdateEventSourceMapping(UUID, Enabled=true) -> State == 'Enabled' and that same parked message delivered.

## Steps to Reproduce
1. CreateQueue <source> with Attributes {VisibilityTimeout: '30'}; CreateQueue <results>.
2. CreateRole trusting lambda.amazonaws.com; attach AWSLambdaBasicExecutionRole and AWSLambdaSQSQueueExecutionRole; PutRolePolicy allowing sqs:SendMessage on <results>'s QueueArn.
3. CreateFunction (Runtime python3.12, Handler index.handler, Timeout 10, Environment RESULTS_QUEUE_URL=<results url>) whose handler forwards {"bodies": [r["body"] for r in event["Records"]], "count": len(Records)} to RESULTS_QUEUE_URL and returns {"batchItemFailures": []}; wait for the function to be Active.
4. CreateEventSourceMapping(FunctionName=<fn arn>, EventSourceArn=<source QueueArn>, BatchSize=1, Enabled=true); poll GetEventSourceMapping until State == 'Enabled'.
5. SendMessage(QueueUrl=<source url>, MessageBody=<warm body>), then poll ReceiveMessage on <results> for up to 60 s.
   Expected: a message whose JSON body has bodies == [<warm body>] and count == 1, and <source> drained.
   Emulator: <results> stays empty; no invocation ever happens; the check times out at 60 s.
6. (Unreached today) UpdateEventSourceMapping(UUID, Enabled=false); poll State == 'Disabled'; SendMessage a second body and observe for 60 s that it is NOT forwarded while ReceiveMessage on <source> still returns it; then UpdateEventSourceMapping(UUID, Enabled=true), poll State == 'Enabled', and that same message must be forwarded.
