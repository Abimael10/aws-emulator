Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__dynamodb.deploy-table-stream-arn-getatt-without-stream-rejected-and-rolled-back cloudformation__dynamodb.deploy-cfn-dynamodb-table-warm-throughput-rejected-below-minimum-then-applies-and-updates-in-place cloudformation__dynamodb.deploy-cfn-dynamodb-table-resource-policy-rejects-unknown-principal-then-updates-and-follows-table
Issue-Key: cloudformation__dynamodb.deploy-table-stream-arn-getatt-without-stream-rejected-and-rolled-back|run#15|AssertionError|assert broken["StackStatus"] == "ROLLBACK_COMPLETE", f"no-stream stack ended {broken['StackStatus']!r}"
Issue-Key: cloudformation__dynamodb.deploy-cfn-dynamodb-table-warm-throughput-rejected-below-minimum-then-applies-and-updates-in-place|run#12|AssertionError|assert failures == [(want_path, want_reason)], (
Issue-Key: cloudformation__dynamodb.deploy-cfn-dynamodb-table-resource-policy-rejects-unknown-principal-then-updates-and-follows-table|run#13|AssertionError|assert message == want_message, f"the handler failure message was {message!r}"
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# CloudFormation AWS::DynamoDB::Table: 3 net-new causes — a GetAtt of a nonexistent attribute completes the stack, WarmThroughput is not range-validated, and the ResourcePolicy handler failure is worded wrongly

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Three net-new root causes in CloudFormation's AWS::DynamoDB::Table resource. All grounded in failing checks listed under '### cloudformation__dynamodb'. Related open issues that do NOT cover these checks: 20261106T020303Z-cloudformation-dynamodb-4-net-new-root-causes-unimplemented.md, 20261109T172142Z-cloudformation-dynamodb-5-net-new-causes-fresh-stacks.md, and (for the DynamoDB-side message) 20261115T104313Z-dynamodb-putresourcepolicy-s-invalid-principal-refusal-is-emulator.md.

Cause 1 of 3: Fn::GetAtt of an attribute the resource does not have is resolved instead of failing the stack.
AWS: AWS::DynamoDB::Table exposes StreamArn only when the table declares a StreamSpecification. An Output whose Value is {'Fn::GetAtt': ['Table', 'StreamArn']} on a table with no stream makes the create fail and roll back: the stack ends ROLLBACK_COMPLETE, it has NO Outputs, the table resource's ROLLBACK_IN_PROGRESS event carries ResourceStatusReason "Attribute 'StreamArn' does not exist. Rollback requested by user." (fixed text, with the attribute name from the template), the table's events include CREATE_COMPLETE then DELETE_COMPLETE, and DescribeTable afterwards answers ResourceNotFoundException 'Requested resource not found: Table: <name> not found'. The control case — the same output on a table WITH a stream — resolves to the table's LatestStreamArn.
Emulator: the no-stream stack ends CREATE_COMPLETE, so the unresolvable attribute is silently satisfied and the table survives.
Why: a template bug that AWS surfaces as a rollback deploys cleanly against the emulator, and the Output is handed to downstream stacks/clients as a value AWS would never have produced.
Fix: the CloudFormation Fn::GetAtt resolver must refuse an attribute the resource type does not expose for the declared properties, and the create must roll back the resource.
Traces: cloudformation__dynamodb.deploy-table-stream-arn-getatt-without-stream-rejected-and-rolled-back

Cause 2 of 3: a WarmThroughput value below the resource schema's minimum passes template validation.
AWS: the AWS::DynamoDB::Table resource schema constrains WarmThroughput/ReadUnitsPerSecond to >= 1, so a template declaring 0 fails validation with exactly one failure naming the property path '/Resources/<LogicalId>/Properties/WarmThroughput/ReadUnitsPerSecond' and the message '#/WarmThroughput/ReadUnitsPerSecond: 0 is not greater or equal to 1'.
Emulator: reports no validation failures at all (an empty failure list), so the out-of-range template is accepted.
Why: a property-range error AWS catches before any resource is touched is let through, so the deploy proceeds and the caller never sees the defect.
Fix: the CloudFormation template validation for AWS::DynamoDB::Table must enforce the resource schema's numeric ranges (WarmThroughput read/write units) and report them as property-path validation failures.
Traces: cloudformation__dynamodb.deploy-cfn-dynamodb-table-warm-throughput-rejected-below-minimum-then-applies-and-updates-in-place

Cause 3 of 3: the ResourcePolicy-with-unknown-principal rollback reason carries the wrong message.
AWS: a table whose ResourcePolicy names a never-created principal rolls the stack back to ROLLBACK_COMPLETE, and the resource's failure reason is 'Resource handler returned message: "One or more parameter values were invalid: Invalid principal in policy document. (Service: DynamoDb, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'. The fixed text is the DynamoDB refusal plus the SDK context suffix; only the request id, attempt count and request token are per-run values. The table is never provisioned (DescribeTable answers ResourceNotFoundException 'Requested resource not found: Table: <name> not found').
Emulator: the handler message is 'One or more parameter values were invalid: Invalid policy document: Invalid principal in policy: "AWS":"<role arn>"' — emulator-authored text that also echoes the principal, and it omits the '(Service: DynamoDb, Status Code: 400, Request ID: ...) (SDK Attempt Count: ...)' suffix AWS's handler always appends.
Why: a caller reading stack events to classify the failure cannot match AWS's message, and the missing SDK suffix means the underlying service, status and request id are not reported at all.
Fix: the DynamoDB PutResourcePolicy invalid-principal validation message, and the CloudFormation resource handler's wrapping of a service refusal (append the SDK context suffix).
Traces: cloudformation__dynamodb.deploy-cfn-dynamodb-table-resource-policy-rejects-unknown-principal-then-updates-and-follows-table

## Evidence
From the behavioral replay:
- GetAtt: "assert broken['StackStatus'] == 'ROLLBACK_COMPLETE' ... AssertionError: no-stream stack ended 'CREATE_COMPLETE'". The check additionally requires no Outputs, the ROLLBACK_IN_PROGRESS reason "Attribute 'StreamArn' does not exist. Rollback requested by user.", table events containing CREATE_COMPLETE and DELETE_COMPLETE, and DescribeTable → ResourceNotFoundException 'Requested resource not found: Table: <name> not found'.
- WarmThroughput: "AssertionError: expected one validation failure ('/Resources/Tbl/Properties/WarmThroughput/ReadUnitsPerSecond', '#/WarmThroughput/ReadUnitsPerSecond: 0 is not greater or equal to 1'), got []".
- ResourcePolicy: "AssertionError: the handler failure message was 'One or more parameter values were invalid: Invalid policy document: Invalid principal in policy: \"AWS\":\"arn:aws:iam::<acct>:role/sim2real-ddbrespol-ghost-<id>\"'"; expected 'One or more parameter values were invalid: Invalid principal in policy document. (Service: DynamoDb, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)' with HandlerErrorCode 'InvalidRequest'.

## Steps to Reproduce
1. Build and boot the emulator, then replay the behavioral suite (run_behavioral_suite).
2. Re-run the three check ids in trace_ids.
3. GetAtt repro: CreateStack with one AWS::DynamoDB::Table (no StreamSpecification) and Outputs {'Stream': {'Value': {'Fn::GetAtt': ['Table','StreamArn']}}} — AWS ends ROLLBACK_COMPLETE with the 'Attribute ... does not exist' reason and deletes the table; the emulator ends CREATE_COMPLETE.
4. WarmThroughput repro: ValidateTemplate/CreateStack with Properties.WarmThroughput.ReadUnitsPerSecond = 0 — AWS reports one validation failure on that property path; the emulator reports none.
5. ResourcePolicy repro: CreateStack with a table whose ResourcePolicy Principal.AWS is a role ARN that was never created, then read the resource's CREATE_FAILED reason from DescribeStackEvents and compare it to AWS's handler message.
