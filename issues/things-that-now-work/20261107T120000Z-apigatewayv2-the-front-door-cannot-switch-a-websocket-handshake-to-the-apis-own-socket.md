Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2.websocket-connect-mock-integration-rejected apigatewayv2.mock-integration-response-selected-by-status-code-pattern apigatewayv2.websocket-post-to-connection-delivers-then-gone-after-disconnect
Issue-Key: apigatewayv2.websocket-connect-mock-integration-rejected|_poll_until#30|Timeout|time.sleep(interval_s)
Issue-Key: apigatewayv2.mock-integration-response-selected-by-status-code-pattern|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: apigatewayv2.websocket-post-to-connection-delivers-then-gone-after-disconnect|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Commit: 5d61882ef55489b75c810fe19d4c281a0249999c

# apigatewayv2: a WebSocket handshake reaches the api's own socket at the address its `ApiEndpoint` announces (1 cause, SERVED)

> _Split out of `issues/things-that-now-work/20261025T030504Z-apigatewayv2-the-websocket-execute-api-data-plane-still.md`, which re-filed these three replay keys as still failing on the belief that each check sends its opening handshake to the emulator's one front door. Replaying them says otherwise: all three pass. The record below is what actually happens, so the report no longer contradicts the state it is filed under._

## Operation
WebSocket upgrade (execute-api data plane)

## Description
One cause, and it is SERVED.

A WEBSOCKET api is reached, in full, at the address its `ApiEndpoint` announces: the socket the
core opened for that api (`engine.listeners().tcp_plaintext`, taken in
`services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/service/websocket/transport.rs`
and stored on the api by `src/ops/create_api.rs`). That socket negotiates encryption in band,
so the `wss://` a client of AWS dials and the `ws://` a client that verifies no certificate
dials are both served on the one announced address; it answers `101 Switching Protocols`,
dispatches each frame by the api's `RouteSelectionExpression`, selects a MOCK integration's
reply among its `IntegrationResponses` by the synthesized status, and registers the connection
in the `@connections` registry so a `PostToConnection` is delivered over that socket and every
later call is `GoneException` once the client closes.

The three checks in `Issue-Trace-Ids` dial THAT address — the value the control plane reported
— and are answered there. The earlier report's premise, that each check resolves the announced
URI onto the endpoint under test and polls the front door for a live socket, is not what the
replay shows: every check completes, and each of the three replay keys above is reported
`fixed`.

What the emulator's own SERVICE endpoint answers is unchanged and is not this cause: a
handshake that arrives there (a client that rewrote the announced URI onto the endpoint it was
already talking to, carrying the api id in the path as `/_aws/execute-api/{api-id}/{stage}`) is
resolved from the api's records and answered with the status those records give it — `403` for
a stage the endpoint does not serve, `401` for a connection request missing an identity source,
`500` for a `$connect` route a MOCK integration cannot reply to — and, where every step let it
through, `501` naming the address the api IS reached at
(`src/service/websocket.rs`, `NO_WEBSOCKET_TRANSPORT_MESSAGE`). No `101` is answered there,
because the front door serves one BUFFERED request per call and the seams a service is given
(`Service::can_handle`, `Service::override_operation`, `Service::override_routing_error`) take
an already-buffered request and return a buffered response: answering `101` with no socket
behind it would leave the client reading EOF. That is an emulator-internal address, not one AWS
publishes, and no recorded unit asks for an upgrade there.

## Evidence
Replay of the three checks against this checkout (3 of 3 replayed, 3 of 3 passed):

* `apigatewayv2.websocket-connect-mock-integration-rejected` — passes, including the `run#16`
  assertion that a `$connect` route bound to a MOCK integration refuses the handshake with
  `500`, and the healthy control leg's `101` on the announced socket.
* `apigatewayv2.mock-integration-response-selected-by-status-code-pattern` — passes: the socket
  is live and the reply is the integration response the synthesized status selects.
* `apigatewayv2.websocket-post-to-connection-delivers-then-gone-after-disconnect` — passes: the
  handshake mints a `ConnectionId`, a `PostToConnection` to it is delivered over that socket,
  and each `@connections` operation is `GoneException` after the client closes.

What holds the behaviour: `tests/all/websocket_tls_transport.rs`, where
`every_reporter_announces_the_uri_a_client_dials_and_that_uri_carries_the_frames` reads
`ApiEndpoint` back off `CreateApi`, `GetApi`, `GetApis` and `UpdateApi`, requires all four to
report the one value, then takes that URI AS GIVEN — its own scheme, its own authority — appends
the stage name as `Api$ApiEndpoint` documents, dials it with a real TLS client, requires `101`,
and sends a frame whose reply names the api, the stage and the route that served it; the rows
span the reporter and the stage-name forms up to the published 128-character maximum. Beside it,
`the_announced_socket_completes_a_tls_handshake_and_carries_frames` and
`a_backend_pushes_its_reply_back_through_the_announced_endpoint` drive the backend's own reply
over the same address, and `tests/all/websocket_transport_data_plane.rs` holds the three
behaviours the checks assert.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. CreateApi(ProtocolType='WEBSOCKET', RouteSelectionExpression='$request.body.action');
   CreateIntegration(IntegrationType='MOCK'); CreateRoute(RouteKey='$default',
   Target='integrations/<integ>'); CreateRouteResponse(RouteResponseKey='$default');
   CreateStage(StageName='s'); CreateDeployment(StageName='s').
3. Open an RFC 6455 upgrade against the `ApiEndpoint` the api announces, AS GIVEN, with the
   stage appended: observe `101 Switching Protocols`, then a frame dispatched by the route
   selection expression and answered by the integration response its status selects.
4. Re-replay the three check ids in trace_ids: 3 of 3 pass.
