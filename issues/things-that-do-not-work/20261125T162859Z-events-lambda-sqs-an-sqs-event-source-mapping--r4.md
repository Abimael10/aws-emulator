Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling
Issue-Key: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling|_drain_queue_for#23|Timeout|time.sleep(interval_s)
Issue-Commit: fbb1a06f1c20de0c01a698c058987963365385aa

# events__lambda__sqs: an SQS event-source mapping reporting State "Enabled" never polls or invokes, so the `delete-event-source-mapping` stops-polling contract is ungradable

> _Note: this issue was identified against the code base at commit `fbb1a06f1c20de0c01a698c058987963365385aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
One root cause.

Cause 1 of 1: a Lambda event-source mapping whose EventSourceArn is an SQS queue is accepted and reports State "Enabled", but nothing ever polls the source queue or invokes the function, so the check's own positive control (the pre-delete relay) never completes and the check is killed at the harness cap before any post-delete requirement is graded. Failing check: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling.

AWS: Smithy (services/service-com-amazonaws-lambda-awsgirapiservice) CreateEventSourceMapping -> EventSourceMappingConfiguration carrying UUID, EventSourceArn, FunctionArn, BatchSize and State; DeleteEventSourceMapping -> 202; UpdateEventSourceMapping -> 202 with modeled ResourceNotFoundException (client error, httpError 404). Once a mapping reports State "Enabled", Lambda's managed poller receives from the source queue and invokes the function with an event of the form {"Records": [{"body": <the SendMessage MessageBody>, "eventSource": "aws:sqs", "eventSourceARN": <queue ARN>, ...}]}, at most BatchSize records per invoke, deleting each successfully processed message. After DeleteEventSourceMapping the poller stops: a message sent afterwards is never relayed and stays receivable on the source queue, and GetEventSourceMapping/UpdateEventSourceMapping on that UUID are refused 404 ResourceNotFoundException with the message "The resource you requested does not exist." — that sentence is fixed text, carrying no request-derived part. The rule (poll-and-invoke while Enabled, stop on delete, 404 on the dead UUID) is the requirement; BatchSize=1, the queue URLs, function name, UUID and the before-/after- marker bodies are per-run values from the request.

Emulator: CreateEventSourceMapping is accepted and GetEventSourceMapping reports State "Enabled" (the check's wait-for-Enabled poll returns), but no invoke ever happens — the sink queue the relay handler writes to stays empty. The positive-control drain (ReceiveMessage in a loop for Records[0].body == marker1) never matches and the run ends "Error: Check timed out after 60.0s", so none of the post-delete requirements are exercised: not the 404 ResourceNotFoundException on UpdateEventSourceMapping, not the held-absence window, not the sink ApproximateNumberOfMessages == "0", and not that the post-delete body stays receivable on the source queue. The mapping surface therefore advertises an enabled, fully configured poller that does no work.

Why: a client wiring SQS to Lambda through an event-source mapping gets a resource reporting Enabled while no message is ever processed — the source queue silently fills and no handler runs. For negative tests the damage is worse: "deletion stops polling" can never be reached because the positive control it calibrates against never fires, so a regression in the delete path is indistinguishable from today's behaviour.

Fix: the Lambda event-source-mapping poller runtime for SQS sources (poll loop, BatchSize batching, aws:sqs record-envelope construction, synchronous invoke, delete-on-success) and its teardown on DeleteEventSourceMapping; until something drives the mapping, the State reported by the create/get handlers must not be "Enabled".

Related already-filed issues, named for the fixer and NOT marked by the suite as owning this check: open 20261125T101001Z-events-lambda-sqs-an-enabled-sqs-event-source-13dbc6.md and 20261030T191609Z-events-lambda-sqs-an-enabled-sqs-event-source.md describe the same never-invoking SQS ESM for this very check, and 20261027T180000Z-lambda-an-sqs-event-source-mapping-never-consumes-or-delivers.md the same cause service-wide; resolved issues 20261119T082621Z-events-lambda-sqs-an-sqs-event-source-mapping--r3.md and 20261118T150346Z-events-lambda-sqs-an-enabled-sqs-event-source-f5f960.md claimed to fix it and the check still makes no progress. They should be closed together with this one.

Traces: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling.

## Evidence
Failing check: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling — verdict mismatch, "Error: Check timed out after 60.0s" (killed inside the positive-control drain loop `_drain_queue_for`, which polls ReceiveMessage on the sink queue with WaitTimeSeconds=5).

Expected (AWS): after `aws lambda create-event-source-mapping --function-name <fn> --event-source-arn <source queue arn> --batch-size 1` reports State "Enabled", one `aws sqs send-message --message-body <marker1>` causes exactly one invoke of the relay handler, which forwards json.dumps(event) to the sink queue; the sink then yields a message whose JSON body has Records[0].body == <marker1>. Then `aws lambda delete-event-source-mapping --uuid <uuid>` makes GetEventSourceMapping and UpdateEventSourceMapping(UUID=<uuid>, BatchSize=2) answer 404 ResourceNotFoundException with Message exactly "The resource you requested does not exist.", a second send-message never reaches the sink (sink ApproximateNumberOfMessages == "0"), and that second body stays receivable on the source queue.

Actual (emulator): the create is accepted, GetEventSourceMapping reports State "Enabled", and the sink queue never produces any message; the check is killed at its 60 s budget inside the drain loop, leaving every post-delete assertion unevaluated.

Queue URLs, function name, UUID and the `before-`/`after-` marker strings are per-run generated values and not part of the requirement.

## Steps to Reproduce
1. CreateQueue twice (a `source` and a `sink`).
2. CreateRole trusting lambda.amazonaws.com with sqs:ReceiveMessage/DeleteMessage/GetQueueAttributes on the source queue and sqs:SendMessage on the sink queue; attach AWSLambdaBasicExecutionRole.
3. CreateFunction (python3.12) whose handler calls sqs.send_message(QueueUrl=os.environ["SINK_QUEUE_URL"], MessageBody=json.dumps(event)); wait for the function to be Active.
4. `aws lambda create-event-source-mapping --function-name <fn> --event-source-arn <source queue arn> --batch-size 1`; poll GetEventSourceMapping until State == "Enabled".
5. `aws sqs send-message --queue-url <source url> --message-body before-X`.
6. Poll ReceiveMessage on the sink queue. Expected: one message whose JSON body has Records[0].body == "before-X". Observed: the sink stays empty and the check times out.
7. (Never reached today) `aws lambda delete-event-source-mapping --uuid <uuid>`; UpdateEventSourceMapping on that UUID must answer 404 ResourceNotFoundException "The resource you requested does not exist.", a second send-message must not reach the sink (ApproximateNumberOfMessages == "0"), and it must remain receivable on the source queue.
