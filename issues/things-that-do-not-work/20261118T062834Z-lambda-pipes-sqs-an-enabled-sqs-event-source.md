Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative
Issue-Key: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative|_drain_json_messages#30|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=max_messages, WaitTimeSeconds=wait_s)
Issue-Commit: c2a45cb39008eec8053ec65a5a3a807cace171f5

# lambda__pipes__sqs: an Enabled SQS event source mapping never invokes its function, so the disabled-then-re-enabled mapping contract reaches no verdict

> _Note: this issue was identified against the code base at commit `c2a45cb39008eec8053ec65a5a3a807cace171f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping / UpdateEventSourceMapping (SQS poller)

## Description
One net-new root cause under the lambda__pipes__sqs heading. The same check name under the lambda__s3__sqs heading is already filed as 20261110T153718Z-lambda-s3-sqs-an-sqs-event-source-mapping-a933d9.md; that issue carries only the lambda__s3__sqs replay key, so this failing check is covered by no open issue for this heading (the heading's other open issues — e.g. 20261104T213346Z-lambda-pipes-sqs-5-net-new-causes-an.md, 20261110T174759Z-lambda-pipes-sqs-a-cloudformation-deployed-aws-pipes.md, 20261109T174004Z-lambda-pipes-sqs-2-net-new-causes-a-18a916.md — concern the Pipes resource, not the Lambda SQS poller).

Cause 1 of 1: an SQS event source mapping that reports State "Enabled" never invokes its target function, so the check cannot establish the single delivery it needs as its baseline and never reaches the Enabled=false / re-enable assertions.
AWS: Smithy — CreateEventSourceMapping(FunctionName, EventSourceArn, BatchSize, Enabled) and UpdateEventSourceMapping(UUID, Enabled) are modelled; GetEventSourceMapping reports State. Delivery timing is recorded behavior only: a mapping whose State is "Enabled" receives from the queue and invokes the function once per batch (BatchSize=1 here), and a record the handler returns successfully (empty batchItemFailures) is deleted from the queue. UpdateEventSourceMapping(Enabled=false) moves State to "Disabled" and stops delivery: a message sent while disabled stays on the queue, visible to a direct ReceiveMessage, and the function is not invoked; UpdateEventSourceMapping(Enabled=true) resumes polling and delivers exactly that queued message.
Emulator: the check is killed with "Error: Check timed out after 60.0s" and no assertion reached. Its first leg after the mapping settles to "Enabled" is a single SendMessage plus a 60 s wait for the function's forwarded result on a second queue, so the mapping's enabled-state delivery is what fails to happen inside the runner's window; nothing downstream (the Disabled withholding, the message staying on the source queue, the re-enabled redelivery) is ever exercised.
Why: every SQS-triggered Lambda workload is dead — messages accumulate on the queue while the mapping reports Enabled, with no error anywhere. And because nothing is delivered in the enabled state either, the negative half of the contract is vacuous: a mapping that ignores Enabled=false cannot be distinguished from one that honours it.
Fix: the Lambda event-source-mapping poller for SQS sources — an Enabled mapping must receive from the queue and invoke the function honouring BatchSize, delete successfully processed records, suspend receiving (without losing queued messages) while Enabled=false, and resume on re-enable. Delivery must also be prompt enough to be observable, since the mapping's measured latency is the baseline the contract is graded against.
Traces: lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative.

## Evidence
Check lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative, verdict [mismatch]:
  Error: Check timed out after 60.0s
with no assertion reached.

Setup before the first wait: a source queue with VisibilityTimeout=30, a results queue, an execution role allowing sqs:SendMessage on the results queue, a function with Timeout=10 that forwards each received record's body to the results queue and returns {"batchItemFailures": []}, and CreateEventSourceMapping(FunctionName=<fn>, EventSourceArn=<source arn>, BatchSize=1, Enabled=True) polled until GetEventSourceMapping reports State "Enabled".
Expected: SendMessage on the source queue is followed, within the mapping's delivery latency, by a results-queue message whose "bodies" contains the sent body; then UpdateEventSourceMapping(Enabled=False) -> State "Disabled", a message sent afterwards is NOT forwarded and is still returned by a direct ReceiveMessage on the source queue; then UpdateEventSourceMapping(Enabled=True) -> State "Enabled" and that withheld message is forwarded.
Actual: the first forwarded message never appears, so the check is cut off with nothing compared. Note the check's later legs use a 45 s drain plus a >=60 s observation window, so a fix must make the enabled-state delivery observable for the check to reach a verdict at all. Queue URLs, function names and mapping UUIDs are per-run values.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. Create a source SQS queue (VisibilityTimeout=30) and a results queue; create an execution role allowing sqs:SendMessage on the results queue.
3. Create a Lambda function (Timeout=10) that sends each received record body to the results queue and returns {"batchItemFailures": []}.
4. CreateEventSourceMapping(FunctionName=<fn>, EventSourceArn=<source queue arn>, BatchSize=1, Enabled=True) and poll GetEventSourceMapping until State == "Enabled".
5. SendMessage to the source queue and poll the results queue: AWS forwards the body within seconds; the emulator forwards nothing and the check is killed at its cap.
6. Then UpdateEventSourceMapping(UUID, Enabled=False) -> State "Disabled", SendMessage again, confirm no invocation and that ReceiveMessage on the source queue still returns the message; UpdateEventSourceMapping(UUID, Enabled=True) and confirm that message is delivered.
7. Re-replay lambda__pipes__sqs.sqs-esm-disabled-mapping-no-invocation-negative to confirm the fix.
