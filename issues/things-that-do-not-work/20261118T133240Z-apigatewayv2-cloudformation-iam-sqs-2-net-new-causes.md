Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected
Issue-Key: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected|run#15|AssertionError|assert got == _MISSING_MESSAGE_BODY, (
Issue-Commit: 1f604c3d80924acff5038a505405bc1871ebd9db

# apigatewayv2__cloudformation__iam__sqs: 2 net-new causes — the SQS-SendMessage subtype refusal relayed into a CFN stack event lists its parameters alphabetically and names the wrong SDK service

> _Note: this issue was identified against the code base at commit `1f604c3d80924acff5038a505405bc1871ebd9db`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
Two net-new root causes in the refusal a CloudFormation AWS::ApiGatewayV2::Integration CREATE relays when the SQS-SendMessage subtype's required parameter mappings are incomplete. Both are grounded in the same failing check: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected. Both must be closed.

Cause 1 of 2: the bracketed required-parameter list is emitted alphabetically instead of in AWS's per-subtype order.
AWS: Smithy (apigatewayv2 CreateIntegration) models IntegrationSubtype and RequestParameters and declares BadRequestException (errorKind client, httpError 400); the refusal text is not modelled, so it is recorded behavior only. Recorded, as the stack event's handler reason carries it: `Operation: SQS-SendMessage requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [QueueUrl, MessageBody] (Service: ApiGatewayV2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)`, HandlerErrorCode `InvalidRequest`. Fixed text of the first sentence: `Operation: ` … ` requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [` … `]`. Variable parts: the subtype the template named, and the bracketed list — the subtype's whole required set in AWS's own per-subtype order, which for SQS-SendMessage is QueueUrl, MessageBody (and for SQS-DeleteMessage the reverse, ReceiptHandle, QueueUrl, so the order is per-subtype and never alphabetical).
Emulator: `... parameter mapping: [MessageBody, QueueUrl]` — sorted.
Why: the stack event an operator reads, and any pipeline that matches the CREATE_FAILED reason, sees a parameter list AWS never emits.
Fix: the first-class-subtype catalog holding each subtype's required set and the single renderer of that sentence — store/render AWS's per-subtype order.

Cause 2 of 2: the SDK suffix of the relayed message names the wrong service and omits the attempt-count clause.
AWS: recorded behavior only — a stack-event reason for a refusal the API Gateway v2 control plane raised carries the AWS SDK's own trailer, `(Service: ApiGatewayV2, Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)`. Fixed text: the literal `(Service: `, `, Status Code: `, `, Request ID: `, `)` and the separate `(SDK Attempt Count: ` … `)` clause. Variable parts: the SDK service id of the service that refused (here `ApiGatewayV2`, the sdkId of this service — not `ApiGateway`, which is the v1 REST service), the HTTP status of the refusal (400), the request id of the refused call, and the attempt count of the SDK call. The whole trailer sits inside `Resource handler returned message: "…" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)`.
Emulator: the reason reads `(Service: ApiGateway, Status Code: 400, Request ID: <id>)` — the v1 service id — and the `(SDK Attempt Count: <n>)` clause is absent entirely.
Why: the stack event attributes the failure to the wrong AWS service, so an operator or log pipeline triaging a failed deploy looks at API Gateway v1 instead of v2, and tooling that parses the SDK trailer (attempt count included) cannot read it.
Fix: the CloudFormation resource-handler error relay for AWS::ApiGatewayV2::* — the component that wraps a control-plane refusal into the handler reason must stamp the refusing service's own SDK id (ApiGatewayV2) and append the SDK attempt-count clause.

Traces: apigatewayv2__cloudformation__iam__sqs.deploy-cfn-apigatewayv2-integration-sqssendmessage-missing-messagebody-rejected.

## Evidence
Replay (both causes visible in one reason):
`AssertionError: Integration CREATE_FAILED reason masks to ('Operation: SQS-SendMessage requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [MessageBody, QueueUrl] (Service: ApiGateway, Status Code: 400, Request ID: <id>)', 'InvalidRequest'), expected ('Operation: SQS-SendMessage requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [QueueUrl, MessageBody] (Service: ApiGatewayV2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest')` (check line 213).
Raw reason served: `Resource handler returned message: "Operation: SQS-SendMessage requires enabling passthrough, or defining all of the following parameters in the parameter mapping: [MessageBody, QueueUrl] (Service: ApiGateway, Status Code: 400, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)`.
Cause 1 ordering source: services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/state/integration_subtype.rs:76-79 (`required_parameters: &["MessageBody", "QueueUrl"]`) rendered by :152-158.
HandlerErrorCode (`InvalidRequest`) and the 400 status are already correct; only the parameter order and the SDK trailer diverge.

## Steps to Reproduce
1. CreateStack with a template holding an AWS::SQS::Queue, an IAM role API Gateway can assume, an AWS::ApiGatewayV2::Api (HTTP), and an AWS::ApiGatewayV2::Integration with IntegrationType=AWS_PROXY, IntegrationSubtype="SQS-SendMessage", CredentialsArn=<role>, RequestParameters={"QueueUrl": {"Ref": "Queue"}} and NO MessageBody mapping.
2. Poll DescribeStacks/DescribeStackEvents to a terminal state and read the Integration resource's CREATE_FAILED ResourceStatusReason.
Observed: `...parameter mapping: [MessageBody, QueueUrl] (Service: ApiGateway, Status Code: 400, Request ID: <id>)`.
Expected: `...parameter mapping: [QueueUrl, MessageBody] (Service: ApiGatewayV2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)`, HandlerErrorCode InvalidRequest.
