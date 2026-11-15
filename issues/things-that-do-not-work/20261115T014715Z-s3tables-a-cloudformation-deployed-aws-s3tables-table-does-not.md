Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3tables.deploy-table-via-cloudformation-then-policy-enforced
Issue-Shape-Id: com.amazonaws.s3tables#S3TableBuckets
Issue-Key: s3tables|GetTable|NotFoundException
Issue-Commit: 2c9e097b66ada646a313dc09bbc30cfe7dbc4d7b

# A CloudFormation-deployed AWS::S3Tables::Table does not exist in S3 Tables: the stack reaches CREATE_COMPLETE and exports a TableARN, but GetTable on it answers 404 NotFoundException

> _Note: this issue was identified against the code base at commit `2c9e097b66ada646a313dc09bbc30cfe7dbc4d7b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTable

## Description
Cause 1 of 1: the AWS::S3Tables::Table resource provider reports success and yields a TableARN, but no table is ever created in the S3 Tables table bucket, so the deployed estate is non-functional behind a green stack.

AWS: the AWS::S3Tables::Table resource provider calls S3 Tables CreateTable on the referenced table bucket and namespace. Smithy (com.amazonaws.s3tables#S3TableBuckets): CreateTable creates the table under tableBucketARN + namespace and returns its tableARN; after the stack reaches CREATE_COMPLETE the table is real — GetTable (GET /get-table, 200, output name/type/tableARN/namespace/versionToken/warehouseLocation/createdAt/format all @required) returns it by (tableBucketARN, namespace, name) with name == the template's TableName and namespace == [<Namespace>], and ListTables (GET /tables/{tableBucketARN}, 200, `tables` @required) lists exactly that table with the tableARN the stack exported via Fn::GetAtt Table.TableARN. NotFoundException (@httpError 404, message `The specified table does not exist.`) is what a genuinely absent table gets; here it must not be raised, because CloudFormation created the table.

Emulator: the stack with AWS::S3Tables::TableBucket + AWS::S3Tables::Namespace + AWS::S3Tables::Table (OpenTableFormat: ICEBERG, WithoutMetadata: "Yes", DependsOn: Namespace) reaches CREATE_COMPLETE and the Outputs resolve, but the table is absent from s3tables state: GetTable(tableBucketARN=<exported bucket arn>, namespace=<NamespaceName>, name=<TableName>) is refused `404 NotFoundException: The specified table does not exist.` (that message is the correct template for a genuinely missing table — the defect is that the table is missing, not the wording). Hypothesis: the AWS::S3Tables::Table handler records a physical id and a synthesized TableARN without calling CreateTable against the bucket.

Why: a client that provisions an S3 Tables estate declaratively gets a green stack, an exported table ARN, and nothing behind it. Every subsequent read, Iceberg load/commit, maintenance call and table-level policy test against the templated table fails with a 404 that contradicts CloudFormation's own CREATE_COMPLETE, and resource-level policies written against the exported ARN protect a table that does not exist.

Fix: the AWS::S3Tables::Table resource handler and the s3tables table state it must write — the create path has to materialize the table under the referenced bucket/namespace (and the delete path remove it), so the control-plane reads (GetTable, ListTables) and the maintenance/policy paths see it. Note the open issue 20261113T154534Z-s3tables-a-cloudformation-deployed-aws-s3tables-namespace-does-not.md reports the same shape of defect for AWS::S3Tables::Namespace and names only the Namespace handler; this is the Table resource type and is not covered by it. The sibling failure under the cloudformation__s3tables heading (cfn-s3tables-table-compaction-target-file-size-below-minimum-rejected, where ListTables on the deployed bucket is empty and the template's Compaction settings are never applied) is filed separately because it belongs to a different service heading.

Traces: s3tables.deploy-table-via-cloudformation-then-policy-enforced (GetTable -> 404 NotFoundException after CREATE_COMPLETE).

## Evidence
Behavioral replay (102/156 passed), check s3tables.deploy-table-via-cloudformation-then-policy-enforced.

Expected: after a stack whose Resources are {TableBucket: AWS::S3Tables::TableBucket, Namespace: AWS::S3Tables::Namespace {TableBucketARN: Fn::GetAtt TableBucket.TableBucketARN, Namespace: <ns>}, Table: AWS::S3Tables::Table {TableBucketARN: Fn::GetAtt TableBucket.TableBucketARN, Namespace: <ns>, TableName: <tbl>, OpenTableFormat: ICEBERG, WithoutMetadata: "Yes", DependsOn: Namespace}} reaches CREATE_COMPLETE and exports TableBucketArn/TableArn, s3tables GetTable(tableBucketARN=<exported bucket arn>, namespace=<ns>, name=<tbl>) returns 200 with name == <tbl> and namespace == [<ns>], which is the precondition for the table-level resource-policy enforcement the check then grades.

Actual: `botocore.errorfactory.NotFoundException: An error occurred (NotFoundException) when calling the GetTable operation: The specified table does not exist.` (HTTP 404) — the check errors out on that read, so none of the policy trajectory after it is reached.

## Steps to Reproduce
1. Boot the emulator; point cloudformation and s3tables clients at it.
2. CreateStack with Resources {TableBucket: {Type: AWS::S3Tables::TableBucket, Properties: {TableBucketName: <name>}}, Namespace: {Type: AWS::S3Tables::Namespace, Properties: {TableBucketARN: {Fn::GetAtt: [TableBucket, TableBucketARN]}, Namespace: <ns, [0-9a-z_] only>}}, Table: {Type: AWS::S3Tables::Table, DependsOn: Namespace, Properties: {TableBucketARN: {Fn::GetAtt: [TableBucket, TableBucketARN]}, Namespace: <ns>, TableName: <tbl, [0-9a-z_] only>, OpenTableFormat: ICEBERG, WithoutMetadata: "Yes"}}} and Outputs exporting TableBucket.TableBucketARN and Table.TableARN; wait for CREATE_COMPLETE.
3. Call s3tables GetTable(tableBucketARN=<exported bucket arn>, namespace=<ns>, name=<tbl>) — expect 200 with name/namespace echoed; observe 404 NotFoundException "The specified table does not exist."
4. Call s3tables ListTables(tableBucketARN=<exported bucket arn>) — expect one row whose tableARN equals the stack's exported TableArn.
5. Re-replay s3tables.deploy-table-via-cloudformation-then-policy-enforced.
