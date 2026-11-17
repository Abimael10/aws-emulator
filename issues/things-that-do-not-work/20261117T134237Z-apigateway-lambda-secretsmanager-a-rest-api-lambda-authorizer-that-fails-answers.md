Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda__secretsmanager.apigw-lambda-authorizer-missing-secretsmanager-permission-blocks-admission
Issue-Key: apigateway__lambda__secretsmanager.apigw-lambda-authorizer-missing-secretsmanager-permission-blocks-admission|run#11|AssertionError|assert text == '{"message":null}', (
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# A REST API Lambda authorizer that fails answers the body {"message":"Internal server error"} instead of API Gateway's authorizer-configuration-error body {"message":null}

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAuthorizer

## Description
Cause 1 of 1: when a REST API's Lambda (TOKEN/REQUEST) authorizer function fails — here because its execution role lacks the secretsmanager permission the authorizer needs — the gateway's refusal body is the generic internal-error body rather than the authorizer-configuration-error body API Gateway words for this class.

AWS: recorded behavior only for the execute-api data plane (it is not in the Smithy model; the control plane is apigateway CreateAuthorizer/PutMethod). An authorizer invocation that raises is API Gateway's `AUTHORIZER_CONFIGURATION_ERROR` gateway-response type, whose default body template is `{"message":$context.error.messageString}`; for this class `$context.error.message` is unset, so the rendered body is exactly `{"message":null}` — the literal JSON null, not the string "Internal server error". The fixed text of that body is `{"message":` + `}`; the variable part is `$context.error.messageString`, which is null here and would carry the gateway's own message if one were set. The request is refused before the backend integration is invoked.

Emulator: the body is `{"message":"Internal server error"}` — the `DEFAULT_5XX` / internal-error body — so the refusal is rendered through the wrong gateway-response template and the `null` is replaced by a quoted string.

Why: a client distinguishes an authorizer that is misconfigured from a backend that blew up by exactly this body. Returning the generic internal-error body makes an authorizer-configuration failure indistinguishable from an integration failure, so error handling and alerting keyed on the documented body misroute.

Fix: the execute-api authorizer path's refusal serializer — the mapping from an authorizer invocation failure to the AUTHORIZER_CONFIGURATION_ERROR gateway response and that response's default body template, where `$context.error.messageString` must render as JSON `null` when no message is set.

Traces: apigateway__lambda__secretsmanager.apigw-lambda-authorizer-missing-secretsmanager-permission-blocks-admission

## Evidence
Expected response body (fixed text, no per-run values): {"message":null}
Actual, verbatim:
  AssertionError: expected the default authorizer-configuration-error body '{"message":null}', got '{"message":"Internal server error"}'
So the authorizer-configuration-error gateway response is rendered with the generic internal-error body: the JSON `null` is replaced with the string "Internal server error".

## Steps to Reproduce
1. Create a Lambda authorizer function that reads a secret via secretsmanager GetSecretValue, with an execution role that does NOT grant secretsmanager:GetSecretValue.
2. apigateway CreateAuthorizer (TOKEN) referencing it; PutMethod with authorizationType CUSTOM on a method backed by a working integration; deploy a stage.
3. Call the method through execute-api with a token.
4. Observe the response body `{"message":"Internal server error"}`. Expected exactly `{"message":null}`, and the backend integration not invoked.
