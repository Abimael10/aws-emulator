Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup__cloudformation.deploy-backup-plan-invalid-schedule-expression-rejected
Issue-Key: backup__cloudformation.deploy-backup-plan-invalid-schedule-expression-rejected|run#25|AssertionError|assert got == _REJECTED, f"Plan reason {got!r}, expected {_REJECTED!r}"
Issue-Commit: 16dd3a69d4b4af3373dd95173ca2a60641ca57cc

# The CREATE_FAILED reason for a rejected AWS::Backup::BackupPlan drops the SDK error suffix from the handler message

> _Note: this issue was identified against the code base at commit `16dd3a69d4b4af3373dd95173ca2a60641ca57cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Cause 1 of 1: the ResourceStatusReason CloudFormation reports when the AWS::Backup::BackupPlan handler is refused by AWS Backup omits the SDK's error suffix.
AWS: recorded behavior only. The handler message is the SDK rendering of the Backup error: `<service message> (Service: Backup, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)`, carried inside CloudFormation's `Resource handler returned message: "..." (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)` wrapper. Fixed text: the service message for the error class (here `Error in some rules due to : Cron expression is not valid: Invalid cron expression`), the literal ` (Service: Backup, Status Code: 400, Request ID: ` fragment and the trailing ` (SDK Attempt Count: <n>)`; variable parts: the request id and the attempt count (per run).
Emulator: the reason carries only the bare service message — `Error in some rules due to : Cron expression is not valid: Invalid cron expression` — with the entire ` (Service: Backup, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)` suffix missing. The HandlerErrorCode (InvalidRequest) is correct.
Why: a client that reads the failure reason to learn which downstream service refused the deploy, with what HTTP status, and under which request id — the normal way a failed stack is correlated with the service's own record — finds none of that, and automation that pins the AWS reason shape treats the deploy failure as unrecognised.
Fix: the AWS::Backup::BackupPlan resource handler's failure-reason serializer (re-render the downstream Backup error with the SDK suffix before wrapping it).
Traces: backup__cloudformation.deploy-backup-plan-invalid-schedule-expression-rejected

## Evidence
AssertionError: Plan reason ('Error in some rules due to : Cron expression is not valid: Invalid cron expression', 'InvalidRequest'), expected ('Error in some rules due to : Cron expression is not valid: Invalid cron expression (Service: Backup, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest') — the message text and the handler error code agree; only the SDK suffix is absent.

## Steps to Reproduce
Replay the behavioral suite and re-run backup__cloudformation.deploy-backup-plan-invalid-schedule-expression-rejected: create a stack whose AWS::Backup::BackupPlan rule carries an invalid ScheduleExpression and read the plan resource's CREATE_FAILED ResourceStatusReason from DescribeStackEvents.
