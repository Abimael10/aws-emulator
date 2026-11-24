Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs__stepfunctions.sfn-task-token-heartbeat-timeout-fails-execution
Issue-Key: lambda__sqs__stepfunctions.sfn-task-token-heartbeat-timeout-fails-execution|run#12|AssertionError|assert desc_ok["status"] == "SUCCEEDED", (
Issue-Commit: 2ab4672abb66b9678d70a9b781838637fd327d95

# lambda__sqs__stepfunctions: SendTaskHeartbeat does not extend a .waitForTaskToken task's HeartbeatSeconds window — a heartbeating worker's execution still FAILS with States.Timeout

> _Note: this issue was identified against the code base at commit `2ab4672abb66b9678d70a9b781838637fd327d95`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendTaskHeartbeat

## Description
Cause 1 of 1 — SendTaskHeartbeat is accepted (HTTP 200, empty body) but does not reset the task token's heartbeat deadline, so a .waitForTaskToken task whose worker heartbeats inside the window is still killed with States.Timeout. Failing check id: lambda__sqs__stepfunctions.sfn-task-token-heartbeat-timeout-fails-execution (the in-file POSITIVE control of that check, which must SUCCEED, fails; the check's negative case is therefore never reached).

AWS: a Task state with Resource 'arn:aws:states:::lambda:invoke.waitForTaskToken' and HeartbeatSeconds declared stays alive for HeartbeatSeconds counted from the MOST RECENT SendTaskHeartbeat on that token, not from the moment the token was issued. A worker that calls SendTaskHeartbeat inside the window and later calls SendTaskSuccess drives the execution to status SUCCEEDED with no 'error' and no TaskTimedOut history event, and the execution's total wall time legitimately exceeds one HeartbeatSeconds window. Only a worker that sends nothing inside the window fails the task with error 'States.Timeout'. Smithy: SendTaskHeartbeat takes only taskToken (@required, @length 1..2048, @sensitive) and returns an empty output; its modeled errors are InvalidToken, TaskDoesNotExist and TaskTimedOut (all client, HTTP 400), and the model says nothing about the heartbeat clock itself — that part is documented behavior only (https://docs.aws.amazon.com/step-functions/latest/dg/state-task.html, 'Task state' / heartbeat timeouts). SendTaskHeartbeat answers 200 in this replay, so the divergence is in the execution outcome and not in a refusal.

Emulator: with HeartbeatSeconds=5 and an invoked Lambda that sleeps 3 s, calls SendTaskHeartbeat on $$.Task.Token (accepted), sleeps a further 3 s and then calls SendTaskSuccess, DescribeExecution reports status 'FAILED' with error 'States.Timeout' instead of 'SUCCEEDED'. The heartbeat is a no-op against the deadline: the deadline stays pinned to token-issue time, so the task dies at the first window boundary regardless of how many heartbeats arrive. The state's Retry clause covers only States.TaskFailed, so this States.Timeout is the genuine heartbeat outcome and not a retried-away IAM-propagation failure.

Why: a client relying on AWS's liveness contract loses it entirely — HeartbeatSeconds degenerates into a hard maximum task duration, so any callback whose work outlasts a single window is aborted no matter how diligently the worker heartbeats, and the documented pattern of a short HeartbeatSeconds plus periodic heartbeats for a long-running worker cannot be used at all. Downstream, the SUCCEEDED/no-TaskTimedOut history shape of a healthy heartbeating task is unobservable.

Fix: the SendTaskHeartbeat handler (and the task-token state it writes) must reset that token's heartbeat deadline to now + HeartbeatSeconds, in the same state read by the heartbeat-expiry sweeper that fails the task with States.Timeout and by the TaskTimedOut history-event writer.

Related open issues that do NOT carry this check's replay key: 20261109T185037Z-lambda-sqs-stepfunctions-4-net-new-causes-lambda-event-invocation-task.md lists this check id, but its recorded key for it is the later 'elapsed_ok >= 5' assertion (the execution then succeeded too fast); the replay now fails earlier, on the SUCCEEDED status assertion, so that key no longer matches and the suite did not mark this failure owned. 20261119T204102Z-lambda-stepfunctions-a-waitfortasktoken-task-times-out-with.md describes the same behaviour under the lambda__stepfunctions heading, and 20261116T170218Z-lambda-stepfunctions-a-waitfortasktoken-heartbeat-lapse-ignores-catch.md concerns Catch ordering on a heartbeat LAPSE. Fixing the heartbeat deadline reset should close all of them together.

Traces: lambda__sqs__stepfunctions.sfn-task-token-heartbeat-timeout-fails-execution.

## Evidence
AssertionError: expected SUCCEEDED, got FAILED: States.Timeout

Raised at check_lambda__sqs__stepfunctions_sfn_task_token_heartbeat_timeout_fails_execution.py line 100, the in-file positive control:
  assert desc_ok["status"] == "SUCCEEDED", f"expected SUCCEEDED, got {desc_ok['status']}: {desc_ok.get('error')}"

Expected (AWS): DescribeExecution -> status 'SUCCEEDED', no 'error' member, stopDate-startDate >= HeartbeatSeconds (5 s here) because the heartbeat kept the task alive past the first window.
Actual (emulator): DescribeExecution -> status 'FAILED', error 'States.Timeout'. The error code 'States.Timeout' is the fixed Step Functions error name; the HeartbeatSeconds value (5), the state name, the function ARN and the execution ARN are values taken from the request/state, not part of the requirement.

## Steps to Reproduce
1. CreateRole trusting lambda.amazonaws.com with states:SendTaskHeartbeat + states:SendTaskSuccess; CreateRole trusting states.amazonaws.com with lambda:InvokeFunction.
2. CreateFunction (python3.12, Timeout 30) whose handler reads event['token'], sleeps 3 s, calls SendTaskHeartbeat(taskToken=token), sleeps 3 s, then SendTaskSuccess(taskToken=token, output='{"done": true}').
3. CreateStateMachine with a single Task state: Resource 'arn:aws:states:::lambda:invoke.waitForTaskToken', Parameters {FunctionName: <fn>, Payload: {"token.$": "$$.Task.Token"}}, HeartbeatSeconds 5, Retry [{ErrorEquals:["States.TaskFailed"], IntervalSeconds:2, MaxAttempts:5, BackoffRate:2.0}], End true.
4. StartExecution with input '{}' and poll DescribeExecution until terminal.
5. Expected: status SUCCEEDED, no error, elapsed >= 5 s, and GetExecutionHistory containing no TaskTimedOut event. Actual: status FAILED with error 'States.Timeout'.
