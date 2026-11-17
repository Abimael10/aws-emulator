Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected
Issue-Key: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected|run#10|AssertionError|assert status == "ROLLBACK_COMPLETE", f"integration missing MessageBody settled at {status!r}"
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# CloudFormation accepts an AWS::ApiGatewayV2::Integration (SQS-SendMessage) with no MessageBody and reaches CREATE_COMPLETE

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: the AWS::ApiGatewayV2::Integration resource handler does not validate the subtype's required request parameters, so a stack that must roll back completes.

AWS: recorded behavior only for the CFN resource; the underlying apigatewayv2 CreateIntegration (Smithy) declares BadRequestException (400) and AWS requires the `SQS-SendMessage` subtype's mandatory request parameters (`QueueUrl` and `MessageBody`) in RequestParameters. The recording shows the resource create failing, so the stack settles ROLLBACK_COMPLETE.

Emulator: the create succeeds and the stack settles `CREATE_COMPLETE`: `AssertionError: integration missing MessageBody settled at 'CREATE_COMPLETE'`.

Why: an invalid template deploys "successfully" and the defect surfaces only as a silently broken route at runtime; a client relying on CloudFormation to gate bad configuration loses that gate.

Fix: the AWS::ApiGatewayV2::Integration resource handler's create validation (it must call/replicate CreateIntegration's subtype parameter validation and fail the resource, letting the stack roll back).

Traces: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected.

## Evidence
Expected stack status: ROLLBACK_COMPLETE (the integration create is refused because the SQS-SendMessage subtype's MessageBody request parameter is missing).
Actual: `AssertionError: integration missing MessageBody settled at 'CREATE_COMPLETE'`

## Steps to Reproduce
1. CreateStack with an AWS::ApiGatewayV2::Api, a queue, a role, and an AWS::ApiGatewayV2::Integration with IntegrationType=AWS_PROXY, IntegrationSubtype=SQS-SendMessage and RequestParameters containing only QueueUrl (no MessageBody).
2. Poll DescribeStacks until terminal.
Observed: CREATE_COMPLETE. Expected: the integration resource fails and the stack settles ROLLBACK_COMPLETE.
