Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin
Issue-Key: apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin|_attempt_handshake#31|Timeout|return status, body
Issue-Commit: 364d717fb0950a5d6ba16d8ba80d0b55cb268a4c

# apigatewayv2__iam__lambda__logs__sts: the DisableExecuteApiEndpoint manual-stage/WebSocket-twin scenario still reaches no verdict — neither HTTP stage refuses 404 nor does the WebSocket stage answer a handshake inside the run budget (1 cause)

> _Note: this issue was identified against the code base at commit `364d717fb0950a5d6ba16d8ba80d0b55cb268a4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateApi

## Description
Cause 1 of 1: after UpdateApi(DisableExecuteApiEndpoint=true) the execute-api data plane produces no response at all for either protocol, so the deploy-gated asymmetry between an HTTP API's stages and a WebSocket API's never-redeployed stage is unobservable.

AWS: UpdateApi accepts the boolean member DisableExecuteApiEndpoint (Smithy: UpdateApi input member DisableExecuteApiEndpoint -> __boolean, jsonName disableExecuteApiEndpoint; errors BadRequestException 400, ConflictException 409, NotFoundException 404, TooManyRequestsException 429; HTTP PATCH /v2/apis/{ApiId}, 200), and GetApi echoes it back (GetApi output member DisableExecuteApiEndpoint). The wire effect on the data plane is documented behavior, not modelled: with the default execute-api endpoint disabled, a request to an HTTP API stage on its <api-id>.execute-api.<region>.amazonaws.com URL is refused with HTTP 404 and the body {"message":"Not Found"} — for the AutoDeploy $default stage and for a manually-deployed stage that was never redeployed alike (https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-disable-default-endpoint.html). A WebSocket API's never-redeployed stage, by contrast, keeps completing the RFC 6455 handshake (101 Switching Protocols) until that stage is itself redeployed; only then is the handshake refused with HTTP 403 and a JSON body carrying exactly the members connectionId, message and requestId, with message being the fixed text "Forbidden" (https://docs.aws.amazon.com/apigateway/latest/developerguide/websocket-api-disable-default-endpoint.html). In that body the connectionId and requestId are per-request values; the 403 status, the member set and the "Forbidden" text are the requirement. For the HTTP refusal, 404 and the exact two-character-key body {"message":"Not Found"} are entirely fixed text — nothing in it comes from the request.

Emulator: the check reaches no assertion whatsoever — the replay reports only 'Error: Check timed out after 60.0s'. Neither of the two HTTP stages ever answers (so neither the pre-update 200 positive control nor the post-update 404 {"message":"Not Found"} is produced), the WebSocket stage never answers a signed handshake with 101, and the post-redeploy 403/Forbidden refusal is never produced either. The scenario is killed at the harness budget with no HTTP status, no error code and no body to compare.

Why: a client cannot tell whether disabling the default endpoint took effect. Code that hardens an API by calling UpdateApi(DisableExecuteApiEndpoint=True) and then verifies the default URL is closed hangs instead of seeing 404, and a deployment pipeline that depends on the documented 'a WebSocket stage stays open until it is redeployed' semantics gets no signal at all — it cannot distinguish 'still serving as AWS would' from 'never served'.

Fix: the apigatewayv2 execute-api front door — its per-stage, per-protocol DisableExecuteApiEndpoint enforcement and the stage-deployment snapshot that enforcement reads, plus the WEBSOCKET upgrade path the same front door must serve so the twin leg can answer at all. The refusal bodies belong in that front door's error serializer, not in the control-plane handler.

Related open issues that do NOT cover this check (the suite left it unowned, so none of them carries its current replay key): 20261110T153046Z-apigatewayv2-iam-lambda-logs-sts-updateapi-disableexecuteapiendpoint-true.md (same check, keyed on a _attempt_handshake frame timeout), 20261112T190000Z-apigatewayv2-the-disableexecuteapiendpoint-check-still-hangs-on-the-front-door-upgrade.md, 20261109T143515Z-apigatewayv2-iam-lambda-logs-sts-a-signed-websocket-handshake-to-a-manually-created.md (there the handshake answered 500), and 20261122T132825Z-apigatewayv2-an-http-api-with-disableexecuteapiendpoint-true-never.md (HTTP-only leg).

Traces: apigatewayv2__iam__lambda__logs__sts.disable-execute-api-endpoint-manual-stage-and-websocket-twin.

## Evidence
Verdict: [mismatch]. Detail, verbatim: 'Error: Check timed out after 60.0s'.

Expected (AWS): Beat 1 — signed GET /hit on both the $default and the manual HTTP stage answers 200 with the integration's body, and a signed WebSocket handshake on the manual WebSocket stage answers 101. Beat 2 — GetApi reports DisableExecuteApiEndpoint true on both APIs. Beat 3 — both HTTP stages answer 404 with the body {"message":"Not Found"} (fixed text, no request-derived parts). Beat 4 — the never-redeployed WebSocket stage still answers 101 on every read across a sustained window. Beat 5/6 — once that stage is redeployed onto a new deployment, the handshake is refused 403 with a JSON body whose members are exactly connectionId, message, requestId and whose message is "Forbidden" (connectionId and requestId are per-request values).

Actual (emulator): no response of any kind is produced on either plane within the harness budget; the check is killed at 60.0 s before reaching any of those assertions, so no status, error code or body exists to compare.

## Steps to Reproduce
1. Create a Lambda function returning 200 with a marker body.
2. apigatewayv2 CreateApi(ProtocolType=HTTP); CreateIntegration(AWS_PROXY, PayloadFormatVersion 2.0) to that function; CreateRoute('GET /hit', AuthorizationType=AWS_IAM); CreateStage('$default', AutoDeploy=True); CreateDeployment then CreateStage('manual', DeploymentId=<that deployment>).
3. Create a second API with ProtocolType=WEBSOCKET, RouteSelectionExpression='$request.body.action', an AWS_PROXY $connect route (AuthorizationType=AWS_IAM), CreateDeployment, and CreateStage('live', DeploymentId=<that deployment>).
4. Grant apigateway lambda:InvokeFunction on both execute-api ARNs.
5. Send a SigV4-signed GET /hit to each HTTP stage's invoke URL and a signed wss handshake to the WebSocket stage — expect 200/200/101.
6. UpdateApi(ApiId=<http>, DisableExecuteApiEndpoint=True) and UpdateApi(ApiId=<ws>, DisableExecuteApiEndpoint=True); GetApi on each must report the flag true.
7. Re-request both HTTP stages — expect 404 {"message":"Not Found"} on both. Re-handshake the WebSocket stage repeatedly — expect 101 each time.
8. CreateDeployment on the WebSocket API and UpdateStage('live', DeploymentId=<new>), then re-handshake — expect 403 with a body holding connectionId, message='Forbidden', requestId.
Observed: the run never produces any of these responses and is killed at the 60 s cap.
