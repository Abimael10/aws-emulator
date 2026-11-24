Issue-Source: trace-sweep-divergence
Issue-Trace: ivschat-chat-token-and-messaging
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ivschat-chat-token-and-messaging
Issue-Shape-Id: com.amazonaws.ivschat#AmazonInteractiveVideoServiceChat
Issue-Key: ivschat|CreateChatToken|2xx->2xx|$.token
Issue-Commit: 879ae682537ce416a59e3587d521f3c8b93d4970

# ivschat CreateChatToken mints a non-URL-safe standard-base64 token, not the shape AWS issues

> _Note: this issue was identified against the code base at commit `879ae682537ce416a59e3587d521f3c8b93d4970`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChatToken

## Description
Cause 1 of 1: CreateChatToken returns a `token` whose FORM is not the form Amazon IVS Chat issues, so the recorded fixture's shape assertion on `$.token` fails even though the status (200) and every other response member match.

AWS: the Smithy model constrains the value only loosely — `CreateChatTokenResponse.token` targets `ChatToken`, a `@sensitive` string with NO `@length` and NO `@pattern` (Smithy: shape `ChatToken` = string, sensitive; member doc "The issued client token, encrypted."), and `CreateChatToken` is documented as creating "an encrypted token that is used by a chat participant to establish an individual WebSocket chat connection to a room". The form is therefore recorded behavior only: the fixture does not assert the literal token (it is per-run material, expected `<shaped-not-asserted>`) but it DOES assert the token's shape, and the recorded AWS token satisfies that shape while the emulator's does not. Hypothesis (not stated by the model): AWS issues this token as a JWT — dot-delimited segments rendered in the URL-safe base64 alphabet (`A-Za-z0-9`, `-`, `_`, no padding), which is what makes it safe to carry verbatim as the `token` query parameter of the chat WebSocket URL the same documentation tells the participant to open.

Emulator: it answers a single opaque blob in the STANDARD base64 alphabet, with no segment structure at all — e.g. the replayed value is one 584-character run containing `+`, `/` and a trailing `=` and no `.` separator. That is exactly what the implementation mints: `seal_chat_token` renders a 12-byte AES-GCM nonce followed by ciphertext+tag through `aws_smithy_types::base64::encode` (standard alphabet, padded), and the crate's own unit test asserts the alphabet is `alphanumeric | '+' | '/' | '='`. So the value is high-entropy and decryptable, but it is not shaped like the token AWS hands back.

Why: a client that does what the operation is for — take the returned token and use it to open the chat connection, or inspect/route on the token's segments — mishandles it. `+`, `/` and `=` are not URL-query-safe, so a token pasted into the WebSocket URL is either corrupted by escaping or rejected, and any client or SDK helper that splits the token on `.` to read its segments (or validates it as a JWT before connecting) fails against the emulator while succeeding against AWS. The breakage is in client code that is correct against the real service, which is the worst class of emulator divergence: nothing in the response says the token is the wrong kind of string.

Fix: the token minting in this service's state, not the handler's control flow — the operation's own logic (room lookup, capability and `sessionDurationInMinutes` validation, expiry computation, record keeping) is right and should not change. Render the sealed envelope in the URL-safe, unpadded base64 alphabet and give the issued value the segment structure AWS's recorded token has, keeping the round-trip property the module already relies on (`chat_token_claims` must still read the session back out of the value, and `CHAT_TOKEN_MIN_LENGTH` and the crate's alphabet assertion must be updated to the new rendering rather than left asserting `+`/`/`/`=`).

Traces: ivschat-chat-token-and-messaging#task#2 — CreateChatToken, expected 200 / actual 200, `$.token: value_mismatch (expected '<shaped-not-asserted>', got '3M4tMmwL35H2B4EbI+rR4/W5BD4giR28…')`. Implementation under test: services/service-com-amazonaws-ivschat-amazoninteractivevideoservicechat/src/state/chat_tokens.rs lines 167-181 (`seal_chat_token`, the base64 rendering), lines 248-249 (`CHAT_TOKEN_MIN_LENGTH`) and lines 346-351 (the alphabet assertion); the handler that returns it is src/ops/create_chat_token.rs lines 252-280. Note: this is NOT the already-open ivschat issue "empty-bodied 204 tag responses omit the content-length: 0 framing header" (20261030T164443Z-…), which owns the TagResource/UntagResource header divergences in ivschat-tagging-workflow, nor the open ivschat WebSocket-endpoint issue (20261118T013251Z-…), which is about the wss upgrade being answered 404 rather than about the token's rendering.

## Evidence
Fixture ivschat-chat-token-and-messaging, step task#2, operation CreateChatToken.
Expected: HTTP 200 with a `token` whose shape the recording asserts (literal value not asserted: `<shaped-not-asserted>`), alongside `tokenExpirationTime`/`sessionExpirationTime`.
Actual: HTTP 200, but `$.token: value_mismatch` — the emulator returned `3M4tMmwL35H2B4EbI+rR4/W5BD4giR282sqaXCybhz1a1Jk3ewGface0EIQCC/R97Xj9…yVLyN38THr5cULxEtY=`: one unsegmented 584-character standard-base64 run containing `+`, `/` and trailing `=`, with no `.`-separated segments and no URL-safe alphabet.
Status and all other asserted members of the step matched; only the token's form diverged. Divergence key: service=ivschat|CreateChatToken|status=200->200|keys=$.token (verdict mismatch, no stub_path, not owned by any open issue).
Source of the emulator's form: src/state/chat_tokens.rs `seal_chat_token` → `aws_smithy_types::base64::encode(nonce || ciphertext||tag)`, and the module's own test asserting the alphabet is `alphanumeric | '+' | '/' | '='`.

## Steps to Reproduce
1. Replay the wire corpus (trace set simparity-wire-aws-v1) against the emulator; fixture ivschat-chat-token-and-messaging diverges at step task#2.
2. Or by hand: POST /CreateRoom to create a room, then POST /CreateChatToken with {"roomIdentifier": "<the room ARN>", "userId": "<any id>"}.
3. Inspect the returned `token`: it is a single padded standard-base64 run containing `+` and `/` and no `.` separators, instead of the URL-safe, dot-delimited token AWS returns.
4. Confirm the client-visible effect by placing the token verbatim in a WebSocket URL query parameter (`?token=<token>`) — the `+`, `/` and `=` characters require escaping, whereas AWS's token does not.
