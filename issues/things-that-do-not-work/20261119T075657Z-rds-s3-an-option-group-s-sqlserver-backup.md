Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.deploy-rds-parameter-group-s3-integration-positive rds__s3.rds-restore-db-cluster-from-s3-missing-bucket-negative
Issue-Key: rds__s3.deploy-rds-parameter-group-s3-integration-positive|run#21|AssertionError|assert iam_setting["Value"] == role_arn, (
Issue-Key: rds__s3.rds-restore-db-cluster-from-s3-missing-bucket-negative|run#10|AssertionError|assert message == (
Issue-Commit: d3d6261999f9a11a24fded61323b830533f5b9d0

# rds__s3: an option group's SQLSERVER_BACKUP_RESTORE IAM_ROLE_ARN setting reads back empty, and RestoreDBClusterFromS3's missing-bucket refusal uses the DB-instance wording

> _Note: this issue was identified against the code base at commit `d3d6261999f9a11a24fded61323b830533f5b9d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeOptionGroups

## Description
2 net-new causes in rds__s3, both in the RDS S3-integration surface. (The open issue 20261116T230421Z-rds-s3-restoredbinstancefroms3-s-invalids3bucketfault-message-names-the.md concerns RestoreDBInstanceFromS3's InvalidS3BucketFault and does not carry either of these checks' replay keys.)

Cause 1 of 2: DescribeOptionGroups reports an empty Value for the SQLSERVER_BACKUP_RESTORE option's IAM_ROLE_ARN setting, discarding the ARN the option group was created with.
Failing check ids: rds__s3.deploy-rds-parameter-group-s3-integration-positive
AWS: Smithy: rds DescribeOptionGroups -> OptionGroups.OptionGroupsList of OptionGroup, each OptionGroup.Options a list of Option whose OptionSettings is a list of OptionSetting with members Name and Value (both String). AWS stores the value supplied for an option setting when the option is added — through AddOptionToOptionGroup / ModifyOptionGroup, or through an AWS::RDS::OptionGroup resource's OptionConfigurations[].OptionSettings[] — and DescribeOptionGroups echoes it back verbatim. For SQLSERVER_BACKUP_RESTORE (the SQL Server native-backup S3 integration option) the IAM_ROLE_ARN setting's Value is the IAM role ARN the request named; it is the identity RDS assumes to reach the bucket, so it is required for the option to function and is never defaulted to empty.
Emulator: after a stack deploys an option group with SQLSERVER_BACKUP_RESTORE and an IAM_ROLE_ARN option setting naming the stack's role, DescribeOptionGroups returns that setting with Value == "" (empty string). The setting's Name survives; only the Value is dropped. The replay reports: AssertionError: option group's IAM_ROLE_ARN setting is '', expected the deployed role ARN 'arn:aws:iam::<account>:role/<stack>-S3IntegrationRole-<suffix>' — the ARN there is the value the request supplied, not a constant.
Why: the role ARN is the only thing that tells a client (and RDS itself) which identity the option uses to read and write the S3 bucket. A client that reads it back to verify its deployment, to render it, or to re-apply it on ModifyOptionGroup sees an empty string: it concludes S3 integration is unconfigured, or it writes the empty value back and destroys the association. Drift detection over the option group reports a difference that does not exist in the request.
Fix: the option-group state the RDS emulator keeps for option settings — the write path that records OptionSettings[].Value when an option is added to or modified in a group (AddOptionToOptionGroup / ModifyOptionGroup, and the CloudFormation AWS::RDS::OptionGroup handler that calls it), and/or the DescribeOptionGroups serializer that renders OptionSetting.Value. The supplied Value must round-trip.
Traces: rds__s3.deploy-rds-parameter-group-s3-integration-positive

Cause 2 of 2: RestoreDBClusterFromS3 refuses a non-existent source bucket with the DB-instance message instead of the cluster one.
Failing check ids: rds__s3.rds-restore-db-cluster-from-s3-missing-bucket-negative
AWS: Smithy: rds RestoreDBClusterFromS3 requires S3BucketName and S3IngestionRoleArn and models InvalidS3BucketFault (HTTP 400, awsQueryError code InvalidS3BucketFault) among its errors; the refusal AWS actually returns when the named bucket does not exist is the service-common InvalidParameterValue at HTTP 400, whose message for this error class is the fixed text "The specified Amazon S3 bucket cannot be found. Make sure that you have created an AWS Identity and Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you." That message has no variable parts — it names neither the bucket nor the role. It is the cluster-restore wording and is distinct from the DB-instance (RestoreDBInstanceFromS3) wording, which does name the request's SourceS3BucketName and S3IngestionRoleArn parameters.
Emulator: the code is correct (InvalidParameterValue) but the message is the DB-instance one: "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to access the specified Amazon S3 bucket. Verify the SourceS3BucketName and S3IngestionRoleArn values and try again." Those two sentences are entirely fixed text in the emulator's response; none of it is request-derived, so this is a wholesale wrong template rather than a bad substitution.
Why: the two messages distinguish "the bucket cannot be found" from "cannot be found or RDS is not authorized", and they instruct the operator differently — one says to create an IAM role that lets RDS reach S3, the other says to re-check two named request parameters. A client or runbook that matches on the message (the usual way to tell these apart, since both share the InvalidParameterValue code) takes the authorization branch for what is purely a missing bucket, and any assertion on RestoreDBClusterFromS3's documented wording fails. The emulator correctly created no cluster (DescribeDBClusters then answers DBClusterNotFoundFault), so only the message diverges.
Fix: the RestoreDBClusterFromS3 handler's source-bucket validation — it must carry the cluster-restore InvalidParameterValue message, not reuse RestoreDBInstanceFromS3's.
Traces: rds__s3.rds-restore-db-cluster-from-s3-missing-bucket-negative

## Evidence
Cause 1 — rds__s3.deploy-rds-parameter-group-s3-integration-positive:
Expected: DescribeOptionGroups(OptionGroupName=<og>) -> OptionGroupsList[0].Options[SQLSERVER_BACKUP_RESTORE].OptionSettings[IAM_ROLE_ARN].Value equals the role ARN the option group was created with.
Actual: Value == '' (empty string).
Replay output:
  AssertionError: option group's IAM_ROLE_ARN setting is '', expected the deployed role ARN 'arn:aws:iam::000000000000:role/sim2real-xsvc-stk-bb6b4633ea0d-S3IntegrationRole-242654FE9DD3'
  (check line 64: assert iam_setting["Value"] == role_arn)
The account id, stack name and role suffix in that ARN are this run's values; the requirement is that whatever ARN the request supplied is echoed back. Everything before this point passed: the instance reported OptionGroupMemberships[0].Status == "in-sync" and OptionGroupName == the deployed group.

Cause 2 — rds__s3.rds-restore-db-cluster-from-s3-missing-bucket-negative:
Request: RestoreDBClusterFromS3 with Engine=aurora-mysql, SourceEngine=mysql, SourceEngineVersion=8.0.32, S3BucketName=<a bucket that was never created>, S3IngestionRoleArn=<a role granted s3 access to exactly that bucket ARN>, DBSubnetGroupName=<a valid subnet group>.
Expected: HTTP 400, Error.Code "InvalidParameterValue", Error.Message exactly "The specified Amazon S3 bucket cannot be found. Make sure that you have created an AWS Identity and Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you." (all fixed text, no request values interpolated).
Actual: HTTP 400, Error.Code "InvalidParameterValue" (correct), Error.Message "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to access the specified Amazon S3 bucket. Verify the SourceS3BucketName and S3IngestionRoleArn values and try again." — the RestoreDBInstanceFromS3 wording.
Replay output:
  AssertionError: expected the bucket-not-found message, got "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to access the specified Amazon S3 bucket. Verify the SourceS3BucketName and S3IngestionRoleArn values and try again."
  (check line 52)
Effect-absent assertions after it were not reached but the preceding code assertion passed; DescribeDBClusters on the identifier is expected to answer DBClusterNotFoundFault with message "DBCluster <id> not found."

## Steps to Reproduce
Cause 1:
1. Resolve the default sqlserver-ex engine version (DescribeDBEngineVersions Engine=sqlserver-ex DefaultOnly=true) and take its major version.
2. Deploy a stack containing an S3 bucket, an IAM role for S3 integration, an AWS::RDS::OptionGroup for that sqlserver-ex major version with an OptionConfiguration for SQLSERVER_BACKUP_RESTORE whose OptionSettings set IAM_ROLE_ARN to the role's ARN, and a sqlserver-ex DB instance using that option group.
3. Poll DescribeDBInstances until OptionGroupMemberships[0].Status == "in-sync".
4. Call DescribeOptionGroups(OptionGroupName=<the group>) and read Options[SQLSERVER_BACKUP_RESTORE].OptionSettings[IAM_ROLE_ARN].Value.
5. Expected: the deployed role ARN. Observed: ''.

Cause 2:
1. Create a DB subnet group and an IAM role whose policy grants S3 access on arn:aws:s3:::<name> for a bucket name that is never created.
2. Call RestoreDBClusterFromS3 with Engine=aurora-mysql, SourceEngine=mysql, SourceEngineVersion=8.0.32, S3BucketName=<that non-existent bucket>, S3IngestionRoleArn=<that role>, DBSubnetGroupName=<the subnet group>.
3. Inspect Error.Code and Error.Message. Expected InvalidParameterValue with "The specified Amazon S3 bucket cannot be found. Make sure that you have created an AWS Identity and Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you."; observed the SourceS3BucketName/S3IngestionRoleArn wording.
4. DescribeDBClusters(DBClusterIdentifier=<the id>) must answer DBClusterNotFoundFault "DBCluster <id> not found." (it does).
