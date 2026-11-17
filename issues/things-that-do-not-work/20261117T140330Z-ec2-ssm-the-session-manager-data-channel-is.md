Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.session-data-channel-carries-shell-input-and-its-output-in-sequence ec2__ssm.session-data-channel-refuses-another-sessions-token ec2__ssm.shell-exit-closes-the-session-data-channel-and-terminates-the-session ec2__ssm.terminated-session-token-opens-a-data-channel-with-no-shell-output
Issue-Key: ec2__ssm.session-data-channel-carries-shell-input-and-its-output-in-sequence|__init__#3|WebSocketBadStatusException|self.ws = websocket.create_connection(ctx.endpoint_url(stream_url, websocket=True),
Issue-Key: ec2__ssm.session-data-channel-refuses-another-sessions-token|__init__#5|WebSocketBadStatusException|self.ws = websocket.create_connection(ctx.endpoint_url(stream_url, websocket=True),
Issue-Key: ec2__ssm.shell-exit-closes-the-session-data-channel-and-terminates-the-session|__init__#5|WebSocketBadStatusException|self.ws = websocket.create_connection(ctx.endpoint_url(stream_url, websocket=True),
Issue-Key: ec2__ssm.terminated-session-token-opens-a-data-channel-with-no-shell-output|__init__#5|WebSocketBadStatusException|self.ws = websocket.create_connection(ctx.endpoint_url(stream_url, websocket=True),
Issue-Commit: 845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab

# ec2__ssm: the Session Manager data channel is unreachable — a WebSocket handshake to the StreamUrl StartSession returns is answered 404 UnknownService

> _Note: this issue was identified against the code base at commit `845c80d629e2b6091bbba6d5ecc9b7e37b15e9ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSession

## Description
Cause 1 of 1: StartSession hands out a StreamUrl the emulator does not serve, so no Session Manager data channel can ever be opened.
AWS: Smithy com.amazonaws.ssm#StartSession returns SessionId, TokenValue and StreamUrl. Recorded behavior: StreamUrl is a wss:// endpoint for that session; a WebSocket handshake to it completes with HTTP 101 Switching Protocols, after which the client sends the open-data-channel JSON {"MessageSchemaVersion":"1.0","RequestId":<uuid>,"TokenValue":<the TokenValue StartSession returned>,"ClientId":<uuid>,"ClientVersion":"1.2.0.0"} and then exchanges binary agent frames (input_stream_data / output_stream_data with acknowledgements) carrying keystrokes and the shell's output. A token belonging to a different session, or a session already terminated, is rejected/closed on that same channel rather than at the handshake.
Emulator: the handshake to the StreamUrl the emulator itself returned fails with HTTP 404 and the body {"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"} and header x-amzn-errortype: UnknownService — the emulator's request router does not recognise its own session stream URL as a route, so the upgrade never happens. All four data-channel checks abort at the handshake with WebSocketBadStatusException.
Why: the Session Manager data plane does not exist for any client: no interactive shell, no port forwarding, no SSM-based access to an instance, and none of the data-channel's own rules (token scoping to one session, close-on-shell-exit, a terminated session producing no output) can be observed. The emulator announces an endpoint it does not serve, so a client cannot tell an unimplemented data plane from a broken session.
Fix: the StartSession/ResumeSession handlers and the session data-channel endpoint in the ssm service crate, together with the emulator's HTTP routing — route the StreamUrl path to a WebSocket handler that completes the upgrade, validates the open-data-channel TokenValue against the named session, and speaks the agent framing.
Traces: ec2__ssm.session-data-channel-carries-shell-input-and-its-output-in-sequence, ec2__ssm.session-data-channel-refuses-another-sessions-token, ec2__ssm.shell-exit-closes-the-session-data-channel-and-terminates-the-session, ec2__ssm.terminated-session-token-opens-a-data-channel-with-no-shell-output.

## Evidence
Behavioral suite, service heading `### ec2__ssm`. All four `session-data-channel` / shell-exit / terminated-token checks fail identically, inside the shared _DataChannel constructor which does websocket.create_connection(ctx.endpoint_url(started["StreamUrl"], websocket=True)) and then sends the open-data-channel JSON with started["TokenValue"]:

    File "_handshake.py", line 73, in handshake
    websocket._exceptions.WebSocketBadStatusException: Handshake status 404 Not Found
      -+-+- {'content-type': 'application/x-amz-json-1.1', 'x-amzn-errortype': 'UnknownService', 'content-length': '149', 'date': ...}
      -+-+- {"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"}

Expected (AWS): HTTP 101 Switching Protocols on that URL, then binary agent frames — the checks go on to assert a shell prompt, that `echo $((6*7))-<marker>` returns `42-<marker>` in sequence with acknowledged frames, that another session's TokenValue is refused on the channel, that shell exit closes the socket and terminates the session, and that a terminated session's token yields no shell output.

The host/port inside StreamUrl and the per-run ids are not requirements; the requirement is that the URL the emulator returns completes a WebSocket upgrade on the emulator it was obtained from.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Launch an EC2 instance with an instance profile carrying AmazonSSMManagedInstanceCore and wait until it registers with Systems Manager.
3. Call ssm StartSession for that instance and read SessionId, TokenValue and StreamUrl from the response.
4. Open a WebSocket connection to that StreamUrl against the same emulator endpoint. Observe HTTP 404 with x-amzn-errortype: UnknownService and body {"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"}; AWS completes the upgrade with 101 and accepts the open-data-channel message carrying TokenValue.
5. Re-replay the 4 checks listed in trace_ids.
