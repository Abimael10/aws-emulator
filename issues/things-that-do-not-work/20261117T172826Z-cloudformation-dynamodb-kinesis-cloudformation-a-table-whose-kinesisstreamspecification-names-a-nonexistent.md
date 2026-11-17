Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__dynamodb__kinesis.deploy-cfn-dynamodb-table-kinesis-stream-specification-nonexistent-stream-rejected
Issue-Key: cloudformation__dynamodb__kinesis.deploy-cfn-dynamodb-table-kinesis-stream-specification-nonexistent-stream-rejected|run#24|AssertionError|assert broken_stack["StackStatus"] == "ROLLBACK_COMPLETE", (
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# CloudFormation: a table whose KinesisStreamSpecification names a nonexistent stream deploys CREATE_COMPLETE instead of failing to stabilize and rolling back

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: AWS::DynamoDB::Table accepts a KinesisStreamSpecification whose StreamArn does not exist.
AWS: the table handler enables the Kinesis data stream destination and then waits for it to reach ACTIVE. When the named stream does not exist the destination never becomes ACTIVE, so the resource fails to stabilize: the stack ends ROLLBACK_COMPLETE, the table's CREATE_FAILED event carries exactly one reason 'Resource handler returned message: "Resource of type \'AWS::DynamoDB::Table\' with identifier \'<table name>\' did not stabilize." (RequestToken: <uuid>, HandlerErrorCode: NotStabilized)' — the fixed text with the resource type and the table's identifier from the template, and the handler error code NotStabilized — and the table is removed, so DescribeTable afterwards answers ResourceNotFoundException 'Requested resource not found: Table: <name> not found'. The control case, the same template naming a real stream, reaches CREATE_COMPLETE with the table's destination (StreamArn, 'ACTIVE').
Emulator: the broken stack ends CREATE_COMPLETE with no StackStatusReason, i.e. the nonexistent destination is treated as healthy and the table is left in place.
Why: a template that references a stream in another account/region, a typo'd ARN, or a stream deleted out from under the stack deploys green against the emulator; the stack then owns a table whose Kinesis destination can never deliver, and no CREATE_FAILED/NotStabilized signal reaches the caller to roll it back.
Fix: the AWS::DynamoDB::Table resource handler's stabilization check for KinesisStreamSpecification — resolve the StreamArn against Kinesis, require the destination to reach ACTIVE, and fail the resource with HandlerErrorCode NotStabilized (rolling the table back) when it does not.
Traces: cloudformation__dynamodb__kinesis.deploy-cfn-dynamodb-table-kinesis-stream-specification-nonexistent-stream-rejected

## Evidence
From the behavioral replay: "assert broken_stack['StackStatus'] == 'ROLLBACK_COMPLETE' ... AssertionError: the stack referencing a nonexistent stream must roll back; got 'CREATE_COMPLETE': None".
The check then requires the table's single CREATE_FAILED reason to parse to (HandlerErrorCode, message) == ('NotStabilized', "Resource of type 'AWS::DynamoDB::Table' with identifier '<name>' did not stabilize.") and the table to be absent (DescribeTable → ('ResourceNotFoundException', 'Requested resource not found: Table: <name> not found')). The positive control in the same check — a table whose KinesisStreamSpecification names a real ACTIVE 1-shard stream, with DescribeKinesisStreamingDestination reporting (streamArn, 'ACTIVE') — is what the broken case must be distinguished from.

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run cloudformation__dynamodb__kinesis.deploy-cfn-dynamodb-table-kinesis-stream-specification-nonexistent-stream-rejected.
3. Manual repro: CreateStack with one AWS::DynamoDB::Table whose Properties.KinesisStreamSpecification.StreamArn is 'arn:aws:kinesis:<region>:<account>:stream/<never-created-name>'.
4. Poll DescribeStacks — AWS reaches ROLLBACK_COMPLETE and DescribeStackEvents shows the table CREATE_FAILED with HandlerErrorCode NotStabilized; the emulator reaches CREATE_COMPLETE.
5. Confirm the control path still works by deploying the same template against a real ACTIVE stream.
