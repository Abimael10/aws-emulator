Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-runtime.converse-stream-hand-signed-http2-and-http1-1-both-serve-the-event-stream bedrock-runtime.converse-stream-hand-signed-http2-model-needing-inference-profile-rejected-as-plain-json
Issue-Shape-Id: com.amazonaws.bedrockruntime#AmazonBedrockFrontendService
Issue-Key: bedrock-runtime.converse-stream-hand-signed-http2-and-http1-1-both-serve-the-event-stream|run#7|AssertionError|assert h2_meta["error"] is None, (
Issue-Key: bedrock-runtime.converse-stream-hand-signed-http2-model-needing-inference-profile-rejected-as-plain-json|run#5|AssertionError|assert rejected_meta["error"] is None, (
Issue-Commit: 0a24b18ae59262f0cdae7b98999a169258844aef

# bedrock-runtime: the HTTPS front door refuses h2 in ALPN (fatal no_application_protocol alert), so ConverseStream's HTTP/2 event-stream data plane is unreachable

> _Note: this issue was identified against the code base at commit `0a24b18ae59262f0cdae7b98999a169258844aef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ConverseStream

## Description
Cause 1 of 1: the emulator's own announced bedrock-runtime HTTPS endpoint aborts the TLS handshake during ALPN instead of selecting `h2`, so no HTTP/2 session to ConverseStream can ever be established. Failing checks: bedrock-runtime.converse-stream-hand-signed-http2-and-http1-1-both-serve-the-event-stream, bedrock-runtime.converse-stream-hand-signed-http2-model-needing-inference-profile-rejected-as-plain-json.

AWS: Smithy (com-amazonaws-bedrockruntime-amazonbedrockfrontendservice) models ConverseStream as `POST /model/{modelId}/converse-stream` with a single output member `stream` targeting ConverseStreamOutput, marked @streaming and @httpPayload — i.e. an `application/vnd.amazon.eventstream` response body — and its modeled client errors include ValidationException (httpError 400). The transport itself is wire identity rather than a modeled trait, so the rest is recorded behavior only: the real bedrock-runtime endpoint advertises `h2` in the TLS ALPN extension alongside `http/1.1`, and the identical hand-signed SigV4 request succeeds over either transport. Over an h2-only connection a valid ConverseStream request is answered `:status 200` with `content-type: application/vnd.amazon.eventstream`, carrying the event types messageStart, contentBlockDelta, contentBlockStop, messageStop, metadata for a one-token answer; a request naming a model that is reachable only through a cross-region inference profile is instead refused on the same h2 session with `:status 400`, `content-type: application/json`, header `x-amzn-errortype: ValidationException` and body `{"message": "Invocation of model ID <modelId> with on-demand throughput isn’t supported. Retry your request with the ID or ARN of an inference profile that contains this model."}` — never as an event stream. In that message the fixed text is `Invocation of model ID `, ` with on-demand throughput isn’t supported. Retry your request with the ID or ARN of an inference profile that contains this model.` (note the U+2019 curly apostrophe in `isn’t`); the only variable part is the model id taken from the request path. The SDK/HTTP-1.1 form of that same refusal already matches in this replay — the Converse control assertion in the second check passed — so only the h2 transport is missing.

Emulator: connecting an HTTP/2 client (node:http2 `http2.connect`, which offers only `h2` in ALPN) to the endpoint the emulator announced for bedrock-runtime makes the server send a fatal TLS alert during ALPN: `error:0A000460:SSL routines:ssl3_read_bytes:tlsv1 alert no application protocol:ssl/record/rec_layer_s3.c:918:SSL alert number 120` (alert 120, no_application_protocol). No session is established, `alpnProtocol` is never `h2`, no `:status` is observed and zero response bytes arrive, so neither the 200 event-stream leg nor the 400 plain-JSON refusal leg can be graded at all. The HTTP/1.1 calls of the very same checks (Converse through the SDK, including its ValidationException refusal) succeed against that same endpoint, so the gap is isolated to ALPN/h2 negotiation. The host:port in the quoted alert is a per-run value and is not part of the requirement; it is the origin the emulator itself announced, so the refusal is the emulator's and not the runner's network.

Why: ConverseStream is the streaming data plane every chat/assistant client uses, and clients that negotiate HTTP/2 (the AWS SDKs' event-stream path, gRPC-style proxies, browsers and node:http2) fail at connect time with a TLS alert that looks like a certificate or network fault rather than an unimplemented feature. A client relying on AWS behavior cannot stream tokens at all, and it also cannot see the modeled ValidationException for a profile-only model over that transport — it sees a handshake failure, which is classified as a retryable connectivity error instead of a permanent 400.

Fix: the HTTPS front door's TLS configuration for the bedrock-runtime endpoint — advertise `h2` in the ALPN protocol list alongside `http/1.1` and serve HTTP/2, so the @streaming ConverseStream response can be framed as `application/vnd.amazon.eventstream` and its error path can be serialized as plain JSON on the same session. Open issue 20261115T203530Z-kinesis-the-https-front-door-still-refuses-h2.md reports the same ALPN gap for the kinesis endpoint and SubscribeToShard, and 20261116T120000Z-elbv2-four-target-health-alpn-listener-lifecycle-and-data-plane-causes-still-open.md touches ALPN for elbv2; neither carries either bedrock-runtime check as a replay key, so closing them would leave the bedrock-runtime ConverseStream legs unrun.

Traces: bedrock-runtime.converse-stream-hand-signed-http2-and-http1-1-both-serve-the-event-stream (fails at check line 100, `assert h2_meta["error"] is None`) and bedrock-runtime.converse-stream-hand-signed-http2-model-needing-inference-profile-rejected-as-plain-json (fails at check line 108, `assert rejected_meta["error"] is None`) — the only two bedrock-runtime failures in this replay marked neither "ALREADY FILED" nor "OWNED by".

## Evidence
From the replayed behavioral suite (simparity-behavioral-aws-v1); 95/156 passed overall, bedrock-runtime 46/87 (53%), 41 failed, of which only the two checks below are neither ALREADY FILED nor OWNED by an open issue.

[mismatch] bedrock-runtime.converse-stream-hand-signed-http2-and-http1-1-both-serve-the-event-stream
  File "check_bedrock_runtime_converse_stream_hand_signed_http2_and_http1_1_both_serve_the_event_stream.py", line 100, in run
    assert h2_meta["error"] is None, (
  AssertionError: ConverseStream over hand-signed HTTP/2 to <the emulator's announced origin> failed: 'session: error:0A000460:SSL routines:ssl3_read_bytes:tlsv1 alert no application protocol:ssl/record/rec_layer_s3.c:918:SSL alert number 120'

[mismatch] bedrock-runtime.converse-stream-hand-signed-http2-model-needing-inference-profile-rejected-as-plain-json
  File "check_bedrock_runtime_converse_stream_hand_signed_http2_model_needing_inference_profile_rejected_as_plain_json.py", line 108, in run
    assert rejected_meta["error"] is None, (
  AssertionError: ConverseStream for 'amazon.nova-micro-v1:0' over hand-signed HTTP/2 failed at the transport: 'session: error:0A000460:SSL routines:ssl3_read_bytes:tlsv1 alert no application protocol:ssl/record/rec_layer_s3.c:918:SSL alert number 120'

Observed (both): TLS alert 120 (no_application_protocol) sent by the server during ALPN; no HTTP/2 session, alpnProtocol never 'h2', status = null, 0 response bytes.

Expected: ALPN selects `h2`; the hand-signed SigV4 `POST /model/{modelId}/converse-stream` is answered on that h2 session with `:status 200` and `content-type: application/vnd.amazon.eventstream`, whose frames decode to event types [messageStart, contentBlockDelta, contentBlockStop, messageStop, metadata] for a maxTokens=1 answer, identical to what the same request returns over HTTP/1.1; and for a model reachable only through a cross-region inference profile, `:status 400` with `content-type: application/json`, `x-amzn-errortype: ValidationException` and body {"message": "Invocation of model ID <modelId> with on-demand throughput isn’t supported. Retry your request with the ID or ARN of an inference profile that contains this model."} — fixed text as quoted, with <modelId> taken from the request path.

In the second check the preceding SDK-level control (Converse for the profile-only model over HTTP/1.1) already returned exactly ('ValidationException', that message), so the handler's refusal is correct and only the h2 transport is missing. `require_tools("node")` passed in both checks, so this is not a missing-runner-tool skip.

## Steps to Reproduce
1. Point an HTTP/2-only client at the endpoint the emulator announces for bedrock-runtime, e.g. `node -e 'require("node:http2").connect(ORIGIN)'` (node:http2 offers only `h2` in ALPN). Expect ALPN to select `h2`; observe instead the server's fatal no_application_protocol alert, SSL alert number 120, and no session.
2. With h2 available, serialize ConverseStream with botocore, SigV4-sign it, and send it on that session as `:method POST`, `:path /model/amazon.nova-lite-v1%3A0/converse-stream`, with messages=[{role:user,content:[{text:"Say hi."}]}] and inferenceConfig={maxTokens:1}. Expect `:status 200`, `content-type: application/vnd.amazon.eventstream`, and frames decoding to event types messageStart, contentBlockDelta, contentBlockStop, messageStop, metadata — the same answer the identical request returns over plain HTTP/1.1.
3. Repeat step 2 with modelId `amazon.nova-micro-v1:0` (reachable only through a cross-region inference profile). Expect `:status 400`, `content-type: application/json`, `x-amzn-errortype: ValidationException`, and body {"message": "Invocation of model ID amazon.nova-micro-v1:0 with on-demand throughput isn’t supported. Retry your request with the ID or ARN of an inference profile that contains this model."} — never an event stream.
4. Re-replay bedrock-runtime.converse-stream-hand-signed-http2-and-http1-1-both-serve-the-event-stream and bedrock-runtime.converse-stream-hand-signed-http2-model-needing-inference-profile-rejected-as-plain-json to confirm.
