Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-cluster-restore-error-paths
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-cluster-restore-error-paths rds-db-tenant-database-and-restore-error-paths rds-db-instance-snapshot-and-restore rds-db-instance-target-error-paths rds-db-parameter-group-memory-formula-value-range rds-global-cluster-mutation-envelopes rds-restore-pit-and-export-errors
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|RestoreDBClusterFromS3|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: rds|RestoreDBClusterFromS3|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|RestoreDBClusterFromS3|4xx->4xx|status
Issue-Key: rds|RestoreDBInstanceFromS3|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: rds|RestoreDBInstanceFromS3|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|RestoreDBInstanceFromS3|4xx->4xx|status
Issue-Key: rds|ModifyDBParameterGroup|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|ModifyDBParameterGroup|2xx->4xx|$.ModifyDBParameterGroupResponse
Issue-Key: rds|ModifyDBParameterGroup|2xx->4xx|status
Issue-Key: rds|DeleteGlobalCluster|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|RestoreDBInstanceToPointInTime|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: c00bf07ce89c73b0369514a40ab33e3142015054

# rds: restore and parameter-group refusals carry the wrong status, error code and message text (7 causes)

> _Note: this issue was identified against the code base at commit `c00bf07ce89c73b0369514a40ab33e3142015054`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RestoreDBClusterFromS3

## Description
Seven refusal paths answer with the wrong HTTP status, the wrong error code or an emulator-authored message — and one rejects a request AWS accepts. Shared fault: these handlers validate in the wrong order and/or build their fault text from a generic emulator template instead of AWS's per-error wording.

--- Cause 1 of 7: RestoreDBClusterFromS3 with a nonexistent DBSubnetGroupName ---
AWS: Smithy: RestoreDBClusterFromS3 errors include DBSubnetGroupNotFoundFault (errorKind client, httpError 404, awsQueryError code DBSubnetGroupNotFoundFault) and InvalidS3BucketFault (httpError 400). AWS resolves the DB subnet group before it checks the S3 source, so a request naming a subnet group that does not exist is refused 404 / DBSubnetGroupNotFoundFault with the message "DB subnet group '<DBSubnetGroupName>' does not exist." — fixed text "DB subnet group '" ... "' does not exist.", the one variable part being the DBSubnetGroupName from the request.
Emulator: HTTP 400 with Error.Code 'InvalidS3BucketFault' and the S3 message "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to access the specified Amazon S3 bucket. Verify the SourceS3BucketName and S3IngestionRoleArn values and try again."
Why: a client gets a 400 naming an S3 problem for what is a 404 naming a missing subnet group, so retry/branching keyed on the status or on DBSubnetGroupNotFoundFault never fires and the operator is pointed at the wrong resource.
Fix: RestoreDBClusterFromS3's input validation ordering — resolve DBSubnetGroupName (and raise DBSubnetGroupNotFoundFault) before the S3 bucket/ingestion-role check.
Traces: rds-db-cluster-restore-error-paths#task#3; same cluster member rds-db-tenant-database-and-restore-error-paths#task#5

--- Cause 2 of 7: RestoreDBInstanceFromS3 with an unusable S3IngestionRoleArn ---
AWS: Smithy: RestoreDBInstanceFromS3 input requires S3IngestionRoleArn, SourceEngine and SourceEngineVersion; InvalidParameterValue is the service-common client error (400). When the ingestion role is invalid or lacks the required permissions, AWS refuses 400 InvalidParameterValue with "IAM role ARN value is invalid or does not include the required permissions for: S3_SNAPSHOT_INGESTION" — the whole sentence is fixed text, the trailing token naming the required permission set (S3_SNAPSHOT_INGESTION).
Emulator: HTTP 400 with Error.Code 'InvalidParameterCombination' and "You can't restore to mysql from an S3 backup of mysql 8.0.35." The emulator never evaluates the ingestion role at all; it instead rejects an engine/source-engine pair that AWS accepts (restoring mysql from a mysql 8.0.35 S3 backup), so the only refusal it can produce is the wrong one.
Why: a client cannot distinguish an IAM problem from an engine-compatibility problem — the credential fault it must fix is never reported, and a supported engine pair appears unsupported.
Fix: RestoreDBInstanceFromS3 validation — add the S3IngestionRoleArn check raising InvalidParameterValue, and correct the SourceEngine/SourceEngineVersion compatibility rule that currently rejects a same-engine restore.
Traces: rds-db-instance-snapshot-and-restore#task#8; same cluster member rds-db-instance-target-error-paths#task#13

--- Cause 3 of 7: RestoreDBInstanceFromS3 with a nonexistent DBSubnetGroupName ---
AWS: Smithy: RestoreDBInstanceFromS3 errors include DBSubnetGroupNotFoundFault (httpError 404, awsQueryError code DBSubnetGroupNotFoundFault). The instance-level variant words the message "DBSubnetGroup '<DBSubnetGroupName>' not found." — fixed text "DBSubnetGroup '" ... "' not found.", variable part the requested DBSubnetGroupName. (Note the wording differs from the cluster-level message in Cause 1; both are requirements.)
Emulator: HTTP 400 with Error.Code 'InvalidParameterCombination' and the same engine-version sentence "You can't restore to mysql from an S3 backup of mysql 8.0.35." — the subnet group is never resolved.
Why: same as Cause 1 — wrong status class (400 instead of 404) and an error code that names nothing the caller can act on.
Fix: RestoreDBInstanceFromS3's input validation ordering — resolve the DB subnet group and raise DBSubnetGroupNotFoundFault before the engine/source checks.
Traces: rds-db-tenant-database-and-restore-error-paths#task#9

--- Cause 4 of 7: ModifyDBParameterGroup's out-of-range message publishes the wrong upper bound ---
AWS: Smithy: ModifyDBParameterGroup (input DBParameterGroupName + Parameters, output DBParameterGroupNameMessage). A parameter value outside the engine-default allowed range is refused 400 with "Value: <value> is outside of range: <min>-<max> for parameter: <name>" — fixed text "Value: ", " is outside of range: ", "-", " for parameter: "; the variable parts are the rejected value, the parameter's published minimum and maximum, and the parameter name, all taken from the request and the engine-default parameter catalog. For the MySQL/Aurora MySQL parameter innodb_buffer_pool_size the published maximum is the unsigned 64-bit ceiling 18446744073709551615.
Emulator: emits the same template but with the maximum 9223372036854775807 — the signed 64-bit ceiling.
Why: the advertised upper bound is off by half the range, so a caller that reads the limit out of the refusal and retries at the reported maximum still submits a value the real service would accept but which the emulator's catalog cannot express. Hypothesis: the allowed-range metadata is parsed/stored as a signed 64-bit integer and saturates at i64::MAX.
Fix: the engine-default parameter catalog's range representation and ModifyDBParameterGroup's range validator (unsigned 64-bit range support).
Traces: rds-db-parameter-group-memory-formula-value-range#task#2; same cluster member rds-db-parameter-group-memory-formula-value-range#task#8

--- Cause 5 of 7: ModifyDBParameterGroup rejects a value AWS accepts ---
AWS: Smithy: ModifyDBParameterGroup -> DBParameterGroupNameMessage{DBParameterGroupName}; the modeled errors are only DBParameterGroupNotFoundFault (404) and InvalidDBParameterGroupStateFault (400). The recorded response is HTTP 200 carrying ModifyDBParameterGroupResult.DBParameterGroupName echoing the requested group name, i.e. the value is inside the parameter's allowed range and the modify succeeds.
Emulator: HTTP 400 — no ModifyDBParameterGroupResponse at all. Hypothesis: the same signed-64-bit range ceiling as Cause 4, so a value legal under the parameter's unsigned range is judged out of range.
Why: a legitimate parameter-group modification is impossible against the emulator; any tuning flow that sets a large memory-formula value fails outright rather than being accepted.
Fix: ModifyDBParameterGroup's parameter range validation (same component as Cause 4).
Traces: rds-db-parameter-group-memory-formula-value-range#task#4

--- Cause 6 of 7: DeleteGlobalCluster's deletion-protection refusal does not name the global cluster ---
AWS: Smithy: DeleteGlobalCluster input GlobalClusterIdentifier (required); errors GlobalClusterNotFoundFault (404) and InvalidGlobalClusterStateFault (400, awsQueryError code InvalidGlobalClusterStateFault). Deleting a global cluster that has deletion protection enabled is refused 400 with "Cannot delete protected Global Cluster <GlobalClusterIdentifier>, please disable deletion protection and try again." — fixed text "Cannot delete protected Global Cluster " and ", please disable deletion protection and try again.", with the requested GlobalClusterIdentifier interpolated between them.
Emulator: the same status and code but the message "Cannot delete protected Global Cluster, please disable deletion protection and try again." — the identifier is dropped, leaving the template's two fixed halves concatenated.
Why: in a batch teardown that deletes several global clusters, the refusal no longer says which one is protected, so the caller cannot attribute the failure or target the DeletionProtection flip.
Fix: the DeleteGlobalCluster handler's InvalidGlobalClusterStateFault message construction.
Traces: rds-global-cluster-mutation-envelopes#task#6

--- Cause 7 of 7: RestoreDBInstanceToPointInTime's source-not-found message is the generic emulator template ---
AWS: Smithy: RestoreDBInstanceToPointInTime input SourceDBInstanceIdentifier; error DBInstanceNotFoundFault (errorKind client, httpError 404, awsQueryError code DBInstanceNotFound). For a PITR whose source instance does not exist, AWS words it "The source instance could not be found: <SourceDBInstanceIdentifier>" — fixed text "The source instance could not be found: " followed by the identifier from the request.
Emulator: the correct 404 and code, but the message "DBInstance not found: <identifier>", its generic not-found template, which does not say the missing instance is the restore SOURCE.
Why: for an operation that names both a source and a target instance, the emulator's wording leaves the caller unable to tell which identifier was not found.
Fix: the RestoreDBInstanceToPointInTime handler's DBInstanceNotFoundFault message (it must use the source-specific wording, not the shared not-found template).
Traces: rds-restore-pit-and-export-errors#task#1

Related open issues that do NOT cover these steps (none of these clusters is owned by one): 20261116T230421Z-rds-s3-restoredbinstancefroms3-s-invalids3bucketfault-message-names-the.md (a different RestoreDBInstanceFromS3 message), 20261113T145556Z-rds-parameter-group-reads-are-served-from-the.md, 20261114T091417Z-rds-not-found-and-switchover-refusals-carry-emulator.md, 20261109T135215Z-rds-restoredbinstancetopointintime-stub.md.

## Evidence
Cause 1 — rds-db-cluster-restore-error-paths#task#3, RestoreDBClusterFromS3:
  status: expected 404, got 400
  $.ErrorResponse.Error.Code: expected 'DBSubnetGroupNotFoundFault', got 'InvalidS3BucketFault'
  $.ErrorResponse.Error.Message: expected "DB subnet group '<requested DBSubnetGroupName>' does not exist.", got "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to access the specified Amazon S3 bucket. Verify the SourceS3BucketName and S3IngestionRoleArn values and try again."
  (cluster also contains rds-db-tenant-database-and-restore-error-paths#task#5)

Cause 2 — rds-db-instance-snapshot-and-restore#task#8, RestoreDBInstanceFromS3, 400 vs 400:
  $.ErrorResponse.Error.Code: expected 'InvalidParameterValue', got 'InvalidParameterCombination'
  $.ErrorResponse.Error.Message: expected 'IAM role ARN value is invalid or does not include the required permissions for: S3_SNAPSHOT_INGESTION', got "You can't restore to mysql from an S3 backup of mysql 8.0.35."
  (cluster also contains rds-db-instance-target-error-paths#task#13)

Cause 3 — rds-db-tenant-database-and-restore-error-paths#task#9, RestoreDBInstanceFromS3:
  status: expected 404, got 400
  $.ErrorResponse.Error.Code: expected 'DBSubnetGroupNotFoundFault', got 'InvalidParameterCombination'
  $.ErrorResponse.Error.Message: expected "DBSubnetGroup '<requested DBSubnetGroupName>' not found.", got "You can't restore to mysql from an S3 backup of mysql 8.0.35."

Cause 4 — rds-db-parameter-group-memory-formula-value-range#task#2, ModifyDBParameterGroup, 400 vs 400:
  $.ErrorResponse.Error.Message: expected 'Value: 5242879 is outside of range: 5242880-18446744073709551615 for parameter: innodb_buffer_pool_size', got 'Value: 5242879 is outside of range: 5242880-9223372036854775807 for parameter: innodb_buffer_pool_size'
  (the rejected value, min and parameter name are request/catalog values; the diverging part is the published maximum — unsigned 64-bit ceiling vs signed)
  (cluster also contains rds-db-parameter-group-memory-formula-value-range#task#8)

Cause 5 — rds-db-parameter-group-memory-formula-value-range#task#4, ModifyDBParameterGroup:
  status: expected 200, got 400
  $.ModifyDBParameterGroupResponse: expected {'ModifyDBParameterGroupResult': {'DBParameterGroupName': '<the requested group>'}, 'ResponseMetadata': {...}}, got None

Cause 6 — rds-global-cluster-mutation-envelopes#task#6, DeleteGlobalCluster, 400 vs 400:
  $.ErrorResponse.Error.Message: expected 'Cannot delete protected Global Cluster <GlobalClusterIdentifier>, please disable deletion protection and try again.', got 'Cannot delete protected Global Cluster, please disable deletion protection and try again.'

Cause 7 — rds-restore-pit-and-export-errors#task#1, RestoreDBInstanceToPointInTime, 404 vs 404:
  $.ErrorResponse.Error.Message: expected 'The source instance could not be found: <SourceDBInstanceIdentifier>', got 'DBInstance not found: <SourceDBInstanceIdentifier>'

## Steps to Reproduce
1. RestoreDBClusterFromS3 naming a DBSubnetGroupName that does not exist (fixture rds-db-cluster-restore-error-paths, step task#3): expect 404 DBSubnetGroupNotFoundFault "DB subnet group '<name>' does not exist."; the emulator answers 400 InvalidS3BucketFault.
2. RestoreDBInstanceFromS3 with an S3IngestionRoleArn that is invalid / lacks permissions (fixture rds-db-instance-snapshot-and-restore, step task#8): expect 400 InvalidParameterValue 'IAM role ARN value is invalid or does not include the required permissions for: S3_SNAPSHOT_INGESTION'; the emulator answers 400 InvalidParameterCombination with an engine-version sentence.
3. RestoreDBInstanceFromS3 naming a nonexistent DBSubnetGroupName (fixture rds-db-tenant-database-and-restore-error-paths, step task#9): expect 404 DBSubnetGroupNotFoundFault "DBSubnetGroup '<name>' not found.".
4. ModifyDBParameterGroup setting innodb_buffer_pool_size below its minimum (fixture rds-db-parameter-group-memory-formula-value-range, step task#2): the refusal's reported range maximum is 9223372036854775807 instead of 18446744073709551615.
5. ModifyDBParameterGroup with the in-range memory-formula value of step task#4 of that same fixture: expect 200 with ModifyDBParameterGroupResult.DBParameterGroupName; the emulator answers 400.
6. DeleteGlobalCluster on a global cluster with DeletionProtection enabled (fixture rds-global-cluster-mutation-envelopes, step task#6): the refusal message omits the global cluster identifier.
7. RestoreDBInstanceToPointInTime with a SourceDBInstanceIdentifier that does not exist (fixture rds-restore-pit-and-export-errors, step task#1): the 404 message is 'DBInstance not found: <id>' instead of 'The source instance could not be found: <id>'.
