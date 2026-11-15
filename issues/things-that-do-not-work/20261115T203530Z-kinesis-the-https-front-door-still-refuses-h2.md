Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis.subscribe-to-shard-event-stream-framing-over-a-hand-signed-http2-request
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis.subscribe-to-shard-event-stream-framing-over-a-hand-signed-http2-request|run#14|AssertionError|assert meta["error"] is None, (
Issue-Commit: fe8c913264a3b44bc469f94b7b22f781b21e8d5c

# kinesis: the HTTPS front door still refuses h2 in ALPN (fatal no_application_protocol alert), so SubscribeToShard's HTTP/2-only enhanced fan-out data plane is unreachable

> _Note: this issue was identified against the code base at commit `fe8c913264a3b44bc469f94b7b22f781b21e8d5c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubscribeToShard

## Description
REOPENS 20261103T091152Z-kinesis-4-net-new-root-causes-listchannels-classifies.md: its fix left the call refused. That issue is marked fixed and carried kinesis.subscribe-to-shard-event-stream-framing-over-a-hand-signed-http2-request as a replay key, with its Cause 4 being exactly this ALPN gap; replaying that same check still ends in the same fatal TLS no_application_protocol alert, so the enhanced fan-out data plane is still unreachable. This is not described as a new defect.

Cause 1 of 1: the emulator's announced Kinesis HTTPS endpoint aborts the TLS handshake during ALPN instead of selecting h2.

AWS: Smithy (com-amazonaws-kinesis-kinesis-20131202) models SubscribeToShard with required input ConsumerARN/ShardId/StartingPosition and a single required output member EventStream targeting SubscribeToShardEventStream marked @streaming — i.e. an `application/vnd.amazon.eventstream` response body — and the operation is offered ONLY over HTTP/2, so the endpoint must advertise `h2` in the TLS ALPN extension. The rest is recorded behavior only (the transport is wire identity, not a modeled trait): a hand-signed HTTP/2 POST carrying `X-Amz-Target: Kinesis_20131202.SubscribeToShard` with an `application/x-amz-json-1.1` body is answered `:status 200` with `content-type: application/vnd.amazon.eventstream`; frames are laid out [total_length:4][headers_length:4][prelude_crc:4][headers][payload][message_crc:4] with both CRCs CRC-32 (zlib) over the preceding bytes and header value type 7 (UTF-8 string) throughout; an `initial-response` message precedes the first SubscribeToShardEvent; each SubscribeToShardEvent's embedded JSON payload carries `:content-type application/x-amz-json-1.1`; and ContinuationSequenceNumber is strictly greater than the SequenceNumber of the last record in the batch carrying it and is non-decreasing across every event of the subscription (including record-less heartbeats).

Emulator: the server aborts the TLS handshake during ALPN with a fatal no_application_protocol alert — `error:0A000460:SSL routines:ssl3_read_bytes:tlsv1 alert no application protocol:ssl/record/rec_layer_s3.c:918:SSL alert number 120` (TLS alert 120) — when an HTTP/2 client (node:http2 http2.connect, which offers `h2` in ALPN) connects to the emulator's OWN announced HTTPS origin. No HTTP/2 session is established, no `:status` is ever observed and zero response bytes arrive, so nothing about the SubscribeToShard handler, the event-stream framing or the CRCs can be graded at all. The address is the one the emulator announced for itself, so this refusal is the emulator's and not the runner's network; the control-plane calls in the same check (CreateStream, PutRecord, RegisterStreamConsumer, DescribeStreamConsumer over HTTP/1.1) all succeed against that same endpoint, so only ALPN/h2 is missing.

Why: enhanced fan-out is the KCL 2.x default and SubscribeToShard has no HTTP/1 form, so an endpoint that refuses h2 by ALPN makes every EFO consumer fail at connect time with a TLS alert that looks like a certificate or network fault rather than an unimplemented feature; the whole vnd.amazon.eventstream framing contract can never be served.

Fix: the HTTPS front door's TLS configuration for the Kinesis endpoint — advertise `h2` alongside `http/1.1` in the ALPN protocol list and serve HTTP/2, so the @streaming SubscribeToShard response can be framed. Open issue 20261109T214127Z-kinesis-4-net-new-causes-the-expirediteratorexception-message.md reports the same ALPN gap as its Cause 4, but carries only kinesis.put-record-then-get-records-round-trip-over-a-hand-signed-http2-request and kinesis.put-record-signed-for-a-different-authority-host-rejected as replay keys — the SubscribeToShard check below appears in neither, so a fixer closing that issue would leave the EFO leg unrun. 20261101T034628Z-kinesis-the-https-front-door-aborts-the-tls.md reports a different alert (decode_error) under concurrent PutRecords.

Traces: kinesis.subscribe-to-shard-event-stream-framing-over-a-hand-signed-http2-request (the only kinesis failure in this replay marked neither "ALREADY FILED" nor "OWNED by").

## Evidence
From the replayed behavioral suite (simparity-behavioral-aws-v1); per-service pass rate kinesis 187/224 (83%), 37 failed, of which only the check below is neither ALREADY FILED nor OWNED.

[mismatch] kinesis.subscribe-to-shard-event-stream-framing-over-a-hand-signed-http2-request
  File "check_kinesis_subscribe_to_shard_event_stream_framing_over_a_hand_signed_http2_request.py", line 142, in run
    assert meta["error"] is None, (
  AssertionError: the HTTP/2 client failed talking to <the emulator's announced https origin>: session: error:0A000460:SSL routines:ssl3_read_bytes:tlsv1 alert no application protocol:ssl/record/rec_layer_s3.c:918:SSL alert number 120
  . SubscribeToShard is only offered over HTTP/2, so a target that cannot serve h2 has no enhanced fan-out data plane

Observed: TLS alert 120 (no_application_protocol) during ALPN; status = null, 0 response bytes. (The host:port in the quoted text is a per-run value and is not part of the requirement; it is the origin the emulator itself announced.)

Expected: ALPN selects `h2`; the hand-signed `X-Amz-Target: Kinesis_20131202.SubscribeToShard` POST is answered `:status 200` with `content-type: application/vnd.amazon.eventstream`; the frames parse with recomputing prelude and message CRC-32s; an `initial-response` message precedes the first SubscribeToShardEvent; the records inside carry exactly the sequence numbers the preceding PutRecord calls returned, in order.

The preceding control-plane steps of the same check (CreateStream ON_DEMAND -> ACTIVE, 5x PutRecord, RegisterStreamConsumer, DescribeStreamConsumer -> ACTIVE) all succeeded against the same endpoint over HTTP/1.1, so the failure is isolated to ALPN/h2 negotiation.

## Steps to Reproduce
1. Create an ON_DEMAND Kinesis stream, wait for ACTIVE, PutRecord 5 payloads with one partition key, RegisterStreamConsumer and wait for the consumer to reach ACTIVE.
2. Open an HTTP/2 connection to the emulator's announced Kinesis HTTPS origin, e.g. `node -e 'require("node:http2").connect(ORIGIN)'` (node:http2 offers `h2` in ALPN) — expect ALPN to select `h2`; observe the server's fatal no_application_protocol alert, SSL alert number 120, and no session.
3. With h2 available, POST a SigV4-signed request on that session with `:method POST`, `:path /`, `content-type: application/x-amz-json-1.1`, `x-amz-target: Kinesis_20131202.SubscribeToShard` and a body naming the ConsumerARN, ShardId and StartingPosition (TRIM_HORIZON); read `:status` and `content-type`, then parse the response bytes as vnd.amazon.eventstream frames: verify each frame's prelude CRC-32 and message CRC-32, that an `initial-response` message comes first, that each SubscribeToShardEvent payload carries `:content-type application/x-amz-json-1.1`, and that the records' SequenceNumbers equal the PutRecord results in order.
4. Re-replay kinesis.subscribe-to-shard-event-stream-framing-over-a-hand-signed-http2-request to confirm.
