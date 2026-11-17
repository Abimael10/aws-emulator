Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-create-route-jwt-authorization-validation
Issue-Step: task#10
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-create-route-jwt-authorization-validation apigatewayv2-route-response-non-default-key-rejected
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|CreateRoute|4xx->2xx|$.message
Issue-Key: apigatewayv2|CreateRoute|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: apigatewayv2|CreateRoute|4xx->2xx|status
Issue-Key: apigatewayv2|CreateRouteResponse|4xx->2xx|$.message
Issue-Key: apigatewayv2|CreateRouteResponse|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: apigatewayv2|CreateRouteResponse|4xx->2xx|status
Issue-Key: apigatewayv2|GetRouteResponses|2xx->2xx|$.items[*]
Issue-Commit: 64b7111d560a56db8d780b5c63dd8a34d5bc25c3

# apigatewayv2 create operations accept requests AWS refuses with 400 BadRequestException

> _Note: this issue was identified against the code base at commit `64b7111d560a56db8d780b5c63dd8a34d5bc25c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRoute

## Description
This bundle carries 3 causes. All three are the same emulator fault: the apigatewayv2 create handlers persist a resource whose shape API Gateway v2 refuses at validation time, so the refusal never happens and the stored collection ends up holding a resource AWS would never have created.

Cause 1 of 3 — CreateRoute with AuthorizationType=JWT and an authorizerId that is not a JWT authorizer of the API is accepted.
AWS: Smithy apigatewayv2#CreateRoute models errors BadRequestException (httpError 400), ConflictException (409), NotFoundException (404), TooManyRequestsException (429); input members AuthorizationType (AuthorizationType enum) and AuthorizerId (Id). The recorded response for this step is 400 with the REST-JSON error envelope: header x-amzn-errortype naming the error shape and body member `message` = "Invalid authorizer ID specified. Setting the authorization type to JWT requires a valid JWT authorizer." That whole sentence is FIXED text for this error class — it contains no value taken from the request or from state, so it must be reproduced verbatim, with the x-amzn-errortype header naming BadRequestException.
Emulator: answers 201 Created and mints the route. No `message` member and no x-amzn-errortype header are emitted at all.
Why: a client that relies on API Gateway rejecting a JWT route whose authorizerId does not name a JWT authorizer (a typo'd id, an id belonging to a REQUEST authorizer, an id from another API) gets a route that looks authorized but has no usable authorizer behind it; code that catches BadRequestException to fall back or report a configuration error never sees the error and proceeds on a broken route.
Fix: CreateRoute's input validation in the apigatewayv2 route handler — cross-check AuthorizerId against the API's authorizers and their type before inserting into state, and raise BadRequestException (400) with the message above.
Traces: apigatewayv2-create-route-jwt-authorization-validation#task#10.

Cause 2 of 3 — CreateRouteResponse with a RouteResponseKey other than $default is accepted.
AWS: Smithy apigatewayv2#CreateRouteResponse models errors BadRequestException (400), ConflictException (409), NotFoundException (404), TooManyRequestsException (429); RouteResponseKey targets SelectionKey and is required. The recorded response for this step is 400 with body member `message` = "Currently, only $default is supported as a RouteResponseKey. The creation of the RouteResponse with a key of $default will indicate to API Gateway that this Route is intended to handle two-way communication." and an x-amzn-errortype header. The whole message is fixed text for this error class — the only variable-looking token, `$default`, is the literal AWS names, not a request value — so it must be reproduced exactly.
Emulator: answers 201 Created and creates the route response with the client's non-$default key, emitting neither `message` nor x-amzn-errortype.
Why: WebSocket two-way communication is enabled only by a $default route response. An emulator that accepts an arbitrary key lets a client believe a non-$default route response does something, and hides the configuration mistake AWS reports at create time.
Fix: CreateRouteResponse's input validation in the apigatewayv2 route-response handler — reject any RouteResponseKey != "$default" with BadRequestException (400) and the message above, before writing state.
Traces: apigatewayv2-route-response-non-default-key-rejected#task#4.

Cause 3 of 3 — GetRouteResponses then lists the resource that should never have existed, and over-serializes its item.
AWS: Smithy apigatewayv2#GetRouteResponses returns items of RouteResponse (routeResponseId, routeResponseKey, modelSelectionExpression, responseModels, responseParameters). In the recording the listing's first item is exactly {routeResponseKey: "$default", routeResponseId: <minted id>} — the key is the $default one created earlier, and the empty responseModels/responseParameters maps are ABSENT, not serialized as {}.
Emulator: the first item is the invalid route response from cause 2 (routeResponseKey "custom") and it carries responseModels: {} and responseParameters: {} — two members the recording omits when the maps are empty.
Why: a client enumerating a route's responses to decide whether two-way communication is configured reads a key AWS would have rejected, and any code that tests member presence (`"responseParameters" in item`) to distinguish "unset" from "set empty" is misled on every item.
Fix: GetRouteResponses' serializer in the apigatewayv2 route-response handler (omit empty responseModels/responseParameters maps), plus the cause-2 validation that keeps the invalid entry out of state. Note the per-run route-response id in the recording is a generated value, not a requirement.
Traces: apigatewayv2-route-response-non-default-key-rejected#task#5.

## Evidence
Cause 1 — apigatewayv2-create-route-jwt-authorization-validation#task#10, CreateRoute: expected 400, got 201. Diff: status expected 400 got 201; $.message missing (expected 'Invalid authorizer ID specified. Setting the authorization type to JWT requires a valid JWT authorizer.', got None); $headers.x-amzn-errortype missing.
Cause 2 — apigatewayv2-route-response-non-default-key-rejected#task#4, CreateRouteResponse: expected 400, got 201. Diff: status expected 400 got 201; $.message missing (expected 'Currently, only $default is supported as a RouteResponseKey. The creation of the RouteResponse with a key of $default will indicate to API Gateway that this Route is intended to handle two-way communication.', got None); $headers.x-amzn-errortype missing.
Cause 3 — apigatewayv2-route-response-non-default-key-rejected#task#5, GetRouteResponses: 200 vs 200 but $.items[0] extra — expected {'routeResponseId': <id>, 'routeResponseKey': '$default'}, got {'responseModels': {}, 'responseParameters': {}, 'routeResponseId': <id>, 'routeResponseKey': 'custom'}.

## Steps to Reproduce
1. Replay fixture apigatewayv2-create-route-jwt-authorization-validation and compare step task#10 (CreateRoute with authorizationType=JWT and an authorizerId that is not a JWT authorizer of the API): expect 400 BadRequestException with the JWT-authorizer message, observe 201.
2. Replay fixture apigatewayv2-route-response-non-default-key-rejected and compare step task#4 (CreateRouteResponse with routeResponseKey other than $default): expect 400 BadRequestException with the $default-only message, observe 201.
3. In the same fixture compare step task#5 (GetRouteResponses): expect the single $default route response with no empty responseModels/responseParameters members, observe the 'custom' route response carrying both empty maps.
