Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.rds-export-task-parquet-format-schema-preserved rds__s3.rds-export-task-partial-table-list-positive rds__s3.rds-export-task-respects-s3-prefix-positive rds__s3.rds-start-export-task-writes-objects-to-s3-positive rds__s3.rds-export-manifest-json-format-preserved rds__s3.rds-export-task-creates-s3-objects-positive rds__s3.rds-export-task-s3-bucket-policy-denies-negative rds__s3.rds-snapshot-export-missing-iam-role-negative rds__s3.rds-snapshot-export-nonexistent-bucket-negative rds__s3.cfn-deploy-rds-instance-nonexistent-role-arn-negative rds__s3.deploy-rds-role-trust-s3-service-mismatch-negative
Issue-Key: rds__s3.rds-export-task-parquet-format-schema-preserved|run#11|OperationalError|conn = pymysql.connect(
Issue-Key: rds__s3.rds-export-task-partial-table-list-positive|run#10|OperationalError|conn = pymysql.connect(host=endpoint_dial, port=port_dial, user="sim2root", password=_PASSWORD, connect_timeout=30)
Issue-Key: rds__s3.rds-export-task-respects-s3-prefix-positive|_mysql_exec#9|OperationalError|conn = pymysql.connect(host=host_dial, port=port_dial, user=user, password=password, database=db, connect_timeout=30)
Issue-Key: rds__s3.rds-start-export-task-writes-objects-to-s3-positive|_mysql_exec#8|OperationalError|conn = pymysql.connect(
Issue-Key: rds__s3.rds-export-manifest-json-format-preserved|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.rds-export-task-creates-s3-objects-positive|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.rds-export-task-s3-bucket-policy-denies-negative|_wait_for#9|Timeout|last = probe()
Issue-Key: rds__s3.rds-snapshot-export-missing-iam-role-negative|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.rds-snapshot-export-nonexistent-bucket-negative|_poll_until#4|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.cfn-deploy-rds-instance-nonexistent-role-arn-negative|run#12|AssertionError|assert reason == _expected_role_integration_reason(reason), (
Issue-Key: rds__s3.deploy-rds-role-trust-s3-service-mismatch-negative|_deploy_cfn#4|AssertionError|raise AssertionError(
Issue-Commit: ec0bf1370387c704b02f9ce325bdddc38624e8ec

# rds__s3: the two causes split out of the six-cause report — a COMPLETE export writes no Parquet object for a snapshot holding no user table, and a CloudFormation DB instance's role failure reason diverges

> _Split out of `issues/things-that-now-work/20261107T061500Z-rds-s3-6-remaining-causes-in-engine-s3-statements-and-cloudformation.md`, whose causes 1, 2, 4, 8 and 9 are served and whose cause 7 is served apart from the reason TEXT below. These two are NOT fixed, and they keep the replay keys of the units that still fail so nothing is recorded as working while it diverges._

## Operation
StartExportTask

## Description

Cause 1 of 2: `StartExportTask` reaches `COMPLETE` writing only its two JSON manifests when the
source snapshot's database holds no table a client created — so a positive export has no data to
read and every check that waits for a `.parquet` object spends its whole budget waiting.
AWS: the objects an export leaves under `<S3Prefix>/<ExportTaskIdentifier>/` include one directory
per exported table, `<database>/<schema>.<table>/`, holding the Parquet part files; the recorded
layout of an RDS for MySQL snapshot export includes the engine's OWN system schema (a check of
this suite reads for keys under `<task>/mysql/mysql.help_topic/`), so an export of a snapshot whose
customer database is empty still writes table data.
Emulator: `services/service-com-amazonaws-rds-amazonrdsv19/src/state/export_objects.rs::exported_tables`
SKIPS every database in `state/data_plane/wire_session.rs::MYSQL_SYSTEM_DATABASES`
(`information_schema`, `mysql`, `performance_schema`, `sys`), and the `mysql` schema a DB
instance's engine comes with holds no table at all until a client's own account statement creates
one — so a snapshot of an instance whose user database is empty exports nothing, the manifest says
`tablesExported: 0` / `perTableStatus: []`, and the two objects written are
`export_info_<id>.json` and `export_tables_info_<id>_from_1_to_1.json`.
Why: a COMPLETE export that produced no data is a silent total loss reported as a success, and it
makes every negative in this area unmeasurable — each asserts a refusal and then proves the
positive control still exports.
Fix: give a MySQL DB instance's engine the system schema a MySQL server really holds (the grant
tables with the master account's own row, which a session can then read) and export the `mysql`
schema with the customer's databases, leaving out only the schemas that hold no stored data
(`information_schema`, `performance_schema`, `sys`). Both halves have to be real: the export writes
the tables the engine HOLDS, so the capability is the engine's state and not a special case in the
export.
NOTE: the same cause is the subject of
`issues/things-that-do-not-work/20261117T083457Z-rds-s3-2-net-new-causes-startexporttask-reaches.md`
(its cause 1), which carries the two units' own assertion keys. Whichever is fixed first closes
both; name the other when merging.
Checks: rds-export-task-parquet-format-schema-preserved, rds-export-task-partial-table-list-positive,
rds-export-task-respects-s3-prefix-positive, rds-start-export-task-writes-objects-to-s3-positive,
rds-export-manifest-json-format-preserved, rds-export-task-creates-s3-objects-positive,
rds-export-task-s3-bucket-policy-denies-negative, rds-snapshot-export-missing-iam-role-negative,
rds-snapshot-export-nonexistent-bucket-negative.

Cause 2 of 2: the `ResourceStatusReason` a CloudFormation DB instance carries when Amazon RDS
refuses one of its declared `AssociatedRoles` is not the text the recorded deployment carries.
AWS: the resource fails and the stack rolls back, and the reason quotes RDS's own refusal of the
role integration — the fixed sentence `IAM role ARN value is invalid or does not include the
required permissions for: AWS_ROLE_INTEGRATION` — inside CloudFormation's own envelope, with the
SDK diagnostic of the refused call and the request token beside it. The checks build the expected
reason FROM the reason they were given (`_expected_role_integration_reason(reason)`), so only the
fixed parts of the envelope decide the comparison; the recorded response is where those parts can
be read.
Emulator: the resource now fails and the stack rolls back (that half is fixed), and the reason is
`Resource handler returned message: "IAM role ARN value is invalid or does not include the required
permissions for: AWS_ROLE_INTEGRATION (Service: Rds, Status Code: 400, Request ID: <id>)"
(RequestToken: <id>, HandlerErrorCode: InvalidRequest)` — the RDS envelope
`services/service-com-amazonaws-cloudformation-cloudformation/src/state/provisioning/sdk_diagnostics.rs`
records from the corpus for `AWS::RDS::DBSubnetGroup`. Which fixed part the DB instance provider's
own reason differs in is not visible from the replay output, which withholds the recorded string.
Why: a deploy tool that recognises AWS's reason to tell a bad role from an unavailable feature
never matches, so a well-understood failure is surfaced as an unknown one.
Fix: read the recorded `ResourceStatusReason` of these two checks and word the envelope
`provisioning::sdk_diagnostics::handler_failure_reason` builds for the Amazon RDS DB instance
provider the way that provider words it (the per-run request id and request token stay per-run
values).
Checks: cfn-deploy-rds-instance-nonexistent-role-arn-negative,
deploy-rds-role-trust-s3-service-mismatch-negative.

## Evidence
Replay against the emulator built from commit `ec0bf1370387c704b02f9ce325bdddc38624e8ec`:

Cause 1 — rds-export-manifest-json-format-preserved:
  `AssertionError: expected parquet objects under base prefix, got: ['<task>/export_info_<task>.json', '<task>/export_tables_info_<task>_from_1_to_1.json']`
  rds-export-task-creates-s3-objects-positive: `no .parquet objects among exported keys: [the same two]`.
  The remaining checks of this cause report `Check timed out after 60.0s` while a poll waits for
  those objects or for the positive control that follows a refusal. That the SEEDED path exports
  correctly is pinned by
  `services/service-com-amazonaws-rds-amazonrdsv19/tests/all/wire_seeded_snapshot_export.rs`, so
  what is missing is the system-schema half described above.

Cause 2 — cfn-deploy-rds-instance-nonexistent-role-arn-negative:
  `AssertionError: assert reason == _expected_role_integration_reason(reason)`; the stack does fail
  and roll back, so the preceding assertion (`final_status != "CREATE_COMPLETE"`) now passes.
  deploy-rds-role-trust-s3-service-mismatch-negative: the same comparison on `db_failure`.

## Steps to Reproduce
1. Create a MySQL DB instance with `DBName` set, wait for `available`, take a snapshot of it
   WITHOUT writing a table first, create a bucket, a symmetric KMS key and a role trusting
   `export.rds.amazonaws.com`, then `StartExportTask` and poll `DescribeExportTasks` to `COMPLETE`:
   `ListObjectsV2` under the task prefix reports only the two JSON manifests (cause 1).
2. Deploy a CloudFormation `AWS::RDS::DBInstance` whose `AssociatedRoles` names a role ARN IAM
   holds nothing under: the stack rolls back, and the DB instance's `CREATE_FAILED`
   `ResourceStatusReason` is the one quoted above (cause 2).
