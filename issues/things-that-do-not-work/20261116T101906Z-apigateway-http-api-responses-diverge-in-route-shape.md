Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-import-api-basepath-split
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-import-api-basepath-split apigatewayv2-import-export-reimport-api-round-trip apigatewayv2-http-api-route-config-cleanup apigatewayv2-route-request-parameter-lifecycle apigatewayv2-vpc-link-errors
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|GetRoutes|2xx->2xx|$.items[*].authorizationScopes
Issue-Key: apigateway|GetRoutes|2xx->2xx|$.items[*].requestParameters
Issue-Key: apigateway|DeleteRouteRequestParameter|2xx->2xx|$headers.content-length
Issue-Key: apigateway|CreateVpcLink|4xx->4xx|$.message
Issue-Commit: f42da0d4917d7016500e6277673593e18f63e86f

# apigateway HTTP API responses diverge in route shape, 204 framing and VPC link refusal wording

> _Note: this issue was identified against the code base at commit `f42da0d4917d7016500e6277673593e18f63e86f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRoutes

## Description
Five net-new replay clusters on the apigateway (API Gateway V2 / HTTP API) surface. They are three distinct emulator defects: the Route representation drops empty collection members, the DeleteRouteRequestParameter 204 is framed without Content-Length, and CreateVpcLink's empty-SubnetIds refusal is worded by the emulator instead of by AWS. All five causes must be closed before this issue is submitted.

Cause 1 of 5: GetRoutes items omit authorizationScopes and requestParameters when those collections are empty (imported HTTP API).
AWS: Smithy: GetRoutes -> GetRoutesResponse.Items (jsonName `items`, list of Route); Route.AuthorizationScopes (jsonName `authorizationScopes`, list AuthorizationScopes) and Route.RequestParameters (jsonName `requestParameters`, map RouteParameters). Recorded behavior: AWS serializes BOTH members on every route item even when the route carries no scopes and no request parameters, as `"authorizationScopes": []` and `"requestParameters": {}` — the members are always present, their contents come from the route's own configuration.
Emulator: both members are absent from each `items[*]` entry; the empty list/map is skipped during serialization.
Why: a client that reads `route["requestParameters"]` or iterates `route["authorizationScopes"]` — which is safe against AWS because the members are always present — raises KeyError / NoSuchKey against the emulator, so route-configuration readers and any diffing logic break on exactly the common case of an unconfigured route.
Fix: the GetRoutes response serializer / the shared Route representation — emit empty lists and maps rather than omitting empty collections.
Traces: apigatewayv2-import-api-basepath-split#task#2 (same cluster also covers apigatewayv2-import-api-basepath-split#task#4 and #task#6).

Cause 2 of 5: the same two Route members are omitted on the import/export round-trip read.
AWS: as cause 1 — Route.AuthorizationScopes and Route.RequestParameters are serialized as `[]` / `{}` for a route with none.
Emulator: `$.items[0].authorizationScopes` and `$.items[0].requestParameters` missing from the GetRoutes page served after ImportApi.
Why: as cause 1.
Fix: as cause 1 (same serializer path, exercised through the ImportApi-created route set).
Traces: apigatewayv2-import-export-reimport-api-round-trip#task#2.

Cause 3 of 5: GetRoutes omits requestParameters after ReimportApi.
AWS: as cause 1 — Route.RequestParameters is present as `{}` on a route that declares no request parameters.
Emulator: `$.items[0].requestParameters` missing on the GetRoutes page read after the API is reimported.
Why: as cause 1.
Fix: as cause 1.
Traces: apigatewayv2-import-export-reimport-api-round-trip#task#6.

Cause 4 of 5: DeleteRouteRequestParameter answers 204 with no Content-Length header.
AWS: Smithy: DeleteRouteRequestParameter http {method DELETE, uri /v2/apis/{ApiId}/routes/{RouteId}/requestparameters/{RequestParameterKey}, code 204}, output shape Unit (no body); modeled errors NotFoundException (404), TooManyRequestsException (429). The header itself is recorded behavior only: the recorded 204 carries `Content-Length: 0`, i.e. explicit zero-length body framing on the empty success response.
Emulator: the status 204 is correct, but the `content-length` header is absent from the response entirely.
Why: a client or intermediary that frames the response body from Content-Length (and SDKs/proxies that treat a missing Content-Length on a non-chunked response as an unframed body) must fall back to connection-close semantics or report a malformed response, so a successful delete can surface as a transport error and keep-alive reuse is lost.
Fix: the shared REST-JSON empty-body (204) response serializer for this operation — emit `Content-Length: 0` instead of omitting the header.
Note: the open issues `20261029T220031Z-apigateway-204-responses-omit-the-content-length-0.md` and `20261031T154120Z-apigateway-content-length-0-missing-on-204-deletes-for.md` report the same fault on other operations (DeleteApiMapping, DeleteAuthorizer, DeleteCorsConfiguration, DeleteRouteSettings, DeleteIntegrationResponse, DeleteModel, DeletePortalProduct, DeleteRouteResponse) and own those replay keys; DeleteRouteRequestParameter is a different operation with its own replay key and is therefore filed here.
Traces: apigatewayv2-http-api-route-config-cleanup#task#6 (cluster also covers apigatewayv2-route-request-parameter-lifecycle#task#5).

Cause 5 of 5: CreateVpcLink's refusal of an empty subnetIds list is worded by the emulator, not by AWS.
AWS: Smithy: CreateVpcLink http {POST /v2/vpclinks, code 201}; CreateVpcLinkRequest.SubnetIds (jsonName `subnetIds`, list SubnetIdList) is required; modeled error BadRequestException (httpError 400, member `message`). SubnetIdList carries no @length constraint, so the non-empty rule is recorded behavior only: AWS refuses an empty list with 400 BadRequestException and the message `SubnetIds for a vpc link cannot be empty` — that text is entirely fixed, it interpolates no request or state value.
Emulator: answers the right status (400) but the message `Invalid value for SubnetIds: a VPC link must specify at least one subnet.` — emulator-authored prose in the generic "Invalid value for <member>" template, not AWS's sentence.
Why: a client that asserts on, logs, or branches on the refusal message (and any test or operator runbook matching AWS's text) sees a message AWS never emits, so the emulator mis-reports why the VPC link was rejected.
Fix: CreateVpcLink's input validation — refuse an empty subnetIds list with AWS's fixed message text; do not route it through the generic invalid-value message builder.
Note: the open issues `20261112T104410Z-apigateway-vpc-link-available-status-message-is-emulator-authored.md`, `20261113T111007Z-apigateway-getvpclinks-serves-emulator-authored-vpclinkstatusmessage-prose-for.md` and `20261113T220336Z-apigateway-getvpclinks-available-vpclinkstatusmessage-is-emulator-authored-prose.md` concern the vpcLinkStatusMessage of an AVAILABLE link, not this CreateVpcLink refusal, and do not carry this replay key.
Traces: apigatewayv2-vpc-link-errors#task#8.

## Evidence
Replay of corpus simparity-wire-aws-v1 (emulator up, /__admin/health 200): 5 net-new mismatch clusters, none carrying stub_path or owned_by.

Cause 1 — service=apigateway|GetRoutes|status=200->200: expected 200, actual 200; detail `$.items[0].authorizationScopes: missing (expected [], got None)`, `$.items[0].requestParameters: missing (expected {}, got None)`, `$.items[1].authorizationScopes: missing (expected [], got None)`, `$.items[1].requestParameters: missing (expected {}, got None)` (fixture apigatewayv2-import-api-basepath-split, steps task#2, task#4, task#6).

Cause 2 — service=apigateway|GetRoutes|status=200->200: detail `$.items[0].authorizationScopes: missing (expected [], got None)`, `$.items[0].requestParameters: missing (expected {}, got None)` (apigatewayv2-import-export-reimport-api-round-trip#task#2).

Cause 3 — service=apigateway|GetRoutes|status=200->200: detail `$.items[0].requestParameters: missing (expected {}, got None)` (apigatewayv2-import-export-reimport-api-round-trip#task#6).

Cause 4 — service=apigateway|DeleteRouteRequestParameter|status=204->204: expected 204, actual 204; detail `$headers.content-length: missing (expected '<shaped-not-asserted>', got None)` (apigatewayv2-http-api-route-config-cleanup#task#6, apigatewayv2-route-request-parameter-lifecycle#task#5).

Cause 5 — service=apigateway|CreateVpcLink|status=400->400: expected 400, actual 400; detail `$.message: value_mismatch (expected 'SubnetIds for a vpc link cannot be empty', got 'Invalid value for SubnetIds: a VPC link must specify at least one subnet.')` (apigatewayv2-vpc-link-errors#task#8). Only the message text diverges; status and error type match.

## Steps to Reproduce
Replay the wire corpus simparity-wire-aws-v1 against the emulator built from this checkout.

Cause 1: ImportApi an OpenAPI document that declares routes with no authorization scopes and no request parameters, then GET /v2/apis/{apiId}/routes (fixture apigatewayv2-import-api-basepath-split, step task#2) and inspect each `items[*]` entry — `authorizationScopes` and `requestParameters` are absent instead of `[]` and `{}`.
Cause 2: same read on the import/export round-trip fixture (apigatewayv2-import-export-reimport-api-round-trip#task#2).
Cause 3: ReimportApi the exported document, then GET the routes again (apigatewayv2-import-export-reimport-api-round-trip#task#6) — `requestParameters` is absent.
Cause 4: create an HTTP API route with a request parameter, then DELETE /v2/apis/{apiId}/routes/{routeId}/requestparameters/{key} (apigatewayv2-http-api-route-config-cleanup#task#6) and inspect the response headers — 204 arrives with no content-length.
Cause 5: POST /v2/vpclinks with `{"name": "<name>", "subnetIds": []}` (apigatewayv2-vpc-link-errors#task#8) and read the 400 body's `message`.
