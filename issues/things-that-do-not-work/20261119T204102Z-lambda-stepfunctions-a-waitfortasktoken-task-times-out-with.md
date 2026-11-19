Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__stepfunctions.lambda-waitfortasktoken-heartbeat-timeout-history-event
Issue-Key: lambda__stepfunctions.lambda-waitfortasktoken-heartbeat-timeout-history-event|run#13|AssertionError|assert desc_ok["status"] == "SUCCEEDED", f"expected SUCCEEDED, got {desc_ok['status']}: {desc_ok.get('error')}"
Issue-Commit: 0d5fdb6d7a78988c971aec85849026eed9fa5958

# lambda__stepfunctions: a .waitForTaskToken task times out with States.Timeout even though its worker sent SendTaskHeartbeat inside the HeartbeatSeconds window

> _Note: this issue was identified against the code base at commit `0d5fdb6d7a78988c971aec85849026eed9fa5958`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendTaskHeartbeat

## Description
Cause 1 of 1 — SendTaskHeartbeat is accepted but does not extend the task's heartbeat deadline, so the TaskTimedOut/States.Timeout history check can never reach its negative case: its positive control already fails.

AWS: a Task state with Resource 'arn:aws:states:::lambda:invoke.waitForTaskToken' and HeartbeatSeconds declared keeps the task alive for HeartbeatSeconds AFTER the most recent SendTaskHeartbeat on its token. A worker that heartbeats inside the window and later calls SendTaskSuccess therefore drives the execution to SUCCEEDED with no TaskTimedOut event in GetExecutionHistory; only a silent worker fails the task with error States.Timeout, recorded as the GENERIC TaskTimedOut history event (not LambdaFunctionTimedOut, which belongs to the optimized non-callback integration). The Step Functions Smithy model does not model the heartbeat clock or history-event selection, so this is documented behavior only (https://docs.aws.amazon.com/step-functions/latest/dg/state-task.html). SendTaskHeartbeat answers 200 here, so the divergence is in the execution outcome, not a refusal.

Emulator: the positive-control execution — invoked Lambda sleeps 3 s, calls SendTaskHeartbeat (accepted), sleeps 3 s, calls SendTaskSuccess, under HeartbeatSeconds=5 — ends FAILED with error States.Timeout. The deadline remains fixed at the moment the token was issued; the heartbeat is a no-op against it.

Why: any callback task whose work outlasts a single HeartbeatSeconds window is killed no matter how diligently it heartbeats, so HeartbeatSeconds degenerates into a hard task duration cap and the liveness contract it expresses is unavailable. It also makes the heartbeat-timeout history-event shape unobservable, because every heartbeating execution lands in the timeout branch.

Fix: the SendTaskHeartbeat handler must reset the task-token record's heartbeat deadline to now + HeartbeatSeconds, in the same task-token state the heartbeat-expiry sweeper and the TaskTimedOut history-event writer read.

Note: the same root cause is reported under the lambda__sqs__stepfunctions heading in this sweep; the open issues 20261116T170218Z-lambda-stepfunctions-a-waitfortasktoken-heartbeat-lapse-ignores-catch.md (Catch ordering on a heartbeat LAPSE) and 20261113T224249Z-stepfunctions-every-tasktimedout-refusal-...md (TaskTimedOut refusal message text) concern different behaviour and do not carry this check.

Traces: lambda__stepfunctions.lambda-waitfortasktoken-heartbeat-timeout-history-event.

## Evidence
AssertionError: expected SUCCEEDED, got FAILED: States.Timeout

Asserted at the in-file positive control (check line 83): DescribeExecution of the execution whose invoked Lambda calls SendTaskHeartbeat inside HeartbeatSeconds=5 and then SendTaskSuccess returns status 'FAILED' with error 'States.Timeout' instead of 'SUCCEEDED' with no TaskTimedOut history event. The state's Retry clause only retries States.TaskFailed, so this States.Timeout is the genuine heartbeat outcome and not a retried-away IAM propagation error.

## Steps to Reproduce
1. CreateRole for Lambda allowing states:SendTaskHeartbeat and states:SendTaskSuccess; CreateRole for Step Functions allowing lambda:InvokeFunction.
2. CreateFunction (python) whose handler reads event['token'], sleeps 3 s, calls SendTaskHeartbeat(taskToken=token), sleeps 3 s, then SendTaskSuccess(taskToken=token, output='{"done": true}').
3. CreateStateMachine with one Task: Resource 'arn:aws:states:::lambda:invoke.waitForTaskToken', Parameters {FunctionName: <fn>, Payload: {"token.$": "$$.Task.Token"}}, HeartbeatSeconds 5, Retry on States.TaskFailed only, End true.
4. StartExecution with '{}' and poll DescribeExecution to terminal.
5. Expected: SUCCEEDED and GetExecutionHistory with no 'TaskTimedOut' type. Actual: FAILED with error 'States.Timeout'.
