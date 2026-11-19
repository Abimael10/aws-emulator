Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.cfn-rds-load-from-s3-role-missing-read-policy-negative rds__s3.cli-rds-add-role-to-db-instance-then-verify-s3-access rds__s3.cli-rds-import-from-s3-aurora-workflow rds__s3.cli-rds-remove-role-from-db-instance-then-import-fails-negative rds__s3.deploy-rds-instance-missing-associated-role-negative rds__s3.rds-load-from-s3-role-association-positive rds__s3.rds-mysql-load-data-from-s3-missing-role-negative rds__s3.rds-mysql-load-data-from-s3-positive rds__s3.rds-postgres-aws-s3-extension-import-positive rds__s3.rds-s3-export-query-export-to-s3-positive rds__s3.rds-s3-import-feature-name-mismatch-negative rds__s3.rds-s3-import-load-data-from-s3-positive rds__s3.rds-s3-import-missing-role-association-negative rds__s3.rds-s3-import-role-missing-s3-getobject-negative rds__s3.rds-s3-import-role-removed-during-operation-negative rds__s3.rds-s3-import-role-trust-missing-rds-principal-negative rds__s3.cli-rds-export-task-status-poll-until-complete rds__s3.rds-restore-db-cluster-from-s3-missing-bucket-negative rds__s3.cli-rds-create-export-task-verify-s3-sdk rds__s3.cli-rds-describe-export-tasks-status-poll rds__s3.cli-rds-describe-export-tasks-status-progression rds__s3.cli-rds-export-task-s3-object-listing-verify rds__s3.deploy-cfn-rds-export-task-custom-resource-positive rds__s3.iam-role-trust-policy-rds-s3-positive rds__s3.rds-associate-iam-role-for-s3-import-positive rds__s3.rds-export-manifest-format-preserved rds__s3.rds-export-task-arn-resolution rds__s3.rds-load-from-s3-missing-role-negative rds__s3.rds-postgres-aws-s3-extension-missing-permission-negative rds__s3.rds-snapshot-export-task-manifest-file-in-s3 rds__s3.rds-snapshot-export-to-s3-positive rds__s3.cfn-rds-associated-roles-field-populated-positive rds__s3.cfn-update-stack-add-s3-export-role-positive rds__s3.deploy-cfn-rds-instance-with-s3-import-role-positive rds__s3.deploy-rds-aurora-s3-import-extension-positive rds__s3.deploy-rds-cluster-s3-export-cross-resource-reference-positive rds__s3.deploy-rds-instance-s3-import-role-association-positive rds__s3.deploy-rds-parameter-group-s3-integration-positive
Issue-Key: rds__s3|CreateVpcEndpoint|NotImplemented
Issue-Key: rds__s3.cli-rds-export-task-status-poll-until-complete|_mysql_exec#11|OperationalError|conn = pymysql.connect(host=host_dial, port=port_dial, user=user, password=password, database=db, connect_timeout=30)
Issue-Key: rds__s3.rds-export-task-writes-to-s3-positive|_seed_table_data#10|OperationalError|conn = pymysql.connect(
Issue-Key: rds__s3.rds-restore-db-cluster-from-s3-missing-bucket-negative|run#9|AssertionError|assert code == "InvalidParameterValue", f"expected InvalidParameterValue, got {code}"
Issue-Key: rds__s3.cli-rds-create-export-task-verify-s3-sdk|_start_export_task_with_retry#15|Timeout|time.sleep(delay_s)
Issue-Key: rds__s3.cli-rds-describe-export-tasks-status-poll|_poll_until#8|Timeout|last = probe()
Issue-Key: rds__s3.cli-rds-describe-export-tasks-status-progression|_start_export_task_with_retry#15|Timeout|time.sleep(delay_s)
Issue-Key: rds__s3.cli-rds-export-task-s3-object-listing-verify|_poll_until#6|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.deploy-cfn-rds-export-task-custom-resource-positive|_poll_until#15|Timeout|last = probe()
Issue-Key: rds__s3.iam-role-trust-policy-rds-s3-positive|_wait_for#6|Timeout|last = probe()
Issue-Key: rds__s3.rds-associate-iam-role-for-s3-import-positive|_add_role_with_retry#4|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.rds-export-manifest-format-preserved|_wait_for#5|Timeout|last = probe()
Issue-Key: rds__s3.rds-export-task-arn-resolution|_wait_for#6|Timeout|last = probe()
Issue-Key: rds__s3.rds-load-from-s3-missing-role-negative|_connect#15|Timeout|time.sleep(2)
Issue-Key: rds__s3.rds-postgres-aws-s3-extension-missing-permission-negative|_add_role_to_instance#10|Timeout|time.sleep(2)
Issue-Key: rds__s3.rds-snapshot-export-task-manifest-file-in-s3|_poll_until#4|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.rds-snapshot-export-to-s3-positive|_wait_for#5|Timeout|last = probe()
Issue-Key: rds__s3.cfn-rds-associated-roles-field-populated-positive|_deploy_cfn#7|AssertionError|raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
Issue-Key: rds__s3.cfn-update-stack-add-s3-export-role-positive|_deploy_cfn#4|AssertionError|raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
Issue-Key: rds__s3.deploy-cfn-rds-instance-with-s3-import-role-positive|_deploy_cfn#16|AssertionError|raise AssertionError(
Issue-Key: rds__s3.deploy-rds-aurora-s3-import-extension-positive|_deploy_cfn#20|AssertionError|raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
Issue-Key: rds__s3.deploy-rds-cluster-s3-export-cross-resource-reference-positive|_deploy_cfn#8|AssertionError|raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
Issue-Key: rds__s3.deploy-rds-instance-s3-import-role-association-positive|_deploy_cfn#7|AssertionError|raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
Issue-Key: rds|DescribeOptionGroups|OptionGroupNotFoundFault
Issue-Commit: f099a2d3bf6bcdb075a9bf115e0dc86bb75f03d7

# rds__s3: the in-engine S3 statements, RestoreDBClusterFromS3's parameter refusal, and the CloudFormation RDS resources — RESOLVED

> _This report is the record of the causes that are now served. The two that are NOT are filed on their own, with the replay keys that still fail: `issues/things-that-do-not-work/20261119T064500Z-rds-s3-the-export-writes-no-parquet-and-the-cfn-role-failure-reason.md`. Nothing that still fails is recorded here._

## Operation
StartExportTask

## What now works

Cause 1 of 9 — ec2 `CreateVpcEndpoint` with route-table associations: served by
`services/service-com-amazonaws-ec2-*/src/ops/create_vpc_endpoint.rs`, so the private Amazon S3
gateway endpoint every RDS↔S3 check creates is created. Verified by replay: the key
`rds__s3|CreateVpcEndpoint|NotImplemented` no longer diverges.

Cause 2 of 9 — the in-engine Amazon S3 statements: the published DB endpoint serves them.
`LOAD DATA FROM S3` and `SELECT … INTO OUTFILE S3` on the MySQL engines
(`services/service-com-amazonaws-rds-amazonrdsv19/src/state/data_plane/mysql_s3_load.rs`) and
`aws_s3.table_import_from_s3(…)` / `aws_s3.query_export_to_s3(…)` on the PostgreSQL ones
(`state/data_plane/pg_aws_s3.rs`), each reached from `state/data_plane/server_statements.rs`,
read and write the object through Amazon S3's own store under the role the resource is
associated with for the feature, and refuse a statement whose role association, trust policy or
bucket permission is missing with the engine's own S3 error rather than a syntax error. Pinned by
`tests/all/mysql_s3_load.rs`, `tests/all/postgres_aws_s3_import_export.rs` and
`tests/all/wire_seeded_snapshot_export.rs` — the last of which pins the half the snapshot export
rests on: a stock MySQL driver session (autocommit off, its own `CREATE DATABASE`, a commit)
writes tables the snapshot carries and the export writes as Parquet objects.

Cause 4 of 9 — `RestoreDBClusterFromS3`: implemented, and its source location is now refused as
Amazon RDS refuses it. A value naming a bucket this Region's object store does not reach (no such
bucket, one held in another Region) is `InvalidParameterValue`; a bucket that IS there holding no
backup under the named prefix is the modelled `InvalidS3BucketFault`. Both carry the one sentence
the model states for that error class on the operation. The reading is
`state/restore_from_s3.rs::ingestion_location_defect`, shared with `RestoreDBInstanceFromS3`;
pinned by `tests/all/restore_db_cluster_from_s3.rs` and `tests/all/restore_db_instance_from_s3.rs`.

Cause 6 of 9 — the role and export flows that converge: the role side converges. An association is
decided against the role before the feature name, so a declaration CloudFormation carries to RDS
is answered the way that service answers it, and the endpoints a stack publishes for a DB instance
and a DB cluster are the ones RDS published, so a client that dials a stack output reaches the
database instead of being refused an unroutable name. What still does not converge is the export
writing no Parquet object for a snapshot whose source holds no user table, which is filed on its
own (see the note above).

Cause 7 of 9 — CloudFormation's `AWS::RDS::DBInstance` `AssociatedRoles`: the property is carried
to Amazon RDS by `AddRoleToDBInstance` once that service reports the instance up
(`services/service-com-amazonaws-cloudformation-cloudformation/src/state/provisioner/rds_role_associations.rs`),
and a refusal of that call is the resource's own failure in that service's words. The role gate
itself now decides the ROLE first: a `RoleArn` naming no role this account's IAM holds, or one
whose trust policy does not let `rds.amazonaws.com` assume it, is `InvalidParameterValue` with the
fixed `AWS_ROLE_INTEGRATION` sentence, and only a role the service can take on is held to the
engine version's `SupportedFeatureNames`
(`services/service-com-amazonaws-rds-amazonrdsv19/src/ops/add_role_to_db_instance.rs`,
`add_role_to_db_cluster.rs`; pinned by `tests/all/role_association_role_gate_order.rs` and
`core/router/tests/all/cloudformation_rds_option_group_and_endpoints.rs`). The remaining
divergence is the exact `ResourceStatusReason` text, which is filed on its own (see the note
above).

Cause 8 of 9 — the CloudFormation RDS cluster: `AWS::RDS::DBCluster` has its own provisioner arm
(`src/state/provisioner/rds_db_clusters.rs`), so a stack declaring a cluster and a member DB
instance reaches CREATE_COMPLETE, and the endpoints either type publishes are read off Amazon RDS
once it reports the resource serving (`src/state/provisioner/rds_published_endpoints.rs`).

Cause 9 of 9 — `AWS::RDS::OptionGroup`: the type has a provisioner arm
(`src/state/provisioner/rds_option_groups.rs`) that creates the group through `CreateOptionGroup`,
includes the declared `OptionConfigurations` through `ModifyOptionGroup` and deletes it with the
stack, so `DescribeOptionGroups` reports the group a completed stack declared.

## Evidence
Replay of the issue's units: the keys listed above are fixed or moved past their divergence. The
two causes that remain open carry their own keys in the report named at the top of this file.
