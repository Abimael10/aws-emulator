Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.parameter-mapping-rewrites-a-real-request-and-response apigatewayv2.route-throttling-returns-429-while-twin-route-serves apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.parameter-mapping-rewrites-a-real-request-and-response|run#40|AssertionError|assert injected and len(injected) >= 8, (
Issue-Key: apigatewayv2.route-throttling-returns-429-while-twin-route-serves|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Key: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore|_wait_status#24|Timeout|time.sleep(interval_s)
Issue-Commit: fcaef5045051e2f19f2ef72b682687f38c3f2a70

# apigatewayv2: 3 net-new causes — HTTP_PROXY request parameter mapping is never applied at the origin, per-route throttling never sheds a burst with 429, and a disabled default execute-api endpoint never answers 404 {"message":"Not Found"}

> _Note: this issue was identified against the code base at commit `fcaef5045051e2f19f2ef72b682687f38c3f2a70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateIntegration

## Description
Three net-new root causes in the apigatewayv2 execute-api data plane. A fixer must close ALL THREE before submitting.

=== Cause 1 of 3: HTTP_PROXY integration RequestParameters (parameter mapping) is stored but never applied to the request the integration receives. Failing check: apigatewayv2.parameter-mapping-rewrites-a-real-request-and-response.

AWS: Smithy (com.amazonaws.apigatewayv2#ApiGatewayV2): CreateIntegration (POST /v2/apis/{ApiId}/integrations, 201) takes RequestParameters (target IntegrationParameters, jsonName requestParameters, a map of __string -> StringWithLengthBetween1And512) and ResponseParameters (target ResponseParameters, jsonName responseParameters, map of status-code -> IntegrationParameters, documented 'Supported only for HTTP APIs. You use response parameters to transform the HTTP response from a backend integration before returning the response to clients'); GetIntegration reports both back. The data-plane effect is recorded behavior only (the execute-api data plane is not modelled in Smithy): for an HTTP API with a non-proxy/HTTP_PROXY integration, each RequestParameters key is '<op>:<location>.<name>' where <op> is overwrite|append|remove and <location> is header|querystring|path, and the gateway applies it to the request BEFORE it leaves for the integration — 'overwrite:header.<name>' sets that header to the mapping's value (a static value or a $context/$request variable such as $context.requestId), 'remove:header.<name>' strips exactly that one named header and leaves every other client header intact, and 'append:querystring.<name>' adds that query parameter to the request the origin receives. An unmapped twin route to the same integration target shows none of these effects.

Emulator: the response-side mapping is honoured (the client-visible status was rewritten 200->201 and x-sim2real-mapped was appended), but the request-side mapping is not: the origin reports NO x-sim2real-injected header at all. Replay detail: 'AssertionError: the origin must receive x-sim2real-injected populated from $context.requestId, got None (the client never sent this header)'. GetIntegration had already echoed RequestParameters['overwrite:header.x-sim2real-injected'] == '$context.requestId', so the mapping is persisted and merely never evaluated on the forwarding path (the remove: and append:querystring: forms are on the same unevaluated path and are not reached by the check once the injection assertion fires).

Why: a client that relies on parameter mapping to inject correlation/identity headers, to strip a client-supplied header before it reaches a backend, or to add a required query parameter gets a request the backend sees unmodified. Backends that authenticate or correlate on an injected header fail, and headers the mapping was supposed to remove are forwarded to the origin — a silent data leak rather than an error.

Fix: the execute-api front door's HTTP_PROXY forwarding path — the stage that evaluates an integration's RequestParameters (overwrite/append/remove over header, querystring and path, including $context variable resolution) against the outbound request, alongside the already-working ResponseParameters evaluation.

Traces: apigatewayv2.parameter-mapping-rewrites-a-real-request-and-response.

=== Cause 2 of 3: a per-route throttle on a stage never sheds any part of an over-limit burst with 429, so the throttling leg reaches no verdict. Failing check: apigatewayv2.route-throttling-returns-429-while-twin-route-serves.

AWS: Smithy: CreateStage (POST /v2/apis/{ApiId}/stages, 201) takes RouteSettings (RouteSettingsMap, jsonName routeSettings) and DefaultRouteSettings (RouteSettings, jsonName defaultRouteSettings); RouteSettings carries ThrottlingRateLimit (__double, 'Specifies the throttling rate limit') and ThrottlingBurstLimit (__integer, 'Specifies the throttling burst limit'); DeleteRouteSettings (DELETE /v2/apis/{ApiId}/stages/{StageName}/routesettings/{RouteKey}, 204) removes one route's override. The enforcement is recorded behavior only: a route whose settings cap it far below the offered rate has part of the burst shed by the gateway with HTTP 429 and the gateway's 'Too Many Requests' envelope body (that text is fixed; no value from the request appears in it), shed BEFORE the integration runs so no backend body appears in a throttled response, while the remainder of the burst is served normally (200 from the integration). The limit is scoped to the one RouteKey: a twin route on the same stage and integration under the generous DefaultRouteSettings sheds nothing in the same instant. After DeleteRouteSettings the same burst against that route sheds nothing.

Emulator: the check reaches no verdict — replay detail 'Error: Check timed out after 60.0s'. The check's data-plane legs poll for the two routes serving and then fire an interleaved burst expecting at least one 429 on the limited route; no settled state the check waits for is reached inside the harness budget, so neither the shedding nor its per-route scoping nor the post-DeleteRouteSettings restoration is observable. Hypothesis: the front door has no per-route token bucket at all, so every request is served and the state the check polls toward never appears.

Why: a client cannot verify that a per-route throttle protects a backend, and cannot distinguish 'throttled by the gateway' (429 + Too Many Requests, integration not invoked) from a backend error. Load-shedding and quota-regression tests pass against the emulator while the same configuration sheds traffic on AWS.

Fix: the execute-api front door's rate-limiting stage — per-route and per-stage token buckets derived from a stage's RouteSettings/DefaultRouteSettings, the 429 'Too Many Requests' refusal emitted before the integration is invoked, and the removal of an override by DeleteRouteSettings taking effect on a live AutoDeploy stage.

Traces: apigatewayv2.route-throttling-returns-429-while-twin-route-serves.

=== Cause 3 of 3: an HTTP API whose default execute-api endpoint is disabled never answers the documented 404 refusal, so the disable/restore transition reaches no verdict. Failing check: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore. This check is NOT marked owned by any open issue in this replay; the open issue 20261122T132825Z-apigatewayv2-an-http-api-with-disableexecuteapiendpoint-true-never.md describes the same cause and names related prior art (20261114T024946Z-apigatewayv2-3-net-new-causes-a-disabled-execute.md, 20261112T190000Z-apigatewayv2-the-disableexecuteapiendpoint-check-still-hangs-on-the-front-door-upgrade.md) — it may be closed together with this cause.

AWS: Smithy: UpdateApi (PATCH /v2/apis/{ApiId}, 200) takes DisableExecuteApiEndpoint (__boolean, jsonName disableExecuteApiEndpoint) and echoes it; GetApi reports the same member, so the flag is readable per-API state. UpdateApi's modelled errors (BadRequestException 400, ConflictException 409, NotFoundException 404, TooManyRequestsException 429) do not apply — the control-plane call succeeds. The data-plane consequence is recorded behavior only: with the flag set, any request to that API's default execute-api URL is refused with HTTP 404 and the body {"message":"Not Found"} — entirely fixed text, carrying no value from the request or state — and the integration is NOT invoked (nothing from a refused request reaches the backend or its log). The refusal is scoped to the one API: a twin API with the flag clear keeps answering 200 with its backend's body in the same instant. Clearing the member returns the same URL to 200 from the integration.

Emulator: no verdict — replay detail 'Error: Check timed out after 60.0s'. After the successful UpdateApi the check polls the target URL for a settled run of 404s, and after re-enabling for a settled run of 200s; neither run materialises inside the harness budget. Hypothesis: the front door's disabled-endpoint gate answers a different refusal class than 404 {"message":"Not Found"} (an earlier filing records 403 {"message":"Forbidden"} from this gate), so the status the poll waits for never appears.

Why: a client cannot tell that disabling an API's default endpoint took effect as AWS defines it, nor that re-enabling restored it. Endpoint-lockdown and custom-domain-only cutovers key on 404/{"message":"Not Found"} to separate 'default endpoint off' from an authorization failure or a missing route.

Fix: the execute-api front door's DisableExecuteApiEndpoint gate for HTTP APIs — the status and envelope it answers, refusing before invoking the integration, its per-API scoping, and the path back to serving when UpdateApi clears the member.

Traces: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore.

## Evidence
Cause 1 — check apigatewayv2.parameter-mapping-rewrites-a-real-request-and-response, verdict mismatch. Expected (AWS): with a front HTTP API route 'GET /mapped' on an HTTP_PROXY integration carrying RequestParameters {'overwrite:header.x-sim2real-injected': '$context.requestId', 'remove:header.x-sim2real-client': "''", 'append:querystring.injected': 'static-q'}, the ORIGIN sees the injected header populated with the gateway's request id (a per-run value; only its presence and non-client origin are the requirement), does NOT see the removed header, still sees every other client header, and sees the appended query parameter; the unmapped twin route shows none of these. Actual (emulator): the origin echo shows x-sim2real-injected = None — 'AssertionError: the origin must receive x-sim2real-injected populated from $context.requestId, got None (the client never sent this header)' — while GetIntegration echoes the mapping as authored and the response-side mapping (200 -> 201, appended x-sim2real-mapped) is applied.

Cause 2 — check apigatewayv2.route-throttling-returns-429-while-twin-route-serves, verdict mismatch, detail 'Error: Check timed out after 60.0s'. Expected (AWS): on a $default AutoDeploy stage with DefaultRouteSettings {ThrottlingRateLimit 500.0, ThrottlingBurstLimit 200} and RouteSettings {'GET /limited': {ThrottlingRateLimit 1.0, ThrottlingBurstLimit 1}}, an interleaved burst sheds at least one request on GET /limited with HTTP 429 whose body contains 'Too Many Requests' and never the backend's body, serves the rest as 200 from the backend, sheds nothing on the twin GET /twin in the same instant, and sheds nothing on GET /limited after DeleteRouteSettings. Actual (emulator): no verdict inside the harness cap — no 429 is ever observed and none of the polled states is reached. The 429 status and the 'Too Many Requests' envelope are the requirement; the generated marker, api ids and exact shed counts are environment values.

Cause 3 — check apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore, verdict mismatch, detail 'Error: Check timed out after 60.0s'. Expected (AWS): after UpdateApi(DisableExecuteApiEndpoint=True) and GetApi reporting true on the target and false on the twin, every GET of the target's execute-api URL answers 404 with body {"message":"Not Found"} (fixed text), no refused request's nonce ever appears in the backend log, the twin answers 200 with the backend body, and after UpdateApi(DisableExecuteApiEndpoint=False) the same URL answers 200 again with the request visible in the backend log. Actual (emulator): the settled run of 404s is never answered, so the restore leg is never reached; api ids, marker and nonces are per-run values, not part of the contract.

## Steps to Reproduce
Cause 1 (parameter mapping):
1. Boot the emulator. Create a Lambda function whose handler echoes back event['headers'], event['queryStringParameters'] and a MARKER environment variable.
2. Create an origin HTTP API with an AWS_PROXY integration (PayloadFormatVersion 2.0) to that function, route 'GET /origin', $default stage with AutoDeploy=True; confirm its invoke URL serves 200 with the marker.
3. Create a front HTTP API with two HTTP_PROXY integrations to the origin's invoke URL (IntegrationMethod GET, PayloadFormatVersion 1.0): one with RequestParameters {'overwrite:header.x-sim2real-injected': '$context.requestId', 'remove:header.x-sim2real-client': "''", 'append:querystring.injected': 'static-q'} and ResponseParameters {'200': {'overwrite:statuscode': '201', 'append:header.x-sim2real-mapped': 'mapped-static'}}, and one with no parameters. Route them 'GET /mapped' and 'GET /plain'; create a $default AutoDeploy stage.
4. GET /plain and GET /mapped with client headers X-Sim2Real-Client and X-Sim2Real-Keep. Compare what the origin echoes for both: AWS shows the injection, the removal (scoped to the one header) and the appended query only on /mapped.
5. Re-replay apigatewayv2.parameter-mapping-rewrites-a-real-request-and-response.

Cause 2 (per-route throttling):
1. Create an HTTP API with one AWS_PROXY integration to a marker-echo Lambda and two routes, 'GET /limited' and 'GET /twin'.
2. CreateStage '$default' with AutoDeploy=True, DefaultRouteSettings {ThrottlingRateLimit 500.0, ThrottlingBurstLimit 200} and RouteSettings {'GET /limited': {ThrottlingRateLimit 1.0, ThrottlingBurstLimit 1}}; confirm GetStage reports the override on 'GET /limited' only.
3. Fire an interleaved burst of ~24 requests at each route and record statuses and bodies: AWS answers 429 with a 'Too Many Requests' body for part of the /limited burst (no backend body in those) and 200 for the rest, and 200 for every /twin request.
4. Call DeleteRouteSettings(ApiId, StageName='$default', RouteKey='GET /limited') and repeat the burst: nothing is shed.
5. Re-replay apigatewayv2.route-throttling-returns-429-while-twin-route-serves.

Cause 3 (disabled default endpoint):
1. Create two HTTP APIs ('target' and 'twin'), each with an AWS_PROXY integration to the same marker-echo Lambda, route 'GET /hit', $default AutoDeploy stage; confirm both serve 200 with the marker.
2. UpdateApi(ApiId=target, DisableExecuteApiEndpoint=True); confirm GetApi reports true for target, false for twin.
3. GET the target URL repeatedly: expect a settled HTTP 404 with body {"message":"Not Found"} and no backend invocation, while the twin still serves 200.
4. UpdateApi(ApiId=target, DisableExecuteApiEndpoint=False) and GET again: expect a settled 200 with the marker and the request visible in the backend log.
5. Re-replay apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore.
