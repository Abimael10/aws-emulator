Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__dynamodb.cfn-dynamodb-provisioned-throughput-drift-reports-expected-and-actual
Issue-Key: cloudformation__dynamodb.cfn-dynamodb-provisioned-throughput-drift-reports-expected-and-actual|_error_code#30|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# cloudformation__dynamodb: after DeleteStack reaches DELETE_COMPLETE the stack's AWS::DynamoDB::Table is still present — DescribeTable answers 200 with TableStatus DELETING instead of ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTable

## Description
Cause 1 of 1: a CloudFormation stack reports DELETE_COMPLETE while the AWS::DynamoDB::Table it owned is left parked in a non-terminal DELETING state, so the table never becomes absent to DynamoDB.

AWS:      Smithy (dynamodb, awsJson1_0): DescribeTable's modeled errors are InternalServerError (server, 500), InvalidEndpointException (client, 421) and ResourceNotFoundException (client, HTTP 400). Once a table has been deleted, DescribeTable is refused with ResourceNotFoundException and the message AWS words as `Requested resource not found: Table: <table-name> not found` — the fixed text is `Requested resource not found: Table: ` and ` not found`; the only variable part is the table name, taken from the request's TableName. CloudFormation only transitions a stack to DELETE_COMPLETE once every resource it owns has actually been deleted, so a client that waits on the stack_delete_complete waiter and then describes the table is guaranteed the not-found refusal, never a DELETING snapshot. (Recorded behavior confirms: the check's beats 1-3 — IN_SYNC on a fresh stack, DRIFTED after an out-of-band UpdateTable, and the /ProvisionedThroughput/ReadCapacityUnits PropertyDifference with ExpectedValue=template and ActualValue=live — all matched; only the post-delete beat diverged.)

Emulator: DeleteStack drives the stack to DELETE_COMPLETE, and DescribeTable(TableName=<the table's PhysicalResourceId>) then SUCCEEDS with HTTP 200 and a full TableDescription whose TableStatus is `DELETING`. The row carries its ProvisionedThroughput, TableArn, TableId and WarmThroughput intact and is never removed, so the stack's DELETE_COMPLETE is premature with respect to the resource it owned.

Why:      a client that tears a stack down and then verifies the table is gone — a teardown assertion, a leak/cost audit, or a re-create of the same table name — is told the table still exists. DynamoDB's own `table_not_exists` waiter never settles, so automated teardown hangs, and a re-create under the same name can collide with the lingering row.

Fix:      the AWS::DynamoDB::Table resource handler's delete path and dynamodb's table state: the resource delete must carry the table through DELETING to actual removal before CloudFormation marks the logical resource (and hence the stack) DELETE_COMPLETE, and DescribeTable must then answer ResourceNotFoundException with AWS's message built from the requested TableName.

Related open issue that does NOT carry this check's replay key: 20261114T080626Z-cloudformation-dynamodb-describetable-still-answers-200-with-tablestatus.md (same root cause reached through a different check, cfn-resource-existence-dynamodb-table); this one is grounded in the drift check, whose stack is a single PROVISIONED-mode table with no GSI or stream.

Traces:   cloudformation__dynamodb.cfn-dynamodb-provisioned-throughput-drift-reports-expected-and-actual.

## Evidence
Expected, after the stack reaches DELETE_COMPLETE:
  dynamodb DescribeTable(TableName=<physical resource id>) -> refused
  HTTP 400, Error.Code == "ResourceNotFoundException"
  Error.Message == "Requested resource not found: Table: <table-name> not found"

Actual (replay):
  File "check_cloudformation__dynamodb_cfn_dynamodb_provisioned_throughput_drift_reports_expected_and_actual.py", line 119, in run
    code, message = _error_code(lambda: ddb.describe_table(TableName=table_name))
  AssertionError: expected the call to be refused, but it succeeded:
  {'Table': {'AttributeDefinitions': [{'AttributeName': 'Id', 'AttributeType': 'S'}],
             'TableName': '<generated>', 'KeySchema': [{'AttributeName': 'Id', 'KeyType': 'HASH'}],
             'TableStatus': 'DELETING',
             'ProvisionedThroughput': {'LastIncreaseDateTime': <ts>, 'NumberOfDecreasesToday': 0,
                                       'ReadCapacityUnits': 10, 'WriteCapacityUnits': 5},
             'TableSizeBytes': 0, 'ItemCount': 0, 'TableArn': '<arn>', 'TableId': '<uuid>',
             'DeletionProtectionEnabled': False,
             'WarmThroughput': {'ReadUnitsPerSecond': 10, 'WriteUnitsPerSecond': 5, 'Status': 'ACTIVE'}},
   'ResponseMetadata': {'HTTPStatusCode': 200, ...}}

The diverging requirements are the outcome and status (200 with a TableDescription instead of a 400 ResourceNotFoundException), the error code, and the lingering TableStatus 'DELETING'. The generated table name, ARN, TableId, request id and timestamps in the recording are per-run values that fill the message template — they are not part of it.

## Steps to Reproduce
1. cloudformation CreateStack with one AWS::DynamoDB::Table: AttributeDefinitions [{Id,S}], KeySchema [{Id,HASH}], BillingMode PROVISIONED, ProvisionedThroughput {ReadCapacityUnits: 5, WriteCapacityUnits: 5}.
2. Wait for CREATE_COMPLETE; read the table's PhysicalResourceId via DescribeStackResource(LogicalResourceId="Table").
3. (Passes today) DetectStackDrift -> IN_SYNC; then dynamodb UpdateTable with ReadCapacityUnits 10; DetectStackDrift -> DRIFTED with a /ProvisionedThroughput/ReadCapacityUnits PropertyDifference ExpectedValue 5 / ActualValue 10.
4. cloudformation DeleteStack and wait for DELETE_COMPLETE.
5. Call dynamodb DescribeTable(TableName=<physical resource id>).
6. Observe a 200 TableDescription with TableStatus 'DELETING' instead of ResourceNotFoundException "Requested resource not found: Table: <name> not found". Polling further never clears it.
