Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__s3tables.cfn-s3tables-table-compaction-target-file-size-below-minimum-rejected
Issue-Key: cloudformation__s3tables.cfn-s3tables-table-compaction-target-file-size-below-minimum-rejected|run#13|AssertionError|assert [(t["name"], t["tableARN"]) for t in tables] == [(table_name, out["TableArn"])], (
Issue-Commit: 2c9e097b66ada646a313dc09bbc30cfe7dbc4d7b

# cloudformation__s3tables: an AWS::S3Tables::Table in a CREATE_COMPLETE stack is not created in the table bucket — ListTables is empty and the template's Compaction settings are never applied

> _Note: this issue was identified against the code base at commit `2c9e097b66ada646a313dc09bbc30cfe7dbc4d7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTable

## Description
Cause 1 of 1: CloudFormation reports an AWS::S3Tables::Table resource as created and resolves its TableARN output, but the table never appears in the S3 Tables table bucket, so neither the table nor the Compaction configuration the template declared exists.

AWS: the AWS::S3Tables::Table resource provider calls S3 Tables CreateTable on the referenced table bucket/namespace and, when the resource declares Compaction, applies that maintenance configuration to the new table. Smithy (com.amazonaws.s3tables#S3TableBuckets): ListTables = GET /tables/{tableBucketARN}, 200, output `tables` @required (TableSummaryList) — after the stack reaches CREATE_COMPLETE it lists exactly the stack's table, with `name` equal to the template's TableName and `tableARN` equal to the value the stack exported through Fn::GetAtt Table.TableARN. GetTableMaintenanceConfiguration = GET /tables/{tableBucketARN}/{namespace}/{name}/maintenance, 200, output tableARN + configuration @required — for a resource declaring Compaction {Status: enabled, TargetFileSizeMB: <n>} it reports configuration.icebergCompaction.status == "enabled" and configuration.icebergCompaction.settings.icebergCompaction.targetFileSizeMB == <n> (the template's value, whatever it is; the schema minimum is 64).

Emulator: the stack (AWS::S3Tables::TableBucket + AWS::S3Tables::Namespace + AWS::S3Tables::Table with Compaction) reaches CREATE_COMPLETE and its Outputs resolve, but ListTables on the exported TableBucketARN returns an empty `tables` list — the table is absent from s3tables state, so the maintenance read that would grade the template's Compaction value cannot run either. Hypothesis: the AWS::S3Tables::Table handler records a physical id and synthesizes a TableARN without calling CreateTable (and without applying Compaction via PutTableMaintenanceConfiguration) on the bucket.

Why: declaratively provisioned S3 Tables estates are green but empty. A client reads a table ARN out of the stack Outputs, then every listing, read, Iceberg operation and maintenance query against it fails or comes back empty, contradicting CloudFormation's own CREATE_COMPLETE; and a compaction policy expressed in the template silently does not exist, so data layout guarantees the template asserts are not in force.

Fix: the AWS::S3Tables::Table resource handler in the CloudFormation resource-provider layer and the s3tables table/maintenance state it must write — the create path has to call CreateTable under the referenced bucket and namespace and apply the declared Compaction settings, and the delete path remove the table, so ListTables/GetTable/GetTableMaintenanceConfiguration see it. The open issue 20261113T154534Z-s3tables-a-cloudformation-deployed-aws-s3tables-namespace-does-not.md reports the same shape of defect for AWS::S3Tables::Namespace and names only the Namespace handler, so it does not cover the Table resource type; the same Table defect observed through the s3tables heading is filed separately because an issue carries one service token.

Traces: cloudformation__s3tables.cfn-s3tables-table-compaction-target-file-size-below-minimum-rejected (the positive-control stack's ListTables is empty).

## Evidence
Behavioral replay (102/156 passed), check cloudformation__s3tables.cfn-s3tables-table-compaction-target-file-size-below-minimum-rejected.

The check's positive control deploys a stack with AWS::S3Tables::TableBucket, AWS::S3Tables::Namespace and AWS::S3Tables::Table {OpenTableFormat: ICEBERG, WithoutMetadata: "Yes", Compaction: {Status: enabled, TargetFileSizeMB: 64}} and Outputs TableBucketArn / TableArn. The stack status assertion (CREATE_COMPLETE) and the Outputs read both pass.

Expected: ListTables(tableBucketARN=<Outputs.TableBucketArn>), paginated, yields exactly [(<TableName>, <Outputs.TableArn>)], and GetTableMaintenanceConfiguration(tableBucketARN=<Outputs.TableBucketArn>, namespace=<ns>, name=<TableName>) reports icebergCompaction status "enabled" with settings.icebergCompaction.targetFileSizeMB == 64.

Actual: `AssertionError: ListTables in the control bucket found []` at `assert [(t["name"], t["tableARN"]) for t in tables] == [(table_name, out["TableArn"])]` — the deployed table does not exist, so the compaction read after it is never reached.

## Steps to Reproduce
1. Boot the emulator; point cloudformation and s3tables clients at it.
2. CreateStack with Resources {TableBucket: {Type: AWS::S3Tables::TableBucket, Properties: {TableBucketName: <name>}}, Namespace: {Type: AWS::S3Tables::Namespace, Properties: {TableBucketARN: {Fn::GetAtt: [TableBucket, TableBucketARN]}, Namespace: <ns, [0-9a-z_] only>}}, Table: {Type: AWS::S3Tables::Table, DependsOn: Namespace, Properties: {TableBucketARN: {Fn::GetAtt: [TableBucket, TableBucketARN]}, Namespace: <ns>, TableName: <tbl>, OpenTableFormat: ICEBERG, WithoutMetadata: "Yes", Compaction: {Status: enabled, TargetFileSizeMB: 64}}}} and Outputs exporting TableBucket.TableBucketARN and Table.TableARN; wait for CREATE_COMPLETE.
3. Call s3tables ListTables(tableBucketARN=<Outputs.TableBucketArn>) across all pages — expect exactly one row (<tbl>, <Outputs.TableArn>); observe an empty list.
4. Call s3tables GetTableMaintenanceConfiguration(tableBucketARN=<Outputs.TableBucketArn>, namespace=<ns>, name=<tbl>) — expect configuration.icebergCompaction.status == "enabled" and settings.icebergCompaction.targetFileSizeMB == 64.
5. Re-replay cloudformation__s3tables.cfn-s3tables-table-compaction-target-file-size-below-minimum-rejected.
