Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup__events.eventbridge-rule-target-missing-startbackupjob-permission-denied
Issue-Key: backup__events.eventbridge-rule-target-missing-startbackupjob-permission-denied|_poll_until#29|Timeout|time.sleep(interval_s)
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# backup__events: the EventBridge→Step Functions→StartBackupJob least-privilege chain never completes, so neither the AccessDeniedException refusal nor the FAILED execution can be observed

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartBackupJob

## Description
Cause 1 of 1 — the rule-driven StartBackupJob denial chain never converges within the check's budget.
AWS: a principal with no backup permissions at all is refused by StartBackupJob with HTTP 400 AccessDeniedException whose message is AWS's fixed identity-based-policy sentence with the request's own values substituted: `User: <the caller's assumed-role ARN, arn:aws:sts::<account>:assumed-role/<role name>/<session name>> is not authorized to perform: backup:StartBackupJob on resource: <the backup vault ARN from the request> because no identity-based policy allows the backup:StartBackupJob action`. The fixed text is everything but the principal ARN, the resource ARN and the action name. Driving the same call from a Step Functions Task whose execution role lacks `backup:StartBackupJob` makes the execution reach FAILED and creates no backup job (ListBackupJobs ByResourceArn stays empty), while an otherwise identical chain whose role HAS the grant succeeds and produces a job.
Emulator: the check never reaches any of those assertions — the replay reports `Check timed out after 60.0s`. Hypothesis: one of the chain's convergence waits does not complete — either the denial wait on the assumed-role StartBackupJob never sees an AccessDeniedException, or the rule never invokes the state machine so no execution ever appears for ListExecutions(statusFilter=FAILED) to return.
Why: the whole least-privilege contract for AWS Backup driven from EventBridge is unverifiable: a client cannot tell whether a missing grant is enforced at all, and an unenforced grant means a backup job AWS would refuse is created here.
Fix: the StartBackupJob handler's identity-based authorization (and its AccessDeniedException message), plus the EventBridge rule → Step Functions target invocation path the chain depends on. Related open issues 20261111T065053Z-backup-events-the-failed-backup-job-state-change.md and 20261115T205809Z-backup-events-2-net-new-causes-a-vault.md cover other backup__events checks and do not carry this check's replay key.
Traces: backup__events.eventbridge-rule-target-missing-startbackupjob-permission-denied.

## Evidence
`Check timed out after 60.0s` — no assertion reached. The first assertion the check would make is `code == "AccessDeniedException"` with message `User: arn:aws:sts::<account>:assumed-role/<role>/sim2real-negcheck is not authorized to perform: backup:StartBackupJob on resource: <vault arn> because no identity-based policy allows the backup:StartBackupJob action`; the next is that the EventBridge-triggered execution reaches FAILED with ListBackupJobs(ByResourceArn) staying empty.

## Steps to Reproduce
1. Opt DynamoDB into Backup, create a backup service role, a DynamoDB table and a backup vault.
2. Create a role trusted by states.amazonaws.com and by the test account with NO backup permissions; assume it and call StartBackupJob — it must be refused with AccessDeniedException carrying the message above.
3. Create a state machine using that role whose Task calls backup:StartBackupJob, plus an EventBridge invoke role, a rule on a unique source and a Step Functions target.
4. PutEvents on that source; the execution must appear and reach FAILED, and ListBackupJobs ByResourceArn must stay empty.
5. Repeat with a role that HAS backup:StartBackupJob and confirm a job is created.
