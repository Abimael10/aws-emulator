Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.rds-export-manifest-file-format-positive rds__s3.rds-export-manifest-json-format-preserved rds__s3.rds-export-task-creates-s3-objects-positive rds__s3.rds-export-task-parquet-format-schema-preserved rds__s3.rds-export-task-partial-table-list-positive rds__s3.rds-export-task-respects-s3-prefix-positive rds__s3.rds-export-task-role-trusts-export-service-positive rds__s3.rds-start-export-task-writes-objects-to-s3-positive rds__s3.rds-snapshot-export-task-manifest-file-in-s3 rds__s3.iam-role-trust-policy-rds-s3-positive rds__s3.rds-export-task-role-wrong-trust-principal-rejected rds__s3.rds-export-task-missing-iam-role-negative rds__s3.rds-export-task-missing-s3-permission-negative rds__s3.rds-export-task-nonexistent-bucket-rejected rds__s3.rds-export-task-cross-region-bucket-rejected rds__s3.rds-export-task-object-key-collision-negative rds__s3.rds-export-task-s3-bucket-policy-deny-negative rds__s3.cfn-deploy-rds-cluster-s3-export-role-positive
Issue-Key: rds__s3.rds-export-manifest-file-format-positive|run#26|AssertionError|assert isinstance(per_table_status, list) and per_table_status, (
Issue-Key: rds__s3.rds-export-manifest-json-format-preserved|run#13|AssertionError|assert parquet_keys, f"expected parquet objects under base prefix, got: {[o['Key'] for o in objects]}"
Issue-Key: rds__s3.rds-export-task-creates-s3-objects-positive|run#15|AssertionError|assert parquet_objects, f"no .parquet objects among exported keys: {[o['Key'] for o in objects]}"
Issue-Key: rds__s3.rds-export-task-parquet-format-schema-preserved|_poll_until#37|Timeout|return last
Issue-Key: rds__s3.rds-export-task-partial-table-list-positive|_poll_until#22|Timeout|return last
Issue-Key: rds__s3.rds-export-task-respects-s3-prefix-positive|_poll_until#27|Timeout|return last
Issue-Key: rds__s3.rds-export-task-role-trusts-export-service-positive|_poll_until#17|Timeout|return last
Issue-Key: rds__s3.rds-start-export-task-writes-objects-to-s3-positive|_poll_until#18|Timeout|return last
Issue-Key: rds__s3.rds-snapshot-export-task-manifest-file-in-s3|_poll_until#22|Timeout|return last
Issue-Key: rds__s3.iam-role-trust-policy-rds-s3-positive|_wait_for#21|Timeout|return last
Issue-Key: rds__s3.rds-export-task-role-wrong-trust-principal-rejected|_poll_until#25|Timeout|return last
Issue-Key: rds__s3.rds-export-task-missing-iam-role-negative|_poll_until#24|Timeout|return last
Issue-Key: rds__s3.rds-export-task-missing-s3-permission-negative|_poll_until#20|Timeout|return last
Issue-Key: rds__s3.rds-export-task-nonexistent-bucket-rejected|_poll_until#23|Timeout|return last
Issue-Key: rds__s3.rds-export-task-cross-region-bucket-rejected|_poll_until#28|Timeout|return last
Issue-Key: rds__s3.rds-export-task-object-key-collision-negative|_poll_until#20|Timeout|return last
Issue-Key: rds__s3.rds-export-task-s3-bucket-policy-deny-negative|_poll_until#26|Timeout|return last
Issue-Key: rds__s3.cfn-deploy-rds-cluster-s3-export-role-positive|run#28|AssertionError|assert error.get("Message") == (
Issue-Commit: 7f5dd6cdee8fc233932a8e3b3b251e9cc2539379

# rds__s3: 2 net-new causes — StartExportTask reaches COMPLETE writing only its two JSON manifests (no Parquet table data, tablesExported 0, perTableStatus empty), and AddRoleToDBCluster's unsupported-FeatureName refusal carries emulator-authored text

> _Note: this issue was identified against the code base at commit `7f5dd6cdee8fc233932a8e3b3b251e9cc2539379`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExportTask

## Description
Two net-new root causes on the RDS snapshot-export-to-S3 surface, found by replaying the behavioral suite against the emulator built from this checkout. They are independent and each must be closed on its own.

Cause 1 of 2: StartExportTask reports COMPLETE but exports no table data — only the two JSON manifests are written, and the manifest itself says nothing was exported.
AWS: StartExportTask requires ExportTaskIdentifier, SourceArn, S3BucketName, IamRoleArn and KmsKeyId and exports the snapshot's data to the bucket (Smithy: StartExportTask -> output ExportTask {Status, PercentProgress, TotalExtractedDataInGB, ExportOnly, S3Bucket, S3Prefix, FailureCause, WarningMessage, ...}). The object layout and manifest contents are not in the model, so recorded behavior supplies them: under <S3Prefix>/<ExportTaskIdentifier>/ RDS writes (a) export_info_<ExportTaskIdentifier>.json, (b) export_tables_info_<ExportTaskIdentifier>_from_<n>_to_<m>.json, and (c) for every exported table a directory <database>/<schema>.<table>/ holding one or more Apache Parquet part files whose column names and types follow the source table's schema. In export_tables_info_*.json, tablesExported is the number of tables written and perTableStatus is a NON-EMPTY list with one entry per exported table (its target/status/size); an ExportOnly=['db.table'] filter narrows which tables appear, and S3Prefix is the leading path segment of every key. Every variable part — the task identifier, bucket, prefix, table and database names, counts and sizes — comes from the request and the snapshot, never from one recording.
Emulator: the export task reaches Status COMPLETE, PercentProgress is reported, and exactly two objects exist under the prefix: export_info_<id>.json and export_tables_info_<id>_from_1_to_1.json. There is NO .parquet object and no per-table directory at all, and the manifest contradicts the COMPLETE status: tablesExported is 0 and perTableStatus is []. So a COMPLETE export produces no data and a manifest that admits it exported nothing.
Why: the whole point of StartExportTask is the exported data. A client that polls for COMPLETE and then reads the Parquet files (Athena, Glue, Spark, or any downstream loader) finds nothing to read, and a client that reads export_tables_info_*.json to discover what was exported gets an empty list behind a success status — so a silent, total data loss is reported as a successful export. It also makes every negative in this area unmeasurable: those checks assert a refusal and then prove the positive control still exports, so they too spend their whole budget waiting for data objects that never appear.
Fix: the StartExportTask handler and the export worker it drives in the RDS crate (the S3-writing side of the export-task state, services/service-com-amazonaws-rds-amazonrdsv19/src/ops/start_export_task.rs and the export state machine behind it) — materialise the snapshot's tables as Parquet objects under <S3Prefix>/<ExportTaskIdentifier>/<database>/<schema>.<table>/, honour ExportOnly and S3Prefix, and populate tablesExported / perTableStatus / TotalExtractedDataInGB from what was actually written. A task that writes no table data must not report COMPLETE.
Traces (all grounded on this one cause; the three with explicit assertions name it directly, the rest exhaust their budget waiting for the data objects or for the positive control that follows their refusal): rds__s3.rds-export-manifest-file-format-positive, rds__s3.rds-export-manifest-json-format-preserved, rds__s3.rds-export-task-creates-s3-objects-positive, rds__s3.rds-export-task-parquet-format-schema-preserved, rds__s3.rds-export-task-partial-table-list-positive, rds__s3.rds-export-task-respects-s3-prefix-positive, rds__s3.rds-export-task-role-trusts-export-service-positive, rds__s3.rds-start-export-task-writes-objects-to-s3-positive, rds__s3.rds-snapshot-export-task-manifest-file-in-s3, rds__s3.iam-role-trust-policy-rds-s3-positive, rds__s3.rds-export-task-role-wrong-trust-principal-rejected, rds__s3.rds-export-task-missing-iam-role-negative, rds__s3.rds-export-task-missing-s3-permission-negative, rds__s3.rds-export-task-nonexistent-bucket-rejected, rds__s3.rds-export-task-cross-region-bucket-rejected, rds__s3.rds-export-task-object-key-collision-negative, rds__s3.rds-export-task-s3-bucket-policy-deny-negative.

Cause 2 of 2: AddRoleToDBCluster's refusal of an unsupported FeatureName carries emulator-authored text.
AWS: AddRoleToDBCluster takes DBClusterIdentifier, RoleArn and FeatureName (Smithy: AddRoleToDBCluster -> input FeatureName String, RoleArn IAMRoleArn pattern ^arn:aws[a-z-]*:iam::[0-9]*:role/.*$; modeled errors DBClusterNotFoundFault / DBClusterRoleAlreadyExistsFault / DBClusterRoleQuotaExceededFault / InvalidDBClusterStateFault). A FeatureName the cluster's engine does not report in DescribeDBEngineVersions SupportedFeatureNames — 's3Export' on Aurora MySQL — is refused with code InvalidParameterValue and the message 'IAM role ARN value is invalid or does not include the required permissions for: AWS_ROLE_INTEGRATION'. That whole sentence is fixed text; nothing in it varies with the request. The cluster's AssociatedRoles must stay empty afterwards.
Emulator: the Type/Sender classification and the InvalidParameterValue code are right, but the Message is 'The feature name s3Export is not valid for the engine Aurora MySQL.' — an emulator-authored sentence that shares no fixed text with AWS's.
Why: a client or deploy tool that recognises AWS's AWS_ROLE_INTEGRATION sentence (to tell 'this feature is not available for this engine' from a genuinely malformed role ARN) never matches, so a well-understood rejection is surfaced as an unknown failure. Because the code matches, the divergence is silent.
Fix: the AddRoleToDBCluster handler's FeatureName/engine-capability gate, where the InvalidParameterValue error is constructed — emit AWS's fixed AWS_ROLE_INTEGRATION sentence instead of the handler's own prose.
Traces: rds__s3.cfn-deploy-rds-cluster-s3-export-role-positive.

## Evidence
Behavioral suite replayed against the emulator built from this checkout: 417/676 checks passed (61.7%); rds__s3 scored 76/136 (56%) with 60 failures. The failures below are the ones the replay reported neither ALREADY FILED (blocked by an unimplemented stub) nor OWNED by an open issue.

Cause 1 — rds__s3.rds-export-manifest-file-format-positive (export task reached COMPLETE):
  AssertionError: export_tables_info.json has no non-empty perTableStatus entry: {'exportTaskIdentifier': '<task id>', 'sourceArn': 'arn:aws:rds:<region>:<account>:snapshot:<snapshot id>', 'sourceType': 'SNAPSHOT', 's3Bucket': '<bucket>', 's3Prefix': '', 'status': 'COMPLETE', 'tablesExported': 0, 'perTableStatus': []}
  Expected: tablesExported equal to the number of tables written and perTableStatus a non-empty list with one entry per exported table.
Cause 1 — rds__s3.rds-export-task-creates-s3-objects-positive:
  AssertionError: no .parquet objects among exported keys: ['<prefix>/<task id>/export_info_<task id>.json', '<prefix>/<task id>/export_tables_info_<task id>_from_1_to_1.json']
Cause 1 — rds__s3.rds-export-manifest-json-format-preserved:
  AssertionError: expected parquet objects under base prefix, got: ['<task id>/export_info_<task id>.json', '<task id>/export_tables_info_<task id>_from_1_to_1.json']
  So exactly the two manifests are written and no table data at all.
Cause 1 — the remaining 14 rds__s3 checks listed in trace_ids each exhaust the per-check budget ('Check timed out after 60.0s') while waiting either for the Parquet objects of a positive export or for the positive control that each negative runs after its refusal (their docstrings: '... while the same role and snapshot succeed against a real bucket / exports the same snapshot to COMPLETE with objects written'). No other diagnosis appears in their replay output.

Cause 2 — rds__s3.cfn-deploy-rds-cluster-s3-export-role-positive: the export reached COMPLETE and KeyCount>0 (the two manifests), and DescribeDBEngineVersions reported no Aurora MySQL version advertising s3Export, so the refusal is the contract. AddRoleToDBCluster(DBClusterIdentifier=<cluster>, RoleArn=<role>, FeatureName='s3Export') answered:
  got:      {'Type': 'Sender', 'Code': 'InvalidParameterValue', 'Message': 'The feature name s3Export is not valid for the engine Aurora MySQL.'}
  expected: Code 'InvalidParameterValue', Message 'IAM role ARN value is invalid or does not include the required permissions for: AWS_ROLE_INTEGRATION'
  Message fixed text: the entire sentence; no part of it varies with the request.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1).
3. Cause 1: create an encrypted MySQL DB instance, create a table with rows over the native wire, snapshot it, create a role trusted by export.rds.amazonaws.com with full S3 access to a bucket, then StartExportTask(SourceArn=<snapshot arn>, S3BucketName=<bucket>, IamRoleArn=<role>, KmsKeyId=<key>, S3Prefix=<prefix>). Poll DescribeExportTasks to COMPLETE, then ListObjectsV2 on the bucket under the prefix: only export_info_<id>.json and export_tables_info_<id>_from_1_to_1.json exist, no .parquet object, and export_tables_info_*.json reports tablesExported 0 / perTableStatus []. Checks: rds__s3.rds-export-task-creates-s3-objects-positive, rds__s3.rds-export-manifest-file-format-positive, rds__s3.rds-export-manifest-json-format-preserved.
4. Cause 1 (the rest): re-run the other rds__s3 checks in trace_ids; each waits for those data objects (or for the positive export that follows its refusal) until the per-check budget expires.
5. Cause 2: run rds__s3.cfn-deploy-rds-cluster-s3-export-role-positive — deploy the cluster+role+bucket stack, confirm DescribeDBEngineVersions(Engine='aurora-mysql', IncludeAll=True) lists no version with s3Export in SupportedFeatureNames, then call AddRoleToDBCluster with FeatureName='s3Export'. The code InvalidParameterValue is right; the Message is 'The feature name s3Export is not valid for the engine Aurora MySQL.' instead of 'IAM role ARN value is invalid or does not include the required permissions for: AWS_ROLE_INTEGRATION'. DescribeDBClusters AssociatedRoles correctly stays empty.
