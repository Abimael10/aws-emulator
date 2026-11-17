Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative
Issue-Key: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative|_run_execution_expecting#27|Timeout|time.sleep(backoff_s)
Issue-Commit: 85ee122b95e0a025d1600c3d9babe834193f0ff3

# lambda__s3__stepfunctions: a Lambda Task whose state-machine role has lost lambda:InvokeFunction never reaches a terminal execution state — the grant/revoke/regrant scenario exhausts its budget

> _Note: this issue was identified against the code base at commit `85ee122b95e0a025d1600c3d9babe834193f0ff3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: a STANDARD state machine with a single Task state whose Resource is a Lambda function ARN is run three times against the same role — once with an inline lambda:InvokeFunction grant on that function (positive control), once with the grant revoked, once with the identical grant restored. The scenario never reaches a graded assertion: the executions never settle, so neither the positive control's SUCCEEDED nor the denied leg's FAILED is observable, and the S3 side-effect marker the handler writes can never be compared across the three legs.

AWS: Smithy (sfn) models DescribeExecution with a required `status` of type ExecutionStatus and the optional `error`/`cause` members, so every execution a client starts must converge to a terminal status that the API reports. A Task state that invokes a Lambda function the state machine's execution role is not permitted to invoke fails the task: the execution reaches status FAILED and reports the authorization failure in `error`/`cause` (AWS surfaces it as the Lambda-service error relayed by States — an access-denied Task failure, recorded in GetExecutionHistory as the corresponding LambdaFunctionFailed/TaskFailed event). Recorded behavior only for the exact cause text; the requirement asserted here is weaker and purely structural: the execution must terminate, the denied leg must terminate FAILED with an access-denied-class error or cause, the granted legs must terminate SUCCEEDED, and only the denied leg may leave the Lambda's S3 marker absent.

Emulator: the replay reports only `Error: Check timed out after 60.0s`. No execution of the three legs is observed in a terminal state inside the budget, so DescribeExecution never yields SUCCEEDED or FAILED and no error/cause is readable. Hypothesis: the Task state's Lambda invocation is neither authorized nor failed, leaving the execution in a non-terminal state indefinitely; a Lambda invoke that never returns would produce the same timeout.

Why: a client cannot detect a missing lambda:InvokeFunction grant at all. The signal AWS gives — a FAILED execution naming the denial — never arrives, so a broken least-privilege role is indistinguishable from a hung workflow; any waiter on DescribeExecution (including the SDK's execution waiters and CI gates) blocks instead of failing, and a positive control cannot be established either, so the enforcement cannot be observed in either direction.

Fix: the Step Functions Task-state execution path for a bare-Lambda-ARN Resource — the invoke must be authorized against the state machine's execution role, an authorization failure must fail the task and drive the execution to a terminal FAILED status with the access-denied error/cause recorded on DescribeExecution and in the execution history, and a granted invoke must drive it to SUCCEEDED. Two open issues describe the same symptom but neither carries this run's replay key, so this check is not covered by them: 20261117T143723Z-lambda-s3-stepfunctions-a-task-state-whose-lambda.md and 20261117T134237Z-lambda-s3-stepfunctions-the-denied-lambda-invokefunction-step-functions-task-scenario.md — fixing this cause should let all three be re-replayed together.

Traces: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative.

## Evidence
Expected (AWS): with lambda:InvokeFunction granted on the Task's function, StartExecution converges to DescribeExecution status 'SUCCEEDED' and the Lambda's S3 marker object exists; with the identical grant revoked, the next execution converges to status 'FAILED' whose error/cause is an access-denied-class Task failure (an AccessDenied / not-authorized / States.TaskFailed classification) and NO S3 marker is written; re-granting the identical policy makes the next execution 'SUCCEEDED' with its marker present again.
Actual (emulator): `Error: Check timed out after 60.0s` — none of the three executions reaches a terminal status inside the budget, so DescribeExecution reports no SUCCEEDED/FAILED status, no error and no cause, and the marker comparison across the three legs is never made.

## Steps to Reproduce
1. Create an S3 bucket and a Lambda function whose handler writes a marker object triggered/<token>.txt into it.
2. Create an IAM role trusted by states.amazonaws.com for the state machine.
3. CreateStateMachine(type=STANDARD) with one state: {"Type":"Task","Resource":"<function ARN>","End":true}.
4. PutRolePolicy granting lambda:InvokeFunction on that function ARN; StartExecution and poll DescribeExecution to a terminal status.
Observed: the execution does not reach a terminal status; the scenario hits its 60 s budget before the positive control can be graded.
5. DeleteRolePolicy to revoke the grant; StartExecution again and poll DescribeExecution.
Observed: no FAILED status and no access-denied error/cause is ever reported; the S3 marker's absence can therefore not be attributed to the revoked grant.
6. Re-apply the identical PutRolePolicy and StartExecution a third time.
Observed: no SUCCEEDED status is reported either.
