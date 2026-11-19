Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs__stepfunctions.sfn-task-token-heartbeat-timeout-fails-execution
Issue-Key: lambda__sqs__stepfunctions.sfn-task-token-heartbeat-timeout-fails-execution|run#12|AssertionError|assert desc_ok["status"] == "SUCCEEDED", (
Issue-Commit: 0d5fdb6d7a78988c971aec85849026eed9fa5958

# lambda__sqs__stepfunctions: SendTaskHeartbeat does not reset a .waitForTaskToken task's HeartbeatSeconds window — a worker that heartbeats in time still fails the execution with States.Timeout

> _Note: this issue was identified against the code base at commit `0d5fdb6d7a78988c971aec85849026eed9fa5958`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendTaskHeartbeat

## Description
Cause 1 of 1 — the heartbeat window is started but never refreshed, so heartbeating does not keep a callback task alive.

AWS: for a Task state using a .waitForTaskToken integration (here arn:aws:states:::lambda:invoke.waitForTaskToken) with HeartbeatSeconds declared, each successful SendTaskHeartbeat on that task's token RESETS the heartbeat deadline to now + HeartbeatSeconds. A worker that is invoked, calls SendTaskHeartbeat inside the window, keeps working past the original deadline and finally calls SendTaskSuccess therefore drives the execution to SUCCEEDED, and no TaskTimedOut event appears in its history. Only a token that goes HeartbeatSeconds without a heartbeat fails the task, with error States.Timeout. The Step Functions Smithy model has no modelled semantics for the heartbeat clock, so this is documented behavior only (https://docs.aws.amazon.com/step-functions/latest/dg/state-task.html); SendTaskHeartbeat itself answers 200 in both the emulator and AWS, so the divergence is in the execution outcome, not a refusal.

Emulator: the positive-control execution — whose Lambda sleeps 3 s, calls SendTaskHeartbeat (which succeeds), sleeps another 3 s and calls SendTaskSuccess, all under HeartbeatSeconds=5 — ends FAILED with error States.Timeout. The heartbeat is accepted and then ignored: the task's deadline stays pinned to the moment the token was issued, so any callback whose total work exceeds one HeartbeatSeconds window is timed out regardless of how often it heartbeats.

Why: the entire reason HeartbeatSeconds exists — bounding worker liveness without bounding worker duration — is unavailable. Long-running human-approval or external-job callbacks cannot be modelled at all: declaring HeartbeatSeconds now caps total task duration instead of idle time, and a correct worker is killed.

Fix: the SendTaskHeartbeat handler must write back the task's heartbeat deadline (now + HeartbeatSeconds) in the task-token state the heartbeat-expiry sweeper reads.

Note: this check's replay key is not carried by any open issue. The open issue 20261109T185037Z-lambda-sqs-stepfunctions-4-net-new-causes-lambda-event-invocation-task.md names this check id, but it was keyed to a different assertion ('succeeded too fast to have required the heartbeat') from when HeartbeatSeconds was not enforced at all; the failure has since moved to the opposite end — the window is now enforced but never reset. The fixed issue 20261110T010311Z-stepfunctions-10-net-new-causes-assign-catch-credentials.md ('HeartbeatSeconds uninterpreted') covers the enforcement half, not the reset half.

Traces: lambda__sqs__stepfunctions.sfn-task-token-heartbeat-timeout-fails-execution.

## Evidence
AssertionError: expected SUCCEEDED, got FAILED: States.Timeout

Asserted at the in-file positive control (check line 100): DescribeExecution of the execution whose invoked Lambda sends SendTaskHeartbeat inside the HeartbeatSeconds=5 window and then SendTaskSuccess. Expected status 'SUCCEEDED' with no TaskTimedOut history event; the emulator reports status 'FAILED' with error 'States.Timeout'. The sibling execution on identical wiring whose Lambda never heartbeats is expected to fail with States.Timeout and does — so the window is enforced, but a heartbeat inside it has no effect.

## Steps to Reproduce
1. CreateRole for Lambda allowing states:SendTaskHeartbeat and states:SendTaskSuccess; CreateRole for Step Functions allowing lambda:InvokeFunction.
2. CreateFunction (python, timeout 30) whose handler reads event['token'], sleeps 3 s, calls SendTaskHeartbeat(taskToken=token), sleeps 3 s more, then calls SendTaskSuccess(taskToken=token, output='{"done": true}').
3. CreateStateMachine with one Task: Resource 'arn:aws:states:::lambda:invoke.waitForTaskToken', Parameters {FunctionName: <fn>, Payload: {"token.$": "$$.Task.Token"}}, HeartbeatSeconds 5, End true.
4. StartExecution with input '{}' and poll DescribeExecution to a terminal state.
5. Expected: SUCCEEDED, total wall time >= 5 s, and GetExecutionHistory containing no TaskTimedOut. Actual: FAILED with error States.Timeout.
