Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|GetIntegration|2xx->2xx|$.integrationResponseSelectionExpression
Issue-Key: apigatewayv2|UpdateIntegration|2xx->2xx|$.integrationResponseSelectionExpression
Issue-Key: apigatewayv2|UpdateIntegration|2xx->2xx|$.requestParameters.integration.request.querystring.stage
Issue-Key: apigatewayv2|GetIntegration|2xx->2xx|$.requestParameters.integration.request.querystring.stage
Issue-Commit: 64b7111d560a56db8d780b5c63dd8a34d5bc25c3

# apigatewayv2 WebSocket integration reads omit integrationResponseSelectionExpression and keep cleared request parameters

> _Note: this issue was identified against the code base at commit `64b7111d560a56db8d780b5c63dd8a34d5bc25c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIntegration

## Description
This bundle carries 3 causes, one per diverging step, all of them the same emulator fault in the apigatewayv2 integration handler: the derived member integrationResponseSelectionExpression is never serialized for a WebSocket integration, and a requestParameters entry cleared by an empty string is retained as a key.

Cause 1 of 3 — GetIntegration omits integrationResponseSelectionExpression.
AWS: Smithy apigatewayv2#GetIntegration output models IntegrationResponseSelectionExpression (jsonName integrationResponseSelectionExpression, target SelectionExpression). For a WebSocket integration the service DERIVES it; the recording shows the value '${integration.response.statuscode}' — that is the service's fixed derived expression for this integration class, not a value taken from the request.
Emulator: the member is absent from the GetIntegration response.
Why: this expression is what a client uses to know which integration response will be selected at runtime; its absence makes a WebSocket integration look as if it has no response-selection behaviour, and code that reads it to build an integration response gets None.
Fix: the GetIntegration serializer / integration state in the apigatewayv2 integration handler — populate the derived IntegrationResponseSelectionExpression when the API's protocol is WEBSOCKET.
Traces: apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#3.

Cause 2 of 3 — UpdateIntegration omits the same derived member AND leaves a cleared requestParameters key in place.
AWS: Smithy apigatewayv2#UpdateIntegration (PATCH /v2/apis/{ApiId}/integrations/{IntegrationId}) returns the same IntegrationResponseSelectionExpression member, and RequestParameters is a map (IntegrationParameters). Recorded behavior: the response carries integrationResponseSelectionExpression '${integration.response.statuscode}', and the requestParameters entry whose value was set to the empty string is GONE from the map — the key is removed, not retained with an empty/null value.
Emulator: integrationResponseSelectionExpression is missing, and the map still contains the cleared key (reported as an extra key at $.requestParameters.integration.request.querystring.stage where the recording has no key at all).
Why: a client that clears one request-parameter mapping and re-reads the map sees a key AWS has removed; serializing that map back into another UpdateIntegration re-sends a mapping with no value, and any count/iteration over requestParameters is off by that entry.
Fix: UpdateIntegration's state mutation and serializer in the apigatewayv2 integration handler — delete a requestParameters key whose incoming value is the empty string instead of retaining it, and emit the derived response-selection expression.
Traces: apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#4.

Cause 3 of 3 — the follow-up GetIntegration confirms both defects are persisted, not response-only.
AWS: as cause 1, with the cleared requestParameters key absent from the stored integration.
Emulator: GetIntegration again omits integrationResponseSelectionExpression and again returns the cleared requestParameters key, so the divergence is in stored state as well as in the update response.
Why: the read-back a client uses to confirm its clear took effect reports the key still there, so the clear appears to have silently failed.
Fix: same component — the integration state behind Get/UpdateIntegration in the apigatewayv2 integration handler.
Traces: apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#5.

## Evidence
Cause 1 — apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#3, GetIntegration: 200 vs 200, $.integrationResponseSelectionExpression missing (expected '${integration.response.statuscode}', got None).
Cause 2 — same fixture, step task#4, UpdateIntegration: 200 vs 200, $.integrationResponseSelectionExpression missing (expected '${integration.response.statuscode}', got None); $.requestParameters.integration.request.querystring.stage extra (expected None — no such key — got a retained key).
Cause 3 — same fixture, step task#5, GetIntegration: 200 vs 200, same two diffs ($.integrationResponseSelectionExpression missing; $.requestParameters.integration.request.querystring.stage extra).

## Steps to Reproduce
1. Replay fixture apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found.
2. Compare step task#3 (GetIntegration on the WebSocket API's integration): expect integrationResponseSelectionExpression '${integration.response.statuscode}', observe the member absent.
3. Compare step task#4 (UpdateIntegration clearing a requestParameters mapping with an empty string): expect the derived expression present and the cleared key removed from requestParameters, observe the expression missing and the key retained.
4. Compare step task#5 (GetIntegration read-back): same two divergences persist in stored state.
