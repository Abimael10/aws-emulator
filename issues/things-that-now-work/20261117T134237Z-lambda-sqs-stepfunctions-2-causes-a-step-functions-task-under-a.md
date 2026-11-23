Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sqs__stepfunctions.deploy-missing-lambda-invoke-permission-sfn-template-negative lambda__sqs__stepfunctions.deploy-missing-sqs-sendmessage-permission-template-negative lambda__sqs__stepfunctions.sfn-invoke-lambda-missing-permission-negative lambda__sqs__stepfunctions.sfn-send-sqs-missing-permission-negative lambda__sqs__stepfunctions.cli-detach-lambda-invoke-policy-breaks-sfn-task-negative
Issue-Key: lambda__sqs__stepfunctions.deploy-missing-lambda-invoke-permission-sfn-template-negative|run#13|AssertionError|assert status == "FAILED", f"expected the denied execution to be FAILED, got {status!r}"
Issue-Key: lambda__sqs__stepfunctions.deploy-missing-sqs-sendmessage-permission-template-negative|run#12|AssertionError|assert status == "FAILED", (
Issue-Key: lambda__sqs__stepfunctions.sfn-invoke-lambda-missing-permission-negative|_poll_until#22|Timeout|time.sleep(interval_s)
Issue-Key: lambda__sqs__stepfunctions.sfn-send-sqs-missing-permission-negative|_run_sfn_until_permission_revoked#24|Timeout|time.sleep(5 + attempt * 5)
Issue-Key: lambda__sqs__stepfunctions.cli-detach-lambda-invoke-policy-breaks-sfn-task-negative|run#47|Timeout|time.sleep(5)
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# 2 causes: a Step Functions Task under a role lacking lambda:InvokeFunction or sqs:SendMessage SUCCEEDS instead of FAILING, and three sibling least-privilege scenarios never complete in 60 s

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Two distinct root causes, both about Step Functions Task states run under a state machine role that is missing the grant the Task needs.

Cause 1 of 2: a Task state whose state machine role does not grant the action it performs still runs the action and the execution ends SUCCEEDED.
AWS: a Task state calls the target service with credentials from the state machine's execution role. If that role does not allow the action, the call is denied and the execution terminates FAILED — stepfunctions DescribeExecution reports `status: FAILED` with an `error` naming the service-prefixed access denial (`Lambda.AccessDeniedException` for an optimized lambda:invoke Task, `Sqs.AccessDeniedException` for an optimized sqs:sendMessage Task) and a `cause` quoting the IAM refusal. (Smithy: stepfunctions DescribeExecution output members `status` (enum ExecutionStatus includes FAILED and SUCCEEDED), `error`, `cause` — recorded behavior only for the error naming.)
Emulator: both executions report `status` `'SUCCEEDED'` — the Task performed the action despite the role lacking `lambda:InvokeFunction` in one case and `sqs:SendMessage` in the other, so the role's permissions are never consulted.
Why: this is a fail-open authorization divergence. Every least-privilege test passes when the real deployment would fail, so a client ships a state machine whose role is missing grants and discovers it only in production — and, conversely, cannot verify that a deliberately scoped-down role actually blocks the Task.
Fix: the Step Functions Task integration's authorization step — the optimized lambda:invoke and sqs:sendMessage integrations must authorize the call against the state machine's execution role (or the Task's `Credentials` role) and, on denial, fail the execution with the service-prefixed access-denied error and cause.
Checks: lambda__sqs__stepfunctions.deploy-missing-lambda-invoke-permission-sfn-template-negative, lambda__sqs__stepfunctions.deploy-missing-sqs-sendmessage-permission-template-negative

Cause 2 of 2: three sibling least-privilege scenarios never reach a verdict at all — each is abandoned at the 60 s ceiling.
AWS: recorded behavior only for the end-to-end timing (the suite reports no response to compare). AWS terminates each of these executions promptly: a Task denied `lambda:InvokeFunction` or `sqs:SendMessage` fails within seconds, and detaching the invoke policy from a live state machine role makes the next execution fail the same way, so DescribeExecution reaches a terminal FAILED status rather than staying RUNNING.
Emulator: all three are abandoned at the 60 s ceiling — including the variant that detaches the lambda-invoke policy through the CLI and re-runs. No execution status is ever observed, so these scenarios neither enforce nor report the denial, and they do not even reach the SUCCEEDED-instead-of-FAILED symptom of Cause 1.
Why: a client gets a hang instead of an answer. The execution neither succeeds nor fails inside any reasonable budget, so nothing distinguishes "denied", "still running" and "the service stopped responding", and a caller cannot build a timeout or retry around it.
Fix: the Step Functions execution driver for a Task whose service call is refused — it must reach a terminal execution status (FAILED, with the access-denied error/cause) instead of leaving the execution unresolved — together with the CLI-driven policy-detach path that re-runs the same state machine.
Checks: lambda__sqs__stepfunctions.sfn-invoke-lambda-missing-permission-negative, lambda__sqs__stepfunctions.sfn-send-sqs-missing-permission-negative, lambda__sqs__stepfunctions.cli-detach-lambda-invoke-policy-breaks-sfn-task-negative

## Evidence
Cause 1 — actual, verbatim:
  AssertionError: expected the denied execution to be FAILED, got 'SUCCEEDED'
  AssertionError: execution with a role lacking sqs:SendMessage must terminate FAILED; got 'SUCCEEDED'
Expected DescribeExecution status 'FAILED' with error 'Lambda.AccessDeniedException' / 'Sqs.AccessDeniedException' and a cause quoting the IAM refusal.
Cause 2 — actual, verbatim (three checks):
  Error: Check timed out after 60.0s
No execution status was ever read in any of the three — the emulator produced no status, error or cause inside the budget.

## Steps to Reproduce
Cause 1: CreateStack (or create directly) a state machine whose role trusts states.amazonaws.com but grants no `lambda:InvokeFunction`, with a single Task invoking a real function; StartExecution and poll DescribeExecution. Observed status 'SUCCEEDED'; expected 'FAILED' with error `Lambda.AccessDeniedException` and a cause quoting the IAM refusal. Repeat with a role lacking `sqs:SendMessage` and an optimized sqs:sendMessage Task; observed 'SUCCEEDED', expected 'FAILED' with `Sqs.AccessDeniedException`.
Cause 2: Run the same two least-privilege scenarios in their standalone form, and the variant that attaches the invoke policy, runs a successful execution, detaches the policy with the CLI and re-runs. Observed: each check is abandoned after 60 s with no execution status read. Expected a terminal FAILED execution carrying the access-denied error and cause.
