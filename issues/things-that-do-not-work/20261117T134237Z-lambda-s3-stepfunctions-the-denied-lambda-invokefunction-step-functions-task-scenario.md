Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative
Issue-Key: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative|_poll_until#27|Timeout|last = probe()
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# The denied-lambda:InvokeFunction Step Functions task scenario never completes within the 60 s budget

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: the least-privilege Step Functions scenario — a Task whose state machine role lacks `lambda:InvokeFunction` — never reaches a verdict; the whole check is abandoned at the 60 s ceiling.

AWS: recorded behavior only for the end-to-end timing (the suite reports no response to compare). The behavior AWS serves is documented: a Task state whose state machine role is not granted `lambda:InvokeFunction` on the target function fails the execution promptly, and stepfunctions DescribeExecution reports `status: FAILED` with an `error` naming the access denial (`Lambda.AccessDeniedException` for the optimized integration) and a `cause` quoting the IAM refusal. The execution terminates; it does not stay RUNNING.

Emulator: the check is abandoned at the 60 s ceiling. No execution status is ever observed, so the denial is neither enforced nor reported inside the budget.

Why: least privilege is the whole point of a state machine role. A client cannot find out that its role is missing a grant — the execution neither fails nor answers, so the run hangs instead of surfacing the denial, and nothing distinguishes "denied" from "still running".

Fix: the Step Functions Task integration's IAM authorization check for the optimized Lambda invoke, and the execution-termination path that must move the execution to FAILED with the access-denied error/cause rather than leaving it unresolved.

Traces: lambda__s3__stepfunctions.sfn-task-lambda-missing-invoke-permission-negative

## Evidence
Expected: DescribeExecution reports status FAILED with error `Lambda.AccessDeniedException` and a cause quoting the IAM refusal.
Actual, verbatim:
  Error: Check timed out after 60.0s
No execution status was ever read — the check produced no status, error or cause from the emulator inside the budget.

## Steps to Reproduce
1. CreateFunction with a trivial handler.
2. Create a state machine role that trusts states.amazonaws.com but grants no `lambda:InvokeFunction`.
3. stepfunctions CreateStateMachine with a single Task state targeting that function; StartExecution.
4. Poll DescribeExecution.
5. Observe: the check never reaches a verdict within 60 s. Expected status FAILED with an access-denied error and a cause quoting the IAM refusal.
