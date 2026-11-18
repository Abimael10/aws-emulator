Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ivschat.native-send-message-websocket-round-trip ivschat.native-send-message-without-capability-forbidden ivschat.native-send-event-delivered-to-websocket-subscriber ivschat.native-delete-message-triggers-delete-event ivschat.native-disconnect-user-closes-websocket-connection ivschat.send-event-reserved-prefix-rejected
Issue-Shape-Id: com.amazonaws.ivschat#AmazonInteractiveVideoServiceChat
Issue-Key: ivschat.native-send-message-websocket-round-trip|_ws_connect#7|WebSocketBadStatusException|return websocket.create_connection(endpoint, subprotocols=[token], timeout=20)
Issue-Key: ivschat.native-send-message-without-capability-forbidden|_ws_connect#6|WebSocketBadStatusException|return websocket.create_connection(endpoint, subprotocols=[token], timeout=20)
Issue-Key: ivschat.native-send-event-delivered-to-websocket-subscriber|_ws_connect#7|WebSocketBadStatusException|return websocket.create_connection(endpoint, subprotocols=[token], timeout=20)
Issue-Key: ivschat.native-delete-message-triggers-delete-event|_ws_connect#7|WebSocketBadStatusException|return websocket.create_connection(endpoint, subprotocols=[token], timeout=20)
Issue-Key: ivschat.native-disconnect-user-closes-websocket-connection|_ws_connect#10|WebSocketBadStatusException|return websocket.create_connection(endpoint, subprotocols=[token], timeout=20)
Issue-Key: ivschat.send-event-reserved-prefix-rejected|_ws_connect#6|WebSocketBadStatusException|return websocket.create_connection(endpoint, subprotocols=[token], timeout=20)
Issue-Commit: 775d8b7a5dc0233a720d935fa0de7ffef061d5f2

# ivschat: the native chat messaging WebSocket endpoint is not served — the wss upgrade is answered HTTP 404 UnknownService, so the entire IVS Chat data plane (SEND_MESSAGE, event fan-out, DisconnectUser, capability enforcement) is unreachable (1 cause)

> _Note: this issue was identified against the code base at commit `775d8b7a5dc0233a720d935fa0de7ffef061d5f2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChatToken

## Description
Cause 1 of 1: the emulator's front door does not recognize the Amazon IVS Chat messaging (edge) endpoint, so an RFC 6455 WebSocket handshake to it is rejected by the generic service router instead of being switched to the ivschat data plane. Every behavioral check that opens a chat connection dies at `websocket.create_connection(...)` before it can assert anything about the data plane.

AWS: the IVS Chat control plane modeled in Smithy (ivschat, AWS JSON over REST, `POST /CreateChatToken` -> `{token, tokenExpirationTime, sessionExpirationTime}`; Smithy: CreateChatToken.roomIdentifier @required @pattern ^arn:aws:ivschat:[a-z0-9-]+:[0-9]+:room/[a-zA-Z0-9-]+$, CreateChatToken.capabilities ChatTokenCapabilities) mints a chat token that a client then presents to the SEPARATE Amazon IVS Chat Messaging API at the regional edge host (wss://edge.ivschat.<region>.amazonaws.com), passing the token as the sole WebSocket subprotocol. AWS answers that handshake with `101 Switching Protocols` and thereafter speaks the chat messaging frame protocol over the socket. That messaging protocol is not in the Smithy model (recorded behavior only); the recordings show these required behaviors over the upgraded socket:
  - a client frame `{"Action":"SEND_MESSAGE","Content":<text>,"RequestId":<id>}` from a connection whose token carries the SEND_MESSAGE capability is answered to the sender with a `Type":"MESSAGE"` frame echoing the same `RequestId`, the exact `Content`, `Sender.UserId` equal to the token's userId, and an `Id` that is a 12-character string (same shape as the control plane's `ID`: @length 12/12, @pattern ^[a-zA-Z0-9]+$), and is broadcast as a MESSAGE frame to every other connection in the room;
  - a SEND_MESSAGE frame from a connection whose token carries NO capabilities is refused to the sender with a frame `Type":"ERROR"`, `ErrorCode` 403 and `ErrorMessage` exactly the fixed text `missing SEND_MESSAGE capability`, and the message is NOT broadcast to any other connection;
  - the control-plane `SendEvent` (Smithy: SendEvent -> output `id` @length 12/12) is fanned out live to every connected client as a frame `Type":"EVENT"` whose `EventName` is the request's eventName verbatim, whose `Attributes` is the request's attributes map verbatim, and whose `Id` equals the `id` SendEvent returned;
  - the control-plane `DeleteMessage` (Smithy: DeleteMessage.id @length 12/12, output `id`) pushes a frame `Type":"EVENT"`, `EventName":"aws:DELETE_MESSAGE"` whose `Attributes.MessageId` is the deleted message's id and whose `Id` equals the `id` DeleteMessage returned;
  - the control-plane `DisconnectUser` (Smithy: DisconnectUser.roomIdentifier/userId @required, empty output) actually terminates that user's live socket and leaves every other connection in the room open and usable.

Emulator: the TLS/HTTP front door never reaches ivschat at all. The handshake request to the edge host is answered `HTTP 404 Not Found` with headers `content-type: application/x-amz-json-1.1`, `x-amzn-errortype: UnknownService` and body `{"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"}`. The websocket client raises `websocket._exceptions.WebSocketBadStatusException: Handshake status 404 Not Found`. The router's own message names the cause: a WebSocket handshake carries no SigV4 Authorization header and no X-Amz-Target, so the only usable signal is the Host header (`edge.ivschat.<region>.amazonaws.com`), and the front door has no route for the ivschat edge host.

Why: a client relying on AWS's behavior cannot use IVS Chat at all beyond minting tokens. CreateChatToken succeeds and hands back a token that no connection can ever present, so no message can be sent or received, no application or aws:DELETE_MESSAGE event is ever delivered, DisconnectUser's effect is unobservable, and the SEND_MESSAGE capability check is never exercised — a capability-less token is neither refused nor enforced, which is silently permissive for anything that depends on the socket. Chat applications, moderation flows and any integration test that reads the room through a second connection all fail or, worse, pass vacuously against an emulator that broadcasts nothing.

Fix: the front-door host/endpoint resolution that maps an inbound request to a service must recognize the ivschat messaging edge host and hand the connection to the ivschat crate, and the ivschat crate needs a data-plane WebSocket handler that (a) completes the RFC 6455 upgrade with 101 and echoes the chat token as the negotiated subprotocol, (b) validates the token against the room and extracts userId plus capabilities, (c) implements the SEND_MESSAGE action with sender-echo plus room broadcast, the ERROR 403 capability refusal, and the MESSAGE id format, and (d) is driven by the existing SendEvent/DeleteMessage/DisconnectUser control-plane handlers so those operations push EVENT frames and close the targeted socket respectively, leaving other sockets in the room live.

Traces: all six ivschat data-plane checks below fail identically at the handshake, before any assertion runs — they are examples of the one unserved endpoint, and each additionally pins one of the frame behaviors listed above, so a fix must satisfy all six.

## Evidence
Expected (AWS): `websocket.create_connection("wss://edge.ivschat.<region>.amazonaws.com", subprotocols=[<CreateChatToken token>])` completes with `HTTP/1.1 101 Switching Protocols`, then the chat messaging frames above are exchanged.

Actual (emulator), identical for all six checks:
```
websocket._exceptions.WebSocketBadStatusException: Handshake status 404 Not Found
-+-+- {'content-type': 'application/x-amz-json-1.1', 'x-amzn-errortype': 'UnknownService', 'content-length': '149'}
-+-+- {"__type":"UnknownService","message":"could not resolve a target service from the request (no SigV4 credential scope, X-Amz-Target, or Host header)"}
```
Fixed text vs variable parts: the diverging status `404` with error type `UnknownService` and the quoted router message are entirely fixed text emitted by the front door — nothing in it is derived from the request, which is itself the defect (the Host header `edge.ivschat.<region>.amazonaws.com` is present and is the signal that should have resolved ivschat). The region in that host and the room ARN / token / 12-char message ids in the checks are per-run values, not part of any requirement.

Unreached assertions each check carries (what the fix must then satisfy):
- native-send-message-websocket-round-trip: sender's own frame `Type=="MESSAGE"`, same `RequestId`, exact `Content`, `Sender.UserId` == token userId, `Id` a 12-char string.
- native-send-message-without-capability-forbidden: capability-less SEND_MESSAGE -> `Type=="ERROR"`, `ErrorCode == 403`, `ErrorMessage == "missing SEND_MESSAGE capability"`; independent observer never receives the blocked content; an empowered connection's message IS relayed to that observer.
- native-send-event-delivered-to-websocket-subscriber: `Type=="EVENT"` with `EventName` and `Attributes` verbatim from SendEvent and `Id` == SendEvent's returned `id`.
- native-delete-message-triggers-delete-event: `Type=="EVENT"`, `EventName=="aws:DELETE_MESSAGE"`, `Attributes.MessageId` == deleted message id, `Id` == DeleteMessage's returned `id`.
- native-disconnect-user-closes-websocket-connection: target socket closes after DisconnectUser while a bystander socket stays open and answers a ping.
- send-event-reserved-prefix-rejected: after the ValidationException leg, a subscriber's first `EVENT` frame is the application event with `EventName=="app_defined_event"` and `Id` == the `id` SendEvent returned.

## Steps to Reproduce
1. Boot the emulator and point an ivschat client at it.
2. `CreateRoom(name=<unique>)` -> note `arn`.
3. `CreateChatToken(roomIdentifier=<arn>, userId="u1", capabilities=["SEND_MESSAGE"])` -> note `token`.
4. Open a WebSocket to the emulator for host `edge.ivschat.<region>.amazonaws.com` with the token as the only subprotocol, e.g. `websocket.create_connection(url, subprotocols=[token], timeout=20)`.
5. Observe the handshake is answered `404 Not Found` with `x-amzn-errortype: UnknownService` instead of `101 Switching Protocols`; the client raises WebSocketBadStatusException and no frame can be exchanged.
6. Repeat with a token minted with no `capabilities` and with SendEvent/DeleteMessage/DisconnectUser calls against the room — none of their WebSocket-visible effects can be observed because step 4 never connects.
