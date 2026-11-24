Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__iam__stepfunctions.sfn-no-permission-to-write-ddb
Issue-Key: dynamodb__iam__stepfunctions.sfn-no-permission-to-write-ddb|_run_until_failed#22|Timeout|time.sleep(5)
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# A Step Functions execution whose role cannot write to DynamoDB never settles: the check never completes

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: an execution whose task role is denied dynamodb:PutItem never reaches a terminal state within the suite's budget.

AWS: recorded behavior only — the Smithy model (sfn StartExecution/DescribeExecution/GetExecutionHistory) does not time anything, but the recording shows an execution whose role lacks the DynamoDB write permission settles promptly to FAILED, with the task's failure carrying the AccessDenied cause, so the whole check completes well inside its budget.

Emulator: the check never gets that far — the replay reports `Check timed out after 60.0s`. The denied execution does not settle (or the task never reports an outcome), so nothing terminal is ever observable.

Why: a client polling DescribeExecution for a state machine whose role is misconfigured waits forever instead of getting a FAILED execution naming the denial — the normal way an operator diagnoses a missing grant.

Fix: the Step Functions task executor's error path for a denied downstream call (the task must fail the execution with the AccessDenied cause and the execution must settle to FAILED). Hypothesis: the task is left pending because the denial is never converted into a task failure.

Traces: dynamodb__iam__stepfunctions.sfn-no-permission-to-write-ddb.

## Evidence
Expected: the execution settles FAILED with the task failure naming the DynamoDB AccessDenied cause, within seconds.
Actual: `Error: Check timed out after 60.0s` — no terminal execution state was ever reached.

## Steps to Reproduce
1. Create a DynamoDB table and a state machine with a dynamodb:putItem task, with a role that does NOT allow dynamodb:PutItem.
2. StartExecution and poll DescribeExecution/GetExecutionHistory.
Observed: the execution never settles (the check is killed at 60s). Expected: a prompt FAILED execution whose task failure names the denial.
