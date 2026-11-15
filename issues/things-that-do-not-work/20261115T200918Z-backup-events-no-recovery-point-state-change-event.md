Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup__events.backup-continuous-backup-event-delivered-to-sns-via-eventbridge
Issue-Key: backup__events.backup-continuous-backup-event-delivered-to-sns-via-eventbridge|_poll_sqs_for_notification#18|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=10)
Issue-Commit: 12324ebe9a30c68b02ff3d648ddb0d8f8dd552a1

# backup__events: no 'Recovery Point State Change' event is published for a completed backup job, so an EventBridge rule's SNS target never notifies its SQS subscriber

> _Note: this issue was identified against the code base at commit `12324ebe9a30c68b02ff3d648ddb0d8f8dd552a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartBackupJob

## Description
Cause 1 of 1: a backup job that runs to COMPLETED against a continuously-backed-up DynamoDB table produces no observable 'Recovery Point State Change' delivery, so an ENABLED rule matching {"source":["aws.backup"],"detail-type":["Recovery Point State Change"]} with an SNS-topic target never publishes, the topic's SQS subscription receives nothing, and the check exhausts its budget before reaching any of its envelope assertions. Failing check: backup__events.backup-continuous-backup-event-delivered-to-sns-via-eventbridge.

AWS: recorded behavior only for the event envelope — the EventBridge envelope is not in either service's Smithy model. Smithy (backup) does fix the surrounding contract: StartBackupJob is PUT /backup-jobs -> 200 with BackupJobId + RecoveryPointArn, requires BackupVaultName/ResourceArn/IamRoleArn and models only InvalidParameterValueException / InvalidRequestException / LimitExceededException / MissingParameterValueException / ResourceNotFoundException (400) and ServiceUnavailableException (500); DescribeRecoveryPoint.Status is the enum RecoveryPointStatus with values COMPLETED | PARTIAL | DELETING | EXPIRED | AVAILABLE | STOPPED | CREATING, and the recovery point is listable by ListRecoveryPointsByBackupVault. On AWS, when the job's recovery point reaches its terminal status, Backup publishes one envelope on the EventBridge default bus with source="aws.backup", detail-type="Recovery Point State Change" (both fixed text) and a detail carrying resourceArn=<the ARN of the backed-up resource, from the request>, status="COMPLETED" (the fixed RecoveryPointStatus token for a finished point; "PARTIAL" for a partial one) and backupVaultArn=<the vault ARN, from state>. A rule matching that source/detail-type with an SNS topic target, where the topic policy allows events.amazonaws.com SNS:Publish on the topic with Condition ArnEquals aws:SourceArn=<rule ARN>, publishes that envelope to the topic; the topic's sqs-protocol subscription then delivers an SNS notification whose TopicArn is the topic ARN and whose Message is the JSON event envelope verbatim.

Emulator: the replay reports only `Error: Check timed out after 60.0s`. The check's terminal read polls the subscribed SQS queue and returns as soon as ONE notification arrives whose inner event has detail.resourceArn == the table ARN and detail.status in (COMPLETED, PARTIAL), so the budget can only be exhausted because no such notification is ever observed; every producer-side step before it (CreateTable -> ACTIVE, UpdateContinuousBackups, StartBackupJob, DescribeBackupJob to COMPLETED, ListRecoveryPointsByBackupVault containing the point, PutRule/PutTargets, SetTopicAttributes, Subscribe) is a bounded read that returns immediately when the emulator answers. Hypothesis: either Backup publishes no 'Recovery Point State Change' envelope at all for a completed job, or it publishes one that the EventBridge rule-match/SNS-publish/SNS-to-SQS leg does not carry to the subscriber; the determinable fact from the replay is that nothing matching that filter is ever delivered. This is consistent with the already-filed neighbouring emission gaps, which cover OTHER detail-types only and do not carry this detail-type or this check's key: 20261107T205257Z-backup-events-4-net-new-causes-backup-emits.md (framework-compliance, vault-notification, backup-job state change), 20261111T065053Z-backup-events-the-failed-backup-job-state-change.md (the FAILED 'Backup Job State Change'), 20261110T114929Z-backup-events-no-restore-job-state-change-event.md ('Restore Job State Change') and 20261105T081748Z-backup-events-no-backup-plan-state-change-event.md ('Backup Plan State Change').

Why: a client that reacts to completed recovery points — lifecycle automation, cross-region copy orchestration, compliance alerting, or any SNS fan-out subscribed to Backup's recovery-point stream — observes nothing and cannot distinguish "no backup finished" from "not delivered". Any consumer branch keyed on detail-type="Recovery Point State Change" or on detail.status is dead code against this emulator, and a negative test on that stream passes vacuously.

Fix: the Backup recovery-point lifecycle's state-change emission path (a recovery point reaching its terminal status must publish an aws.backup "Recovery Point State Change" envelope with detail.resourceArn / detail.status / detail.backupVaultArn on the EventBridge default bus), plus the EventBridge rule-match and SNS-target publish leg that must carry a matched envelope to the topic and on to its sqs subscription.

Traces: backup__events.backup-continuous-backup-event-delivered-to-sns-via-eventbridge.

## Evidence
Expected (AWS): after StartBackupJob settles COMPLETED for a PITR-enabled DynamoDB table, the rule scoped to {"source":["aws.backup"],"detail-type":["Recovery Point State Change"]} publishes to the SNS topic, and the subscribed queue yields a notification with:
  envelope.TopicArn == <the topic ARN>
  json.loads(envelope.Message).detail.resourceArn == <the backed-up table ARN>
  json.loads(envelope.Message).detail.status == "COMPLETED"
  json.loads(envelope.Message).detail.backupVaultArn == <the vault ARN>

Actual ([mismatch] backup__events.backup-continuous-backup-event-delivered-to-sns-via-eventbridge):
  Error: Check timed out after 60.0s
No notification matching detail.resourceArn == <table ARN> and detail.status in (COMPLETED, PARTIAL) is ever received, so none of the four assertions above is reached. The poll returns on the first match, so the timeout is the absence of the delivery, not the cost of extra matches.

Table/vault/topic/queue names, ARNs, job and recovery-point ids and the harness budget are per-run values; the requirement is the envelope (fixed source "aws.backup", fixed detail-type "Recovery Point State Change", fixed status token from RecoveryPointStatus) and its delivery through the rule's SNS target.

## Steps to Reproduce
1. dynamodb.CreateTable (PAY_PER_REQUEST, single HASH key) and wait for TableStatus=ACTIVE; dynamodb.UpdateContinuousBackups(PointInTimeRecoverySpecification={PointInTimeRecoveryEnabled: true}) (retrying ContinuousBackupsUnavailableException during the warm-up window).
2. backup.CreateBackupVault -> note BackupVaultArn.
3. iam.CreateRole trusting backup.amazonaws.com for sts:AssumeRole and attach AWSBackupServiceRolePolicyForBackup + AWSBackupServiceRolePolicyForRestores.
4. sns.CreateTopic; events.PutRule(State=ENABLED, EventPattern={"source":["aws.backup"],"detail-type":["Recovery Point State Change"]}); sns.SetTopicAttributes(Policy=...) allowing events.amazonaws.com SNS:Publish on the topic with Condition ArnEquals aws:SourceArn=<rule ARN>; events.PutTargets with the topic ARN.
5. sqs.CreateQueue, set a queue policy allowing sns.amazonaws.com sqs:SendMessage with Condition ArnEquals aws:SourceArn=<topic ARN>, and sns.Subscribe(Protocol="sqs", Endpoint=<queue ARN>).
6. backup.StartBackupJob(BackupVaultName=<vault>, ResourceArn=<table ARN>, IamRoleArn=<role ARN>) and poll backup.DescribeBackupJob until State=COMPLETED; backup.ListRecoveryPointsByBackupVault shows the returned RecoveryPointArn.
7. sqs.ReceiveMessage on the subscribed queue in a loop -> AWS yields an SNS notification whose Message is the aws.backup "Recovery Point State Change" envelope for that resource with detail.status="COMPLETED"; the emulator yields nothing and the check times out.
8. Replay backup__events.backup-continuous-backup-event-delivered-to-sns-via-eventbridge.
