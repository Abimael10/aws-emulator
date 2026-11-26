Issue-Severity: LOW
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.rds-snapshot-export-task-manifest-file-in-s3
Issue-Key: rds__s3.rds-snapshot-export-task-manifest-file-in-s3|_poll_until#23|Timeout|return last
Issue-Commit: c439b0c06ad4c0c74cea84caf475018e0421a0f9

# rds__s3: the Aurora-seeded snapshot-export manifest flow stalls — the check never reaches an observation and ends at the replay budget

> _Note: this issue was identified against the code base at commit `c439b0c06ad4c0c74cea84caf475018e0421a0f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExportTask

## Description
Cause 1 of 1: the Aurora MySQL Serverless v2 -> RDS Data API seed -> cluster snapshot -> StartExportTask chain never produces an observation.
Failing check: rds__s3.rds-snapshot-export-task-manifest-file-in-s3

AWS: recorded behavior only for the timings; the shapes are modelled. The recorded run creates an aurora-mysql cluster with EnableHttpEndpoint=true and ServerlessV2ScalingConfiguration, adds a db.serverless instance, polls DescribeDBInstances until DBInstanceStatus=='available', seeds one table through rds-data ExecuteStatement (CREATE TABLE / INSERT), creates a DB cluster snapshot and polls DescribeDBClusterSnapshots until Status=='available', then calls StartExportTask against that DB cluster snapshot ARN and reads the export's manifest object out of S3. Each of those is a milestone the emulator can answer immediately, since it provisions nothing physical.
Emulator: the replay reports only "Error: Check timed out after 60.0s". No assertion was evaluated, so the divergence is that one of the milestones above never arrives: the instance never reports 'available', the rds-data ExecuteStatement seed never succeeds (it is retried on a 180 s poll), or the cluster snapshot never reaches 'available'. Hypothesis: the stall is in the Aurora Serverless v2 + RDS Data API leg, because the sibling snapshot-export checks under this same heading DO reach their assertions (they are filed separately) and differ from this one mainly in using that leg. The suite output records only the timeout, not which poll never satisfied.
Why: a client cannot get as far as an export at all — it waits on a status transition or a Data API statement that never completes, with no error to branch on. A stalled 'creating'/'pending' status is indistinguishable from slowness, so callers hang rather than fail.
Fix: whichever of these never advances — the rds DB instance/cluster status progression to 'available', the rds-data ExecuteStatement path against a cluster whose HTTP endpoint is enabled, or the DB cluster snapshot status progression to 'available' — and the StartExportTask manifest write behind them.
Traces: rds__s3.rds-snapshot-export-task-manifest-file-in-s3.

## Evidence
expected: CreateDBCluster(aurora-mysql, EnableHttpEndpoint=true, ServerlessV2ScalingConfiguration) + CreateDBInstance(db.serverless) -> DescribeDBInstances reports DBInstanceStatus=='available' -> rds-data ExecuteStatement seeds the table -> CreateDBClusterSnapshot -> DescribeDBClusterSnapshots reports Status=='available' -> StartExportTask -> the export's manifest object is readable in S3.
actual:   Error: Check timed out after 60.0s — the run never reached any of those observations, so no manifest assertion ran.

## Steps to Reproduce
1. CreateDBSubnetGroup over the run's subnets.
2. CreateDBCluster: Engine=aurora-mysql, EngineVersion from DescribeDBEngineVersions, MasterUsername/MasterUserPassword, DatabaseName, ServerlessV2ScalingConfiguration={MinCapacity:0.5,MaxCapacity:2.0}, EnableHttpEndpoint=true, BackupRetentionPeriod=1, StorageEncrypted=true.
3. CreateDBInstance with DBInstanceClass=db.serverless in that cluster; poll DescribeDBInstances for DBInstanceStatus=='available'.
4. Create a Secrets Manager secret with the master credentials; call rds-data ExecuteStatement twice (CREATE TABLE IF NOT EXISTS export_rows ...; INSERT ... ON DUPLICATE KEY UPDATE ...) against the cluster ARN.
5. CreateDBClusterSnapshot; poll DescribeDBClusterSnapshots for Status=='available'.
6. StartExportTask with that DBClusterSnapshotArn, an S3 bucket/prefix, an IAM role and a KMS key; then read the export manifest object from S3.
Against the emulator the run never gets past these polls and is cut off at the suite's 60 s cap.
