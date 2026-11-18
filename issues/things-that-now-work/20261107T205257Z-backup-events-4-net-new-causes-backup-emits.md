Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup__events.backup-framework-compliance-change-event backup__events.backup-vault-notifications-eventbridge-rule-scoped-by-detail-type backup__events.cfn-deploy-backup-plan-with-eventbridge-rule-target backup__events.cli-create-backup-plan-and-verify-eventbridge-rule-target backup__events.eventbridge-target-input-transformer-passes-backup-vault-param
Issue-Key: backup__events.backup-framework-compliance-change-event|_wait_for_event_matching#26|AssertionError|raise AssertionError(f"no matching event arrived on {queue_url} within {timeout_s}s")
Issue-Key: backup__events.backup-vault-notifications-eventbridge-rule-scoped-by-detail-type|_wait_for_event_matching#17|AssertionError|raise AssertionError(f"no matching event arrived on {queue_url} within {timeout_s}s")
Issue-Key: backup__events.cfn-deploy-backup-plan-with-eventbridge-rule-target|_poll_until#17|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Key: backup__events.cli-create-backup-plan-and-verify-eventbridge-rule-target|run#26|AssertionError|assert messages, f"expected the native state-change event for job {real_job_id} to reach the SQS target"
Issue-Key: backup__events.eventbridge-target-input-transformer-passes-backup-vault-param|run#23|AssertionError|assert executions, "input-transformed event never produced a successful execution within 120s (IAM role propagation?)"
Issue-Commit: 959ef406fada22402fcbc8404625fdf48db790f8

# backup__events: 4 net-new causes — Backup emits no framework-compliance, vault-notification or job-state-change event to EventBridge targets, and a rule target's Step Functions aws-sdk:backup task never succeeds

> _Note: this issue was identified against the code base at commit `959ef406fada22402fcbc8404625fdf48db790f8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartBackupJob / PutBackupVaultNotifications / DescribeFramework / PutTargets

## Description
Four distinct causes, all in AWS Backup's eventing surface. They are NOT the already-owned "no Backup Job State Change event" issues: the suite reports these five checks as un-owned, and each exercises a different event class or delivery path. Related open issues naming the neighbouring causes: 20261029T175130Z-backup-events-no-backup-copy-restore-recovery-point.md, 20261102T091831Z-backup-events-no-aws-backup-state-change-event.md, 20261030T103029Z-backup-events-no-copy-job-state-change-event.md.

Cause 1 of 4: no Backup framework compliance-change event is published.
AWS: creating/updating a framework (CreateFramework/UpdateFramework) and letting it evaluate publishes an aws.backup event whose detail-type names the framework compliance change on the default bus, so a rule matching source aws.backup and that detail-type delivers to its SQS target. Recorded behavior only (the Smithy model for backup carries no event documentation); the check's reference behavior is a delivered envelope on the queue.
Emulator: nothing matching ever arrives — "no matching event arrived on <queue-url> within 90.0s".
Failing check: backup__events.backup-framework-compliance-change-event.

Cause 2 of 4: backup vault notifications are not published as EventBridge events scoped by detail-type.
AWS: PutBackupVaultNotifications on a vault causes the named vault events to be published; a rule scoped to one detail-type receives only the matching record and its sibling rule receives none.
Emulator: no event ever arrives — "no matching event arrived on <queue-url> within 120.0s".
Failing check: backup__events.backup-vault-notifications-eventbridge-rule-scoped-by-detail-type.

Cause 3 of 4: a COMPLETED "Backup Job State Change" event never reaches a rule target, neither through SNS nor natively to SQS, whether the rule was created by CloudFormation or by the CLI.
AWS: StartBackupJob running to COMPLETED publishes a "Backup Job State Change" event on the default bus naming the job id; a rule with an SNS target forwards it to the subscribed queue, and a rule with an SQS target delivers the envelope directly.
Emulator: neither path ever yields the event — "COMPLETED Backup Job State Change event delivered via SNS->SQS not met within 120s. Last result: None" and "expected the native state-change event for job <job-id> to reach the SQS target" (the job id is a per-run value).
Failing checks: backup__events.cfn-deploy-backup-plan-with-eventbridge-rule-target, backup__events.cli-create-backup-plan-and-verify-eventbridge-rule-target.

Cause 4 of 4: an EventBridge rule target that starts a state machine with an input transformer never produces a successful execution.
AWS: the transformed input is handed to StartExecution and the state machine's Task state runs its AWS SDK integration (arn:aws:states:::aws-sdk:backup:startBackupJob) to completion, so the execution reaches SUCCEEDED.
Emulator: no successful execution appears — "input-transformed event never produced a successful execution within 120s". Hypothesis: the same gap the sibling (owned) backup checks show verbatim — the Step Functions Task integration layer answers 'arn:aws:states:::aws-sdk:backup:startBackupJob' names an integration with 'backup', which is not one of the services this emulator makes a 'Task' state's integration call to, so nothing was run for it — which fails the execution with States.Runtime.
Failing check: backup__events.eventbridge-target-input-transformer-passes-backup-vault-param.

Why: a client that wires compliance, vault-notification or job-state-change automation off AWS Backup sees its rules never fire, and a rule whose target is a state machine calling the Backup SDK integration sees every execution fail; remediation, auditing and fan-out pipelines silently do nothing.

Fix: the Backup service's event-publication path (framework compliance evaluation, PutBackupVaultNotifications, and backup-job state transitions must each put an aws.backup event onto the default bus) for causes 1-3, and the Step Functions Task AWS-SDK integration dispatch table (it must include backup) for cause 4.

Traces: the five failing check ids in trace_ids.

## Evidence
Cause 1 — Expected: an aws.backup framework-compliance-change envelope on the rule's queue. Actual: AssertionError: no matching event arrived on https://sqs.<region>.amazonaws.com/<account>/<queue> within 90.0s.
Cause 2 — Expected: the vault-notification envelope on the detail-type-scoped queue only. Actual: AssertionError: no matching event arrived on <queue-url> within 120.0s.
Cause 3 — Expected: the COMPLETED Backup Job State Change envelope via SNS->SQS and natively on the SQS target. Actual: AssertionError: COMPLETED Backup Job State Change event delivered via SNS->SQS not met within 120s. Last result: None; and AssertionError: expected the native state-change event for job <job-id> to reach the SQS target.
Cause 4 — Expected: execution status SUCCEEDED. Actual: AssertionError: input-transformed event never produced a successful execution within 120s; sibling executions report error 'States.Runtime', cause "state 'StartBackupJob': 'arn:aws:states:::aws-sdk:backup:startBackupJob' names an integration with 'backup', which is not one of the services this emulator makes a 'Task' state's integration call to, so nothing was run for it".

## Steps to Reproduce
Cause 1: CreateFramework with a control, PutRule {"source":["aws.backup"],"detail-type":[<framework compliance change>]}, PutTargets -> SQS, let the framework evaluate, ReceiveMessage.
Cause 2: CreateBackupVault, PutBackupVaultNotifications for the vault events, PutRule scoped to one detail-type + SQS target, run a job against the vault, ReceiveMessage.
Cause 3: create a backup plan (via CloudFormation and via CLI), PutRule {"source":["aws.backup"],"detail-type":["Backup Job State Change"]}, target an SNS topic subscribed by a queue (and in the second case the queue directly), StartBackupJob and wait for COMPLETED, ReceiveMessage.
Cause 4: PutRule with a target whose InputTransformer supplies the vault name to a state machine whose Task is arn:aws:states:::aws-sdk:backup:startBackupJob, PutEvents the matching event, DescribeExecution.

## Resolution

All five recorded checks pass, and each of the four causes is served by the event-publication paths and the Task AWS-SDK dispatch now in the tree. Two things about this report's prose are worth recording, because they are claims the recording does not support:

- Cause 1 names "a framework compliance change" detail-type. The check it cites asserts `detail-type` `Backup Plan State Change` with `detail.state` `MODIFIED`, which is what a plan's own mutation publishes (`services/service-com-amazonaws-backup-cryocontrollerusermanager/src/state/plan_events.rs`). No framework-compliance detail-type is published, and the recording asks for none: a framework's create and update publish nothing at all.
- Cause 2 names `PutBackupVaultNotifications`. What the check waits for is a detail-type-scoped delivery of the moves AWS Backup publishes for the records in that vault, which arrives without any notification configuration. The three vault-notification operations themselves (`PutBackupVaultNotifications`, `GetBackupVaultNotifications`, `DeleteBackupVaultNotifications`) are STILL unimplemented scaffold stubs answering HTTP 501, tracked by their own report with its own recorded unit: `20261029T175130Z-backup-putbackupvaultnotifications-stub.md` (unit `backup.backup-vault-notifications-lifecycle`). That part of this report is not resolved here.

Pinned by `core/router/tests/all/backup_job_events_are_scoped_by_detail_type.rs` (which detail-type clause admits which kind of record) and `core/router/tests/all/backup_event_rule_target_runs_its_backup_task.rs` (a rule's state-machine target started with the document its input member renders, running the `aws-sdk:backup:startBackupJob` task to a real stored job).
