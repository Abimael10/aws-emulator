Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__lambda__stepfunctions.metrics-states-stream-lambda-invoke-denied-role-fails-execution-function-series-empty cloudwatch__lambda__stepfunctions.obs-sfn-1-optimized-lambda-invoke-denied-and-ok-never-populate-function-arn-series
Issue-Key: cloudwatch__lambda__stepfunctions.metrics-states-stream-lambda-invoke-denied-role-fails-execution-function-series-empty|run#15|AssertionError|assert denied["status"] == "FAILED", (
Issue-Key: cloudwatch__lambda__stepfunctions.obs-sfn-1-optimized-lambda-invoke-denied-and-ok-never-populate-function-arn-series|run#16|AssertionError|assert d["status"] == "FAILED", f"denied execution {n} must FAIL; got {d['status']!r}"
Issue-Commit: e00df5b4e39e0b77a91b9413050affc17449d7b8

# stepfunctions: an execution whose state-machine role is denied lambda:InvokeFunction SUCCEEDS instead of FAILING with Lambda.AccessDeniedException

> _Note: this issue was identified against the code base at commit `e00df5b4e39e0b77a91b9413050affc17449d7b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: a Task state's optimized Lambda invoke is performed regardless of the state machine's execution role, so an execution that AWS fails completes successfully.
AWS: a Step Functions Task state using the optimized Lambda integration invokes the function with the STATE MACHINE's execution role. If that role's policy does not allow lambda:InvokeFunction on the target function, the task fails with the error 'Lambda.AccessDeniedException' (cause: the IAM not-authorized message for lambda:InvokeFunction), the state's retriers/catchers do not mask it, and DescribeExecution reports status 'FAILED'; the function is never invoked, so no AWS/Lambda Invocations/Errors/Duration datapoint appears for its FunctionName dimension and the AWS/States metrics count the execution as failed. Recorded behavior only (authorization outcomes are not expressed in the Smithy API model).
Emulator: the denied execution reaches status 'SUCCEEDED' — the invoke is carried out without consulting the role — so the FAILED outcome, the Lambda.AccessDeniedException error and the 'empty function series' consequence are all unobservable. Reproduced by two independent checks, one using the states-stream path and one the optimized-invoke path.
Why: this erases the IAM boundary for cross-service calls. A state machine whose role is missing lambda:InvokeFunction appears healthy in the emulator and fails in production; conversely, negative tests that assert a denied role stops the workflow pass vacuously, and error-handling branches (Catch on Lambda.AccessDeniedException, failure alarms on AWS/States ExecutionsFailed) are never exercised.
Fix: the Step Functions task-execution component that performs the optimized Lambda integration — it must evaluate the execution role's permission for lambda:InvokeFunction on the target function and fail the task with Lambda.AccessDeniedException when it is not allowed, rather than invoking unconditionally.
Traces: cloudwatch__lambda__stepfunctions.metrics-states-stream-lambda-invoke-denied-role-fails-execution-function-series-empty, cloudwatch__lambda__stepfunctions.obs-sfn-1-optimized-lambda-invoke-denied-and-ok-never-populate-function-arn-series.

## Evidence
Expected: DescribeExecution of the execution started on the state machine whose role cannot invoke the function reports status 'FAILED' (error 'Lambda.AccessDeniedException'), and no AWS/Lambda series exists for that FunctionName.
Actual: "AssertionError: an execution whose role cannot invoke the function must FAIL; got 'SUCCEEDED'" and "AssertionError: denied execution 0 must FAIL; got 'SUCCEEDED'".

## Steps to Reproduce
1. Create a Lambda function (zip inline handler) and two IAM roles assumable by states.amazonaws.com: one whose policy allows lambda:InvokeFunction on the function, one whose policy does not.
2. CreateStateMachine twice with the same definition — a single Task state using the optimized Lambda integration (Resource = the function ARN, or arn:aws:states:::lambda:invoke with FunctionName) — once per role.
3. StartExecution on the state machine backed by the DENIED role and poll DescribeExecution: expect status 'FAILED' with error 'Lambda.AccessDeniedException'; the emulator reports 'SUCCEEDED'.
4. Confirm with the allowed role that the same execution SUCCEEDS, and that GetMetricStatistics on AWS/Lambda Invocations for the function's FunctionName dimension shows datapoints only for the allowed-role execution.
