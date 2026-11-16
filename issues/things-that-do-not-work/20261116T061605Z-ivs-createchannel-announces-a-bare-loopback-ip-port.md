Issue-Source: trace-sweep-divergence
Issue-Trace: ivs-channel-lifecycle
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ivs-channel-lifecycle ivs-viewer-session-revocation
Issue-Shape-Id: com.amazonaws.ivs#AmazonInteractiveVideoService
Issue-Key: ivs|CreateChannel|2xx->2xx|$.channel.srt.endpoint
Issue-Related: 20261114T063329Z-ivs-createchannel-announces-loopback-host-port-endpoints-and.md
Issue-Commit: 9de958fc7b077b99165fa1382cc42efe4e958ebf

# IVS CreateChannel announces a bare loopback IP:port where AWS announces a DNS hostname

> _Note: this issue was identified against the code base at commit `9de958fc7b077b99165fa1382cc42efe4e958ebf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChannel

## Description
Replaying the wire corpus (simparity-wire-aws-v1) against the IVS emulator (service token `ivs`, crate services/service-com-amazonaws-ivs-amazoninteractivevideoservice) shows CreateChannel returning a channel whose three ANNOUNCED ADDRESS members all fall outside the shape class the recording holds for them. Status and the rest of the channel projection are correct (expected 200, got 200). ONE cause, grounded in the representative step fixture ivs-channel-lifecycle, step task#1; the cluster also covers ivs-viewer-session-revocation#setup#1, so both fixtures close with one fix.

Cause 1 of 1: `$.channel.ingestEndpoint`, `$.channel.playbackUrl` and `$.channel.srt.endpoint` are composed from the emulator's own listener address as a bare loopback IP carrying an explicit `:<port>`, instead of a DNS hostname with no port.

AWS: recorded behavior only. Smithy (com.amazonaws.ivs, restJson1): `Channel.ingestEndpoint` -> `IngestEndpoint`, `Channel.playbackUrl` -> `PlaybackURL` and `Srt.endpoint` -> `SrtEndpoint` are all plain strings with NO @length and NO @pattern (docs: "Channel ingest endpoint, part of the definition of an ingest server, used when you set up streaming software" / "Channel playback URL" / "The endpoint to be used when streaming with IVS using the SRT protocol"), so the model states no form and the recording is the only authority. The corpus does not pin these values literally — it reports them `<shaped-not-asserted>` because the host label, region, account id, channel id and port are per-run — it pins their SHAPE CLASS: the ingest endpoint is a DNS HOSTNAME (a minted lowercase-hex label under a service-owned contribute domain) with NO scheme and NO port; the SRT endpoint is the SAME host as `ingestEndpoint` (corroborated inside this corpus by ivs-batch-get-channel-and-stream-key#task#3, where `$.channels[0].ingestEndpoint` and `$.channels[0].srt.endpoint` are one identical host); and the playback URL is an `https://` URL whose HOST is a hostname of that same class with the recorded `/api/video/v1/<region>.<account>.channel.<channel-id>.m3u8` path layout. The template — hostname form, no port, scheme and path layout — is the requirement; the label, ids and port that fill it are not.

Emulator: the replay reports all three members as shape mismatches on a 200 response. `ingestEndpoint` and `srt.endpoint` come back as a bare loopback IP literal with a `:<port>` suffix and no domain label at all, and `playbackUrl` comes back as `https://<loopback-ip>:<port>/api/video/v1/<region>.<account>.channel.<id>.m3u8` — the recorded PATH layout is preserved but the host is the emulator's own listener address rather than a hostname of the recorded class. src/ops/create_channel.rs is explicit that this is deliberate: lines 250-266 store `ingest_endpoint: None`, `playback_url: None` and `Srt { endpoint: None, .. }` and defer all three to a shared projection (`crate::state::announced_channel`) that composes "the DEPLOYMENT's address" for every request that reports the channel. The specific IP and port are per-run and are NOT the finding; the finding is that an address of that FORM (bare IP, explicit port, no domain label) is a different shape class from the hostname AWS issues.

Why: a client does not treat a channel's contribute address as an opaque string. It composes `rtmps://<ingestEndpoint>:443/app/<streamKey>` or `srt://<srt.endpoint>:<port>`, appending its own port — so an endpoint that already carries `:<port>` yields a doubly-ported, unusable URL; code that splits the host on `.` to recover the channel label, that validates the value as a hostname, or that stores it in a hostname-typed field rejects a bare IP outright. For `playbackUrl` a player that parses the host to select a CDN or to assert TLS-by-hostname mis-handles it. Additionally the announced host is loopback, so any client not on the emulator's own host cannot reach the address the emulator advertises. All of this works against AWS and breaks against this emulator.

Fix: the shared channel projection that composes these three members — `crate::state::announced_channel` in services/service-com-amazonaws-ivs-amazoninteractivevideoservice/src/state.rs, which create_channel.rs delegates to — not the serializer and not the store (the members are never persisted). It must announce an address of AWS's recorded shape class: one hostname (label plus domain) shared by `ingestEndpoint` and `srt.endpoint`, and that same class of host inside the `https://.../api/video/v1/<region>.<account>.channel.<id>.m3u8` playback URL, with no port component — and the host it announces must be one the client is actually reached on, not loopback.

Traces: ivs-channel-lifecycle#task#1 (representative) and ivs-viewer-session-revocation#setup#1 (same cluster, affected_count 2); recorded endpoint shape corroborated by ivs-batch-get-channel-and-stream-key#task#3. Source citations: src/ops/create_channel.rs:231-273 (members left None with the deferring comment) and `crate::state::announced_channel` in src/state.rs.

Related open issues, NONE of which owns this cluster (the replay reports owned_by [] for it): 20261114T063329Z-ivs-createchannel-announces-loopback-host-port-endpoints-and.md describes the same endpoint-form defect for this step but carries replay keys only for `$.channel.ingestEndpoint` and `$.channel.playbackUrl` (plus a `$.channel.srt.passphrase` cause that this replay no longer reports as diverging), so it does not carry the current cluster's key, which also spans `$.channel.srt.endpoint`; 20261103T113752Z-ivs-createchannel-srt-block-mints-an-endpoint-and.md covers only the `$.channel.srt.*` members and likewise does not carry this cluster's key; and 20261109T213051Z-ivs-realtime-stage-responses-announce-upstream-live-video.md is the same class of defect (announced endpoint form) in ivs-realtime.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true ("/__admin/health answered 200 and the :44341 data listener accepts"), steps_total 84, passed 67, mismatched 12, blocked 5, errored 0, score 79.8%, divergence_count 5, bootstrap_signal false.

The one net-new cluster (every other cluster in this replay carries a non-empty owned_by):
  divergence_key: service=ivs|CreateChannel|status=200->200|keys=$.channel.ingestEndpoint,$.channel.playbackUrl,$.channel.srt.endpoint
  verdict: mismatch, owned_by: [], stub_path: ""
  expected_status 200, actual_status 200 (status is CORRECT)
  representative: ivs-channel-lifecycle#task#1; affected_count 2, other member ivs-viewer-session-revocation#setup#1
  detail:
    $.channel.ingestEndpoint: value_mismatch (expected '<shaped-not-asserted>', got '127.0.0.1:39433')
    $.channel.playbackUrl: value_mismatch (expected '<shaped-not-asserted>', got 'https://127.0.0.1:39433/api/video/v1/us-west-2.000000000000.channel.a29c9a9ce359.m3u8')
    $.channel.srt.endpoint: value_mismatch (expected '<shaped-not-asserted>', got '127.0.0.1:39433')

The literal values above are ONE run's output and are NOT themselves requirements: the corpus marks all three members shaped-not-asserted, and the port, the channel id 'a29c9a9ce359' and the account '000000000000' change every run while the mismatch persists. What diverges is the shape class — a bare IP plus explicit port where a hostname with no port is recorded (ingestEndpoint, srt.endpoint, and the host component of playbackUrl, whose recorded path layout the emulator does reproduce).

Recorded shape corroboration from the same corpus: ivs-batch-get-channel-and-stream-key#task#3 records $.channels[0].ingestEndpoint and $.channels[0].srt.endpoint as one identical DNS host (a lowercase-hex label under a service-owned contribute domain, no port).

Smithy (services/service-com-amazonaws-ivs-amazoninteractivevideoservice/smithy-model/com-amazonaws-ivs-amazoninteractivevideoservice.json): Channel.ingestEndpoint -> IngestEndpoint (string, no constraints); Channel.playbackUrl -> PlaybackURL (string, no constraints); Srt.endpoint -> SrtEndpoint (string, no constraints) — hence "recorded behavior only" for all three. Contrast the constrained members of the same structure (Channel.arn @pattern ^arn:aws:ivs:[a-z0-9-]+:[0-9]+:channel/[a-zA-Z0-9-]+$), which the emulator gets right.

Emulator source backing the actual values: src/ops/create_channel.rs:244-273 — `ingest_endpoint: None`, `playback_url: None`, `Srt { endpoint: None, passphrase: Some(..) }`, with the comments at 232-236 and 250-253 stating both announced addresses "are this emulator's own, composed for the request that reports the channel by the shared projection below" (`crate::state::announced_channel`) and "never persisted".

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus), or exercise it by hand against the data listener in region us-west-2:
1. POST /CreateChannel with a body naming only `name` (fixture ivs-channel-lifecycle#task#1; ivs-viewer-session-revocation#setup#1 issues the same call in its setup phase).
2. Read `$.channel.ingestEndpoint`, `$.channel.playbackUrl` and `$.channel.srt.endpoint` from the 200 response.
3. Observe that `ingestEndpoint` and `srt.endpoint` are one bare loopback IP carrying an explicit `:<port>` and no domain label, and that `playbackUrl`'s host is that same IP:port while its path keeps the recorded `/api/video/v1/<region>.<account>.channel.<id>.m3u8` layout. The corpus records these as a DNS hostname with no port (confirmed by ivs-batch-get-channel-and-stream-key#task#3, where ingestEndpoint and srt.endpoint are one identical hostname); the replay reports all three as shape mismatches.
4. Compose what a contributor would: `rtmps://<ingestEndpoint>:443/app/<streamKey>` — the result carries two ports and is unusable.
5. Re-run: the port and channel id change and all three mismatches persist — the per-run values are not the divergence, the shape class is.
