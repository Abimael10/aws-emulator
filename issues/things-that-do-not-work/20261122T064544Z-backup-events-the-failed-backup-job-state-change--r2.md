Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup__events.backup-job-failed-event-matches-pattern
Issue-Key: backup__events.backup-job-failed-event-matches-pattern|_collect_messages#12|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=remaining)
Issue-Commit: b20300340ddfd95fa2f862f6c06bba5164a73eba

# backup__events: the FAILED 'Backup Job State Change' delivery still reaches no graded outcome — the detail.state=FAILED rule check ends with no verdict (1 cause)

> _Note: this issue was identified against the code base at commit `b20300340ddfd95fa2f862f6c06bba5164a73eba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartBackupJob

## Description
This issue contains 1 net-new root cause (net-new in the sense that no OPEN issue carries this check's replay key: the suite did not mark the check OWNED or ALREADY FILED). Prior art a fixer should read first: resolved issue 20261111T065053Z-backup-events-the-failed-backup-job-state-change.md names this exact check and its Resolution states the FAILED publication and the rule→SQS dispatch are now served and pinned; the check nevertheless still fails on this replay, and a resolved issue does not cover a failing check. Open issue 20261115T205809Z-backup-events-2-net-new-causes-a-vault.md owns the two neighbouring vault-ARN-scoped / Lambda-target checks in this service and does not carry this check's key.

Cause 1 of 1: a permission-less StartBackupJob's FAILED 'Backup Job State Change' transition is not observable end to end within a client's normal convergence window — the detail.state=FAILED-scoped rule's SQS target yields nothing the check can grade, and the check ends with no verdict at all.
AWS: recorded behavior only for the EventBridge envelope (it is in neither service's Smithy model). Smithy (backup, com-amazonaws-backup-cryocontrollerusermanager): StartBackupJob PUT /backup-jobs, 200, required BackupVaultName (pattern ^[a-zA-Z0-9\-\_]{2,50}$), ResourceArn, IamRoleArn, modeled errors InvalidParameterValueException / InvalidRequestException / LimitExceededException / MissingParameterValueException / ResourceNotFoundException (400) and ServiceUnavailableException (500); DescribeBackupJob GET /backup-jobs/{BackupJobId} reports State (enum BackupJobState, which includes FAILED), StatusMessage and MessageCategory. Recorded behavior: the call with a role that trusts backup.amazonaws.com but carries no permissions policy is accepted; the job fails asynchronously because the role cannot perform dynamodb:CreateBackup; DescribeBackupJob settles State=FAILED with a non-empty StatusMessage; that transition is published on the default bus of the vault's Region as source="aws.backup", detail-type="Backup Job State Change", detail.state="FAILED", detail.backupJobId=<the started job id>; an ENABLED rule whose pattern is {"source":["aws.backup"],"detail-type":["Backup Job State Change"],"detail":{"state":["FAILED"]}} delivers that envelope unchanged to its SQS target under a queue policy granting events.amazonaws.com sqs:SendMessage with Condition ArnEquals aws:SourceArn=<rule arn>, and delivers none of the same job's non-FAILED transitions. The job id, ARNs, names and timestamps are per-run values; the requirement is the FAILED envelope, its three envelope fields and its delivery to exactly the FAILED-scoped rule.
Emulator: the replay reports '[mismatch] backup__events.backup-job-failed-event-matches-pattern — Error: Check timed out after 60.0s'. No AWS call is refused; the determinable fact is that the check reaches none of its graded assertions ("the FAILED-state-only rule never delivered the failed job's event", and the per-message assertions on detail.state / detail-type / source) inside its budget, so the FAILED half of the Backup eventing contract is ungraded on this build.
Hypothesis (not stated by the model or by the recording, and the first thing a fixer should measure): the budget is spent before the delivery is reachable rather than on the delivery itself — the check's pre-assertion convergences are CreateTable→ACTIVE, the StartBackupJob retry loop that swallows InvalidParameterValueException for up to 90s, and the DescribeBackupJob poll to a terminal state, and its final collection window (180s) runs to its deadline unconditionally. Each emulator-side step on that path must settle in seconds for any client to observe the FAILED event at all; a fixer should instrument which of them consumes the time rather than assume the publication is missing.
Why: a client that reacts to failed backups — an alarm, a remediation Lambda, a queue consumer branching on detail.state — cannot distinguish "no failure happened" from "the failure was never delivered", and a negative test over the FAILED pattern passes vacuously because nothing is ever observed.
Fix: the backup job lifecycle's terminal-FAILED state-change emission and the latency of the states it waits on (job acceptance and settle clock) in the backup crate's start-backup-job / job-state machinery, together with the EventBridge rule-match and SQS target dispatch leg that must carry the matched envelope to the rule's target promptly.
Traces: backup__events.backup-job-failed-event-matches-pattern.

## Evidence
Suite verdict: '### backup__events → [mismatch] backup__events.backup-job-failed-event-matches-pattern — Error: Check timed out after 60.0s' (no OWNED / ALREADY FILED marker on this check).

Expected (AWS): StartBackupJob with a trust-only, permission-less role is accepted 200 with a BackupJobId; DescribeBackupJob reaches State="FAILED" with a StatusMessage (and a MessageCategory); the rule scoped to {"source":["aws.backup"],"detail-type":["Backup Job State Change"],"detail":{"state":["FAILED"]}} delivers at least one SQS message whose body carries source=="aws.backup", detail-type=="Backup Job State Change", detail.backupJobId==<the started job id> and detail.state=="FAILED", and delivers no non-FAILED transition of that job.

Actual (emulator): the check reaches no assertion at all; the replay ends 'Check timed out after 60.0s'. Nothing was refused and no error code or message diverged — the divergence is that none of the graded facts above is observable in the window a client gets.

Context for the fixer: resolved issue 20261111T065053Z-backup-events-the-failed-backup-job-state-change.md recorded the same check with the same shape of failure and claims in its Resolution that delivery now happens within about a second and is pinned by core/router tests; that claim is not reproduced by this replay, so either the replayed path differs from the pinned one or the time is going somewhere else on the pre-delivery path.

## Steps to Reproduce
1. dynamodb.CreateTable (PAY_PER_REQUEST, single HASH key) and wait for ACTIVE; backup.CreateBackupVault(BackupVaultName=<vault>).
2. iam.CreateRole with a trust policy allowing backup.amazonaws.com sts:AssumeRole and NO permissions policy attached.
3. events.PutRule(State=ENABLED, EventPattern={"source":["aws.backup"],"detail-type":["Backup Job State Change"],"detail":{"state":["FAILED"]}}); sqs.CreateQueue; sqs.SetQueueAttributes with a Policy allowing events.amazonaws.com sqs:SendMessage on the queue ARN with Condition ArnEquals aws:SourceArn=<rule arn>; events.PutTargets with the queue ARN.
4. backup.StartBackupJob(BackupVaultName=<vault>, ResourceArn=<table arn>, IamRoleArn=<powerless role arn>, IdempotencyToken=<uuid>) — accepted 200.
5. Poll backup.DescribeBackupJob until a terminal state — AWS: FAILED with a StatusMessage, within seconds.
6. sqs.ReceiveMessage in a loop — AWS yields the aws.backup "Backup Job State Change" envelope with detail.state="FAILED" for that backupJobId and nothing else for that job.
7. Re-replay backup__events.backup-job-failed-event-matches-pattern and record how long each step above takes on the emulator; the whole flow must complete well inside a client's convergence window.
