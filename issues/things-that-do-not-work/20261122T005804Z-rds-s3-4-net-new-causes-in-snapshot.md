Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.cli-rds-describe-export-tasks-status-poll rds__s3.cli-rds-describe-export-tasks-status-progression rds__s3.cli-rds-start-export-task-workflow rds__s3.rds-export-manifest-format-preserved rds__s3.rds-snapshot-export-to-s3-positive rds__s3.rds-export-task-parquet-format-schema-preserved rds__s3.cli-rds-create-snapshot-export-to-s3-workflow rds__s3.cli-rds-export-invalid-bucket-name-negative rds__s3.cli-rds-export-missing-permission-negative rds__s3.rds-snapshot-export-missing-iam-role-negative rds__s3.rds-snapshot-export-missing-s3-permission-negative rds__s3.rds-snapshot-export-nonexistent-bucket-negative rds__s3.rds-snapshot-export-task-creates-s3-objects rds__s3.rds-snapshot-export-task-respects-s3-prefix
Issue-Key: rds__s3.cli-rds-describe-export-tasks-status-poll|run#13|AssertionError|assert initial_status != "COMPLETE", (
Issue-Key: rds__s3.cli-rds-describe-export-tasks-status-progression|run#25|AssertionError|assert pre_complete, "only a single observation was captured; no progression was observed"
Issue-Key: rds__s3.cli-rds-start-export-task-workflow|run#18|AssertionError|assert any(k.endswith(".parquet") for k in keys), f"no parquet objects found: {keys[:10]}"
Issue-Key: rds__s3.rds-export-manifest-format-preserved|run#15|AssertionError|assert matching_base, (
Issue-Key: rds__s3.rds-snapshot-export-to-s3-positive|run#14|AssertionError|assert parquet_keys, f"no .parquet objects found among exported keys: {keys[:10]}"
Issue-Key: rds__s3.rds-export-task-parquet-format-schema-preserved|run#37|AssertionError|assert "timestamp" in created_at_type, f"created_at column exported as {created_at_type!r}, expected a timestamp type"
Issue-Key: rds__s3.cli-rds-create-snapshot-export-to-s3-workflow|_poll_until#19|Timeout|return last
Issue-Key: rds__s3.cli-rds-export-invalid-bucket-name-negative|_poll_until#24|Timeout|return last
Issue-Key: rds__s3.cli-rds-export-missing-permission-negative|_poll_until#24|Timeout|return last
Issue-Key: rds__s3.rds-snapshot-export-missing-iam-role-negative|_poll_until#21|Timeout|return last
Issue-Key: rds__s3.rds-snapshot-export-missing-s3-permission-negative|_poll_until#24|Timeout|return last
Issue-Key: rds__s3.rds-snapshot-export-nonexistent-bucket-negative|_poll_until#19|Timeout|return last
Issue-Key: rds__s3.rds-snapshot-export-task-creates-s3-objects|_poll_until#13|Timeout|return last
Issue-Key: rds__s3.rds-snapshot-export-task-respects-s3-prefix|_poll_until#18|Timeout|return last
Issue-Commit: 05866a96998b37f437bd411ae9396664f070e9d9

# rds__s3: 4 net-new causes in snapshot export to S3 — StartExportTask settles COMPLETE synchronously, a COMPLETE export writes only its two JSON manifests, a DATETIME column exports as Parquet string, and eight export checks never reach a graded assertion

> _Note: this issue was identified against the code base at commit `05866a96998b37f437bd411ae9396664f070e9d9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExportTask

## Description
Four independent net-new root causes on the RDS snapshot-export-to-S3 surface, found by replaying the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout. Each must be closed on its own; none of the 14 failing checks below was reported ALREADY FILED or OWNED by an open issue.

Related OPEN issues a fixer should read first — they describe neighbouring StartExportTask divergences but the replay did NOT mark any of these checks as owned by them, so they do not cover the causes below: 20261117T083457Z-rds-s3-2-net-new-causes-startexporttask-reaches.md (export writes only the two manifests, tablesExported 0), 20261119T064500Z-rds-s3-the-export-writes-no-parquet-and-the-cfn-role-failure-reason.md, 20261120T150929Z-rds-s3-the-snapshot-export-lifecycle-never-settles.md, 20261119T132710Z-rds-s3-the-aurora-seeded-snapshot-export-manifest.md, 20261113T183248Z-rds-s3-5-net-new-causes-in-startexporttask.md, 20261118T090000Z-rds-s3-startexporttask-kms-key-fault-sentence.md. Cause 3 in particular is NEW progress on a check those issues only ever saw time out: Parquet objects are now produced for it, and the divergence has moved to the column types.

Cause 1 of 4: StartExportTask settles the task at COMPLETE synchronously — DescribeExportTasks already reports COMPLETE on the first read after the start call, so the task never passes through a non-terminal status.
AWS: Smithy (service-com-amazonaws-rds-amazonrdsv19) models StartExportTask -> ExportTask{Status, PercentProgress, TaskStartTime, TaskEndTime, TotalExtractedDataInGB, ...} and documents ExportTask.Status as one of CANCELED, CANCELING, COMPLETE, FAILED, IN_PROGRESS, STARTING. An export is a long-running extraction: the task is created in STARTING, advances to IN_PROGRESS while data is extracted, and only reaches COMPLETE once the exported objects exist in the destination bucket. A client therefore observes at least one non-terminal Status, and PercentProgress advances, before COMPLETE. Which non-terminal value is observed and how long it lasts depend on the snapshot; the requirement is that COMPLETE is not the first status an immediately-following DescribeExportTasks reports and that the data objects are already present the instant COMPLETE is reported.
Emulator: the first DescribeExportTasks issued straight after start-export-task answers Status 'COMPLETE', and repeated polls return that same single observation, so no progression exists at all.
Why: a client that polls DescribeExportTasks for progress (the documented pattern, and what the AWS CLI/console do) can never distinguish a running export from a finished one, cannot show progress, and — because COMPLETE here is reported with no data written (Cause 2) — treats a no-op as a finished export. Code paths that only run while a task is non-terminal (CancelExportTask, progress reporting, concurrency guards) become unreachable.
Fix: the export-task state machine behind StartExportTask/DescribeExportTasks in the RDS crate — create the task non-terminal (STARTING), advance it through IN_PROGRESS as the extraction runs, and only stamp COMPLETE/TaskEndTime once every exported object has been written.
Traces: rds__s3.cli-rds-describe-export-tasks-status-poll, rds__s3.cli-rds-describe-export-tasks-status-progression.

Cause 2 of 4: a COMPLETE export writes ONLY its two JSON manifests — no Parquet part object and no per-table directory exists under the task prefix.
AWS: the object layout is not in the Smithy model, so this is recorded behavior: under <S3Prefix>/<ExportTaskIdentifier>/ RDS writes export_info_<ExportTaskIdentifier>.json, export_tables_info_<ExportTaskIdentifier>_from_<n>_to_<m>.json, and for every exported table a directory <database>/<schema>.<table>/ holding one or more Apache Parquet part files (keys ending .parquet). A snapshot of a freshly created MySQL instance that holds no user table still exports the engine's built-in schemas, so keys appear under <task>/mysql/mysql.<table>/ (the recording shows the base prefix <task>/mysql/mysql.help_topic/). The task identifier, bucket, prefix, database/schema/table names and part counts all come from the request and the snapshot; only the shape of the layout is the requirement.
Emulator: exactly two keys exist under the prefix — export_info_<task id>.json and export_tables_info_<task id>_from_1_to_1.json — and nothing else, while Status is COMPLETE. No key ends in .parquet and no <database>/<schema>.<table>/ directory is created.
Why: silent, total data loss behind a success status. Every downstream consumer of a snapshot export (Athena, Glue crawlers, Spark, any loader that lists the task prefix for Parquet parts) finds nothing to read, and a caller that keys off Status=COMPLETE has no signal that the export produced no data.
Fix: the StartExportTask handler and the export worker it drives in the RDS crate (the S3-writing side of the export-task state) — materialise every exported table as Parquet objects under <S3Prefix>/<ExportTaskIdentifier>/<database>/<schema>.<table>/, honour S3Prefix and ExportOnly, and do not report COMPLETE for a task that wrote no table data.
Traces: rds__s3.cli-rds-start-export-task-workflow, rds__s3.rds-export-manifest-format-preserved, rds__s3.rds-snapshot-export-to-s3-positive.

Cause 3 of 4: when Parquet IS written, the column types do not preserve the source column types — a MySQL DATETIME column is exported as Parquet 'string'.
AWS: not modeled in Smithy; recorded behavior and AWS's documented snapshot-export data-type mapping. The exported Parquet schema carries one field per source column with the source names (id, name, score, created_at for the check's table) and types mapped from the source column type — a MySQL DATETIME maps to a Parquet timestamp (annotated INT64 timestamp), not to a byte-array string. The column names and the source types come from the source table's DDL; the requirement is the mapping, not one table's values.
Emulator: the Parquet footer of the exported part reports created_at with type 'string' where the source column is DATETIME, so the assertion that the exported type contains 'timestamp' fails.
Why: a consumer that reads the exported Parquet with a typed reader (Athena/Glue/Spark/pyarrow) gets a string where it declared a timestamp, so time predicates, partition pruning and ordering on the exported column are wrong or fail to bind; the schema of the export silently differs from the schema of the database it was taken from.
Fix: the type-mapping layer of the export writer in the RDS crate (where source column types are translated into the Parquet schema for the objects StartExportTask writes) — map DATETIME/TIMESTAMP columns onto a Parquet timestamp type and keep the rest of the documented mapping, instead of falling back to string.
Traces: rds__s3.rds-export-task-parquet-format-schema-preserved.

Cause 4 of 4: the snapshot-export lifecycle does not settle inside the replay budget — eight export checks are cut off at the per-check cap with NO graded assertion, so neither their refusals nor their positive controls are observable.
AWS: recorded behavior — DescribeDBInstances reports DBInstanceStatus 'available' and DescribeDBSnapshots reports snapshot Status 'available' promptly after creation, and the modeled StartExportTask refusals (InvalidS3BucketFault, IamRoleNotFoundFault, IamRoleMissingPermissionsFault, KMSKeyNotAccessibleFault — all modeled on StartExportTask with httpError 400/404) are answered on the start call itself. Each of these checks does its provisioning (VPC/subnets/subnet group, DB instance, snapshot, KMS key, export role), then either asserts one of those refusals or exports and lists objects, polling each resource with its own generous deadline (900-2400 s).
Emulator: every one of the eight ends with 'Check timed out after 60.0s' and no assertion is ever evaluated; the run is spent inside the checks' own terminal-state polls. Hypothesis: one of the resources these checks wait on (the DB instance's DBInstanceStatus, or the snapshot's Status) does not reach 'available' before the checks' 20-30 s poll intervals exhaust the budget, so the export branch is never entered.
Why: the whole snapshot-export surface is ungradeable for these scenarios — a caller following the documented provision-snapshot-export sequence hangs in a poll instead of getting either an export or one of the modeled refusals, and the negative paths (bad bucket name, missing permission, missing role, nonexistent bucket, prefix handling) cannot be exercised at all.
Fix: the RDS resource state machines these reads serve — the DB-instance status progression behind DescribeDBInstances and the snapshot status behind DescribeDBSnapshots must reach 'available' promptly, and StartExportTask's input validation must answer its modeled refusals on the start call rather than after any wait.
Traces: rds__s3.cli-rds-create-snapshot-export-to-s3-workflow, rds__s3.cli-rds-export-invalid-bucket-name-negative, rds__s3.cli-rds-export-missing-permission-negative, rds__s3.rds-snapshot-export-missing-iam-role-negative, rds__s3.rds-snapshot-export-missing-s3-permission-negative, rds__s3.rds-snapshot-export-nonexistent-bucket-negative, rds__s3.rds-snapshot-export-task-creates-s3-objects, rds__s3.rds-snapshot-export-task-respects-s3-prefix.

## Evidence
Behavioral suite replayed against the emulator built from this checkout: 2224/2681 checks passed (83.0%); 372 mismatched, 85 errored. The 14 rds__s3 failures below are the ones the replay reported neither ALREADY FILED (blocked by an unimplemented stub) nor OWNED by an open issue.

Cause 1 — rds__s3.cli-rds-describe-export-tasks-status-poll (check line 44):
  AssertionError: export task <export task identifier> reported COMPLETE immediately after start-export-task
  Expected: the DescribeExportTasks issued immediately after StartExportTask reports a non-terminal Status (STARTING or IN_PROGRESS per the modeled Status documentation), not COMPLETE. The identifier in the message is the per-run task id; the requirement is the status, not the id.
Cause 1 — rds__s3.cli-rds-describe-export-tasks-status-progression (check line 84):
  AssertionError: only a single observation was captured; no progression was observed
  Expected: at least one pre-COMPLETE observation of Status across the CLI poll loop.

Cause 2 — rds__s3.cli-rds-start-export-task-workflow (check line 74):
  AssertionError: no parquet objects found: ['<task id>/export_info_<task id>.json', '<task id>/export_tables_info_<task id>_from_1_to_1.json']
Cause 2 — rds__s3.rds-snapshot-export-to-s3-positive (check line 43):
  AssertionError: no .parquet objects found among exported keys: ['<task id>/export_info_<task id>.json', '<task id>/export_tables_info_<task id>_from_1_to_1.json']
Cause 2 — rds__s3.rds-export-manifest-format-preserved (check line 56):
  AssertionError: expected object keys under documented base prefix '<task id>/mysql/mysql.help_topic/', got: ['<task id>/export_info_<task id>.json', '<task id>/export_tables_info_<task id>_from_1_to_1.json']
  Expected in all three: in addition to the two manifests, at least one key under <S3Prefix>/<task id>/<database>/<schema>.<table>/ ending in .parquet. The task ids and bucket names above are per-run values.

Cause 3 — rds__s3.rds-export-task-parquet-format-schema-preserved (check line 91). The check creates sim2real_export.widgets(id INT, name VARCHAR(50), score DECIMAL(6,2), created_at DATETIME) over the native MySQL wire, snapshots it, exports with ExportOnly=['sim2real_export.widgets'] and S3Prefix, reaches Status COMPLETE, finds the widgets .parquet object and reads its footer:
  AssertionError: created_at column exported as 'string', expected a timestamp type
  Expected: the Parquet field for a DATETIME source column carries a timestamp type; the column set {id, name, score, created_at} is the source table's, not a recorded constant.

Cause 4 — the eight checks listed in that cause each end with:
  Error: Check timed out after 60.0s
with no assertion evaluated. Their blocking waits are the checks' own terminal-state polls, e.g. rds-snapshot-export-task-creates-s3-objects: _poll_until(DescribeExportTasks Status in (COMPLETE, FAILED, CANCELED), timeout_s=2400, interval_s=30) after _create_source_db_instance's _poll_until(DBInstanceStatus == 'available', timeout_s=1800, interval_s=30) and the snapshot 'available' poll; cli-rds-create-snapshot-export-to-s3-workflow: the same chain with snapshot interval 20 s. Instance/snapshot/task identifiers, bucket names and host/port are per-run values; the requirement is that each of those statuses reaches its terminal value promptly and that the refusal branches answer on the start call.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1) and re-run the 14 rds__s3 check ids in trace_ids.
3. Cause 1: create a DB instance, snapshot it, create a KMS key and a role trusted by export.rds.amazonaws.com with S3 access to a bucket, call StartExportTask, then IMMEDIATELY call DescribeExportTasks(ExportTaskIdentifier=<id>) — the emulator answers Status 'COMPLETE' on that first read, and every later poll returns the same single observation. AWS reports STARTING/IN_PROGRESS first. Checks: cli-rds-describe-export-tasks-status-poll, cli-rds-describe-export-tasks-status-progression.
4. Cause 2: after that export reports COMPLETE, ListObjectsV2(Bucket=<bucket>, Prefix=<task id>/) — only export_info_<task id>.json and export_tables_info_<task id>_from_1_to_1.json exist; no key ends in .parquet and no <database>/<schema>.<table>/ directory exists (AWS writes the built-in mysql schema tables, e.g. under <task id>/mysql/mysql.help_topic/, even for a snapshot with no user table). Checks: cli-rds-start-export-task-workflow, rds-snapshot-export-to-s3-positive, rds-export-manifest-format-preserved.
5. Cause 3: create sim2real_export.widgets(id INT PRIMARY KEY, name VARCHAR(50), score DECIMAL(6,2), created_at DATETIME) over the native MySQL wire, insert a row, snapshot, StartExportTask with ExportOnly=['sim2real_export.widgets'] and an S3Prefix, wait for COMPLETE, GetObject the widgets .parquet part and read its schema — created_at comes back as 'string' instead of a timestamp type. Check: rds-export-task-parquet-format-schema-preserved.
6. Cause 4: run the eight export checks named in that cause; each is killed at the 60 s per-check cap with no graded assertion. Poll DescribeDBInstances and DescribeDBSnapshots directly after creating them to see which status fails to reach 'available' promptly, and call StartExportTask with an invalid bucket name / missing role / role without s3:PutObject / nonexistent bucket to confirm the modeled refusals (InvalidS3BucketFault, IamRoleNotFoundFault, IamRoleMissingPermissionsFault) are answered on the start call.
