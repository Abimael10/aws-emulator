Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ivs.native-rtmps-live-stream-lifecycle
Issue-Shape-Id: com.amazonaws.ivs#AmazonInteractiveVideoService
Issue-Key: ivs.native-rtmps-live-stream-lifecycle|run#17|AssertionError|assert code2 == "ValidationException", f"expected ValidationException (no ad configuration), got {code2!r}"
Issue-Commit: c19026fddc546a37c1d4d3954f77370ec86334b1

# IVS InsertAdBreak answers 404 ChannelNotBroadcasting on a channel with no ad configuration instead of 400 ValidationException

> _Note: this issue was identified against the code base at commit `c19026fddc546a37c1d4d3954f77370ec86334b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InsertAdBreak

## Description
Behavioral replay (simparity-behavioral-aws-v1) of the IVS emulator (service token `ivs`, crate services/service-com-amazonaws-ivs-amazoninteractivevideoservice) shows InsertAdBreak evaluating its preconditions in the wrong order: it refuses a channel that has NO associated ad configuration with the broadcast-state error instead of the ad-configuration validation error. One cause.

Cause 1 of 1: the ad-configuration precondition is never checked, so the stream/broadcast-state check answers first.

AWS: Smithy (services/service-com-amazonaws-ivs-amazoninteractivevideoservice/smithy-model/com-amazonaws-ivs-amazoninteractivevideoservice.json, com.amazonaws.ivs, restJson1) InsertAdBreak(channelArn: ChannelArn required @length 1..128 @pattern ^arn:aws:ivs:[a-z0-9-]+:[0-9]+:channel/[a-zA-Z0-9-]+$, durationSeconds: AdDurationSeconds required @range 1..300) -> adBreakId; modelled errors include ValidationException (@httpError 400, doc "The input fails to satisfy the constraints specified by an Amazon Web Services service") and ChannelNotBroadcasting (@httpError 404, doc "The stream is offline for the given channel ARN"). The ORDER is recorded behavior: for a channel with no ad configuration associated, AWS answers HTTP 400 with error code `ValidationException` (both in the body's error code member and in `x-amzn-errortype`) and the message template `Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: <channelArn>`. In that message the fixed text is `Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: ` and the ONLY variable part is the request's own `channelArn` member (the particular ARN any single run produces is a generated id filling that slot, not part of the requirement). The recording shows this 400 fired identically in all three broadcast states the check drives — before any ingest, while the channel is genuinely LIVE with an RTMPS push, and after StopStream — i.e. the precondition is unconditional and is evaluated BEFORE broadcast state. ChannelNotBroadcasting (404, message `Channel: <channelArn> is not currently online`, fixed text `Channel: ` + ARN + ` is not currently online`) is reached only once an ad configuration IS associated and no stream is live.

Emulator: answers HTTP 404 with error code `ChannelNotBroadcasting` and message `Channel: <channelArn> is not currently online` for the same request. Replay detail: `AssertionError: expected ValidationException (no ad configuration), got 'ChannelNotBroadcasting'` at the first InsertAdBreak of the check (before any ingest), so the ad-configuration check is skipped entirely and the handler falls through to the stream-state lookup.

Why: a client exercising SSAI ad insertion is told the wrong thing about its own misconfiguration. Against AWS it learns the channel needs an ad configuration associated — a non-retryable 400 the caller fixes by attaching one; against the emulator it is told the channel is merely offline, so the caller waits for or starts a stream that would never make the call succeed. Any error branch keyed on ValidationException is dead, while the ChannelNotBroadcasting branch fires spuriously even for a LIVE channel, and the status code differs too (404 vs 400), changing retry/classification decisions in the SDK's error handling.

Fix: the operation's validation, ahead of its state lookup — InsertAdBreak's handler must resolve the channel's associated ad configuration (the channel's adConfiguration linkage / the ad-configuration store in the crate's state) and, when there is none, return ValidationException with the member-named validation message above, regardless of broadcast state, before consulting the stream state that produces ChannelNotBroadcasting.

Related open issue, which does NOT own this check (the replay report lists this failure with no OWNED-by and no stub path): 20261116T010003Z-ivs-insertadbreak-reports-channelnotbroadcasting-instead-of-validating-the.md describes the same root cause from the WIRE corpus and carries only that corpus's replay key (ivs|InsertAdBreak|4xx->4xx|...; fixture ivs-stream-operations-not-broadcasting#task#6); this issue carries the behavioral check id so the fix is also gated on the behavioral lifecycle replay. 20261023T004057Z-ivs-7-behavioural-divergences-error-messages-dropped-on.md, Cause 2 of 7, described InsertAdBreak as an unimplemented 501 stub; the operation now answers a modelled 404, so that cause does not describe this precondition ordering.

Traces: ivs.native-rtmps-live-stream-lifecycle; handler at services/service-com-amazonaws-ivs-amazoninteractivevideoservice/src/ops/insert_ad_break.rs.

## Evidence
Behavioral suite run: 67/85 passed (78.8%), mismatched 8, errored 10, skipped 0, unloadable 0; bootstrap_signal false. Service ivs 67/79 (85%), 12 failed.

Failing check [mismatch] ivs.native-rtmps-live-stream-lifecycle (GROUP native-protocol, SERVICES [ivs]); not marked ALREADY FILED and not marked OWNED by any open issue.

Replay detail (tail):
  File "check_ivs_native_rtmps_live_stream_lifecycle.py", line 54, in run
    assert code2 == "ValidationException", f"expected ValidationException (no ad configuration), got {code2!r}"
  AssertionError: expected ValidationException (no ad configuration), got 'ChannelNotBroadcasting'

The check's own documentation of AWS behavior: "InsertAdBreak requires a valid ad configuration on the channel, checked unconditionally -- confirmed by replay to fire identically before ingest, while genuinely LIVE, and after StopStream, i.e. it is gated by this precondition rather than by broadcast state." The check asserts the same (code, message) pair at all three points:
  expected code: ValidationException (HTTP 400)
  expected message: "Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: <channelArn>"
  actual code:   ChannelNotBroadcasting (HTTP 404)
  actual message: "Channel: <channelArn> is not currently online"

Message anatomy: expected message fixed text = "Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: ", variable part = the request's `channelArn` member, verbatim. The 404 message's fixed text is "Channel: " ... " is not currently online" with the same ARN slot; that message is correct only for the broadcast-state error, which is not the error due here.

Smithy: InsertAdBreak errors = AccessDeniedException 403, ChannelNotBroadcasting 404, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400. Input: channelArn (required, @length 1..128, @pattern ^arn:aws:ivs:[a-z0-9-]+:[0-9]+:channel/[a-zA-Z0-9-]+$), durationSeconds (required, @range 1..300). HTTP binding POST /InsertAdBreak.

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral check ivs.native-rtmps-live-stream-lifecycle, or by hand against the data listener:
1. CreateChannel with a body naming only `name` (and tags) — no ad configuration is associated with it. Note the returned channel arn.
2. Do NOT start a stream on it.
3. POST /InsertAdBreak with {"channelArn": "<that arn>", "durationSeconds": 6}.
4. Observe HTTP 404, x-amzn-errortype `ChannelNotBroadcasting`, message "Channel: <arn> is not currently online". AWS answers HTTP 400, error code `ValidationException`, message "Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: <arn>".
5. Push an RTMPS stream to the channel's ingestEndpoint with the stream key until GetStream reports state LIVE, then repeat step 3: AWS still answers the same 400 ValidationException (the precondition is not state-gated). Repeat once more after StopStream: again the same 400.
