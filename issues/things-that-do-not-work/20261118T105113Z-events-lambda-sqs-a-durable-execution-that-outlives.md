Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs.xsvc-lambda-1-durable-execution-timeout-emits-timed-out-status-change-to-sqs
Issue-Key: events__lambda__sqs.xsvc-lambda-1-durable-execution-timeout-emits-timed-out-status-change-to-sqs|run#16|AssertionError|assert statuses == ["RUNNING", "TIMED_OUT"], (
Issue-Commit: 814109981af2f415bf041bb2099ef5ad4140d55d

# events__lambda__sqs: a durable execution that outlives its ExecutionTimeout ends FAILED instead of TIMED_OUT, and the status-change event reports FAILED

> _Note: this issue was identified against the code base at commit `814109981af2f415bf041bb2099ef5ad4140d55d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One root cause.

=== Cause 1 of 1: the durable-execution timeout path classifies the execution as FAILED, not TIMED_OUT ===
AWS: Smithy (com-amazonaws-lambda-awsgirapiservice) models DurableConfig.ExecutionTimeout as 'The maximum time (in seconds) that a durable execution can run before timing out. This timeout applies to the entire durable execution, not individual function invocations.' (@range min 1, max 31622400), and GetDurableExecution's required Status member targets the ExecutionStatus enum whose values are exactly RUNNING | SUCCEEDED | FAILED | TIMED_OUT | STOPPED. TIMED_OUT is the distinct terminal state for an execution the service itself ended because it ran past ExecutionTimeout; FAILED is reserved for an execution whose handler raised. The matching aws.lambda 'Durable Execution Status Change' event (recorded behavior only — the envelope is not in the model) therefore carries detail.status 'RUNNING' and then 'TIMED_OUT', with the TIMED_OUT event adding an endTimestamp at least ExecutionTimeout after the shared startTimestamp, and GetDurableExecution reporting the same Status and EndTimestamp.
Emulator: for a function created with DurableConfig.ExecutionTimeout = 15 whose handler waits 120s, the status-change events delivered to the SQS target are RUNNING followed by FAILED — the set of delivered detail.status values is {FAILED, RUNNING} where AWS delivers {RUNNING, TIMED_OUT}. The surrounding plumbing works: the warm-up execution in the same check reached SUCCEEDED and its event was routed, so only the timeout classification diverges.
Why: a consumer cannot tell a timed-out workflow from a crashed one. A rule or client branching on detail.status == 'TIMED_OUT' (retry with a longer budget, raise a timeout alarm) never fires, while every FAILED-handler path (alert on a bug, inspect the Error object) fires spuriously; GetDurableExecution reports the same wrong terminal Status, so the error is not recoverable downstream either.
Fix: the lambda durable-execution timeout/terminal-status path — the component that ends an execution when it exceeds DurableConfig.ExecutionTimeout must set the execution's Status to TIMED_OUT (and emit the status-change event with detail.status 'TIMED_OUT' plus an endTimestamp no earlier than startTimestamp + ExecutionTimeout) instead of reusing the handler-failure FAILED path.
Related filing: open issue 20261117T230004Z-events-lambda-sqs-the-durable-execution-status-change.md concerns the same event family but a different cause (the resources array), and does not carry this check's replay key; this is a net-new cause.
Traces: events__lambda__sqs.xsvc-lambda-1-durable-execution-timeout-emits-timed-out-status-change-to-sqs.

## Evidence
Failing check events__lambda__sqs.xsvc-lambda-1-durable-execution-timeout-emits-timed-out-status-change-to-sqs (mismatch):

AssertionError: a 15s execution waiting 120s must deliver RUNNING then TIMED_OUT, got ['FAILED', 'RUNNING']

Expected (AWS): the durable-execution status-change events for the execution carry detail.status 'RUNNING' and then 'TIMED_OUT'; the TIMED_OUT detail carries an endTimestamp with (endTimestamp - startTimestamp) >= ExecutionTimeout, and GetDurableExecution answers Status 'TIMED_OUT' with the same EndTimestamp.
Actual (emulator): the delivered statuses are RUNNING and FAILED (sorted set ['FAILED', 'RUNNING']); no TIMED_OUT event is ever produced. The 15s timeout and 120s wait are the check's inputs, not a requirement — the requirement is that any execution exceeding its configured ExecutionTimeout terminates as TIMED_OUT.
Asserts that passed before this one: CreateFunction returned the predicted function ARN, and the warm-up execution's terminal SUCCEEDED status-change event was delivered to the SQS target through the same rule, so the rule, the target and the event emitter are live.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and point botocore at it.
2. Create an SQS queue and an EventBridge rule on the default bus matching the aws.lambda 'Durable Execution Status Change' events of the function under test, with that queue as target.
3. CreateFunction with durable execution enabled and DurableConfig.ExecutionTimeout = 15, with a handler that calls context.wait(Duration.from_seconds(event['wait_seconds'])).
4. Warm up: invoke once with wait_seconds = 1 and observe the SUCCEEDED status-change event (positive control).
5. lambda.invoke(FunctionName=..., Qualifier='$LATEST', InvocationType='Event', DurableExecutionName=<name>, Payload={"wait_seconds": 120}).
6. Drain the queue until a terminal status for that durableExecutionArn arrives. Observe detail.status 'FAILED' where AWS sends 'TIMED_OUT'; GetDurableExecution likewise reports Status 'FAILED'.
7. Equivalently: replay check events__lambda__sqs.xsvc-lambda-1-durable-execution-timeout-emits-timed-out-status-change-to-sqs.
