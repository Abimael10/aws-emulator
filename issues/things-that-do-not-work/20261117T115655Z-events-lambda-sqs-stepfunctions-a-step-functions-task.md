Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: events__lambda__sqs__stepfunctions.sfn-task-lambda-fails-without-invoke-permission events__lambda__sqs__stepfunctions.sfn-task-sqs-denied-without-permission
Issue-Key: events__lambda__sqs__stepfunctions.sfn-task-lambda-fails-without-invoke-permission|run#7|AssertionError|assert result["status"] == "FAILED", (
Issue-Key: events__lambda__sqs__stepfunctions.sfn-task-sqs-denied-without-permission|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# events__lambda__sqs__stepfunctions: a Step Functions Task state's Lambda invoke is not authorized against the execution role — an execution whose role lacks lambda:InvokeFunction SUCCEEDS

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Two causes on the Step Functions Task-integration authorization path. Each must be closed before this issue is submitted.

Cause 1 of 2 — the Lambda Task integration ignores the state machine's execution role.
AWS: a Task state invoking a Lambda function calls it with the state machine's execution role. If that role carries no `lambda:InvokeFunction` grant for the function, the task fails and the execution reaches status FAILED; the failure is attributed to the denied action — the execution's cause (and the matching TaskFailed / LambdaFunctionFailed event in GetExecutionHistory) names AccessDenied and the action `lambda:InvokeFunction`. Recorded behavior only for the exact cause string; the status and the attribution are the requirement. The emulator's own trust-propagation failures are distinguishable (they say "not authorized to assume"), so the only remaining reason to fail is the missing grant.
Emulator: the execution reaches status SUCCEEDED and returns the Lambda's result (observed: `status: 'SUCCEEDED'`, `output: '{"out":{"doubled":42}}'` for input `{"n": 21}`), i.e. the Task invoked the function with no `lambda:InvokeFunction` grant anywhere in the role.
Why: this is overly permissive — a least-privilege role is not enforced, so a state machine that AWS would fail closed runs to completion here. Clients validating IAM scoping (or testing their Catch/Retry on Lambda.AccessDeniedException) get a green run that hides a real production failure.
Fix: the Step Functions Task integration's authorization hop for the `arn:aws:states:::lambda:invoke` / function-ARN resource — it must evaluate the execution role before dispatching and surface an AccessDenied task failure naming lambda:InvokeFunction.
Traces: events__lambda__sqs__stepfunctions.sfn-task-lambda-fails-without-invoke-permission.

Cause 2 of 2 — the same contract for an SQS Task integration never completes.
AWS: a Task state whose integration sends to an SQS queue is likewise authorized against the execution role: without `sqs:SendMessage` the execution reaches FAILED with an access-denied cause and no message is enqueued, while the same chain with the grant succeeds and the message arrives.
Emulator: the check never reaches its assertions — the replay reports `Check timed out after 60.0s`. Hypothesis: the settle/drain loop that must observe the denied execution's terminal status (or the absence of a message on the queue) never converges within the budget.
Why: the SQS half of the Task-integration authorization contract cannot be established at all.
Fix: the same Task-integration authorization hop for the SQS integration, and the execution-settling path the check polls.
Traces: events__lambda__sqs__stepfunctions.sfn-task-sqs-denied-without-permission.

## Evidence
Cause 1: `AssertionError: execution succeeded without lambda:InvokeFunction -- overly permissive: {'executionArn': 'arn:aws:states:<region>:<account>:execution:sm-noperm<suffix>:<uuid>', 'status': 'SUCCEEDED', 'input': '{"n": 21}', 'output': '{"out":{"doubled":42}}', ...}`. Expected status FAILED, with the execution description plus GetExecutionHistory containing both the string `AccessDenied` and the string `lambda:InvokeFunction`.
Cause 2: `Check timed out after 60.0s` — no assertion reached.

## Steps to Reproduce
1. Create a Lambda function (handler doubling `event["n"]`).
2. Create an IAM role trusted by states.amazonaws.com with NO lambda:InvokeFunction grant.
3. Create a state machine with a single Task state invoking that function, using that role.
4. StartExecution with `{"n": 21}` and wait for a terminal status — it must be FAILED, and describe-execution + get-execution-history must name AccessDenied and lambda:InvokeFunction.
5. Repeat for an SQS Task integration with a role lacking sqs:SendMessage: the execution must fail and the queue must stay empty.
