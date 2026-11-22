Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags apigatewayv2-http-lambda-proxy-route-operation-name-cleared-stage-throttling-zero-and-deployment
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.apiEndpoint
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.apiId
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.apiKeySelectionExpression
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.createdDate
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.disableExecuteApiEndpoint
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.ipAddressType
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.name
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.protocolType
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.routeSelectionExpression
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|$.tags
Issue-Key: apigatewayv2|UpdateApi|2xx->4xx|status
Issue-Key: apigatewayv2|GetApi|2xx->2xx|$.description
Issue-Key: apigatewayv2|GetApi|2xx->2xx|$.disableExecuteApiEndpoint
Issue-Key: apigatewayv2|GetApi|2xx->2xx|$.version
Issue-Key: apigatewayv2|GetRoute|2xx->2xx|$.operationName
Issue-Commit: 64b7111d560a56db8d780b5c63dd8a34d5bc25c3

# apigatewayv2 empty-string field clears are rejected or stored verbatim instead of removing the member

> _Note: this issue was identified against the code base at commit `64b7111d560a56db8d780b5c63dd8a34d5bc25c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateApi

## Description
This bundle carries 3 causes. They share one emulator fault: apigatewayv2 update handlers do not implement API Gateway v2's "empty string clears the field" semantics — one handler refuses the request outright, the others keep the old value or store the empty string as a value.

Cause 1 of 3 — UpdateApi that sets the explicit default selection expressions and clears description/version with empty strings is refused 400.
AWS: Smithy apigatewayv2#UpdateApi (PATCH /v2/apis/{ApiId}) accepts optional ApiKeySelectionExpression and RouteSelectionExpression (SelectionExpression, no length constraint), Description (StringWithLengthBetween0And1024 — min length 0, so "" is a legal value), Version (StringWithLengthBetween1And64), DisableExecuteApiEndpoint; its errors are BadRequestException (400), ConflictException (409), NotFoundException (404), TooManyRequestsException (429). Recorded behavior for this step: the update is ACCEPTED and the response body echoes the API — apiId, name, protocolType, ipAddressType, routeSelectionExpression, apiKeySelectionExpression, apiEndpoint, createdDate, tags and disableExecuteApiEndpoint=false — with no description and no version member, i.e. both fields were cleared by the empty strings. (The recorded status on this step is 201; the model declares http code 200 for UpdateApi. The requirement this issue states is acceptance with the updated API body rather than a refusal.)
Emulator: answers 400 and serializes none of those members, so the whole update is lost.
Why: clearing an API's description/version and pinning the explicit default selection expressions is how a client normalizes an API; a 400 makes a legal, idempotent normalization look like a malformed request, and the API keeps its old configuration.
Fix: UpdateApi's input validation in the apigatewayv2 API handler — treat an empty-string Description/Version as "clear this member" instead of a constraint violation, and accept SelectionExpression values equal to the service defaults. Hostnames, ports, generated api ids and createdDate in the recording are environment values, not requirements.
Traces: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#3.

Cause 2 of 3 — GetApi still reports the pre-update description/version and the wrong disableExecuteApiEndpoint.
AWS: Smithy apigatewayv2#GetApi returns Description, Version and DisableExecuteApiEndpoint among its members. Recorded behavior: after the update, description and version are ABSENT from the API and disableExecuteApiEndpoint is false.
Emulator: description is still 'test description' and version still 'v1' (still present after 30 poll attempts, so this is settled state, not a propagation delay), and disableExecuteApiEndpoint is true where the recording has false.
Why: a client that clears a field and then reads it back to confirm sees the stale value forever; worse, disableExecuteApiEndpoint reads as true, so a caller concludes the default execute-api endpoint is disabled when the recording shows it enabled.
Fix: the API state mutation behind UpdateApi (and the GetApi serializer's omission of cleared members) in the apigatewayv2 API handler — a cleared member must be removed from state, not left at its old value, and the boolean must take the updated value.
Traces: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#4.

Cause 3 of 3 — a route's operationName cleared with an empty string comes back as an empty string instead of being removed.
AWS: Smithy apigatewayv2#GetRoute returns OperationName (StringWithLengthBetween1And64) — a 1..64 string, so the empty string is not a representable value of that member. Recorded behavior: after the route's operationName is cleared, the member is ABSENT from GetRoute's response.
Emulator: GetRoute returns operationName as '' and keeps returning it across 30 polls.
Why: a client reading `route.get('operationName')` gets a falsy-but-present value where AWS gives absence; anything that round-trips the route back into UpdateRoute now sends a zero-length value for a member whose modeled minimum length is 1.
Fix: the route state mutation and GetRoute serializer in the apigatewayv2 route handler — clearing OperationName must delete the member rather than store "".
Traces: apigatewayv2-http-lambda-proxy-route-operation-name-cleared-stage-throttling-zero-and-deployment#task#10.

## Evidence
Cause 1 — apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#3, UpdateApi: expected 201, got 400; $.apiId, $.name, $.protocolType, $.ipAddressType, $.routeSelectionExpression, $.apiKeySelectionExpression, $.apiEndpoint, $.createdDate, $.tags and $.disableExecuteApiEndpoint (expected False) are all missing from the emulator's response.
Cause 2 — same fixture, step task#4, GetApi: 200 vs 200 but $.disableExecuteApiEndpoint value_mismatch (expected False, got True); $.description unexpected_present (expected None, got 'test description', still present after 30 poll attempts); $.version unexpected_present (expected None, got 'v1').
Cause 3 — apigatewayv2-http-lambda-proxy-route-operation-name-cleared-stage-throttling-zero-and-deployment#task#10, GetRoute: 200 vs 200 but $.operationName poll: still present after 30 attempts (expected None, got '').

## Steps to Reproduce
1. Replay fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags. Step task#3 PATCHes /v2/apis/{apiId} setting the explicit default routeSelectionExpression/apiKeySelectionExpression, clearing description and version with empty strings and setting disableExecuteApiEndpoint=false: expect acceptance with the API body, observe 400.
2. In the same fixture, step task#4 GETs /v2/apis/{apiId}: expect no description, no version and disableExecuteApiEndpoint=false, observe the old description/version and true.
3. Replay fixture apigatewayv2-http-lambda-proxy-route-operation-name-cleared-stage-throttling-zero-and-deployment and compare step task#10 (GetRoute after operationName is cleared with an empty string): expect the operationName member absent, observe ''.
