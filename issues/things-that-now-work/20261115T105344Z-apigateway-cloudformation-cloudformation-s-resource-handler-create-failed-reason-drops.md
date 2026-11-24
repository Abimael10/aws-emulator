Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation.cfn-method-request-model-reference-rejected
Issue-Key: apigateway__cloudformation.cfn-method-request-model-reference-rejected|run#27|AssertionError|assert match is not None, f"CREATE_FAILED reason did not match the handler-message shape: {reason!r}"
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# CloudFormation's resource-handler CREATE_FAILED reason drops the (Service/Status Code/Request ID) and (SDK Attempt Count) envelope around the downstream API message

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new cause.

Cause 1 of 1: the ResourceStatusReason CloudFormation records for a resource whose create was refused by the underlying service is missing the wrapped-SDK-error envelope.

AWS: when an AWS::ApiGateway::Method declares RequestModels naming a model that was never created, API Gateway refuses PutMethod and CloudFormation fails the Method resource. The Method CREATE_FAILED event's ResourceStatusReason is the resource-handler wrapper around that refusal, shaped exactly:
  Resource handler returned message: "<api message> (Service: ApiGateway, Status Code: <status>, Request ID: <request-id>) (SDK Attempt Count: <n>)" (RequestToken: <request-token>, HandlerErrorCode: <code>)
Fixed text of the template: `Resource handler returned message: "`, ` (Service: `, `, Status Code: `, `, Request ID: `, `) (SDK Attempt Count: `, `)" (RequestToken: `, `, HandlerErrorCode: `, `)`. Variable parts come from the request and the refusal: the inner API message (here API Gateway's BadRequestException text `Invalid model identifier specified: <RequestModels value>`), the service id (`ApiGateway`), the HTTP status of that refusal (`400`), the per-call request id, the SDK attempt count, the per-call RequestToken, and the handler classification (`InvalidRequest`). Smithy (apigateway): PutMethod declares BadRequestException with httpError 400, so the wrapped status for this refusal class is 400. The stack then rolls back to ROLLBACK_COMPLETE and the Method's inner message and HandlerErrorCode are as above.

Emulator: the inner quoted message stops at the API message and the whole `(Service: ..., Status Code: ..., Request ID: ...)` and `(SDK Attempt Count: ...)` envelope is absent. The reason read back is `Resource handler returned message: "Invalid model identifier specified: <model-name>" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)`. The inner message, HandlerErrorCode and the rollback itself are right; only the envelope that names the service, the HTTP status and the request id is missing, so the reason does not match the handler-message shape at all.

Why: a client or deployment tool that parses a CREATE_FAILED reason to recover which service refused, with what HTTP status, and under which request id (every CDK/CloudFormation error-triage path and every log-correlation step does this) gets nothing back: the regex that matches AWS's reason fails outright, so the failure is reported as unparseable rather than as a 400 from API Gateway.

Fix: the CloudFormation resource-handler error path that formats a downstream service error into a stack-event ResourceStatusReason — it must wrap the service error with the service id, HTTP status code, request id and SDK attempt count inside the quoted message, and keep RequestToken/HandlerErrorCode outside it. Not the API Gateway refusal itself: that message and status are already correct. Open issues 20261109T123922Z-apigateway-cloudformation-6-net-new-causes-a-not.md and 20261115T073242Z-apigateway-cloudformation-3-net-new-causes-in-place.md carry other apigateway__cloudformation causes and do not cover this reason-format defect.

Traces: apigateway__cloudformation.cfn-method-request-model-reference-rejected

## Evidence
apigateway__cloudformation.cfn-method-request-model-reference-rejected — AssertionError: CREATE_FAILED reason did not match the handler-message shape: 'Resource handler returned message: "Invalid model identifier specified: <model-name>" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)'

Expected shape (AWS): Resource handler returned message: "Invalid model identifier specified: <model-name> (Service: ApiGateway, Status Code: 400, Request ID: <uuid>) (SDK Attempt Count: <n>)" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)
Actual (emulator): the `(Service: ApiGateway, Status Code: 400, Request ID: <uuid>)` and `(SDK Attempt Count: <n>)` segments are absent from the quoted inner message.

Everything else in the scenario graded as AWS does: the stack settled ROLLBACK_COMPLETE, exactly one Method CREATE_FAILED event was emitted, and HandlerErrorCode was InvalidRequest.

## Steps to Reproduce
1. CreateStack with a template declaring AWS::ApiGateway::RestApi + AWS::ApiGateway::Resource + AWS::ApiGateway::Method, where the Method's RequestModels is {"application/json": "<a model name that was never created>"} and the Integration is MOCK.
2. Wait for the stack to settle (it reaches ROLLBACK_COMPLETE).
3. DescribeStackEvents and read the ResourceStatusReason of the Method CREATE_FAILED row.
4. Observe the quoted inner message ends at API Gateway's text, with no `(Service: ApiGateway, Status Code: 400, Request ID: ...)` or `(SDK Attempt Count: ...)` segment.
5. Re-replay apigateway__cloudformation.cfn-method-request-model-reference-rejected.
