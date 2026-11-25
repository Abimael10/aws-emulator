Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling
Issue-Key: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling|_drain_queue_for#23|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: b67daa45c46ff33986818a3ecce820dd32cc81e2

# events__lambda__sqs: an Enabled SQS event-source mapping never polls or invokes, so `delete-event-source-mapping` stops-polling is ungradable (1 cause)

> _Note: this issue was identified against the code base at commit `b67daa45c46ff33986818a3ecce820dd32cc81e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
One root cause. Cause 1 of 1: a Lambda event-source mapping whose EventSourceArn is an SQS queue is accepted and reports State "Enabled", but nothing ever polls the source queue or invokes the function, so the check's own positive control (the pre-delete relay) never completes and the whole check is killed at the harness cap before any post-delete assertion is graded.

AWS: Smithy (services/service-com-amazonaws-lambda-awsgirapiservice) CreateEventSourceMapping POST /2015-03-31/event-source-mappings -> 202 EventSourceMappingConfiguration carrying UUID (@length 36/36), EventSourceArn, FunctionArn, BatchSize (@range 1..10000) and State; DeleteEventSourceMapping DELETE /2015-03-31/event-source-mappings/{UUID} -> 202; UpdateEventSourceMapping PUT /2015-03-31/event-source-mappings/{UUID} -> 202 with modeled ResourceNotFoundException (errorKind client, httpError 404). Once a mapping reports State "Enabled", Lambda's managed poller calls sqs:ReceiveMessage on the source queue and invokes the function with an event of the form {"Records": [{"body": <the SendMessage MessageBody>, "messageId": ..., "eventSource": "aws:sqs", "eventSourceARN": <queue ARN>, ...}]}, at most BatchSize records per invoke, deleting each successfully processed message. After DeleteEventSourceMapping the poller stops: a message sent afterwards is never relayed and stays receivable on the source queue, and GetEventSourceMapping/UpdateEventSourceMapping on that UUID are refused 404 ResourceNotFoundException with the message "The resource you requested does not exist." (fixed text; the only request-derived part of this scenario's assertions is the UUID in the request and the message bodies the client sends). The rule (poll-and-invoke while Enabled, stop on delete, 404 afterwards) is the requirement; BatchSize=1, the queue URLs, the function name, the UUID and the before-/after- marker bodies are per-run values from the request.

Emulator: CreateEventSourceMapping is accepted and the mapping reaches State "Enabled" (the check's `_wait_esm_enabled` poll of GetEventSourceMapping returns), but no invoke ever happens: the sink queue the relay handler writes to stays empty forever. The check's positive-control drain (`_drain_queue_for(sink, Records[0].body == marker1)`) never matches and the run ends with "Error: Check timed out after 60.0s", so none of the post-delete requirements are exercised at all — not the 404 ResourceNotFoundException on UpdateEventSourceMapping, not the held absence window (`_hold_while_confirming_deleted`), not the sink ApproximateNumberOfMessages == "0", and not that the post-delete body remains receivable on the source queue. The mapping surface therefore advertises an enabled, fully configured poller that does no work.

Why: a client that wires SQS to Lambda through an event-source mapping gets a resource reporting Enabled while no message is ever processed — the source queue silently fills and no handler runs. For negative tests the damage is worse: the "deletion stops polling" assertion can never be reached because the positive control it calibrates against never fires, so a real regression in the delete path is indistinguishable from today's behaviour.

Fix: the Lambda event-source-mapping poller runtime for SQS sources (the poll loop, BatchSize batching, aws:sqs record-envelope construction, synchronous invoke, delete-on-success) and its teardown on DeleteEventSourceMapping; until something drives the mapping, the State reported by the CreateEventSourceMapping/GetEventSourceMapping handlers must not be "Enabled".

Related open issue, named for the fixer and NOT owning this replay key (the suite reported this check unowned, so it is filed here as its own work item): 20261030T191609Z-events-lambda-sqs-an-enabled-sqs-event-source.md describes the same never-invoking SQS ESM for this very check; see also 20261027T180000Z-lambda-an-sqs-event-source-mapping-never-consumes-or-delivers.md. They should be closed together with this one.

Traces: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling.

## Evidence
Failing check: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling — verdict mismatch, "Error: Check timed out after 60.0s".

Expected (AWS): after `aws lambda create-event-source-mapping --function-name <fn> --event-source-arn <source queue arn> --batch-size 1` reports State "Enabled", one `aws sqs send-message --message-body <marker1>` to the source queue causes exactly one invoke of the relay handler, which forwards json.dumps(event) to the sink queue; the sink then yields a message whose JSON body has Records[0].body == <marker1>. Then `aws lambda delete-event-source-mapping --uuid <uuid>` makes GetEventSourceMapping and UpdateEventSourceMapping(UUID=<uuid>, BatchSize=2) answer 404 ResourceNotFoundException with Message exactly "The resource you requested does not exist.", a second send-message never reaches the sink (sink ApproximateNumberOfMessages == "0"), and that second body stays receivable on the source queue.

Actual (emulator): the create is accepted, GetEventSourceMapping reports State "Enabled", and the sink queue never produces any message. The positive-control drain loop (ReceiveMessage with WaitTimeSeconds=5 in a loop) never matches and the check is killed at the 60 s budget inside that loop, so every post-delete assertion is unevaluated. Queue URLs, function name, UUID and the `before-`/`after-` marker strings are per-run generated values and are not part of the requirement; the requirement is that an Enabled SQS mapping relays one record per sent message and that deletion stops it and makes the UUID 404.

## Steps to Reproduce
1. CreateQueue twice (a `source` and a `sink`).
2. CreateRole trusting lambda.amazonaws.com, with sqs:ReceiveMessage/DeleteMessage/GetQueueAttributes on the source queue and sqs:SendMessage on the sink queue; attach AWSLambdaBasicExecutionRole.
3. CreateFunction (python3.12) whose handler does sqs.send_message(QueueUrl=os.environ["SINK_QUEUE_URL"], MessageBody=json.dumps(event)); wait for function_active_v2.
4. `aws lambda create-event-source-mapping --function-name <fn> --event-source-arn <source queue arn> --batch-size 1`; poll GetEventSourceMapping until State == "Enabled".
5. `aws sqs send-message --queue-url <source url> --message-body before-X`.
6. Poll ReceiveMessage on the sink queue for up to 60 s. Expected: one message whose JSON body has Records[0].body == "before-X". Observed: the sink stays empty and the check times out.
7. (Never reached in the emulator) `aws lambda delete-event-source-mapping --uuid <uuid>`; UpdateEventSourceMapping on that UUID must answer 404 ResourceNotFoundException "The resource you requested does not exist.", a second send-message must not reach the sink (sink ApproximateNumberOfMessages == "0"), and it must remain receivable on the source queue.
