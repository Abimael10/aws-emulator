Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling
Issue-Key: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling|_drain_queue_for#23|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 8e5675a008353ca996c02d5e79f804eabbe67887

# events__lambda__sqs: an Enabled SQS event-source mapping still relays nothing, so `delete-event-source-mapping` stops-polling reaches no verdict (new replay key)

> _Note: this issue was identified against the code base at commit `8e5675a008353ca996c02d5e79f804eabbe67887`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
1 net-new root cause (a NEW replay key for a check an open issue already names; the suite reported this key unowned).

Cause 1 of 1: a Lambda event-source mapping created through the CLI against an SQS source reports State "Enabled" but never polls the queue and never invokes the function, so the check's own positive control (the pre-delete relay) never completes and the negative half — that DeleteEventSourceMapping stops polling — is never graded. The check ends `Error: Check timed out after 60.0s`. Failing check: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling.

AWS: Smithy (service-com-amazonaws-lambda-awsgirapiservice) CreateEventSourceMapping POST /2015-03-31/event-source-mappings -> 202 EventSourceMappingConfiguration (UUID, State, BatchSize, EventSourceArn, FunctionArn); DeleteEventSourceMapping DELETE /2015-03-31/event-source-mappings/{UUID} -> 202; UpdateEventSourceMapping PUT /2015-03-31/event-source-mappings/{UUID} -> 202 with modeled ResourceNotFoundException (httpError 404). Once the mapping reports State "Enabled", Lambda's managed poller calls sqs:ReceiveMessage on the source queue and invokes the function with {"Records": [{"body": <the SendMessage MessageBody>, "messageId": ..., "eventSource": "aws:sqs", "eventSourceARN": <queue ARN>, ...}]}, at most BatchSize records per invoke (BatchSize=1 here is a value from the request; the rule — poll and invoke while Enabled, stop when deleted — is the requirement), and deletes each successfully processed message. After the delete the poller stops: a subsequently sent message is never relayed, stays receivable on the source queue, the sink's ApproximateNumberOfMessages stays "0", and UpdateEventSourceMapping against the deleted UUID is refused 404 ResourceNotFoundException.

Emulator: the create is accepted and the mapping reaches State "Enabled" (the check's `_wait_esm_enabled` passed), but nothing is polled or invoked — the sink queue the relay handler writes to stays empty, the positive control `assert delivered is not None, "event source mapping never relayed the first message (positive control failed)"` never gets a message, and the harness cuts the check off at 60 s. Nothing after it is evaluated: the 404 on UpdateEventSourceMapping, the held absence window calibrated on the positive control's own measured latency, the sink depth of "0", and that the post-delete body remains receivable on the source queue.

Why: a client that wires SQS to Lambda gets a resource that reports Enabled and accepts every knob while no message is ever processed and the queue silently fills. For the delete contract specifically, the absence assertion is calibrated against the positive control's measured latency, so with no positive control a genuinely broken delete path would look exactly like today's behavior.

Fix: the Lambda event-source-mapping poller runtime for SQS sources (poll loop, BatchSize batching, record-envelope construction, invoke, delete-on-success) and its teardown on DeleteEventSourceMapping; a mapping nothing drives must not be reported State "Enabled" by the CreateEventSourceMapping/GetEventSourceMapping handlers.

Related open issue, named for the fixer: 20261030T191609Z-events-lambda-sqs-an-enabled-sqs-event-source.md describes this same cause for this same check, but carries an older replay key (`…|_drain_queue_for#12|Timeout|resp = sqs.receive_message(...)`); the suite did not mark the current key as owned by it, so this is filed as its own work item and the two should be closed together. Sibling open issues for the same never-polling SQS mapping in other suites: 20261029T065950Z-dynamodb-lambda-sqs-an-sqs-event-source-mapping-01e4ea.md, 20261029T072249Z-kms-lambda-sqs-an-enabled-sqs-event-source.md, 20261110T154256Z-lambda-sqs-stepfunctions-an-sqs-event-source-mapping.md.

Traces: events__lambda__sqs.cli-delete-event-source-mapping-stops-polling.

## Evidence
Expected (AWS): after `aws lambda create-event-source-mapping --function-name <fn> --event-source-arn <source queue arn> --batch-size 1` reports State "Enabled", `aws sqs send-message --queue-url <source url> --message-body before-X` causes exactly one invoke of the relay handler, which forwards json.dumps(event) to the sink; the sink yields a message whose JSON body has Records[0].body == "before-X". Then `aws lambda delete-event-source-mapping --uuid <uuid>` makes UpdateEventSourceMapping(UUID=<uuid>, BatchSize=2) answer 404 ResourceNotFoundException with the mapping-gone message, a second send-message never reaches the sink (ApproximateNumberOfMessages == "0"), and that second body remains receivable on the source queue.

Actual (emulator): the sink queue never produces any message. Replay detail for the check is exactly:
  Error: Check timed out after 60.0s
reached inside the positive-control drain (`_drain_queue_for(sink, lambda body: _record_body(body) == marker1)`), before any post-delete assertion runs. Queue URLs, the function name, the mapping UUID and the `before-`/`after-` markers are per-run generated values; the requirement is that an Enabled SQS mapping produces one relayed record per sent message and that deletion stops it.

## Steps to Reproduce
1. CreateQueue twice (a `source` and a `sink`).
2. CreateRole trusting lambda.amazonaws.com with sqs:ReceiveMessage/DeleteMessage/GetQueueAttributes on the source and sqs:SendMessage on the sink.
3. CreateFunction (python3.12) whose handler does sqs.send_message(QueueUrl=os.environ["SINK_QUEUE_URL"], MessageBody=json.dumps(event)); wait for Active.
4. `aws lambda create-event-source-mapping --function-name <fn> --event-source-arn <source arn> --batch-size 1`; poll GetEventSourceMapping until State == "Enabled".
5. `aws sqs send-message --queue-url <source url> --message-body before-X`, then poll ReceiveMessage on the sink.
   -> EXPECT one message whose JSON body has Records[0].body == "before-X".
   -> OBSERVE the sink stays empty until the harness cap.
6. (Never reached) `aws lambda delete-event-source-mapping --uuid <uuid>`; UpdateEventSourceMapping on that UUID must answer 404 ResourceNotFoundException, a second send-message must not reach the sink, and it must stay receivable on the source queue.
7. Or replay the check id in trace_ids.

## Resolution

The cause as stated — an Amazon SQS event source mapping that reports `Enabled` never polls its
source and never invokes its function, so nothing is relayed — does not reproduce on this code
base. The whole sequence of the "Steps to Reproduce" above is now driven inside the OWNING crate,
over the production router with IAM and Amazon SQS mounted beside Lambda on one engine, along the
deploy path a command-line client takes:
`services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/queue_mapping_relay_under_its_written_role.rs`
creates two queues with their DEFAULT attributes, writes the execution role through IAM's own
`CreateRole`/`PutRolePolicy` with only the three queue reads AWS's managed policy for a queue
source grants (on this source alone) plus the one send the handler makes, uploads a `python3.12`
relay as a REAL .zip deployment package created with the DEFAULT `Timeout`, maps it with
`BatchSize` 1 and with a ceiling above what is waiting, and lets the POLL LOOP the create armed
hand the batch over. It asserts the relayed `Records` envelope (`body`, `eventSource`,
`eventSourceARN`, at most `BatchSize` records), and then, through the delete: that the delete
answers `Deleting`, that `UpdateEventSourceMapping` on that UUID answers HTTP 404
`ResourceNotFoundException` with "The resource you requested does not exist.", that nothing further
reaches the sink over a window of many poll intervals, and that the message sent after the delete
stays receivable on the source. Both tests together take under seven seconds.

What this adds to the two sibling resolutions of the same check
(`20261107T135301Z-events-lambda-sqs-deleteeventsourcemapping-never-converges-to-a.md`, which fixed
the one real divergence of the sequence — the refusal class of a call on an already-deleted mapping
— and `20261119T082621Z-events-lambda-sqs-an-sqs-event-source-mapping--r3.md`, which drove the chain
against the BUILT BINARY over TLS with request authentication on) is the configuration neither drove:
the role written with IAM's own operations beside a real .zip upload, in the crate that owns the
poller, with nothing standing in for the production pass. The other queue suites of that crate
either upload a package no engine runs, or make the poll passes themselves, or mount no IAM.

CAVEAT, recorded rather than left implicit: the replay of
`events__lambda__sqs.cli-delete-event-source-mapping-stops-polling` still ends in
`Check timed out after 60.0s`. No call of the sequence is refused or answered wrongly, and the
emulator-side cost of the whole sequence is seconds, so what is left of that 60-second budget is
spent on the check's own CLI-driven sequence (one `aws` process per step, plus its own poll loops)
rather than inside the emulator.
