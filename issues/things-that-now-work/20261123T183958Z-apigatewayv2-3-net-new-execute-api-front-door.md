Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-cors-lambda-default-route-serves-then-destroys apigatewayv2.route-throttling-returns-429-while-twin-route-serves
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore|_wait_status#24|Timeout|time.sleep(interval_s)
Issue-Key: apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-cors-lambda-default-route-serves-then-destroys|_until_gone#37|AssertionError|raise AssertionError(
Issue-Key: apigatewayv2.route-throttling-returns-429-while-twin-route-serves|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Commit: 7b09e9bd4e85e10a661e0eed3332c336e107cfb0

# apigatewayv2: 3 net-new execute-api front-door causes — the DisableExecuteApiEndpoint 404 gate, a deleted API's URL answering 403 instead of 404, and per-route throttling never producing 429 (3 causes)

> _Note: this issue was identified against the code base at commit `7b09e9bd4e85e10a661e0eed3332c336e107cfb0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateApi

## Description
Three net-new root causes in the apigatewayv2 execute-api front door. A fixer must close ALL THREE before submitting; each cause names its own failing checks.

Cause 1 of 3: an HTTP API whose default execute-api endpoint was disabled with UpdateApi(DisableExecuteApiEndpoint=true) never answers the documented refusal on that URL, so neither the disabled state nor the restore back to serving is observable. Failing check: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore.
AWS: Smithy (com.amazonaws.apigatewayv2#ApiGatewayV2): UpdateApi (PATCH /v2/apis/{ApiId}, 200) accepts DisableExecuteApiEndpoint (__boolean, jsonName disableExecuteApiEndpoint) and echoes it in UpdateApiResponse; GetApi reports the same member, so the flag is per-API state readable right after the update. UpdateApi's modelled errors are BadRequestException 400, ConflictException 409, NotFoundException 404, TooManyRequestsException 429 — none applies, the control-plane call succeeds. The data-plane consequence is recorded behavior only (the execute-api data plane is not modelled): with the flag set, a request to that API's default execute-api URL is refused with HTTP 404 and the gateway envelope body {"message":"Not Found"} — that body is entirely fixed text, it carries no value from the request or from state — and the integration is NOT invoked, so nothing from the refused request reaches the backend. The refusal is scoped to the single API: a twin API on the same backend with the flag clear keeps answering 200 in the same instant. Clearing the member returns the same URL to 200 from the integration.
Emulator: no verdict — the replay detail is 'Error: Check timed out after 60.0s'. The first data-plane beat after the update polls the target URL for a settled run of 404s and that run never appears inside the harness budget, so the restore leg is never reached. Hypothesis: the disabled-endpoint gate answers a different refusal class than 404 {"message":"Not Found"} (the open issue 20261112T190000Z-apigatewayv2-the-disableexecuteapiendpoint-check-still-hangs-on-the-front-door-upgrade.md records this gate refusing 403 {"message":"Forbidden"}), so the status the poll waits for never arrives.
Why: a client cannot tell that disabling an API's default endpoint took effect as AWS defines it, nor that re-enabling restored it. Endpoint-lockdown and custom-domain-only cutovers key on 404/{"message":"Not Found"} to distinguish 'default endpoint off' from an authorization failure or a missing route.
Fix: the execute-api front door's DisableExecuteApiEndpoint gate for HTTP APIs — the status and envelope it answers, refusing before invoking the integration, its per-API scoping, and the path that returns the URL to serving when the member is cleared.
Prior art, named rather than relied on: 20261122T132825Z-apigatewayv2-an-http-api-with-disableexecuteapiendpoint-true-never.md carries this check id in its Issue-Trace-Ids but this replay did NOT mark the check OWNED by it (its recorded replay key no longer matches the frame the check times out in), so the check is re-filed here rather than dropped; a fixer may close the two together. Also related and keyed to other checks: 20261110T153046Z-apigatewayv2-iam-lambda-logs-sts-updateapi-disableexecuteapiendpoint-true.md, 20261123T144652Z-apigatewayv2-iam-lambda-logs-sts-the-disableexecuteapiendpoint-manual.md.
Traces: apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore.

Cause 2 of 3: once an HTTP API has been deleted (here by a CloudFormation/CDK stack delete), its execute-api invoke URL keeps answering 403 {"message":"Forbidden"} instead of retiring the endpoint. Failing check: apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-cors-lambda-default-route-serves-then-destroys.
AWS: recorded behavior only for the data plane; the control-plane side is modelled — after the stack delete, GetApi on the removed api id raises NotFoundException (httpError 404) and GetFunction raises ResourceNotFoundException, which the check observed as expected. On AWS the deleted API's invoke URL then either stops resolving (its <api-id>.execute-api.<region>.amazonaws.com name is retired) or answers HTTP 404 with the gateway envelope {"message":"Not Found"} — fixed text, no variable part — and never serves the backend's body again.
Emulator: the poll for that retirement fails with 'AssertionError: GET <invoke-url>/ neither answered 404 nor stopped resolving within 60.0s; last 403 \'{"message":"Forbidden"}\''. So after the API is gone from the control plane, the front door still answers HTTP 403 with error envelope body {"message":"Forbidden"} — wrong status and wrong fixed message text for an unknown/retired api id.
Why: a client (and every teardown-verification script, blue/green cutover and CDK/CFN destroy test) cannot distinguish 'this API no longer exists' from 'you are not allowed to call it'. 403 is a retryable-looking authorization signal, so callers keep credentials-debugging an endpoint that is simply gone, and a test that asserts the documented 404 retirement fails.
Fix: the execute-api front door's unknown-API/unknown-stage dispatch path — the status and envelope it answers when the request's api id (or stage) resolves to no live API must be 404 {"message":"Not Found"}, not 403 {"message":"Forbidden"}. Related open issue, named not relied on: 20261110T170829Z-apigatewayv2-9-net-new-causes-aws-iam-routes.md records the sibling 'missing-stage path answers 403 not 404'; this check shows the same wrong class for a DELETED api.
Traces: apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-cors-lambda-default-route-serves-then-destroys.

Cause 3 of 3: a per-route throttle declared in a stage's RouteSettings is never observable — the route-throttling scenario reaches no verdict at all. Failing check: apigatewayv2.route-throttling-returns-429-while-twin-route-serves.
AWS: Smithy: CreateStage (POST /v2/apis/{ApiId}/stages, 201) accepts RouteSettings (RouteSettingsMap, jsonName routeSettings) and DefaultRouteSettings (RouteSettings), whose members ThrottlingRateLimit (__double, 'Specifies the throttling rate limit') and ThrottlingBurstLimit (__integer, 'Specifies the throttling burst limit') are the per-route override; GetStage echoes RouteSettings keyed by route key, and DeleteRouteSettings (DELETE /v2/apis/{ApiId}/stages/{StageName}/routesettings/{RouteKey}, 204) removes one override. The data-plane consequence is recorded behavior only: requests over the limited route's rate/burst are shed by the gateway BEFORE the integration runs, answered HTTP 429 with the gateway's 'Too Many Requests' envelope (fixed text; no request or state value in it), while the non-throttled part of the same burst still answers 200 from the integration and an interleaved burst on a twin route of the same stage carrying only the generous default settings sheds nothing. After DeleteRouteSettings, the same burst on the formerly limited route sheds nothing.
Emulator: no verdict — the replay detail is 'Error: Check timed out after 60.0s', with no frame reported, so the whole per-route-throttle contract (the 429 class, its envelope, shedding before the integration, per-route scoping, and the post-DeleteRouteSettings return to unthrottled) is ungradeable. Hypothesis: the scenario stalls before the burst, in the beat that polls both routes' AWS_PROXY invoke URLs for a settled 200 carrying the backend marker — the same data-plane non-service recorded by open issues 20261027T053233Z-apigatewayv2-an-http-api-aws-proxy-route-still.md and 20261025T030504Z-apigatewayv2-lambda-an-aws-proxy-route-cannot-answer-200-until-a.md; neither the model nor the recording states a cause beyond that.
Why: a client that provisions a per-route throttle gets no enforcement signal and no 429, so rate-limit handling (back-off on 429, per-route capacity planning, 'is this route the one being shed') cannot be exercised against the emulator, and a throttle that silently does nothing lets load tests pass that AWS would shed.
Fix: the execute-api front door's request-admission stage for HTTP APIs — evaluating the stage's per-route RouteSettings throttle (rate and burst) ahead of integration dispatch, the 429 'Too Many Requests' response it emits, its per-route scoping against the stage's DefaultRouteSettings, and the removal path when DeleteRouteSettings drops the override — together with whatever keeps the AWS_PROXY route from reaching a settled 200 in this scenario.
Traces: apigatewayv2.route-throttling-returns-429-while-twin-route-serves.

## Evidence
Cause 1 — check apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore, verdict mismatch, detail 'Error: Check timed out after 60.0s'.
Expected (AWS): after UpdateApi(ApiId=<target>, DisableExecuteApiEndpoint=True) succeeds and GetApi reports true on the target / false on the twin, every GET of the target's execute-api URL answers 404 with body {"message":"Not Found"} (entirely fixed text) and no backend body; the twin's URL answers 200 with the backend body in the same instant; no refused request appears in the backend's log; after UpdateApi(DisableExecuteApiEndpoint=False) the same URL answers a settled 200 and the request does reach the backend.
Actual (emulator): no verdict inside the harness cap — the settled run of 404s on the disabled URL never appears, so the restore leg is never reached. The 404 status and the {"message":"Not Found"} envelope are the requirement; the api ids, generated marker and per-request nonces in this run are environment values.

Cause 2 — check apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-cors-lambda-default-route-serves-then-destroys, verdict mismatch, detail: 'File "check_apigatewayv2_deploy_apigatewayv2_3_cdk_http_api_cors_lambda_default_route_serves_then_destroys.py", line 379, in _until_gone / AssertionError: GET https://<host>/_aws/execute-api/<api-id>/ neither answered 404 nor stopped resolving within 60.0s; last 403 \'{"message":"Forbidden"}\''.
Expected (AWS): after the stack is deleted (DescribeStacks ValidationError '... does not exist', GetApi NotFoundException, GetFunction ResourceNotFoundException — all three observed as expected), the API's invoke URL either stops resolving or answers HTTP 404 with body {"message":"Not Found"} and never the backend's body.
Actual (emulator): HTTP 403 with body {"message":"Forbidden"} for the whole 60 s window. Both the status (403 vs 404) and the envelope's fixed message text ('Forbidden' vs 'Not Found') diverge; the host, port and api id in the URL are per-run values.

Cause 3 — check apigatewayv2.route-throttling-returns-429-while-twin-route-serves, verdict mismatch, detail 'Error: Check timed out after 60.0s'.
Expected (AWS): with a $default AutoDeploy stage carrying DefaultRouteSettings {ThrottlingRateLimit: 500.0, ThrottlingBurstLimit: 200} and RouteSettings {'GET /limited': {ThrottlingRateLimit: 1.0, ThrottlingBurstLimit: 1}} (GetStage reporting the override for 'GET /limited' only), an interleaved burst answers at least some 'GET /limited' requests with HTTP 429 whose body carries the 'Too Many Requests' envelope and never the backend body, answers the rest 200 from the backend, sheds nothing on 'GET /twin' in the same instant, and — after DeleteRouteSettings(RouteKey='GET /limited') removes the override from GetStage — sheds nothing on either route.
Actual (emulator): no verdict at all inside the harness cap; no 429, no 200-from-backend baseline and no post-delete state was graded. The rate/burst numbers above are this scenario's inputs; the requirement is the 429 status and its 'Too Many Requests' envelope for a request shed by a per-route throttle before the integration runs.

## Steps to Reproduce
Cause 1:
1. Boot the emulator. Create a Lambda that echoes a marker from its environment.
2. Create two HTTP APIs ('target' and 'twin'), each with an AWS_PROXY integration (PayloadFormatVersion 2.0) to that function, route 'GET /hit' targeting it, and a $default stage with AutoDeploy=True; grant apigateway invoke permission on both.
3. Confirm both invoke URLs answer 200 with the marker.
4. UpdateApi(ApiId=<target>, DisableExecuteApiEndpoint=True); confirm GetApi reports true for the target and false for the twin.
5. GET the target URL repeatedly: AWS answers 404 {"message":"Not Found"} and never invokes the backend, while the twin still answers 200. Observe the emulator's status/body.
6. UpdateApi(DisableExecuteApiEndpoint=False) and GET the same URL: expect a settled 200 with the marker.
7. Re-replay apigatewayv2.deep-apigatewayv2-2-enforce-disable-execute-api-endpoint-404-and-restore.

Cause 2:
1. Deploy (CDK synth + CloudFormation create) a stack with an HTTP API, a CorsConfiguration, a Lambda and a $default route; wait for CREATE_COMPLETE and confirm the invoke URL serves 200 with the marker.
2. Delete the stack and wait for DELETE_COMPLETE; confirm GetApi raises NotFoundException and GetFunction raises ResourceNotFoundException.
3. GET the former invoke URL in a loop: AWS answers 404 {"message":"Not Found"} (or the name stops resolving). The emulator answers 403 {"message":"Forbidden"} indefinitely.
4. Re-replay apigatewayv2.deploy-apigatewayv2-3-cdk-http-api-cors-lambda-default-route-serves-then-destroys.

Cause 3:
1. Create a Lambda that returns a marker; create an HTTP API with one AWS_PROXY integration and two routes 'GET /limited' and 'GET /twin' targeting it.
2. CreateStage(StageName='$default', AutoDeploy=True, DefaultRouteSettings={ThrottlingRateLimit:500.0, ThrottlingBurstLimit:200}, RouteSettings={'GET /limited': {ThrottlingRateLimit:1.0, ThrottlingBurstLimit:1}}); confirm GetStage reports the override for 'GET /limited' only.
3. Confirm both routes serve 200 with the marker.
4. Fire ~24 requests at each route strictly interleaved: expect >=1 HTTP 429 with the 'Too Many Requests' envelope (and no marker) on 'GET /limited', 200+marker for the rest, and zero 429 on 'GET /twin'.
5. DeleteRouteSettings(ApiId, StageName='$default', RouteKey='GET /limited'); repeat the burst and expect no 429 on either route.
6. Re-replay apigatewayv2.route-throttling-returns-429-while-twin-route-serves.

## Resolution

Cause 2: FIXED. The address an HTTP api publishes is a SOCKET of this service's own, and the
endpoint that serves it re-applied the core's browser (`Origin` / `Sec-Fetch-Site`) reading to
every request on it and answered `403` with the plain-text body `cross-site origin` itself —
before any record of the api was read. That is why this check never saw the api answer at all:
its very first `GET` of the invoke URL carries an `Origin` the api's `CorsConfiguration` admits
(the replay's first divergence after the gate was lifted was `GET / with an allowed Origin
answered 403 'cross-site origin'`). That reading is now scoped by what the socket being served
STANDS FOR (`src/service/websocket/endpoint_http.rs`'s `Arrival`): it is still applied, exactly
as a core HTTP listener applies it, wherever the address may be addressed at something other
than an api's own data plane — a WEBSOCKET api's endpoint, which also publishes the
`@connections` space, and a published portal's website — and it is not applied where the
socket's own record has already resolved the request to an invoke of an api of this account
(the published `ApiEndpoint`, whose path names the api, and a custom domain name, whose api
mapping names the api and the stage). That is the one class this emulator's front door already
serves cross-site, and the class an api's `CorsConfiguration` exists for: CORS governs what a
browser may do with an ANSWER, so the api's records decide the answer and the configuration
decides only its `Access-Control-*` decoration. The router every request from that endpoint is
handed still reads those fields itself for every request it NAMES AN OPERATION for, so a
`@connections` call or any other modelled operation reached through any of these addresses is
refused as cross-site exactly as at the front door, and the `Host` allowlist (the
DNS-rebinding guard) is untouched. With that, the whole check passes: the stack's api serves
its Lambda through the published URL, and the deleted api's URL answers `404`
`{"message":"Not Found"}`.

Causes 1 and 3: DECLINED — the divergences they state do not exist in this tree, and neither
check is graded (both still end as `Check timed out after 60.0s` with no verdict and no frame,
so the replay is evidence either way for nothing). What this tree answers for every leg either
cause names is now driven at the ADDRESS the control plane publishes, over a real connection to
it, with the route's function actually running, by
`tests/all/published_invoke_socket_front_door.rs`: an api whose `DisableExecuteApiEndpoint` is
set answers `404` `{"message":"Not Found"}` on every form of its own published URL with nothing
of the integration in it, the twin api over the same function is served by it in the same
instant, clearing the member restores serving on the same address, and a DELETED api's address
answers `404` `{"message":"Not Found"}`; a route the serving stage's `RouteSettings` throttle
sheds `429` `{"message":"Too Many Requests"}` without reaching its integration while the twin
route of the same stage on the stage's `DefaultRouteSettings` sheds nothing, and
`DeleteRouteSettings` returns the route to those defaults. The hypotheses both causes offer — a
`403` `{"message":"Forbidden"}` gate for the disabled endpoint, and an `AWS_PROXY` route that
cannot reach a settled `200` — are false here; the whole suite runs in under two seconds, so
speed is not what the checks' polls run out of. The same two checks were investigated the same
way by the fix for
`issues/things-that-now-work/20261114T024946Z-apigatewayv2-3-net-new-causes-a-disabled-execute.md`,
which reached the same conclusion. A later reader should treat them as ungraded rather than as
passing.
