Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-integration-response-content-handling-strategy
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-integration-response-content-handling-strategy
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|UpdateIntegrationResponse|2xx->4xx|$.contentHandlingStrategy
Issue-Key: apigateway|UpdateIntegrationResponse|2xx->4xx|$.integrationResponseId
Issue-Key: apigateway|UpdateIntegrationResponse|2xx->4xx|$.integrationResponseKey
Issue-Key: apigateway|UpdateIntegrationResponse|2xx->4xx|status
Issue-Commit: 64b7111d560a56db8d780b5c63dd8a34d5bc25c3

# apigateway UpdateIntegrationResponse refuses a valid contentHandlingStrategy update with 400

> _Note: this issue was identified against the code base at commit `64b7111d560a56db8d780b5c63dd8a34d5bc25c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateIntegrationResponse

## Description
This bundle carries 1 cause.

Cause 1 of 1 — UpdateIntegrationResponse setting contentHandlingStrategy = CONVERT_TO_BINARY on an existing integration response is refused 400 instead of applied.
AWS: Smithy apigatewayv2#UpdateIntegrationResponse (PATCH /v2/apis/{ApiId}/integrations/{IntegrationId}/integrationresponses/{IntegrationResponseId}, http code 200) accepts ContentHandlingStrategy targeting the ContentHandlingStrategy enum, whose only values are CONVERT_TO_BINARY and CONVERT_TO_TEXT; its errors are BadRequestException (400), ConflictException (409), NotFoundException (404), TooManyRequestsException (429). CONVERT_TO_BINARY is therefore a legal value and the request must succeed. Recorded behavior: 200 with the updated integration response echoed — integrationResponseId (the id addressed in the path), integrationResponseKey '$default' and contentHandlingStrategy 'CONVERT_TO_BINARY'.
Emulator: answers 400 and serializes none of those members, so the content-handling change is neither applied nor reported.
Why: content handling is how a WebSocket integration response declares binary/text payload conversion. A client that configures CONVERT_TO_BINARY gets a BadRequestException for a modeled enum value, so it cannot set the field at all; code that treats 400 as "my request is malformed" will report a configuration bug that does not exist, and the integration response silently keeps its previous conversion behaviour.
Fix: UpdateIntegrationResponse's input validation and state mutation in the integration-response handler — accept both ContentHandlingStrategy enum values, persist the change and echo the updated integration response with 200. The integration-response id in the recording is a generated value, not a requirement; the required parts of the response are the 200 status and the integrationResponseKey/contentHandlingStrategy members reflecting the request.
Traces: apigatewayv2-integration-response-content-handling-strategy#task#8.

## Evidence
apigatewayv2-integration-response-content-handling-strategy#task#8, UpdateIntegrationResponse (reported under service token 'apigateway'): expected 200, got 400. Diffs: status expected 200 got 400; $.integrationResponseId missing; $.integrationResponseKey missing (expected '$default'); $.contentHandlingStrategy missing (expected 'CONVERT_TO_BINARY'). Smithy: ContentHandlingStrategy enum values are exactly CONVERT_TO_BINARY and CONVERT_TO_TEXT, so the refused value is modeled.

## Steps to Reproduce
1. Replay fixture apigatewayv2-integration-response-content-handling-strategy.
2. Compare step task#8 — PATCH /v2/apis/{apiId}/integrations/{integrationId}/integrationresponses/{integrationResponseId} with body {"contentHandlingStrategy": "CONVERT_TO_BINARY"}: expect 200 with integrationResponseKey '$default' and contentHandlingStrategy 'CONVERT_TO_BINARY', observe 400 with none of those members.
