Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling
Issue-Key: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling|_drain_queue_for#23|Timeout|time.sleep(interval_s)
Issue-Commit: 8465be9c60d4839857b29976b5cf2d08632066fc

# events__lambda__sqs: an SQS event-source mapping that reports State "Enabled" never polls the queue or invokes the function, so `delete-event-source-mapping` stops-polling reaches no verdict (new replay key)

> _Note: this issue was identified against the code base at commit `8465be9c60d4839857b29976b5cf2d08632066fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
1 net-new root cause for this suite (a NEW replay key: the suite reported this check's failure UNOWNED — the open issue 20261118T150346Z-events-lambda-sqs-an-enabled-sqs-event-source-f5f960.md carries the key `events__lambda__sqs.cli-delete-event-source-mapping-stops-polling|_drain_queue_for#23|Timeout|resp = sqs.receive_message(...)`, and the replay no longer matches it, so this is filed as its own work item).

Cause 1 of 1: a Lambda event-source mapping created against an SQS queue source is accepted and reaches State "Enabled", but nothing ever polls the source queue and the function is never invoked. The check's own positive control (the pre-delete relay of one message) therefore never completes, and the negative half it gates — that DeleteEventSourceMapping actually stops the poller — is never graded. Failing check: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling.

AWS: Smithy (services/service-com-amazonaws-lambda-awsgirapiservice) CreateEventSourceMapping POST /2015-03-31/event-source-mappings -> 202 EventSourceMappingConfiguration {UUID (@length 36), EventSourceArn, FunctionArn, BatchSize (@range 1..10000), State, LastProcessingResult, StateTransitionReason}; GetEventSourceMapping GET /2015-03-31/event-source-mappings/{UUID} -> 200 (errors ResourceNotFoundException httpError 404); DeleteEventSourceMapping DELETE /2015-03-31/event-source-mappings/{UUID} -> 202, @idempotent; UpdateEventSourceMapping PUT /2015-03-31/event-source-mappings/{UUID} -> 202 with modeled ResourceNotFoundException (errorKind client, httpError 404). The polling semantics themselves are recorded behavior only (the model states the resource, not the poller): once the mapping reports State "Enabled", Lambda's managed poller calls sqs:ReceiveMessage on the source queue under the function's execution role and invokes the function with an event of the form {"Records": [{"eventSource": "aws:sqs", "eventSourceARN": <the source queue's ARN>, "messageId": <the message's id>, "body": <the MessageBody that was sent>, ...}]}, at most BatchSize records per invocation, deleting each successfully-processed message from the source queue. After DeleteEventSourceMapping the poller stops: a message sent afterwards is never relayed, stays receivable on the source queue, the downstream sink's ApproximateNumberOfMessages stays "0", and UpdateEventSourceMapping(UUID=<the deleted uuid>, BatchSize=<n>) is refused HTTP 404 ResourceNotFoundException with message "The resource you requested does not exist." (that text is the whole fixed message for this error class — it interpolates nothing from the request).

Emulator: CreateEventSourceMapping is accepted and GetEventSourceMapping reports State "Enabled" (the check's `_wait_esm_enabled` poll loop passes), but no message is ever polled and the relay function is never invoked: the sink queue the handler writes to stays empty, the positive-control assertion `assert delivered is not None, "event source mapping never relayed the first message (positive control failed)"` never gets its record, and the harness cuts the check off — the replay detail is exactly `Error: Check timed out after 60.0s`. Everything downstream of the positive control is consequently unevaluated: the 404 ResourceNotFoundException (and its message) on UpdateEventSourceMapping against the deleted UUID, the held absence window, the sink depth of "0", and that the post-delete body stays receivable on the source queue.

Why: a client that wires an SQS queue to a Lambda function gets a resource that reports itself Enabled and accepts every knob while no message is ever processed — the queue silently fills and no invocation, batching, or delete-on-success behaviour occurs. For the delete contract specifically, the absence window is calibrated from the positive control's own measured delivery latency, so with no positive control a genuinely-broken delete path is indistinguishable from today's behaviour: the whole stops-polling guarantee is unverifiable.

Fix: the Lambda event-source-mapping poller runtime for SQS sources — the poll loop, BatchSize batching, the `Records` envelope construction, the invoke, and delete-on-success — together with its teardown on DeleteEventSourceMapping; and the CreateEventSourceMapping/GetEventSourceMapping handlers must not report State "Enabled" for a mapping nothing drives.

Related open issues, named for the fixer (neither carries this replay key, so neither covers this failure): 20261118T150346Z-events-lambda-sqs-an-enabled-sqs-event-source-f5f960.md and 20261030T191609Z-events-lambda-sqs-an-enabled-sqs-event-source.md describe this same cause for this same check under older replay keys and should be closed together with this one. Sibling open issues for the same never-polling SQS mapping in other suites: 20261027T180000Z-lambda-an-sqs-event-source-mapping-never-consumes-or-delivers.md, 20261029T065950Z-dynamodb-lambda-sqs-an-sqs-event-source-mapping-01e4ea.md, 20261110T154256Z-lambda-sqs-stepfunctions-an-sqs-event-source-mapping.md, 20261118T062834Z-lambda-pipes-sqs-an-enabled-sqs-event-source.md.

Traces: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling.

## Evidence
Expected (AWS): after `aws lambda create-event-source-mapping --function-name <fn> --event-source-arn <source queue arn> --batch-size 1` and GetEventSourceMapping reporting State "Enabled", `aws sqs send-message --queue-url <source url> --message-body <marker>` causes exactly one invocation of the relay handler, which forwards json.dumps(event) to the sink queue; the sink then yields a message whose JSON body has Records[0].body == <marker>. Then `aws lambda delete-event-source-mapping --uuid <uuid>` makes GetEventSourceMapping(<uuid>) and UpdateEventSourceMapping(UUID=<uuid>, BatchSize=2) answer HTTP 404 ResourceNotFoundException with the message "The resource you requested does not exist." (fixed text, no interpolated values), a second send-message never reaches the sink (ApproximateNumberOfMessages == "0"), and that second body remains receivable on the source queue.

Actual (emulator): the sink queue never produces any message. The replay detail for the check is exactly:
  Error: Check timed out after 60.0s
The timeout is reached inside the positive-control drain (`_drain_queue_for(ctx, sink["url"], lambda body: _record_body(body) == marker1)`), before any post-delete assertion executes — so the emulator's 404 status, error code and message for the deleted UUID are not even observed.

Queue URLs, the function name, the mapping UUID and the `before-`/`after-` marker strings are per-run generated values and are not part of the requirement; the requirement is that an Enabled SQS mapping produces one relayed invocation per sent message (at most BatchSize records each) and that DeleteEventSourceMapping stops it.

## Steps to Reproduce
1. CreateQueue twice: a `source` queue and a `sink` queue; read QueueArn for each via GetQueueAttributes.
2. CreateRole trusting lambda.amazonaws.com, with an inline policy allowing sqs:ReceiveMessage/sqs:DeleteMessage/sqs:GetQueueAttributes on the source queue ARN and sqs:SendMessage on the sink queue ARN; attach AWSLambdaBasicExecutionRole.
3. CreateFunction (Runtime python3.12, Handler index.handler) whose handler does sqs.send_message(QueueUrl=os.environ["SINK_QUEUE_URL"], MessageBody=json.dumps(event)); wait for the function to become Active.
4. `aws lambda create-event-source-mapping --function-name <fn> --event-source-arn <source arn> --batch-size 1`; poll GetEventSourceMapping until State == "Enabled".
5. `aws sqs send-message --queue-url <source url> --message-body <marker>`, then poll ReceiveMessage on the sink queue.
   -> EXPECT one message whose JSON body has Records[0].body == <marker> (eventSource "aws:sqs", eventSourceARN == the source queue ARN).
   -> OBSERVE the sink stays empty and the check is cut at the 60 s harness cap.
6. (Never reached today) `aws lambda delete-event-source-mapping --uuid <uuid>`; GetEventSourceMapping and UpdateEventSourceMapping(UUID=<uuid>, BatchSize=2) must answer 404 ResourceNotFoundException "The resource you requested does not exist."; a second send-message must not reach the sink (sink ApproximateNumberOfMessages == "0") and must stay receivable on the source queue.
7. Or replay the check id in trace_ids.
