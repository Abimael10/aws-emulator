Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin
Issue-Key: apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin|_do_handshake_attempt#31|Timeout|return status, body, sock
Issue-Commit: fcaef5045051e2f19f2ef72b682687f38c3f2a70

# apigatewayv2__iam__lambda__logs__sts: DisableExecuteApiEndpoint reaches no verdict on a manually-deployed HTTP stage or its WebSocket twin — the disabled stages never answer 404 {"message":"Not Found"} and the deploy-gated WebSocket refusal is never observed (1 cause)

> _Note: this issue was identified against the code base at commit `fcaef5045051e2f19f2ef72b682687f38c3f2a70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateApi

## Description
One net-new root cause. Cause 1 of 1: UpdateApi(DisableExecuteApiEndpoint=true) has no observable data-plane effect on an HTTP API's stages, and the WebSocket twin's deploy-gated variant of the same rule cannot be observed either, so the whole check reaches no verdict. Failing check: apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin.

AWS: Smithy (com.amazonaws.apigatewayv2#ApiGatewayV2): UpdateApi (PATCH /v2/apis/{ApiId}, 200) takes DisableExecuteApiEndpoint (__boolean, jsonName disableExecuteApiEndpoint) and echoes it; GetApi reports the same member; CreateDeployment/CreateStage/UpdateStage bind a stage to a DeploymentId. UpdateApi's modelled errors (BadRequestException 400, ConflictException 409, NotFoundException 404, TooManyRequestsException 429) do not apply here — the control-plane calls succeed. The data-plane consequences are recorded behavior only: for an HTTP API, setting the flag refuses requests on EVERY stage of that API's default execute-api host — the AutoDeploy $default stage and a manually-deployed stage still pinned to a deployment created before the flag was set alike, with no redeploy needed — with HTTP 404 and the body {"message":"Not Found"} (entirely fixed text; no value from the request or state appears in it), and the integration is not invoked. For a WEBSOCKET API the same UpdateApi call is deploy-gated: a manually-deployed stage keeps completing signed RFC 6455 handshakes (101) until that stage is itself redeployed (CreateDeployment + UpdateStage), after which the handshake is refused with HTTP 403 and a JSON body whose message member is 'Forbidden' alongside a connectionId and a requestId member (the message text 'Forbidden' and the member set are the requirement; the connection id and request id are per-run values).

Emulator: the check reaches no verdict — replay detail 'Error: Check timed out after 60.0s'. The legs that must settle (the $default stage answering a continuous run of 404s once disabled, then the never-redeployed 'manual' stage answering the same, then six spaced signed WebSocket handshakes still answering 101, then the handshake flipping to 403 after the stage is redeployed) never converge inside the harness budget, so neither the HTTP refusal class nor the WebSocket deploy-gating is observable. Hypothesis: the front door's disabled-endpoint gate answers a refusal class other than 404 {"message":"Not Found"} and the signed WebSocket handshake on a manually created stage does not complete at all — the open issues 20261109T143515Z-apigatewayv2-iam-lambda-logs-sts-a-signed-websocket-handshake-to-a-manually-created.md and 20261110T153046Z-apigatewayv2-iam-lambda-logs-sts-updateapi-disableexecuteapiendpoint-true.md record those symptoms but neither was reported as owning this check's replay key in this run, so the check is re-filed here rather than dropped; a fixer may close them together.

Why: a client cannot verify an endpoint lockdown. On AWS, disabling the default endpoint takes effect immediately on every HTTP stage including stages nobody redeployed, but NOT on a WebSocket stage until it is redeployed — the two halves of that rule are exactly what a cutover to a custom domain depends on. Against the emulator neither half can be observed: a lockdown that silently keeps serving (or an HTTP stage that keeps serving because it was never redeployed) is an exposure, and a WebSocket stage that stops serving before its redeploy is an unexpected outage.

Fix: the execute-api front door's DisableExecuteApiEndpoint gate — for HTTP APIs, the 404 {"message":"Not Found"} refusal applied to all of an API's stages regardless of each stage's pinned deployment and before the integration runs; for WEBSOCKET APIs, the deploy-gated evaluation of the same flag at the handshake, and the 403 refusal body carrying message 'Forbidden' with connectionId and requestId, on the signed-handshake path for a manually-deployed stage.

Traces: apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin.

## Evidence
Check apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin — verdict mismatch, detail 'Error: Check timed out after 60.0s'.

Expected (AWS): after UpdateApi(DisableExecuteApiEndpoint=True) on both an HTTP API (stages '$default' with AutoDeploy and 'manual' pinned to a deployment created before the update) and a WEBSOCKET API (stage 'live' pinned likewise), GetApi reports true for both; every signed GET of both HTTP stage URLs then answers (404, {"message":"Not Found"}) — fixed text, no variable part — while six signed WebSocket handshakes spread over ~30s all still answer 101; after CreateDeployment + UpdateStage on the WebSocket stage, the handshake answers 403 with a body whose message is 'Forbidden' and whose members are connectionId, message, requestId; both HTTP stages still answer (404, {"message":"Not Found"}).

Actual (emulator): no verdict inside the harness cap — none of the polled states (a settled 404 on $default, a settled 404 on 'manual', the sustained 101 window, the post-redeploy 403) is reached. Statuses, error codes and message text above are the requirement; api ids, stage deployment ids, the generated marker, connection ids and request ids are per-run environment values.

## Steps to Reproduce
1. Boot the emulator. Create a Lambda function returning statusCode 200 with a MARKER environment variable as its body.
2. Create an HTTP API with an AWS_PROXY integration (PayloadFormatVersion 2.0) to it, a route 'GET /hit' with AuthorizationType=AWS_IAM, a '$default' stage with AutoDeploy=True, then CreateDeployment and CreateStage 'manual' pinned to that DeploymentId; grant apigateway permission to invoke the function.
3. Create a WEBSOCKET API (RouteSelectionExpression '$request.body.action') with an AWS_PROXY integration to the same function, a '$connect' route with AuthorizationType=AWS_IAM, then CreateDeployment and CreateStage 'live' pinned to it.
4. Confirm both HTTP stages serve a SigV4-signed GET /hit with 200 + marker and that a signed WebSocket handshake to stage 'live' completes with 101.
5. UpdateApi(DisableExecuteApiEndpoint=True) on both APIs; confirm GetApi reports true for both.
6. Poll both HTTP stage URLs with signed GETs: expect a settled HTTP 404 with body {"message":"Not Found"} on BOTH, including the never-redeployed 'manual' stage. Observe the emulator's status and body.
7. Repeat the signed WebSocket handshake several times over ~30s: expect 101 every time (the WebSocket disable is deploy-gated).
8. CreateDeployment on the WebSocket API and UpdateStage(StageName='live', DeploymentId=<new>); repeat the handshake: expect 403 with a JSON body whose message is 'Forbidden' plus connectionId and requestId members, while both HTTP stages still answer 404 {"message":"Not Found"}.
9. Re-replay apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin.
