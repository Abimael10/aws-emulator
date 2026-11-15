Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__lambda__logs.post-to-a-malformed-connection-id-is-rejected-bad-request
Issue-Key: apigatewayv2__lambda__logs.post-to-a-malformed-connection-id-is-rejected-bad-request|_log_messages#15|Timeout|time.sleep(4)
Issue-Commit: dda73b1c89ff90e3a0e7799df332912882e239a1

# apigatewayv2__lambda__logs: PostToConnection to a malformed connection id never answers — the @connections data plane hangs instead of refusing with 400 BadRequestException

> _Note: this issue was identified against the code base at commit `dda73b1c89ff90e3a0e7799df332912882e239a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PostToConnection

## Description
One net-new root cause.

Cause 1 of 1: a PostToConnection call naming a connection id that is not well-formed never gets a response at all.

AWS: Smithy (apigatewaymanagementapi) models PostToConnection as POST /@connections/{ConnectionId} with an empty 200 output and the errors ForbiddenException (403), GoneException (410), LimitExceededException (429) and PayloadTooLargeException (413). A ConnectionId that is syntactically not a connection id is not a closed connection — recorded behavior only: AWS refuses it before any connection lookup with HTTP 400 and error code BadRequestException, and reserves GoneException/410 for a well-formed id whose connection has already closed. The requirement is the status/code pair and that the refusal is produced at all; the body's only variable part is the request id.

Emulator: nothing is served. The @connections endpoint accepts the request and never writes a response, so the whole check is abandoned by the harness with 'Error: Check timed out after 60.0s'. Neither the 400 BadRequestException that AWS gives for a malformed id nor the 410 GoneException it reserves for a closed one is produced.

Why: a client cannot distinguish "you passed garbage" from "the connection is gone", which is exactly the discrimination the @connections API exists to give a WebSocket backend; worse, the call never returns, so a handler posting to a stale/garbled id blocks until its own invoke timeout instead of getting an immediate refusal it can branch on.

Fix: the apigatewaymanagementapi PostToConnection handler and the @connections route's connection-id parsing/validation in the WebSocket management data plane — the route must always terminate with a modeled response.

Traces: apigatewayv2__lambda__logs.post-to-a-malformed-connection-id-is-rejected-bad-request.

## Evidence
Failing check apigatewayv2__lambda__logs.post-to-a-malformed-connection-id-is-rejected-bad-request [mismatch].

Expected (AWS): HTTP 400, error code BadRequestException, for POST /@connections/{ConnectionId} where ConnectionId is malformed; 410 GoneException only for a well-formed id whose connection is closed.

Actual (emulator): no response is ever written. Replay output: 'Error: Check timed out after 60.0s'.

## Steps to Reproduce
1. Create an HTTP/WebSocket API and a stage so an @connections management endpoint exists.
2. Call apigatewaymanagementapi PostToConnection against that endpoint with a ConnectionId that is not a well-formed connection id (e.g. a short non-base64 token) and a small Data payload.
3. Observe: the request never returns. AWS answers HTTP 400 BadRequestException immediately.
