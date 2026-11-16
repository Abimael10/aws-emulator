Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.aurora-restore-from-s3-missing-manifest-rejected rds__s3.rds-restore-db-cluster-from-s3-role-missing-listbucket-negative
Issue-Key: rds__s3.aurora-restore-from-s3-missing-manifest-rejected|run#8|AssertionError|assert message == (
Issue-Key: rds__s3.rds-restore-db-cluster-from-s3-role-missing-listbucket-negative|_client_error#10|AssertionError|raise AssertionError(
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# rds__s3: RestoreDBInstanceFromS3's InvalidS3BucketFault message names the wrong cause, and RestoreDBClusterFromS3 accepts an ingestion role that cannot list the bucket

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RestoreDBInstanceFromS3

## Description
Two net-new root causes in the RDS restore-from-S3 surface. A fixer must close BOTH before submitting.

=== Cause 1 of 2: RestoreDBInstanceFromS3 over an existing, readable but EMPTY bucket is refused with the 'bucket not found / not authorized' message instead of the 'no backup files' one ===
AWS: Smithy RestoreDBInstanceFromS3 (awsQuery; required members S3BucketName, S3IngestionRoleArn, SourceEngine, SourceEngineVersion) models InvalidS3BucketFault with httpError 400 / awsQueryError code 'InvalidS3BucketFault'. That one error code covers several distinct causes, and the MESSAGE is what separates them. For a bucket that exists and the ingestion role can read, but which holds no Percona XtraBackup output, the message is the fixed text 'No backup files were found in the specified Amazon S3 bucket. Check the bucket name or create backup files and try again.' (recorded behavior: the model carries no message literal; the text has no variable parts — it interpolates neither the bucket name nor the role ARN). The separate sentence 'The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to access the specified Amazon S3 bucket. Verify the SourceS3BucketName and S3IngestionRoleArn values and try again.' belongs to a different cause of the same code: a bucket that cannot be found or read at all.
Emulator: the status and code are right (400 / InvalidS3BucketFault) but the message served is the bucket-not-found/not-authorized sentence, for a bucket the check created in the same region and a role that was granted read access to it — only its contents are missing.
Why: the error code alone does not tell a client what to fix, so the message is the diagnosis. A caller (or an operator reading a log) is told to verify the bucket name and the role ARN, both of which are correct, instead of being told to produce the backup files. Automation that branches on the message text takes the wrong remediation path.
Fix: the RestoreDBInstanceFromS3 handler's validation ordering and message selection — distinguish 'bucket absent or unreadable' from 'bucket readable but holding no backup files', and emit AWS's sentence for the latter.
Traces: rds__s3.aurora-restore-from-s3-missing-manifest-rejected

=== Cause 2 of 2: RestoreDBClusterFromS3 is ACCEPTED when the ingestion role lacks s3:ListBucket/s3:GetObject on the source bucket ===
AWS: Smithy RestoreDBClusterFromS3 (awsQuery; required S3BucketName, S3IngestionRoleArn, SourceEngine, SourceEngineVersion) validates the ingestion role synchronously before any cluster exists. With a role that cannot list/read the bucket the call is refused with the awsQuery common error code InvalidParameterValue and the fixed message 'Files from the specified Amazon S3 bucket cannot be listed. Make sure that you have created an AWS Identity and Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you.' (recorded behavior for the message; the model lists InvalidS3BucketFault/KMSKeyNotAccessibleFault etc. for this operation and InvalidParameterValue is the service-common refusal used here). No cluster is created: DescribeDBClusters on that identifier then answers DBClusterNotFoundFault 'DBCluster <id> not found.' A role that DOES hold s3:ListBucket+s3:GetObject on the same bucket is accepted, so the refusal is attributable to the role's permissions and nothing else.
Emulator: the call SUCCEEDS — RestoreDBClusterFromS3 returns without error for the deliberately under-privileged role (the check's deny arm reports 'expected a ClientError, but the call SUCCEEDED'), so the restore is admitted and a cluster is provisioned where AWS refuses outright.
Why: the emulator silently accepts a configuration AWS rejects, so a client validating its ingestion role against the emulator ships a role that real RDS refuses; the deny path (and the cleanup of a cluster that should never have existed) is never exercised.
Fix: the RestoreDBClusterFromS3 handler's pre-flight authorization of S3IngestionRoleArn — evaluate the role's s3:ListBucket/s3:GetObject on S3BucketName (and S3Prefix) before creating any cluster state, and refuse with InvalidParameterValue and AWS's sentence when it cannot list the bucket.
Traces: rds__s3.rds-restore-db-cluster-from-s3-role-missing-listbucket-negative

## Evidence
Cause 1 of 2 — rds__s3.aurora-restore-from-s3-missing-manifest-rejected:
AssertionError: unexpected InvalidS3BucketFault message: "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to access the specified Amazon S3 bucket. Verify the SourceS3BucketName and S3IngestionRoleArn values and try again."
Expected: code InvalidS3BucketFault (HTTP 400) with message 'No backup files were found in the specified Amazon S3 bucket. Check the bucket name or create backup files and try again.' The code assertion passed; only the message diverges. Setup: a bucket created by the check in the region under test, left empty, plus an ingestion role granted read access to it.

Cause 2 of 2 — rds__s3.rds-restore-db-cluster-from-s3-role-missing-listbucket-negative:
AssertionError: expected a ClientError, but the call SUCCEEDED and returned None (raised from _client_error_after_role_propagation, which had already retried the transient 'IAM role ARN value is invalid or does not include the required permissions for: S3_SNAPSHOT_INGESTION' message within its 90 s budget).
Expected: InvalidParameterValue with message 'Files from the specified Amazon S3 bucket cannot be listed. Make sure that you have created an AWS Identity and Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you.', and DescribeDBClusters(<id>) then answering DBClusterNotFoundFault 'DBCluster <id> not found.' Actual: RestoreDBClusterFromS3 returned successfully for a role created WITHOUT s3:ListBucket/s3:GetObject on the bucket (the check's own positive control uses the same bucket with a permitted role).

## Steps to Reproduce
Cause 1: create a bucket in the region under test and leave it empty; create an IAM role RDS can assume with read access to that bucket; call RestoreDBInstanceFromS3(DBInstanceIdentifier=<new>, Engine='mysql', SourceEngine='mysql', SourceEngineVersion='5.7.40', S3BucketName=<bucket>, S3IngestionRoleArn=<role>, DBInstanceClass='db.t3.small', AllocatedStorage=20, MasterUsername/MasterUserPassword set). Compare Error.Code (InvalidS3BucketFault, correct) and Error.Message against 'No backup files were found in the specified Amazon S3 bucket. Check the bucket name or create backup files and try again.'

Cause 2: create a bucket, put any object under backup/, and create an ingestion role whose policy grants NEITHER s3:ListBucket nor s3:GetObject on it; call RestoreDBClusterFromS3(DBClusterIdentifier=<new>, Engine='aurora-mysql', SourceEngine='mysql', SourceEngineVersion='8.0.32', S3BucketName=<bucket>, S3IngestionRoleArn=<weak role>, DBSubnetGroupName=<group>, MasterUsername/MasterUserPassword set). The emulator returns success; AWS refuses with InvalidParameterValue and the 'Files from the specified Amazon S3 bucket cannot be listed...' message and creates no cluster. Repeat with a role that holds s3:ListBucket+s3:GetObject as the positive control.
