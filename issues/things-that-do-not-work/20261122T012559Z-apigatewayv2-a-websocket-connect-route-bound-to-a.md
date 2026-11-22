Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.websocket-connect-mock-integration-rejected
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2.websocket-connect-mock-integration-rejected|run#16|AssertionError|assert result["status"] == 500, (
Issue-Commit: 1bab6ccfadd8e456dfdfaa55eb16233f496412b2

# apigatewayv2: a WEBSOCKET $connect route bound to a MOCK integration completes the RFC 6455 upgrade (101) instead of failing the handshake with HTTP 500

> _Note: this issue was identified against the code base at commit `1bab6ccfadd8e456dfdfaa55eb16233f496412b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRoute

## Description
One net-new cause behind 1 failing apigatewayv2 check.

Cause 1 of 1: the WEBSOCKET execute-api front door upgrades a handshake whose $connect route is backed by a MOCK integration, where AWS refuses the handshake.

AWS: recorded behavior only — the Smithy model constrains neither the pairing nor the data plane (apigatewayv2 CreateRoute.RouteKey -> SelectionKey, CreateIntegration.IntegrationType includes MOCK; no modeled error ties the two together, and the WebSocket handshake itself is not a modeled operation). The recording shows that on a deployed WEBSOCKET stage whose `$connect` route targets a MOCK integration, the RFC 6455 upgrade request is answered with HTTP status 500 — the connection is NOT switched to the WebSocket protocol — while the SAME account/region/API, once `$connect` is re-pointed (UpdateRoute Target=integrations/<AWS_PROXY id>) at a real Lambda and redeployed, answers 101 Switching Protocols. The recording also shows a non-$connect MOCK route (`whoami`) on that same API continues to reply normally, so the refusal is specific to the ($connect, MOCK) pairing and not to MOCK integrations in general.

Emulator: the same handshake is answered 101 (the check observes `status 101`, i.e. the socket is upgraded and no status head is produced), so the refusal never happens and the connection is established.

Why: a client relying on AWS's behavior treats a successful upgrade as proof that $connect was handled by a real integration. On AWS the misconfiguration surfaces immediately as a failed handshake (HTTP 500) at connect time; on the emulator the client gets a live connection whose $connect was never actually executed, so the configuration error is invisible and downstream code (connection registration, @connections push, authorization performed in $connect) proceeds against a connection AWS would never have granted. Nothing in the client can distinguish a correctly Lambda-backed $connect from an unbacked MOCK one.

Fix: the WEBSOCKET execute-api data-plane front door's handshake path — the step that resolves the `$connect` route's target integration before switching protocols. It must reject the upgrade with HTTP 500 (no 101, no Sec-WebSocket-Accept, connection not upgraded) when the resolved $connect integration type cannot produce a connect decision (MOCK), while leaving non-$connect routes bound to MOCK fully functional and leaving an AWS_PROXY-backed $connect upgrading with 101.

Traces: apigatewayv2.websocket-connect-mock-integration-rejected (check asserts `result["status"] == 500` for the MOCK $connect, then asserts 101 for the Lambda-backed $connect control).

## Evidence
Failing check apigatewayv2.websocket-connect-mock-integration-rejected:

  File "check_apigatewayv2_websocket_connect_mock_integration_rejected.py", line 100, in run
    assert result["status"] == 500, (
  AssertionError: a $connect route bound to a MOCK integration must fail the handshake with 500 on real AWS, got status 101 (None)

Expected: the RFC 6455 GET upgrade to the deployed WEBSOCKET stage URL returns an HTTP status line of 500 and the socket is closed without upgrading (fixed text: the status 500; no variable parts).
Actual: the emulator returns 101 Switching Protocols and hands back an upgraded socket (the check's `_ws_connect` reports status 101 and therefore no response head, hence the `None` in the message).

Control inside the same check (not reached): after UpdateRoute re-points $connect at an AWS_PROXY Lambda integration and the stage is re-pointed at the new deployment, the upgrade must answer 101 — that leg is what isolates the 500 to the ($connect, MOCK) pairing.

## Steps to Reproduce
1. CreateApi(ProtocolType="WEBSOCKET", RouteSelectionExpression="$request.body.action").
2. CreateIntegration(IntegrationType="MOCK") on that API, with an IntegrationResponse so non-$connect MOCK routes can reply.
3. CreateRoute(RouteKey="$connect", Target="integrations/<mock integration id>") and a second MOCK route (e.g. RouteKey="whoami").
4. CreateStage + CreateDeployment, wait for DeploymentStatus=DEPLOYED, and point the stage at that deployment.
5. Open an RFC 6455 handshake (GET with Upgrade: websocket, Connection: Upgrade, Sec-WebSocket-Key, Sec-WebSocket-Version: 13) against the stage URL. Expect an HTTP 500 status line and no upgrade; the emulator answers 101.
6. Re-replay apigatewayv2.websocket-connect-mock-integration-rejected to confirm (it also checks that a Lambda-backed $connect still upgrades with 101 and that the MOCK "whoami" route still answers a text frame).
