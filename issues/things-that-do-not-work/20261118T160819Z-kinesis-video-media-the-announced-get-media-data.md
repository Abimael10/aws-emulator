Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis-video-media.get-media-deleted-stream-rejected kinesis-video-media.get-media-after-ingested-stream-deletion-rejected
Issue-Shape-Id: com.amazonaws.kinesisvideomedia#AWSAcuityInletService
Issue-Key: kinesis-video-media.get-media-deleted-stream-rejected|<lambda>#10|EndpointConnectionError|lambda: media.get_media(
Issue-Key: kinesis-video-media.get-media-after-ingested-stream-deletion-rejected|<lambda>#21|EndpointConnectionError|lambda: media.get_media(
Issue-Commit: 6ad539f79015bc27d2d0777d4c5c145ae7d45c50

# kinesis-video-media: the announced GET_MEDIA data endpoint is torn down with the stream, so GetMedia after DeleteStream is unreachable (connection refused) instead of 404 ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `6ad539f79015bc27d2d0777d4c5c145ae7d45c50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMedia

## Description
REOPENS 20261031T070053Z-kinesis-video-media-getmedia-3-net-new-root.md: its fix left the call refused. That issue's Cause 1 is this exact check (kinesis-video-media.get-media-deleted-stream-rejected), which previously reached the handler and answered HTTP 404 ResourceNotFoundException with the wrong message; it is now not answered at all — the TCP connection to the announced data endpoint is refused, so no HTTP status, error code or message is produced. Do not treat this as a brand-new defect: the deleted-stream GetMedia refusal path is still not AWS-faithful, it has merely moved from a message divergence to an unreachable endpoint.

This issue carries ONE root cause.

=== Cause 1 of 1: a stream's announced data endpoint (its listener) is dropped when the stream record is removed, so a client holding that endpoint cannot be answered for the deleted stream ===
Failing checks: kinesis-video-media.get-media-deleted-stream-rejected, kinesis-video-media.get-media-after-ingested-stream-deletion-rejected.

AWS: Smithy (services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/smithy-model/com-amazonaws-kinesisvideomedia-awsacuityinletservice.json) models GetMedia as POST /getMedia, success 200, with errors ClientLimitExceededException 400, ConnectionLimitExceededException 400, InvalidArgumentException 400, InvalidEndpointException 400, NotAuthorizedException 401 and ResourceNotFoundException 404 ("Status Code: 404, The stream with the given name does not exist"). kinesisvideo:GetDataEndpoint is documented as handing back an address the application then dials for the stream's media APIs; that address is a long-lived service host, and AWS does not stop answering there when a stream is deleted. Recorded behavior: after DeleteStream completes (DescribeStream raises ResourceNotFoundException), a GetMedia on the SAME previously-discovered data endpoint is still served and is refused at the HTTP layer with status 404, Code ResourceNotFoundException, Message "The requested stream is not found or not active." — the whole message is fixed text, with no variable part (no StreamName or StreamARN interpolated); the same sentence covers the absent-stream and the exists-but-not-ACTIVE cases. The requirement here is that the endpoint the service announced stays dialable and answers this modeled 404, i.e. the caller gets a ClientError, not a transport failure.

Emulator: the data plane is published per (stream, plane) pair as its own loopback listener (services/service-com-amazonaws-kinesisvideo-kinesisvideo-20170930/src/state/data_endpoint.rs, ensure_data_endpoint / DataPlaneEndpoint, whose `_listener` field holds the socket open), and the stream's removal RETAINS-out that entry, dropping the listener and closing the port (same crate, src/state.rs, settle_clock: `self.data_endpoints.retain(|announced, _| announced.stream_arn != stream_arn)`; the module header states the policy outright: "an endpoint is released again when the stream it belongs to is removed"). A client that discovered the GET_MEDIA endpoint while the stream was ACTIVE — the normal, documented flow, and the only flow these checks use — therefore dials a closed port once the DELETING span elapses, and botocore raises botocore.exceptions.EndpointConnectionError: Could not connect to the endpoint URL: "http://<announced host:port>/getMedia". No response is produced: no HTTP status, no x-amzn-ErrorType, no Message. The two checks' earlier legs prove the authority is reachable while the stream exists (one of them PUTs a fragment and reads it back 200 on that authority, the other asserts 200 on a live stream, and the sibling checks in this service receive real ValidationException/InvalidArgumentException bodies from the same discovery path), so the failing call is specifically the one made after the stream record is gone.

Why: a client cannot distinguish "this stream was deleted" from "the service is unreachable". On AWS the deletion surfaces as a 404 ResourceNotFoundException that an SDK raises as a ClientError and does not retry; here it surfaces as a connection error, which SDK retry policies treat as a transient transport fault and retry (and which many clients escalate as an outage/alarm). Session code that keys a reader on a discovered DataEndpoint — exactly what GetDataEndpoint exists for — also loses the ability to observe any further modeled refusal on that endpoint, including InvalidEndpointException and NotAuthorizedException, because nothing answers there any more. Port churn of this kind is additionally not something AWS exhibits: an announced endpoint never stops listening.

Fix: the data-endpoint lifetime in the kinesisvideo control-plane state, not the GetMedia handler's text. The announced (stream, plane) listener must outlive the stream record — keep the listener bound for the life of the store (or serve all streams of a plane from one listener, as AWS's regional data host does) and let the request reach the media service, so GetMedia for a stream that no longer resolves is answered by the handler's stream-resolution step with the modeled ResourceNotFoundException (404) and AWS's fixed sentence, instead of being dropped at the socket. The stream-removal step must stop closing the port; if the announced-endpoint ceiling is the reason entries are reclaimed, reclaim must not strand an address already handed to a client.

Traces: kinesis-video-media.get-media-deleted-stream-rejected (GetMedia after DeleteStream on an empty stream), kinesis-video-media.get-media-after-ingested-stream-deletion-rejected (same, after a fragment was ingested via PutMedia and read back 200). Code citations: services/service-com-amazonaws-kinesisvideo-kinesisvideo-20170930/src/state.rs lines 528-549 (settle_clock removing the stream and its data_endpoints); src/state/data_endpoint.rs lines 154-167 and 211-274 (the listener held by DataPlaneEndpoint, opened per stream and plane), lines 281-292 (the release-on-removal policy); src/ops/get_data_endpoint.rs lines 195-230 (the address announced to the client).

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout: kinesis-video-media 7/16 (44%), 9 failed, of which 7 are marked OWNED by 20261029T231704Z-kinesis-video-media-getmedia-rejection-fidelity-six-divergences-in-validationexception-invalidargumentexception.md and skipped. The two remaining failures are both verdict 'error':

  [error] kinesis-video-media.get-media-deleted-stream-rejected
    botocore.exceptions.EndpointConnectionError: Could not connect to the endpoint URL: "http://127.0.0.1:43295/getMedia"
  [error] kinesis-video-media.get-media-after-ingested-stream-deletion-rejected
    botocore.exceptions.EndpointConnectionError: Could not connect to the endpoint URL: "http://127.0.0.1:38805/getMedia"

(The 127.0.0.1:<port> authorities are the per-run addresses the emulator itself announced from GetDataEndpoint; the host and port are environment-specific and are not part of the requirement. The requirement is that whatever address GetDataEndpoint announces keeps answering.)

Expected vs actual for the post-deletion GetMedia on the previously announced GET_MEDIA endpoint:
  expected: HTTP 404, Code 'ResourceNotFoundException', Message 'The requested stream is not found or not active.' (entire sentence fixed text, no identifier interpolated) — a botocore ClientError.
  actual:   no HTTP response at all; the TCP connect to the announced authority fails, botocore raises EndpointConnectionError, so there is no status, no error code and no message.

The preceding legs of the same two checks show the endpoint is reachable while the stream exists: get-media-deleted-stream-rejected asserts GetMedia returns 200 on the live stream before DeleteStream, and get-media-after-ingested-stream-deletion-rejected PutMedia's one fragment (HTTP 200, one PERSISTED ack) and reads that fragment back with GetMedia 200 before DeleteStream. The emulator's own code states why the address then dies: services/service-com-amazonaws-kinesisvideo-kinesisvideo-20170930/src/state.rs settle_clock removes the stream and immediately does `self.data_endpoints.retain(|announced, _| announced.stream_arn != stream_arn)`, and DataPlaneEndpoint's held `_listener` closes the port when that record is dropped.

This is also why the previously fixed issue 20261031T070053Z-kinesis-video-media-getmedia-3-net-new-root.md no longer passes its Cause 1 check: the message it corrected can no longer be observed, because the call is never answered.

## Steps to Reproduce
Boot the emulator from this checkout.

1. kinesisvideo.create_stream(StreamName=<name>, DataRetentionInHours=1); poll kinesisvideo.describe_stream(StreamName=<name>) until StreamInfo.Status == 'ACTIVE'.
2. endpoint = kinesisvideo.get_data_endpoint(APIName='GET_MEDIA', StreamName=<name>)['DataEndpoint']; build a kinesis-video-media client with endpoint_url=endpoint (this is the only endpoint a client is told to use, and it is kept for the session).
3. media.get_media(StreamName=<name>, StartSelector={'StartSelectorType':'EARLIEST'}) -> HTTP 200 (the announced authority answers while the stream exists).
4. kinesisvideo.delete_stream(StreamARN=<arn>); poll kinesisvideo.describe_stream until it raises ResourceNotFoundException (the emulator's DELETING span is ~3s, after which settle_clock removes the record).
5. Call media.get_media(StreamName=<name>, StartSelector={'StartSelectorType':'EARLIEST'}) again on the SAME client/endpoint: botocore raises EndpointConnectionError ('Could not connect to the endpoint URL: "<announced endpoint>/getMedia"') instead of a ClientError with HTTP 404, Code 'ResourceNotFoundException' and Message 'The requested stream is not found or not active.'

Variant that also reproduces it (second failing check): between steps 2 and 4, POST the stream's PUT_MEDIA data endpoint with a one-fragment Matroska body (x-amzn-stream-arn, x-amzn-fragment-timecode-type: RELATIVE, x-amzn-producer-start-timestamp), confirm the PERSISTED ack and that GetMedia returns those bytes; after the delete the same GetMedia call is refused at the socket in the same way.

Note the port the emulator announced in step 2 is closed by step 5 — e.g. `curl http://<that authority>/getMedia` fails to connect — which is the observable root cause.
