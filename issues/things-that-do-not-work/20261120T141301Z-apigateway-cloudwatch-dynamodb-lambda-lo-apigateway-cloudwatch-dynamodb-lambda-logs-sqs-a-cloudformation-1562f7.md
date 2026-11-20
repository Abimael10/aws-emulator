Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-sam
Issue-Key: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn|_wait_stack#11|AssertionError|raise AssertionError(
Issue-Key: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period|_wait_stack#11|AssertionError|raise AssertionError(
Issue-Key: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-sam|_wait_stack#10|AssertionError|raise AssertionError(
Issue-Commit: c0e060f2f067b39cd5ab9988281e3b80e6bce20a

# apigateway__cloudwatch__dynamodb__lambda__logs__sqs: a CloudFormation/SAM stack update re-creates the unchanged AWS::Lambda::EventSourceMapping, so Lambda's 409 ResourceConflict rolls the whole update back

> _Note: this issue was identified against the code base at commit `c0e060f2f067b39cd5ab9988281e3b80e6bce20a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Cause 1 of 1: an UpdateStack whose template leaves the AWS::Lambda::EventSourceMapping resource unchanged issues a fresh CreateEventSourceMapping for the same (EventSourceArn, FunctionName) pair instead of taking the no-op path. Lambda correctly refuses the second mapping with 409 ResourceConflictException, the resource reports UPDATE/CREATE_FAILED with HandlerErrorCode ResourceConflict, and the stack ends UPDATE_ROLLBACK_COMPLETE. Failing checks: apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn, apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-cfn-alarm-echoes-declared-period, apigateway__cloudwatch__dynamodb__lambda__logs__sqs.xflavor-order-service-sam.

AWS: UpdateStack diffs each resource's declared properties against the deployed ones; an AWS::Lambda::EventSourceMapping whose properties the update does not change is a no-op (no create, no replacement) and the stack reaches UPDATE_COMPLETE. Lambda only answers 409 when a SECOND mapping is requested for an (event source, function) pair that already has one. Smithy (lambda, CreateEventSourceMapping): errors include ResourceConflictException, errorKind client, httpError 409; POST /2015-03-31/event-source-mappings. The 409 message for this class is the fixed text 'An event source mapping with SQS arn (" <queue arn> ") and function (" <function name> ") already exists. Please update or delete the existing mapping with UUID <uuid>', where the queue ARN, function name and UUID are values taken from the request and from the already-stored mapping. The refusal itself is faithful — the defect is that CloudFormation provokes it at all.

Emulator: on the second apply of the order-service stack (the _update_cfn leg), the event-source-mapping resource — logical id 'Esm' in the plain CloudFormation template and 'WorkerFunctionFromQueue' in the SAM template, so the defect is independent of logical id and IaC flavor — is handed to a create, Lambda answers 409, and DescribeStacks reports UPDATE_ROLLBACK_COMPLETE instead of UPDATE_COMPLETE.

Why: a client can never land a second deployment of any stack that contains an SQS event source. Every update of such a stack rolls back, so the iterate-and-redeploy workflow real AWS supports is unavailable, and the rollback also discards the change the caller actually requested (here the in-place update leg and everything gated behind it).

Fix: the CloudFormation engine's update planner / resource diff for AWS::Lambda::EventSourceMapping — an unchanged mapping must take the no-op path and a changed one the update path, rather than always being created. It does not belong in Lambda's CreateEventSourceMapping validation, which behaves correctly.

Related (does not cover these checks): open issue 20261114T085344Z-apigateway-cloudwatch-dynamodb-lambda-lo-a-stack-update-re-creates-the-unchanged-aws.md records the same root cause for the two CDK-synthesized checks (xflavor-order-service-cdk-assetfree and -alarm-config) and carries only their replay keys; the three plain-CloudFormation and SAM checks listed here are not covered by it and must be re-replayed against any fix.

Traces: the three checks named above all fail identically at _update_cfn/_wait_stack with the same 409 ResourceConflict on the stack's event-source-mapping resource.

## Evidence
Expected: UpdateStack on the already-deployed order-service stack reaches UPDATE_COMPLETE; the unchanged AWS::Lambda::EventSourceMapping is not re-created.
Actual (replay, all three checks): AssertionError: stack <stack> ended in UPDATE_ROLLBACK_COMPLETE: <esm logical id>: Resource handler returned message: "An event source mapping with SQS arn (" <queue arn> ") and function (" <worker function name> ") already exists. Please update or delete the existing mapping with UUID <uuid> (Service: Lambda, Status Code: 409, Request ID: <id>) (SDK Attempt Count: 1)" (RequestToken: <token>, HandlerErrorCode: ResourceConflict).
Fixed text of the message: 'An event source mapping with SQS arn (" ... ") and function (" ... ") already exists. Please update or delete the existing mapping with UUID ...'. Variable parts taken from the request/state: the queue ARN, the function name and the existing mapping's UUID. The stack name, ARNs, UUID, request id and request token are per-run values; the invariant is that the update provokes a 409 and rolls back at all.

## Steps to Reproduce
1. CreateStack with the order-service template (API Gateway REST API + DynamoDB table + Lambda worker + SQS queue + AWS::Lambda::EventSourceMapping + CloudWatch alarm), in either the plain CloudFormation flavor (logical id 'Esm') or the SAM flavor (Function with an SQS event, logical id 'WorkerFunctionFromQueue'); wait for CREATE_COMPLETE.
2. Call UpdateStack with a template in which the event-source-mapping resource's properties are unchanged.
3. Poll DescribeStacks. Expected: UPDATE_COMPLETE, the mapping untouched. Actual: UPDATE_ROLLBACK_COMPLETE, and DescribeStackEvents shows the mapping resource failed with Lambda's 409 ResourceConflictException.
