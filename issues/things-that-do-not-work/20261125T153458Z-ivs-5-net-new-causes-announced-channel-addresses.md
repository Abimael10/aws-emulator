Issue-Source: trace-sweep-divergence
Issue-Trace: ivs-channel-lifecycle
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ivs-channel-lifecycle ivs-viewer-session-revocation ivs-batch-get-channel-and-stream-key
Issue-Shape-Id: com.amazonaws.ivs#AmazonInteractiveVideoService
Issue-Key: ivs|CreateChannel|2xx->2xx|$.channel.ingestEndpoint
Issue-Key: ivs|CreateChannel|2xx->2xx|$.channel.playbackUrl
Issue-Key: ivs|BatchGetChannel|2xx->2xx|$.channels[*].playbackUrl
Issue-Key: ivs|GetChannel|2xx->2xx|$.channel.playbackUrl
Issue-Key: ivs|UpdateChannel|2xx->2xx|$.channel.playbackUrl
Issue-Related: 20261123T105030Z-ivs-2-net-new-causes-createchannel-announces-loopback.md
Issue-Commit: e9279a32366be341735568d509d6722efe13b076

# IVS: 5 net-new causes — announced channel addresses name hosts the emulator does not serve, so playbackUrl, ingestEndpoint and srt.endpoint are undialable on every channel projection

> _Note: this issue was identified against the code base at commit `e9279a32366be341735568d509d6722efe13b076`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChannel

## Description
Replaying the wire corpus (simparity-wire-aws-v1) against the IVS emulator (service token `ivs`, crate services/service-com-amazonaws-ivs-amazoninteractivevideoservice) leaves FIVE net-new divergence clusters that no open issue owns — the replay reports owned_by [] and stub_path "" for all five. They are bundled here because they belong to one service and share one fault: every operation that projects a `Channel` announces address members whose HOST is a name this emulator does not serve, so the replay rejects the value as host_not_own (and, where the corpus pins only the shape class, as a value_mismatch against `<shaped-not-asserted>`). A fixer must close ALL FIVE clusters — they are reported per operation and per step, so each must be re-replayed — before submitting.

Shared root cause, for all five: the projection services/service-com-amazonaws-ivs-amazoninteractivevideoservice/src/state/channel_endpoints.rs composes every announced address from a MINTED name in a zone it derives from the request's reached authority (`announced_zone` -> `::service_common::data_plane_addressing::announced_zone_of_authority`), never from an authority this process actually answers on: `contribute_host_in` yields `<12-hex>.global-contribute.ivs.<zone>` (lines 208-216) and `playback_host_in` yields `<12-hex>.<region>.playback.ivs.<zone>` (lines 276-285), both with no port, and the ports they are supposedly reached at are compile-time constants (`PLAYBACK_PORT = 443`, `CONTRIBUTE_PORTS = [443, 1935]`, lines 118-135) rather than the socket the plane bound. The module's own doc comment concedes the consequence (lines 190-201): the register that resolves those names "serves a connection with a pipe inside this process" and "a client off this machine could not be served a pipe in this process whatever name it was told". So the names are resolvable only in-process, while the response hands them to an out-of-process client as the address to dial.

Cause 1 of 5 — CreateChannel announces all three address members as names the emulator does not serve. Fixture ivs-channel-lifecycle, step task#1 (cluster also covers ivs-viewer-session-revocation#setup#1).
AWS: recorded behavior only. Smithy (services/service-com-amazonaws-ivs-amazoninteractivevideoservice/smithy-model/com-amazonaws-ivs-amazoninteractivevideoservice.json, com.amazonaws.ivs, restJson1): CreateChannel POST /CreateChannel -> 200 {channel: Channel, streamKey: StreamKey}; `Channel$ingestEndpoint` -> `IngestEndpoint`, `Channel$playbackUrl` -> `PlaybackURL` and `Srt$endpoint` -> `SrtEndpoint` are plain strings with NO @length and NO @pattern, documented respectively "Channel ingest endpoint, part of the definition of an ingest server, used when you set up streaming software", "Channel playback URL" and "The endpoint to be used when streaming with IVS using the SRT protocol". The model states no form, so the recording is the only authority, and the corpus does not pin these values literally (it reports them `<shaped-not-asserted>` because the label, Region, account and channel id are per-run): it pins their CLASS. That class is an address a client of the service can actually REACH — the ingest endpoint is a host an encoder opens `rtmps://<ingestEndpoint>:443/app/<streamKey>` against, `srt.endpoint` is that same host, and `playbackUrl` is an `https://` URL a player FETCHES as it stands, laid out `/api/video/v1/<region>.<account>.channel.<channel-id>.m3u8`. Hostname form without a port, the shared ingest/SRT host, the scheme and the path layout are the requirement; the particular label, zone, ids and account of any one run are not.
Emulator: the replay reports all three members as divergent on an otherwise correct 200 (expected 200, got 200): `$.channel.ingestEndpoint` and `$.channel.srt.endpoint` come back as one minted `<12-hex>.global-contribute.ivs.<zone>` name and `$.channel.playbackUrl` as `https://<12-hex>.<region>.playback.ivs.<zone>/api/video/v1/<region>.<account>.channel.<id>.m3u8` — the hostname form and the path layout are now right, but the HOST is a name nothing outside this process resolves to the emulator, and no port of a listener this emulator bound is announced for it. The same values are rejected as host_not_own by the four read/update clusters below, where the corpus does pin them.
Why: a client does not treat these as opaque strings. It points an encoder at `rtmps://<ingestEndpoint>:443/app/<streamKey>` or `srt://<srt.endpoint>:<port>`, and a player GETs the playbackUrl verbatim. Against AWS both connect; here the hostname resolves to nothing for any client not inside the emulator process, so ingest never starts, no stream goes LIVE, and no playlist is ever fetched — the entire IVS data plane is unreachable while the control-plane responses claim success.
Fix: the announced-address projection and the planes it names — `announced_ingest_endpoint`/`contribute_host_in` and `announced_playback_url`/`playback_host_in` in src/state/channel_endpoints.rs, together with `super::ensure_ingest_plane` and `super::playback_plane`/`super::publish_playback_addresses` — must announce an authority THIS process answers on for an out-of-process client (one of the emulator's own hosts plus the port the plane actually bound, or a per-resource listener), not a minted name in a zone resolvable only in-process. Not the serializer and not the store: create_channel.rs persists none of the three members and delegates all of them here.
Traces: ivs-channel-lifecycle#task#1 (representative), ivs-viewer-session-revocation#setup#1.

Cause 2 of 5 — GetChannel announces a playbackUrl whose host the emulator does not serve. Fixture ivs-channel-lifecycle, step task#2 (cluster also covers ivs-channel-lifecycle#task#5 and ivs-viewer-session-revocation#task#5).
AWS: recorded behavior only. Smithy: GetChannel POST /GetChannel (arn: ChannelArn required @length 1..128 @pattern ^arn:aws:ivs:[a-z0-9-]+:[0-9]+:channel/[a-zA-Z0-9-]+$) -> 200 {channel: Channel}; errors AccessDeniedException 403, ResourceNotFoundException 404, ValidationException 400. `Channel$playbackUrl` is an unconstrained string, so the recording is the authority: a read returns the SAME playback URL the create announced, an https URL on a host of the service's playback edge that a player can fetch.
Emulator: 200 with the correct path layout, but the replay's verdict on `$.channel.playbackUrl` is host_not_own — the announced host (`<12-hex>.<region>.playback.ivs.<zone>`) is not one of the emulator's own hosts, so the URL is unfetchable by any client of the emulator even though its text round-trips.
Why: a reader that fetches the URL it was handed (a player, or code that validates the manifest is reachable) gets a name-resolution failure where AWS serves a playlist; nothing of the playback path can be exercised.
Fix: same component as Cause 1 — `announced_playback_url`/`playback_host_in` in src/state/channel_endpoints.rs and the playback plane it names, which must be announced at an authority this emulator serves.
Traces: ivs-channel-lifecycle#task#2 (representative), ivs-channel-lifecycle#task#5, ivs-viewer-session-revocation#task#5.

Cause 3 of 5 — UpdateChannel announces the same unreachable playbackUrl host. Fixture ivs-channel-lifecycle, step task#4.
AWS: recorded behavior only. Smithy: UpdateChannel POST /UpdateChannel (arn required, same ChannelArn constraints) -> 200 {channel: Channel}; errors AccessDeniedException 403, ConflictException 409, PendingVerification 403, ResourceNotFoundException 404, ValidationException 400. The updated channel is returned with the same class of fetchable playback URL as the create and the read.
Emulator: 200, `$.channel.playbackUrl` host_not_own — the host announced after an update is the same in-process-only name.
Why: a client that re-reads the channel after an update to refresh its player configuration is handed an address it still cannot dial.
Fix: same component as Cause 1; UpdateChannel's handler runs its record through `announced_channel`, so repairing the projection repairs this projection too.
Traces: ivs-channel-lifecycle#task#4.

Cause 4 of 5 — BatchGetChannel announces an unreachable playbackUrl host for every channel in the batch. Fixture ivs-batch-get-channel-and-stream-key, step task#3.
AWS: recorded behavior only. Smithy: BatchGetChannel POST /BatchGetChannel (arns: ChannelArnList required @length 1..50) -> 200 {channels: Channels, errors: BatchErrors}; errors AccessDeniedException 403, ServiceUnavailable 503, ValidationException 400. Each element of `channels` is the same `Channel` the single read returns, with a fetchable playback URL; this corpus also records that an element's `ingestEndpoint` and `srt.endpoint` are one identical host.
Emulator: 200, with `$.channels[0].playbackUrl` AND `$.channels[1].playbackUrl` both host_not_own — the defect is per element, so a batch read multiplies it.
Why: a client resolving many channels at once (a console or a multi-channel player) receives a whole list of addresses none of which it can fetch.
Fix: same component as Cause 1; the batch projection also delegates to `announced_channel`.
Traces: ivs-batch-get-channel-and-stream-key#task#3.

Cause 5 of 5 — the same BatchGetChannel defect on a single-element batch. Fixture ivs-batch-get-channel-and-stream-key, step task#4.
AWS: recorded behavior only; same Smithy signature as Cause 4.
Emulator: 200 with `$.channels[0].playbackUrl` host_not_own. Reported as its own cluster (different key set, one element rather than two), so it must be re-replayed separately.
Why: as Cause 4.
Fix: as Cause 4.
Traces: ivs-batch-get-channel-and-stream-key#task#4.

Related open issues, NONE of which owns any of these five clusters (the replay reports owned_by [] for all five): 20261123T105030Z-ivs-2-net-new-causes-createchannel-announces-loopback.md carries a CreateChannel replay key for `$.channel.srt.endpoint` only and constrained the member FORM (hostname, no port) — that form is now served, and what remains is reachability, which it does not cover for `$.channel.ingestEndpoint`/`$.channel.playbackUrl`; 20261124T065254Z-ivs-reopens-ivs-channel-rtmps-ingest-remains-undialable-createchannel.md and 20261125T072712Z-ivs-createchannel-announces-an-ingestendpoint-no-out-of.md are BEHAVIORAL issues about the ingest endpoint's reachability and carry behavioral check ids, not these fixture steps, and neither mentions `playbackUrl` reachability or the GetChannel/UpdateChannel/BatchGetChannel projections at all. The content-length and GetStreamSession clusters in this replay DO carry owned_by and are skipped.

## Evidence
Replay summary: trace_set_id simparity-wire-aws-v1, emulator_up true ("/__admin/health answered 200 and the :41875 data listener accepts"), fixtures_replayed 15, steps_total 84, steps_passed 62, steps_mismatched 17, steps_blocked 5, steps_errored 0, score_pct 73.8, divergence_count 9, bootstrap_signal false. Four of the nine clusters carry a non-empty owned_by (DeleteStreamKey/TagResource/UntagResource content-length -> 20261116T010003Z-ivs-empty-bodied-204-responses-omit-the-content.md; GetStreamSession -> 20261123T105030Z-...) and are skipped. The five below are net-new (owned_by [], stub_path "").

Cause 1 cluster:
  divergence_key: service=ivs|CreateChannel|status=200->200|keys=$.channel.ingestEndpoint,$.channel.playbackUrl,$.channel.srt.endpoint
  verdict mismatch; expected_status 200, actual_status 200 (status is CORRECT)
  representative ivs-channel-lifecycle#task#1; affected_count 2, other member ivs-viewer-session-revocation#setup#1
  detail:
    $.channel.ingestEndpoint: value_mismatch (expected '<shaped-not-asserted>', got '04dd86d4eacc.global-contribute.ivs.localhost')
    $.channel.playbackUrl: value_mismatch (expected '<shaped-not-asserted>', got 'https://897b14e7ad41.us-west-2.playback.ivs.localhost/api/video/v1/us-west-2.000000000000.channel.100c465a6569.m3u8')
    $.channel.srt.endpoint: value_mismatch (expected '<shaped-not-asserted>', got '04dd86d4eacc.global-contribute.ivs.localhost')
  The hex labels '04dd86d4eacc'/'897b14e7ad41', the zone 'localhost', the account '000000000000' and the channel id '100c465a6569' are ONE run's values and change every run; the corpus marks all three members shaped-not-asserted. What diverges is that the announced host is not an authority this emulator serves.

Cause 2 cluster:
  divergence_key: service=ivs|GetChannel|status=200->200|keys=$.channel.playbackUrl
  verdict mismatch; expected_status 200, actual_status 200
  representative ivs-channel-lifecycle#task#2; affected_count 3 — ivs-channel-lifecycle#task#5, ivs-viewer-session-revocation#task#5
  detail: $.channel.playbackUrl: host_not_own (expected 'https://897b14e7ad41.us-west-2.playback.ivs.localhost/api/video/v1/us-west-2.000000000000.channel.100c465a6569.m3u8', got 'https://897b14e7ad41.us-west-2.playback.ivs.localhost/api/video/v1/us-west-2.000000000000.channel.100c465a6569.m3u8')
  Expected and actual TEXT are identical: the verdict is not a text difference but host_not_own — the host in the value is not one of the emulator's own hosts, so no client of the emulator can fetch the URL it was handed.

Cause 3 cluster:
  divergence_key: service=ivs|UpdateChannel|status=200->200|keys=$.channel.playbackUrl
  verdict mismatch; expected_status 200, actual_status 200; representative ivs-channel-lifecycle#task#4, affected_count 1
  detail: $.channel.playbackUrl: host_not_own (expected 'https://897b14e7ad41.us-west-2.playback.ivs.localhost/api/video/v1/us-west-2.000000000000.channel.100c465a6569.m3u8', got same)

Cause 4 cluster:
  divergence_key: service=ivs|BatchGetChannel|status=200->200|keys=$.channels[0].playbackUrl,$.channels[1].playbackUrl
  verdict mismatch; expected_status 200, actual_status 200; representative ivs-batch-get-channel-and-stream-key#task#3, affected_count 1
  detail:
    $.channels[0].playbackUrl: host_not_own (expected 'https://897b14e7ad41.us-west-2.playback.ivs.localhost/api/video/v1/us-west-2.000000000000.channel.a463e31a78c9.m3u8', got same)
    $.channels[1].playbackUrl: host_not_own (expected 'https://897b14e7ad41.us-west-2.playback.ivs.localhost/api/video/v1/us-west-2.000000000000.channel.41ae43fb8551.m3u8', got same)

Cause 5 cluster:
  divergence_key: service=ivs|BatchGetChannel|status=200->200|keys=$.channels[0].playbackUrl
  verdict mismatch; expected_status 200, actual_status 200; representative ivs-batch-get-channel-and-stream-key#task#4, affected_count 1
  detail: $.channels[0].playbackUrl: host_not_own (expected 'https://897b14e7ad41.us-west-2.playback.ivs.localhost/api/video/v1/us-west-2.000000000000.channel.a463e31a78c9.m3u8', got same)

Smithy used (services/service-com-amazonaws-ivs-amazoninteractivevideoservice/smithy-model/com-amazonaws-ivs-amazoninteractivevideoservice.json): CreateChannel POST /CreateChannel 200 -> {channel, streamKey}, errors AccessDeniedException 403 / PendingVerification 403 / ResourceNotFoundException 404 / ServiceQuotaExceededException 402 / ValidationException 400; GetChannel POST /GetChannel 200 -> {channel} (arn required @length 1..128 @pattern ^arn:aws:ivs:[a-z0-9-]+:[0-9]+:channel/[a-zA-Z0-9-]+$); UpdateChannel POST /UpdateChannel 200 -> {channel}; BatchGetChannel POST /BatchGetChannel 200 -> {channels, errors} (arns required @length 1..50). Channel$ingestEndpoint -> IngestEndpoint, Channel$playbackUrl -> PlaybackURL, Srt$endpoint -> SrtEndpoint: all plain strings, no @length, no @pattern — hence "recorded behavior only".

Source: src/state/channel_endpoints.rs — `announced_channel` (lines 155-175) fills all three members on every channel projection; `contribute_host_in` (lines 208-216) composes `<minted-label>.global-contribute.ivs.<zone>`; `playback_host_in` (lines 276-285) composes `<minted-label>.<region-label>.playback.ivs.<zone>`; `announced_zone`/`reached_authority` (lines 330-360) derive the zone from the request's reached authority rather than from a listener this process bound; PLAYBACK_PORT/CONTRIBUTE_PORTS (lines 118-135) are constants, not the bound sockets; and the module doc at lines 190-201 states that the register backing these names serves "a connection with a pipe inside this process" and that "a client off this machine could not be served a pipe in this process whatever name it was told".

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus), or exercise each cause by hand against the data listener in region us-west-2:

Cause 1 (CreateChannel):
1. POST /CreateChannel with a body naming only `name` (fixture ivs-channel-lifecycle#task#1; ivs-viewer-session-revocation#setup#1 issues the same call in its setup phase).
2. Read `$.channel.ingestEndpoint`, `$.channel.playbackUrl` and `$.channel.srt.endpoint` from the 200 response.
3. From OUTSIDE the emulator process, resolve the ingest/SRT host and the playback URL's host. Neither resolves to the emulator: they are minted labels under `global-contribute.ivs.<zone>` / `<region>.playback.ivs.<zone>` that only the in-process register answers, and no listener port of the emulator is announced with them. AWS returns addresses a client actually reaches.
4. Attempt the publish anyway: `ffmpeg ... -f flv "rtmps://<ingestEndpoint>:443/app/<streamKey.value>"` — nothing ingests it. Attempt the playback: `curl "<playbackUrl>"` — the host does not resolve.
5. Re-run: the labels, zone and ids change and the divergence persists — the per-run values are not the finding.

Cause 2 (GetChannel):
1. Create a channel as above, then POST /GetChannel {"arn": "<channel arn>"} (fixture ivs-channel-lifecycle#task#2; also task#5 and ivs-viewer-session-revocation#task#5).
2. GET the returned `$.channel.playbackUrl` from a client of the emulator. The URL's text matches the recording exactly, but its host is not one of the emulator's own hosts, so the fetch fails where AWS serves the HLS master playlist.

Cause 3 (UpdateChannel):
1. POST /UpdateChannel {"arn": "<channel arn>", ...} (fixture ivs-channel-lifecycle#task#4) and read `$.channel.playbackUrl` from the 200 response.
2. Observe the same undialable playback host after the update.

Cause 4 (BatchGetChannel, multi-element):
1. Create two channels, then POST /BatchGetChannel {"arns": ["<arn1>", "<arn2>"]} (fixture ivs-batch-get-channel-and-stream-key#task#3).
2. Observe `$.channels[0].playbackUrl` and `$.channels[1].playbackUrl` both carry a host the emulator does not serve.

Cause 5 (BatchGetChannel, single element):
1. POST /BatchGetChannel {"arns": ["<arn1>"]} (fixture ivs-batch-get-channel-and-stream-key#task#4) and observe the same on `$.channels[0].playbackUrl`.

After a fix, re-replay ALL FIVE: ivs-channel-lifecycle#task#1, ivs-channel-lifecycle#task#2, ivs-channel-lifecycle#task#4, ivs-batch-get-channel-and-stream-key#task#3 and ivs-batch-get-channel-and-stream-key#task#4.
