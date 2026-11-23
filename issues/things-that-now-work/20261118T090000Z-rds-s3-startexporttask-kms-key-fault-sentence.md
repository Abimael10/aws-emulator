Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.cli-rds-export-task-missing-role-negative
Issue-Key: rds__s3.cli-rds-export-task-missing-role-negative|run#16|AssertionError|assert code == "KMSKeyNotAccessibleFault", (
Issue-Key: rds__s3.cli-rds-export-task-missing-role-negative|run#18|AssertionError|assert (code, message) == ("KMSKeyNotAccessibleFault", expected), (
Issue-Related: 20261029T195009Z-rds-s3-5-net-new-root-causes-startexporttask.md
Issue-Commit: f5ff83ec94a8a18f8381cb1ddf82d934ca39bd35

# rds__s3: StartExportTask answers the right KMSKeyNotAccessibleFault CODE but the wrong SENTENCE for the condition the CLI export check hits

> _Note: this issue is filed by the fixer of `20261029T195009Z-rds-s3-5-net-new-root-causes-startexporttask.md`, which closed that bundle's StartExportTask 501 and its bucket/role refusal classes. It carries the ONE replay key of that bundle that is not closed: the wording of the key fault on the export path for the condition `rds__s3.cli-rds-export-task-missing-role-negative` sets up._

## Operation
StartExportTask

## Description
`StartExportTask` is implemented and this check now gets the modelled error CODE it expects — `KMSKeyNotAccessibleFault`, 400, `Sender` — so the check's first assertion passes. Its second assertion compares the MESSAGE and fails: the check expects the generic sentence

```
The specified KMS key [<the KmsKeyId the request carried>] does not exist, is not enabled or you do not have permissions to access it.
```

and the emulator answers the other sentence this fault carries on the export path, `The KMS key <KmsKeyId> doesn't exist or is disabled.`
(`services/service-com-amazonaws-rds-amazonrdsv19/src/state/export_tasks.rs`, `export_kms_key_not_accessible_message`), which the handler uses for a condition of the KEY ITSELF — a key KMS holds nothing under, one it holds disabled, and one whose own policy withholds the `kms:CreateGrant` / `kms:DescribeKey` the export performs on it (`services/service-com-amazonaws-rds-amazonrdsv19/src/state/export_access.rs`, `kms_key_is_inaccessible` and `export_key_withholds_a_required_action`).

AWS: `KmsKeyId` states that "the caller of this operation must be authorized to run the following operations ... kms:CreateGrant kms:DescribeKey", and Amazon RDS reports every condition that leaves the export's data key unusable as the key's own inaccessibility. Recorded behavior decides WHICH of the two sentences each condition carries, and for the condition this check sets up it is the generic one above — the sentence `export_key_grant_not_created_message` already builds for a role the export's grant cannot be created for. So one of the conditions the handler currently answers with the key-absence sentence is a condition AWS answers with the generic sentence. The whole fixed text of the sentence is a requirement; its one variable part is the `KmsKeyId` the request carried, echoed exactly as the request spelled it (key id, key ARN, alias name or alias ARN).

Which condition it is cannot be read off the replay report: the report quotes the sentence the check EXPECTED and withholds the one the emulator answered, and the three export-path conditions that share the key fault are indistinguishable from the report. The sibling checks that pass pin the other wordings — `rds__s3.rds-s3-export-kms-key-mismatch-negative` (a key whose policy denies grant and describe) and `rds__s3.cfn-deploy-rds-missing-s3-export-role-negative` (a role with no S3 policy) — so whatever is re-worded here must leave both of those as they are.

## Evidence
From the behavioral replay report (suite simparity-behavioral-aws-v1), service group rds__s3:

- rds__s3.cli-rds-export-task-missing-role-negative [mismatch]: `AssertionError: expected KMSKeyNotAccessibleFault with message 'The specified KMS key [arn:aws:kms:us-west-2:000000000000:key/<a key the check created in this run>] does not exist, is not enabled or you do not have permissions to access it.', got 'KMSKeyNotAccessibleFault' (stderr='aws: [ERROR]: An error occurred (KMSKeyNotAccessibleFault) when calling the StartExportTask operation ...')` — the code matches and the message does not.

The emulator's own suites pin both sentences today, so a fix has to move the one condition that is wrong rather than re-word the function:

- `services/service-com-amazonaws-rds-amazonrdsv19/tests/all/export_role_grant_refusals.rs` pins the generic sentence for every `IamRoleArn` this account's IAM resolves no role for.
- `services/service-com-amazonaws-rds-amazonrdsv19/tests/all/cluster_role_and_export_key_refusals.rs` pins `The KMS key <id> doesn't exist or is disabled.` for a key KMS holds nothing under and for one it holds disabled.

## Steps to Reproduce
Build and boot the emulator, then replay `rds__s3.cli-rds-export-task-missing-role-negative`.

Manually:
1. Create an S3 bucket, a KMS key and a DB snapshot, then run `aws rds start-export-task --export-task-identifier ... --source-arn <snapshot arn> --s3-bucket-name ... --iam-role-arn <the role the check names> --kms-key-id <the key's ARN>`.
2. Vary what is wrong with the request one condition at a time — the role absent, the role present but not assumable by `export.rds.amazonaws.com`, the role present with no S3 policy, the key absent, the key disabled, the key's policy withholding `kms:CreateGrant`/`kms:DescribeKey` — and record the `(Code, Message)` pair each one answers.
3. The condition the check sets up must answer `KMSKeyNotAccessibleFault` with `The specified KMS key [<KmsKeyId>] does not exist, is not enabled or you do not have permissions to access it.`, with the key reference echoed as the request spelled it; the conditions the two passing sibling checks cover must keep the sentences they answer today.
