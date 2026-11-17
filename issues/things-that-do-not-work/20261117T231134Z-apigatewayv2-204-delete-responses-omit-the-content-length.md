Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Step: task#14
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found deleted-child-resource-not-found-messages apigatewayv2-http-lambda-proxy-route-operation-name-cleared-stage-throttling-zero-and-deployment
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|DeleteApi|2xx->2xx|$headers.content-length
Issue-Key: apigatewayv2|DeleteStage|2xx->2xx|$headers.content-length
Issue-Key: apigatewayv2|DeleteIntegration|2xx->2xx|$headers.content-length
Issue-Key: apigatewayv2|DeleteAuthorizer|2xx->2xx|$headers.content-length
Issue-Key: apigatewayv2|DeleteIntegrationResponse|2xx->2xx|$headers.content-length
Issue-Commit: 64b7111d560a56db8d780b5c63dd8a34d5bc25c3

# apigatewayv2 204 delete responses omit the Content-Length: 0 header

> _Note: this issue was identified against the code base at commit `64b7111d560a56db8d780b5c63dd8a34d5bc25c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteApi

## Description
This bundle carries 5 causes — one per apigatewayv2 delete operation whose 204 framing diverges. The fault is one missing piece of response framing in the apigatewayv2 delete handlers: the 204 No Content response carries no Content-Length header, where the recordings carry Content-Length: 0. An open issue, 20261029T220031Z-apigateway-204-responses-omit-the-content-length-0.md, records the same framing fault for the `apigateway` service token and owns those steps; the five steps below are the `apigatewayv2` ones, which that issue does not carry a replay key for.

AWS (common to all five): Smithy apigatewayv2 models each of these deletes with output Unit and http code 204 (DeleteApi DELETE /v2/apis/{ApiId}; DeleteStage; DeleteIntegration; DeleteAuthorizer; DeleteIntegrationResponse). The recorded responses are 204 with an explicit Content-Length: 0 header and an empty body.
Emulator (common to all five): status and body are right — 204 with no body — but the Content-Length header is absent entirely.
Why (common to all five): a client or proxy that reads Content-Length to decide a response is fully framed sees no length header on a successful delete; strict HTTP intermediaries and SDK response-framing assertions treat the absent header differently from an explicit zero, so a delete that succeeded can look truncated or indeterminate.
Fix (common to all five): the 204 response framing in the apigatewayv2 delete handlers' serializer — emit Content-Length: 0 on an empty 204 rather than omitting the header.

Cause 1 of 5 — DeleteApi. Traces: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#14 (cluster also covers apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#19 and deleted-child-resource-not-found-messages#task#33).
Cause 2 of 5 — DeleteStage. Traces: apigatewayv2-http-lambda-proxy-route-operation-name-cleared-stage-throttling-zero-and-deployment#task#17 (cluster also covers apigatewayv2-websocket-integration-empty-template-parameter-cleared-stage-tags-and-not-found#task#17 and deleted-child-resource-not-found-messages#task#14).
Cause 3 of 5 — DeleteIntegration. Traces: deleted-child-resource-not-found-messages#task#6.
Cause 4 of 5 — DeleteAuthorizer. Traces: deleted-child-resource-not-found-messages#task#20.
Cause 5 of 5 — DeleteIntegrationResponse. Traces: deleted-child-resource-not-found-messages#task#26.

## Evidence
All five clusters report status 204 expected and 204 actual, with the single diff $headers.content-length: missing.
Cause 1 DeleteApi — apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#14: $headers.content-length missing (expected a shaped value, got None).
Cause 2 DeleteStage — apigatewayv2-http-lambda-proxy-route-operation-name-cleared-stage-throttling-zero-and-deployment#task#17: $headers.content-length missing.
Cause 3 DeleteIntegration — deleted-child-resource-not-found-messages#task#6: $headers.content-length missing.
Cause 4 DeleteAuthorizer — deleted-child-resource-not-found-messages#task#20: $headers.content-length missing.
Cause 5 DeleteIntegrationResponse — deleted-child-resource-not-found-messages#task#26: $headers.content-length missing.

## Steps to Reproduce
1. Replay fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags and inspect the raw response headers of step task#14 (DeleteApi): expect 204 with Content-Length: 0, observe 204 with no Content-Length header.
2. Replay fixture apigatewayv2-http-lambda-proxy-route-operation-name-cleared-stage-throttling-zero-and-deployment, step task#17 (DeleteStage) — same check.
3. Replay fixture deleted-child-resource-not-found-messages and check steps task#6 (DeleteIntegration), task#20 (DeleteAuthorizer) and task#26 (DeleteIntegrationResponse) — same check on each.
