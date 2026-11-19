Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis.put-record-then-get-records-round-trip-over-a-hand-signed-http2-request kinesis.put-record-signed-for-a-different-authority-host-rejected
Issue-Shape-Id: com.amazonaws.kinesis#Kinesis_20131202
Issue-Key: kinesis.put-record-then-get-records-round-trip-over-a-hand-signed-http2-request|run#8|AssertionError|assert put_meta["error"] is None, (
Issue-Key: kinesis.put-record-signed-for-a-different-authority-host-rejected|run#10|AssertionError|assert bad_meta["error"] is None, (
Issue-Commit: 2569c6911135fdcd350a29db0e205ccfafae52a0

# kinesis: the HTTPS front door still refuses h2 in ALPN (fatal no_application_protocol alert), so hand-signed HTTP/2 PutRecord/GetRecords never reach the service

> _Note: this issue was identified against the code base at commit `2569c6911135fdcd350a29db0e205ccfafae52a0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRecord

## Description
REOPENS 20261109T214127Z-kinesis-4-net-new-causes-the-expirediteratorexception-message.md: its fix left the call refused. That issue is marked fixed and carried kinesis.put-record-then-get-records-round-trip-over-a-hand-signed-http2-request and kinesis.put-record-signed-for-a-different-authority-host-rejected as replay keys, with its Cause 4 being exactly this ALPN gap; replaying both of those checks still ends in the same fatal TLS no_application_protocol alert, so neither call ever reaches the service. This is not described as a new defect.

Cause 1 of 1: the emulator's own announced Kinesis HTTPS endpoint aborts the TLS handshake during ALPN instead of selecting h2, so no HTTP/2 request of ANY Kinesis operation can be served.

AWS: recorded behavior only for the transport (ALPN is wire identity, not a modeled trait) — the Kinesis HTTPS endpoint advertises `h2` alongside `http/1.1` in the TLS ALPN extension, and a hand-signed HTTP/2 POST with `:method POST`, `:path /`, `content-type: application/x-amz-json-1.1` and `x-amz-target: Kinesis_20131202.PutRecord` is answered `:status 200` with the modeled PutRecordOutput (ShardId/SequenceNumber), which a following GetRecords then returns. Smithy (com-amazonaws-kinesis-kinesis-20131202) additionally models SubscribeToShard's required output member EventStream as @streaming, an operation offered only over HTTP/2, so h2 is not optional for this service. For the mismatched-authority probe the recording shows the service REACHED and refusing at the signature layer: a request signed for the real endpoint host but sent with a different `:authority` is answered HTTP 403 Error.Code InvalidSignatureException (the message's fixed frame is AWS's signature-mismatch prose; the host names in it come from the request), and the record is not stored — a correctly signed retry then succeeds.

Emulator: the TLS listener sends a fatal `no_application_protocol` alert (TLS alert 120) during ALPN when an HTTP/2 client (node:http2 `http2.connect`, which offers `h2`) connects to the origin the emulator itself announced as the Kinesis endpoint. Replay detail from both checks: `stream: The pending stream has been canceled (caused by: ...:error:0A000460:SSL routines:ssl3_read_bytes:tlsv1 alert no application protocol:ssl/record/rec_layer_s3.c:918:SSL alert number 120)`. No HTTP/2 session is established, no `:status` is observed and zero response bytes arrive, so neither the 200 round trip nor the InvalidSignatureException refusal can be observed at all. The control-plane and data-plane calls of the same checks over HTTP/1.1 (CreateStream, DescribeStreamSummary, GetShardIterator, ListShards) succeed against that same endpoint, so only ALPN/h2 negotiation is missing; the address is the one the emulator announced for itself, so the refusal is the emulator's and not the runner's network.

Why: every HTTP/2-only client path is dead — KCL 2.x enhanced fan-out, any SDK configured for h2, and any caller that negotiates h2 opportunistically — and the failure surfaces as a TLS alert that looks like a certificate or network fault rather than an unimplemented feature. A security control that depends on the service refusing a request whose `:authority` does not match what was signed (InvalidSignatureException, nothing stored) cannot be exercised at all, so an emulator user cannot tell a correctly rejecting endpoint from one that accepts the forged authority.

Fix: the HTTPS front door's TLS configuration for the Kinesis endpoint — advertise `h2` in the ALPN protocol list and serve HTTP/2 requests through the same operation router that HTTP/1.1 uses; the signature-verification layer then needs to compare the signed host against the request's `:authority` on the h2 path too.

Traces: kinesis.put-record-then-get-records-round-trip-over-a-hand-signed-http2-request (fails at the PutRecord leg), kinesis.put-record-signed-for-a-different-authority-host-rejected (fails at the forged-authority leg). Related open issue that does NOT carry these two replay keys: 20261115T203530Z-kinesis-the-https-front-door-still-refuses-h2.md reports the same ALPN gap but is grounded only in kinesis.subscribe-to-shard-event-stream-framing-over-a-hand-signed-http2-request, so a fixer closing it would leave both PutRecord legs unrun.

## Evidence
[mismatch] kinesis.put-record-then-get-records-round-trip-over-a-hand-signed-http2-request
  File "check_kinesis_put_record_then_get_records_round_trip_over_a_hand_signed_http2_request.py", line 85, in run
    assert put_meta["error"] is None, (
  AssertionError: PutRecord over hand-signed HTTP/2 to <the emulator's announced https origin> failed: 'stream: The pending stream has been canceled (caused by: ...:error:0A000460:SSL routines:ssl3_read_bytes:tlsv1 alert no application protocol:ssl/record/rec_layer_s3.c:918:SSL alert number 120)'

[mismatch] kinesis.put-record-signed-for-a-different-authority-host-rejected
  File "check_kinesis_put_record_signed_for_a_different_authority_host_rejected.py", line 95, in run
    assert bad_meta["error"] is None, (
  AssertionError: PutRecord signed for '<endpoint netloc>' but sent with :authority 'attacker-controlled.example.com' failed at the transport instead of reaching the service: 'stream: The pending stream has been canceled (caused by: ...tlsv1 alert no application protocol... SSL alert number 120)'

Observed: TLS alert 120 (no_application_protocol) during ALPN; status = null, 0 response bytes, on the emulator's own announced origin. (Host:port and the OpenSSL error id are per-run values, not requirements.)

Expected: ALPN selects `h2`; the hand-signed `x-amz-target: Kinesis_20131202.PutRecord` POST is answered `:status 200` with a PutRecordOutput whose SequenceNumber is then returned by GetRecords on a TRIM_HORIZON iterator; and the same request replayed with `:authority` set to a host other than the one it was signed for is answered HTTP 403 InvalidSignatureException with AWS's signature-mismatch message and stores nothing (a correctly signed retry succeeds).

The HTTP/1.1 steps of both checks (CreateStream -> ACTIVE, ListShards, GetShardIterator) succeeded against the same endpoint, isolating the failure to ALPN/h2.

## Steps to Reproduce
1. Create a Kinesis stream and wait for StreamStatus ACTIVE; take a TRIM_HORIZON shard iterator.
2. Open an HTTP/2 connection to the emulator's announced Kinesis HTTPS origin, e.g. `node -e 'require("node:http2").connect(ORIGIN)'` (node:http2 offers `h2` in ALPN). Expect ALPN to select h2; observe instead the server's fatal no_application_protocol alert (SSL alert number 120) and no session.
3. With h2 available, send a SigV4-signed PutRecord on that session (`:method POST`, `:path /`, `content-type: application/x-amz-json-1.1`, `x-amz-target: Kinesis_20131202.PutRecord`, JSON body with StreamName/Data/PartitionKey) and expect `:status 200` with ShardId and SequenceNumber; then GetRecords on the iterator and expect that record back.
4. Repeat the same signed request but send it with `:authority` set to another host (e.g. attacker-controlled.example.com): expect the service to answer HTTP 403 InvalidSignatureException and to store nothing, then confirm a correctly signed retry succeeds.
5. Re-replay kinesis.put-record-then-get-records-round-trip-over-a-hand-signed-http2-request and kinesis.put-record-signed-for-a-different-authority-host-rejected.
