Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative
Issue-Key: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative|_poll_until#27|Timeout|return last
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# lambda__s3__stepfunctions: a Task state whose Lambda the state machine role cannot invoke never reaches a terminal execution state, so the denied-invoke check reaches no verdict

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: an execution whose Task state invokes a Lambda function that the state machine's role is not permitted to invoke never settles — the check waits on the execution's terminal state and exhausts its whole budget without grading a single assertion.

AWS: Smithy (sfn) models DescribeExecution -> ExecutionStatus and GetExecutionHistory; a Task state calling lambda:InvokeFunction without permission fails the task and the execution reaches a terminal FAILED status, with the failure surfaced in the history as a TaskFailed/LambdaFunctionFailed event naming the authorization failure (AWS classifies it as States.TaskFailed / Lambda.AccessDeniedException). The requirement here is that the execution terminates and reports the refusal; recorded behavior only for the exact event text, and the recording shows no terminal state at all.

Emulator: 'Error: Check timed out after 60.0s' — the execution neither succeeds nor fails within the budget. Hypothesis: the Task state's Lambda invocation is neither authorized nor failed, leaving the execution running indefinitely.

Why: a client cannot detect a missing lambda:InvokeFunction grant at all — the usual signal (a FAILED execution with the access-denied cause in its history) never arrives, so a broken least-privilege role looks like a hung workflow instead of a permissions error, and a waiter on the execution never returns.

Fix: the Step Functions Task-state execution path for a Lambda resource — an authorization failure on the invoke must fail the task and drive the execution to a terminal FAILED status with the cause recorded in the execution history.

Traces: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative.

## Evidence
Expected: StartExecution on a state machine whose role lacks lambda:InvokeFunction for the Task's function leads to DescribeExecution ExecutionStatus 'FAILED' with the access-denied cause in GetExecutionHistory.
Actual: 'Error: Check timed out after 60.0s' — the execution never reaches a terminal status, so no status, error or cause is observable.

## Steps to Reproduce
1. Create a Lambda function and a Step Functions role whose policy does NOT grant lambda:InvokeFunction on it.
2. CreateStateMachine with a single Task state whose Resource is that function's ARN.
3. StartExecution and poll DescribeExecution for a terminal status; read GetExecutionHistory.
Observed: the execution never terminates; the check hits its 60 s budget with no status to read.
