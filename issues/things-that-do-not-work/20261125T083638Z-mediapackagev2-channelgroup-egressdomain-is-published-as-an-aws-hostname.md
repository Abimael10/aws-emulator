Issue-Source: trace-sweep-divergence
Issue-Trace: mediapackagev2-channel-group-lifecycle
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mediapackagev2-channel-group-lifecycle mediapackagev2-channel-policy-lifecycle mediapackagev2-error-paths mediapackagev2-multiple-origin-endpoints-isolation mediapackagev2-tagging-workflow
Issue-Shape-Id: com.amazonaws.mediapackagev2#mediapackagev2
Issue-Key: mediapackagev2|CreateChannelGroup|2xx->2xx|$.EgressDomain
Issue-Key: mediapackagev2|GetChannelGroup|2xx->2xx|$.EgressDomain
Issue-Key: mediapackagev2|UpdateChannelGroup|2xx->2xx|$.EgressDomain
Issue-Commit: 521cfa5511fb97f152150278e0c58394b5b16795

# ChannelGroup EgressDomain is published as an AWS hostname the emulator does not serve

> _Note: this issue was identified against the code base at commit `521cfa5511fb97f152150278e0c58394b5b16795`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChannelGroup

## Description
Every channel-group response member that announces the group's egress DNS name carries a hostname in AWS's own zone instead of the address the emulator is reached on, so the name the client is handed does not resolve to the emulator that published it. The replay verdict for all three causes is `host_not_own` on `$.EgressDomain` with status 200 on both sides: the body is shaped correctly, the announced host is simply not the emulator's. One root cause, three operations that each serialize it.

Cause 1 of 3 - CreateChannelGroup (fixture mediapackagev2-channel-group-lifecycle, step task#1; same cluster also covers mediapackagev2-channel-policy-lifecycle#setup#1, mediapackagev2-error-paths#task#5, mediapackagev2-multiple-origin-endpoints-isolation#setup#1, mediapackagev2-tagging-workflow#task#1).
  AWS: Smithy mediapackagev2 CreateChannelGroup -> CreateChannelGroupResponse.EgressDomain, target String, @required, no pattern or length constraint; HTTP POST /channelGroup, 200. The model does not spell out the hostname's form, so the DNS name itself is recorded behavior only: the recording expects the group's egress domain to be the host the serving endpoint is reachable at (the fixture's expected value is the placeholder `{{group_egress_domain}}`, bound to the emulator's own announced data-plane host), because the published name is what a CDN or player is pointed at next.
  Emulator: mints a name in AWS's published zone, of the form `<8-hex-group-label>.egress.<12-hex-account-label>.mediapackagev2.<region>.amazonaws.com` - a host the emulator neither owns nor serves. Replay detail: `$.EgressDomain: host_not_own`.
  Why: a client that takes EgressDomain and resolves it - as a CDN origin domain, a player base host, or any follow-on fetch - leaves the emulator entirely and is sent at real AWS DNS (or at nothing, in an offline environment). The group it just created becomes unreachable through the value the API handed back, so every egress-side workflow built on a created group breaks even though the create itself reported success.
  Fix: the channel-group create path's derivation of the stored egress domain - the state module's egress-domain minting, which hardcodes AWS's zone rather than deriving the name from the emulator's own announced data-plane host (the same source the group's data-plane authority already uses for ingest and manifest URLs). EgressDomain must be a bare domain name (no scheme, port or path), so the fix is to take the emulator's announced host name, not its authority with a port.
  Traces: mediapackagev2-channel-group-lifecycle#task#1 (representative); see services/service-com-amazonaws-mediapackagev2-mediapackagev2/src/state.rs lines 2442-2469 (egress_domain(), AWS_DNS_ZONE) and the ChannelGroup.egress_domain / data_plane_authority fields at lines 947-960.

Cause 2 of 3 - GetChannelGroup (fixture mediapackagev2-channel-group-lifecycle, step task#2; cluster also covers task#4).
  AWS: Smithy GetChannelGroup -> GetChannelGroupResponse.EgressDomain, target String, @required; HTTP GET /channelGroup/{ChannelGroupName}, 200. A read reports the group's stored egress domain unchanged, so it must report a host that resolves to the service serving the group - recorded behavior only for the name's form.
  Emulator: replays the stored AWS-zone name back on every read, so the wrong host is persistent rather than a one-off of the create. Replay detail: `$.EgressDomain: host_not_own`, status 200 -> 200.
  Why: a client that re-reads the group to discover where to send egress traffic gets the same unreachable name, so re-reading is not a workaround for cause 1.
  Fix: follows from cause 1 - once the stored value is derived from the emulator's own host, the read serializes it correctly; no separate change is wanted in the GetChannelGroup handler, which must keep echoing the stored value byte for byte.
  Traces: mediapackagev2-channel-group-lifecycle#task#2, #task#4.

Cause 3 of 3 - UpdateChannelGroup (fixture mediapackagev2-channel-group-lifecycle, step task#3).
  AWS: Smithy UpdateChannelGroup -> UpdateChannelGroupResponse.EgressDomain, target String, @required; HTTP PUT /channelGroup/{ChannelGroupName}, 200. An update does not re-mint the egress domain; it reports the group's existing one.
  Emulator: the update response carries the same AWS-zone hostname. Replay detail: `$.EgressDomain: host_not_own`, status 200 -> 200.
  Why: the update path is where a client commonly re-reads derived values after a description change, and it too hands back a name that does not resolve to the emulator.
  Fix: same single site as cause 1 (the stored egress domain), plus the group's ETag input - the tag is computed over the egress domain, so correcting the domain changes the tag consistently for create, read and update.
  Traces: mediapackagev2-channel-group-lifecycle#task#3.

Not covered by any open issue: the open mediapackagev2 issues are the TagResource/UntagResource Content-Length one (20261109T115329Z-..., which owns the two 204 content-length divergences in this same replay and is deliberately not re-filed here), an UpdateChannelGroup commit-rule maintainability issue, and a CloudFormation resource-type registration issue. None of them concerns the published egress hostname.

## Evidence
Replay of trace set simparity-wire-aws-v1 (96 steps, 86 passed, 10 mismatched):

1) fixture mediapackagev2-channel-group-lifecycle step task#1, CreateChannelGroup, expected status 200 / actual 200, detail: `$.EgressDomain: host_not_own (expected '{{group_egress_domain}}', got '6c7a8c72.egress.4a962433403d.mediapackagev2.us-west-2.amazonaws.com')`, affected_count 5 (mediapackagev2-channel-group-lifecycle#task#1, mediapackagev2-channel-policy-lifecycle#setup#1, mediapackagev2-error-paths#task#5, mediapackagev2-multiple-origin-endpoints-isolation#setup#1, mediapackagev2-tagging-workflow#task#1).

2) fixture mediapackagev2-channel-group-lifecycle step task#2, GetChannelGroup, expected 200 / actual 200, detail: `$.EgressDomain: host_not_own`, affected_count 2 (task#2, task#4).

3) fixture mediapackagev2-channel-group-lifecycle step task#3, UpdateChannelGroup, expected 200 / actual 200, detail: `$.EgressDomain: host_not_own`, affected_count 1.

Expected: EgressDomain is a bare DNS name (no scheme, no port, no path) naming a host the serving emulator answers on, identical across the create, the read and the update of one group.
Actual: a name in AWS's published zone of the form `<group-label>.egress.<account-label>.mediapackagev2.<region>.amazonaws.com`, which the emulator does not serve. The hex labels, account label and region in the quoted value are per-environment values, not part of the requirement; the requirement is that the announced host be the emulator's own.
Source: services/service-com-amazonaws-mediapackagev2-mediapackagev2/src/state.rs:2460-2469 formats the domain against `AWS_DNS_ZONE = "amazonaws.com"` regardless of the address the request arrived on, and the create persists it (ChannelGroup::egress_domain) so reads and updates reproduce it.

## Steps to Reproduce
1. Boot the emulator from this checkout and note the host:port the mediapackagev2 data listener is reached on.
2. Replay fixture mediapackagev2-channel-group-lifecycle: step task#1 POSTs /channelGroup to create a channel group; read `EgressDomain` in the 200 response. It is `<hex>.egress.<hex>.mediapackagev2.<region>.amazonaws.com` rather than the emulator's own announced host, and resolving it does not reach the emulator.
3. Step task#2 (GET /channelGroup/{ChannelGroupName}) and step task#4 return the same non-owned host; step task#3 (PUT /channelGroup/{ChannelGroupName}) does too.
4. The same create-side divergence reproduces in mediapackagev2-channel-policy-lifecycle#setup#1, mediapackagev2-error-paths#task#5, mediapackagev2-multiple-origin-endpoints-isolation#setup#1 and mediapackagev2-tagging-workflow#task#1.
