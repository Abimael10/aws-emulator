Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.rds-export-task-kms-key-missing-permission-negative rds__s3.rds-s3-export-kms-key-mismatch-negative
Issue-Key: rds__s3.rds-export-task-kms-key-missing-permission-negative|_wait_for#18|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.rds-s3-export-kms-key-mismatch-negative|_wait_export_task_terminal#18|Timeout|time.sleep(interval_s)
Issue-Commit: 4f3c099690bf1a1a45af47b8a28528c58ba9b1de

# rds__s3: 2 net-new causes — StartExportTask never refuses an inaccessible KMS key with KMSKeyNotAccessibleFault (by key id or by key ARN) and its positive-control export never reaches COMPLETE/PercentProgress 100

> _Note: this issue was identified against the code base at commit `4f3c099690bf1a1a45af47b8a28528c58ba9b1de`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExportTask

## Description
Two net-new causes, both on RDS snapshot export to S3 with a customer-managed KMS key. Both are grounded in failing checks the replay report lists under `### rds__s3` and marks neither OWNED nor ALREADY FILED, so neither carries an existing issue's replay key.

Related open issues that do NOT carry these two replay keys (named so a fixer does not assume they do): 20261029T210641Z-rds-s3-3-net-new-root-causes-startexporttask.md lists rds__s3.rds-export-task-kms-key-missing-permission-negative in its Issue-Trace-Ids but its Issue-Key for that check no longer matches the replayed key, which is why the suite reports the check as unowned; 20261112T074631Z-rds-s3-the-whole-snapshot-export-to-s3.md and 20261113T183248Z-rds-s3-5-net-new-causes-in-startexporttask.md cover the hanging export path and other StartExportTask refusal messages for other check ids. Fixing those and re-replaying only their ids leaves both checks below still failing.

Cause 1 of 2: StartExportTask with a KmsKeyId passed as a KEY ID whose key policy explicitly denies kms:CreateGrant and kms:DescribeKey to every principal is not refused, and the accessible-key control never completes.
AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model) models StartExportTask with required ExportTaskIdentifier, SourceArn, S3BucketName, IamRoleArn and KmsKeyId, and among its errors KMSKeyNotAccessibleFault (HTTP 400, awsQueryError code `KMSKeyNotAccessibleFault`). An export needs both kms:CreateGrant and kms:DescribeKey on the key, and AWS refuses the call SYNCHRONOUSLY, wording the denial as absence: fixed text "The KMS key <KmsKeyId> doesn't exist or is disabled." with the key reference echoed exactly as the request passed it — here the bare key id, not the ARN. Nothing is written: the request's S3Prefix stays empty. The same snapshot/bucket/role with a key the role IS granted on is accepted, and DescribeExportTasks reaches Status `COMPLETE` with the export's objects present under the prefix.
Emulator: the check reaches no verdict at all — it is killed by the 60 s harness cap before any of those observables is produced, which is the same symptom as the ~90 sibling rds__s3 export checks already filed as the hanging/unimplemented export path.
Why: this is an over-permissive boundary on customer-managed encryption. A client gets no refusal where AWS refuses, so an export is accepted against a key the exporter cannot use; and because the positive control never completes, the terminal-status contract clients poll on (Status COMPLETE, the delivered objects) is unestablished.
Fix: the StartExportTask handler's KMS-key resolution and its grant/describe authorization step (refuse with the modelled fault and the message above, echoing KmsKeyId as passed, before any task record or S3 write), plus the export-task record's terminal transition and its S3 object delivery through the named IAM role.
Traces: rds__s3.rds-export-task-kms-key-missing-permission-negative.

Cause 2 of 2: the same refusal with KmsKeyId passed as a key ARN, and the terminal task's PercentProgress, are likewise never observable.
AWS: the refusal is identical in code (400 `KMSKeyNotAccessibleFault`) and in fixed text, with the key reference echoed as passed — when the caller passes the key ARN, the message names the ARN: "The KMS key <key ARN> doesn't exist or is disabled." No object exists anywhere in the destination bucket after the refusal. The granted-key control then reaches Status `COMPLETE` with PercentProgress exactly 100 (Smithy ExportTask output: Status, PercentProgress, FailureCause, S3Bucket/S3Prefix/IamRoleArn/KmsKeyId echoed) and objects under its prefix.
Emulator: again no verdict — the check is killed by the 60 s cap, so neither the ARN-form refusal message nor PercentProgress 100 at COMPLETE is ever produced.
Why: a client that passes key ARNs (the form every IaC tool emits) cannot distinguish "key not usable" from "API not answering", and a poller keyed on PercentProgress==100 has no terminal signal to key on.
Fix: same component as Cause 1 — the KmsKeyId echo must preserve the caller's reference form, and the terminal ExportTask must carry PercentProgress 100 at COMPLETE.
Traces: rds__s3.rds-s3-export-kms-key-mismatch-negative.

What the timeout does and does not establish: the harness cap is 60 s and each check first stands up a VPC/subnet group, an S3 bucket, an IAM export role, one or two KMS keys, a DB instance and a snapshot before it ever calls StartExportTask, so the cap does NOT localise the stall to the StartExportTask call itself. What is established independently of the timeout is that neither contract above can hold while the snapshot-export path produces no synchronous refusal and no terminal task. Re-replay both ids after the export path is implemented.

## Evidence
Suite simparity-behavioral-aws-v1, service group rds__s3 39/136 passed. Both checks below are unmarked in the replay report (neither OWNED nor ALREADY FILED) and both fail with `Error: Check timed out after 60.0s`.

Cause 1 — rds__s3.rds-export-task-kms-key-missing-permission-negative [mismatch] asserts, verbatim: `error["Code"] == "KMSKeyNotAccessibleFault"`, then `error["Message"] == f"The KMS key {denied_key_id} doesn't exist or is disabled."` (fixed text; the key reference is the key ID the request passed), then `_list_all_keys(s3, bucket, prefix="kms-denied-attempt") == []`, then a positive control on an accessible key reaching `task["Status"] == "COMPLETE"` with objects under its prefix. The denied key is created with a policy that keeps `kms:*` for arn:aws:iam::<account>:root and adds an explicit Deny of ["kms:CreateGrant", "kms:DescribeKey"] for Principal AWS "*".

Cause 2 — rds__s3.rds-s3-export-kms-key-mismatch-negative [mismatch] asserts `error["Code"] == "KMSKeyNotAccessibleFault"`, `error["Message"] == f"The KMS key {denying_key_arn} doesn't exist or is disabled."` (the ARN form, because the request passed the ARN), `_list_objects(s3, bucket, prefix="") == []`, then `ok_task["Status"] == "COMPLETE"`, `ok_task["PercentProgress"] == 100` and objects under the `kms-ok-attempt` prefix. The export role's policy grants kms:CreateGrant/kms:DescribeKey on the granted key only.

Actual, for both: no response is recorded for StartExportTask at all — each check is killed by the harness cap at 60.0 s with no verdict, so neither the HTTP 400 / `KMSKeyNotAccessibleFault` refusal nor the terminal COMPLETE task is observable. Per-run values in the recorded assertions (key ids/ARNs, bucket and task identifiers, account id) fill the message template and are not part of the requirement; the fixed text "The KMS key <reference> doesn't exist or is disabled." and the 400/`KMSKeyNotAccessibleFault` pair are.

## Steps to Reproduce
Replay the behavioral suite and re-run both check ids in trace_ids.

Manually (one DB snapshot S, one bucket B, one export role R trusting export.rds.amazonaws.com and granted s3:PutObject*/GetObject*/ListBucket/DeleteObject*/GetBucketLocation on B):
1. Create KMS key K_denied whose policy allows kms:* to arn:aws:iam::<account>:root and explicitly Denies [kms:CreateGrant, kms:DescribeKey] to Principal AWS "*"; create KMS key K_ok the role is granted kms:CreateGrant/kms:DescribeKey on.
2. rds.start_export_task(ExportTaskIdentifier=T1, SourceArn=<S arn>, S3BucketName=B, IamRoleArn=R, KmsKeyId=<K_denied KEY ID>, S3Prefix='kms-denied-attempt') -> expect HTTP 400 `KMSKeyNotAccessibleFault`, Message "The KMS key <K_denied key id> doesn't exist or is disabled.", and zero objects under s3://B/kms-denied-attempt/.
3. rds.start_export_task(ExportTaskIdentifier=T2, SourceArn=<S arn>, S3BucketName=B, IamRoleArn=R, KmsKeyId=<K_denied ARN>, S3Prefix='kms-denied-attempt') -> expect the same code with the ARN echoed in the message: "The KMS key <K_denied ARN> doesn't exist or is disabled.", and zero objects anywhere in B.
4. rds.start_export_task(ExportTaskIdentifier=T3, SourceArn=<S arn>, S3BucketName=B, IamRoleArn=R, KmsKeyId=<K_ok>, S3Prefix='kms-ok-attempt') -> expect the ExportTask echoed, then describe_export_tasks(ExportTaskIdentifier=T3) reaching Status COMPLETE with PercentProgress 100 and objects under s3://B/kms-ok-attempt/.
Observed: neither refusal nor terminal task appears; a caller polling steps 2-4 never settles.
