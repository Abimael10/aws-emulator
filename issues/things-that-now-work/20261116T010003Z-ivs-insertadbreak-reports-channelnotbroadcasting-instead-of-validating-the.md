Issue-Source: trace-sweep-divergence
Issue-Trace: ivs-stream-operations-not-broadcasting
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ivs-stream-operations-not-broadcasting
Issue-Shape-Id: com.amazonaws.ivs#AmazonInteractiveVideoService
Issue-Key: ivs|InsertAdBreak|4xx->4xx|$.Code
Issue-Key: ivs|InsertAdBreak|4xx->4xx|$.Message
Issue-Key: ivs|InsertAdBreak|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: ivs|InsertAdBreak|4xx->4xx|status
Issue-Commit: 82088200bb48db9041b40c00c264266c5bbb9a19

# IVS InsertAdBreak reports ChannelNotBroadcasting instead of validating the channel's ad configuration first

> _Note: this issue was identified against the code base at commit `82088200bb48db9041b40c00c264266c5bbb9a19`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
InsertAdBreak

## Description
Replaying the wire corpus (simparity-wire-aws-v1) against the IVS emulator (service token `ivs`, crate services/service-com-amazonaws-ivs-amazoninteractivevideoservice) shows InsertAdBreak refusing a channel that has NO ad configuration with the wrong precondition: it reports the broadcast-state failure (404 ChannelNotBroadcasting) where AWS reports the ad-configuration validation failure (400 ValidationException). One cause.

Cause 1 of 1 — the ad-configuration precondition is not checked, so the broadcast-state check answers first.
AWS: Smithy (com.amazonaws.ivs, restJson1) InsertAdBreak(channelArn: ChannelArn required @length 1..128 @pattern ^arn:aws:ivs:[a-z0-9-]+:[0-9]+:channel/[a-zA-Z0-9-]+$, durationSeconds: AdDurationSeconds required @range 1..300) -> adBreakId; modelled errors include ValidationException (@httpError 400, "The input fails to satisfy the constraints specified by an Amazon Web Services service") and ChannelNotBroadcasting (@httpError 404, "The stream is offline for the given channel ARN"). The ORDER is recorded behavior: the recorded AWS response for an InsertAdBreak against a channel with no associated ad configuration is HTTP 400 with `$.Code` / `x-amzn-errortype` = `ValidationException` and the service's validation message template `Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: <channelArn>` — the fixed text is `Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: `, and the only variable part is the channelArn taken verbatim from the request. The ad-configuration precondition is evaluated BEFORE broadcast state, so this 400 is returned whether the channel is idle or LIVE; ChannelNotBroadcasting (404, message `Channel: <channelArn> is not currently online`) is reached only once an ad configuration IS associated and no stream is live.
Emulator: answers HTTP 404 with `$.Code` = `ChannelNotBroadcasting`, `x-amzn-errortype` = `ChannelNotBroadcasting` and `$.Message` = `Channel: <channelArn> is not currently online` — i.e. it skips the ad-configuration check entirely and falls straight through to the stream-state check. Replay detail: status expected 400, got 404; Code expected 'ValidationException', got 'ChannelNotBroadcasting'; errortype header likewise; Message expected the validation template above, got the offline template.
Why: a client exercising SSAI ad insertion is told the wrong thing about its own misconfiguration. Against AWS it learns that the channel needs an ad configuration associated (a 400 the caller fixes by attaching one); against the emulator it is told the channel is merely offline, so the caller retries/waits for a stream that would never make the call succeed, and any error branch keyed on ValidationException (4xx-non-retryable, surface to the operator) is dead while the ChannelNotBroadcasting branch fires spuriously.
Fix: the operation's validation, ahead of its state lookup — InsertAdBreak's handler must resolve the channel's associated ad configuration (channel.adConfigurationArn / the ad-configuration store in crate::state) and, when there is none, return ValidationException with the member-named validation message above, before consulting the stream/broadcast state that produces ChannelNotBroadcasting.
Traces: ivs-stream-operations-not-broadcasting#task#6 (representative and sole member of the cluster); handler at services/service-com-amazonaws-ivs-amazoninteractivevideoservice/src/ops/insert_ad_break.rs.

Related open issues, NONE of which owns this cluster (the replay reports owned_by [] and stub_path "" for it): 20261023T004057Z-ivs-7-behavioural-divergences-error-messages-dropped-on.md, Cause 2 of 7, described InsertAdBreak as an unimplemented scaffold stub answering HTTP 501 NotImplemented — the operation is now implemented (it answers a modelled 404 with a populated message), so that cause's recorded ACTUAL no longer describes the emulator, it carries behavioural check ids rather than this cluster's wire replay key, and it does not cover the precondition ORDER this issue is about.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true ("/__admin/health answered 200 and the :39603 data listener accepts"), steps_total 84, passed 67, mismatched 12, blocked 5, errored 0, score 79.8%, divergence_count 5, bootstrap_signal false.

Cluster:
  divergence_key: service=ivs|InsertAdBreak|status=400->404|keys=$.Code,$.Message,$headers.x-amzn-errortype,status
  fixture ivs-stream-operations-not-broadcasting, step task#6, verdict mismatch, affected_count 1, owned_by [], stub_path ""
  expected_status 400, actual_status 404
  detail:
    status: expected 400, got 404
    $.Code: value_mismatch (expected 'ValidationException', got 'ChannelNotBroadcasting')
    $.Message: value_mismatch (expected 'Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: arn:aws:ivs:us-west-2:000000000000:channel/b8f130ac9537', got 'Channel: arn:aws:ivs:us-west-2:000000000000:channel/b8f130ac9537 is not currently online')
    $headers.x-amzn-errortype: shape_mismatch (expected 'ValidationException', got 'ChannelNotBroadcasting')

Message anatomy: in the expected message the fixed text is `Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: `; the trailing ARN is the request's own `channelArn` member. The particular ARN in this recording (arn:aws:ivs:us-west-2:000000000000:channel/b8f130ac9537) is a per-run generated channel id filling that slot and is NOT itself the requirement. The HTTP status (400), the error code (`ValidationException`, in both `$.Code` and `x-amzn-errortype`) and the message template ARE the requirements.

Smithy (services/service-com-amazonaws-ivs-amazoninteractivevideoservice/smithy-model/com-amazonaws-ivs-amazoninteractivevideoservice.json): InsertAdBreak errors = AccessDeniedException 403, ChannelNotBroadcasting 404, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400; ValidationException doc "The input fails to satisfy the constraints specified by an Amazon Web Services service"; ChannelNotBroadcasting doc "The stream is offline for the given channel ARN". Both carry their detail under the `exceptionMessage` member; the recorded wire body uses the SDK-readable message key (the emulator already populates `$.Message` here, so only the classification and wording diverge).

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus), or by hand against the data listener in region us-west-2:
1. POST /CreateChannel with a body naming only `name` (no adConfiguration associated) and note the returned channel arn.
2. Do NOT start a stream on it.
3. POST /InsertAdBreak with {"channelArn": "<that arn>", "durationSeconds": 30} (fixture ivs-stream-operations-not-broadcasting#task#6).
4. Observe HTTP 404, x-amzn-errortype `ChannelNotBroadcasting`, body {"Code": "ChannelNotBroadcasting", "Message": "Channel: <arn> is not currently online"}. AWS answers HTTP 400, x-amzn-errortype `ValidationException`, Message "Request member: channelArn failed to satisfy the constraints: valid ad configuration is required to be associated with the channel requested for the ad break with ARN: <arn>" because the ad-configuration precondition is checked first.
