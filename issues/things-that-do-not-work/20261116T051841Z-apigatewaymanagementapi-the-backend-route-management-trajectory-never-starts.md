Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewaymanagementapi.backend-route-pushes-describes-and-disconnects-live-clients
Issue-Shape-Id: com.amazonaws.apigatewaymanagementapi#ApiGatewayManagementApi
Issue-Key: apigatewaymanagementapi.backend-route-pushes-describes-and-disconnects-live-clients|_poll_until#6|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 7b48dd1812adb90a036957570722499f0872b4d2

# apigatewaymanagementapi: the backend-route management trajectory never starts — a freshly deployed WEBSOCKET stage never completes a 101 upgrade before the run budget ends, so GetConnection/PostToConnection/DeleteConnection from a route backend are never exercised

> _Note: this issue was identified against the code base at commit `7b48dd1812adb90a036957570722499f0872b4d2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetConnection

## Description
Cause 1 of 1: the WEBSOCKET stage this scenario deploys never answers a 101 Switching Protocols handshake, so the whole in-backend @connections trajectory is unreachable and the check is cut off with no verdict.

AWS: a WEBSOCKET apigatewayv2 API with $connect/$disconnect/$default routes bound to an AWS_PROXY Lambda integration, a deployment and a stage answers a client's HTTP/1.1 Upgrade: websocket request on that stage's invoke address with 101 Switching Protocols as soon as the stage exists, and keeps the socket open. Once that handshake succeeds, the route backend can call @connections through the management endpoint for that api/stage: GetConnection (Smithy: GET /@connections/{ConnectionId}, 200, output connectedAt/identity/lastActiveAt, errors ForbiddenException 403 / GoneException 410 / LimitExceededException 429) describes the CALLING connection, with Identity.SourceIp equal to the sourceIp the route event reports and ConnectedAt <= LastActiveAt; PostToConnection (POST /@connections/{ConnectionId}, 200, Data @length max 131072) relays bytes to a second live connection; DeleteConnection (DELETE /@connections/{ConnectionId}, 204) closes that client's socket with WebSocket close code 1000, after which GetConnection and PostToConnection on it are refused 410 GoneException (recorded behavior: empty message) while the other connection stays live and readable. Each of those is AWS behavior the check grades; none of it was reached.

Emulator: the check creates an IAM execution role granting only execute-api:ManageConnections, a Lambda chat backend, a WEBSOCKET api with $connect/$disconnect/$default on one AWS_PROXY integration, a deployment and a stage, then polls websocket.create_connection() against that stage's invoke URL every 4 s. Every attempt failed (the helper returns None only on WebSocketBadStatusException or OSError, i.e. a non-101 status or a refused/closed TCP connection) until the runner's remaining time dropped to <= 6 s, at which point the poll raised TimeoutError("a 101 WebSocket upgrade on the new stage: the run budget ended first; not graded") at check line 314/91. No management call was ever issued, so no GetConnection/PostToConnection/DeleteConnection assertion in the trajectory was evaluated. Hypothesis: the same front-door upgrade gap already filed for apigatewayv2 WebSocket stages (20261111T090000Z-apigatewayv2-iam-lambda-logs-the-front-door-cannot-upgrade-the-connect-handshake.md, 20261112T190000Z-apigatewayv2-the-disableexecuteapiendpoint-check-still-hangs-on-the-front-door-upgrade.md, 20261113T053027Z-apigatewayv2-iam-lambda-logs-...) reaches this stage shape too — those issues are filed under apigatewayv2 and do not carry this check's replay key, and 20261113T113139Z-apigatewaymanagementapi-4-net-new-causes-connections-is-served.md explicitly reasons from handshakes that DO succeed, so neither covers this failure.

Why: a client that builds a WebSocket backend the documented way — deploy the api/stage, connect, and let the route handler describe, push to and disconnect peers through @connections — cannot connect at all for this stage shape, so the entire backend-driven management path (self-description, peer relay, server-initiated disconnect with code 1000, and the subsequent GoneException pruning) is unusable and silently ungradeable rather than failing fast.

Fix: the WebSocket data-plane front door that serves a deployed apigatewayv2 WEBSOCKET stage's invoke address — it must complete the HTTP Upgrade handshake with 101 for a stage whose $connect integration exists, immediately after the stage is created, and keep the connection registered so the apigatewaymanagementapi @connections handlers for that api/stage can describe, post to and delete it. If the refusal is instead a readiness delay, the stage must not be observable as created before its socket accepts upgrades.

Traces: apigatewaymanagementapi.backend-route-pushes-describes-and-disconnects-live-clients.

## Evidence
Check: apigatewaymanagementapi.backend-route-pushes-describes-and-disconnects-live-clients ([error]).

expected: websocket.create_connection(<stage invoke url>) -> 101 Switching Protocols, then (a) the $default backend's GetConnection on its own connectionId returns Identity.SourceIp == the route event's identity.sourceIp and ConnectedAt <= LastActiveAt; (b) PostToConnection relays {"from": <alice id>, "text": <text>} to the second client; (c) DeleteConnection closes that client with close code 1000; (d) GetConnection and PostToConnection on the deleted id are refused ('GoneException', '') while the surviving connection still reports its SourceIp.

actual: no upgrade ever succeeded. Replay output:
  File "check_apigatewaymanagementapi_backend_route_pushes_describes_and_disconnects_live_clients.py", line 314, in run
    alice = _poll_until(ctx, lambda: _connect(ctx, url), bool, interval_s=4.0,
  File "...", line 91, in _poll_until
    raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: a 101 WebSocket upgrade on the new stage: the run budget ended first; not graded

_connect() returns None only for websocket.WebSocketBadStatusException (a non-101 handshake status) or OSError (refused/closed connection), so every poll in the window was refused by the emulator; none of the graded @connections assertions (a)-(d) were reached. The other 27 of 29 apigatewaymanagementapi checks pass, so this is specific to the stage/upgrade path this scenario deploys, not to the service being absent.

## Steps to Reproduce
1. Create an IAM role assumable by lambda.amazonaws.com whose only inline grant is execute-api:ManageConnections (deliberately no logs permissions).
2. Create a Lambda function with that role whose handler returns 200 for $connect/$disconnect and, for $default, builds an apigatewaymanagementapi client on https://<domainName>/<stage> and answers 'whoami' with GetConnection, 'send' with PostToConnection to another id, and 'kick' with DeleteConnection, replying to the caller via PostToConnection.
3. Create a WEBSOCKET apigatewayv2 API (RouteSelectionExpression $request.body.action), one AWS_PROXY integration on that function, routes $connect/$disconnect/$default targeting it, lambda:AddPermission for apigateway.amazonaws.com, then CreateDeployment and CreateStage.
4. Open a WebSocket client to the stage's invoke URL (websocket.create_connection), retrying every 4 s. Observe that the upgrade is never answered 101 — every attempt raises WebSocketBadStatusException/OSError until the run budget ends.
5. Consequently none of the following is observable: GetConnection on the caller's own connection (Identity.SourceIp == event identity.sourceIp, ConnectedAt <= LastActiveAt), PostToConnection relay to a second client, DeleteConnection closing that client with code 1000, and the 410 GoneException refusals afterwards with the first client still live.
