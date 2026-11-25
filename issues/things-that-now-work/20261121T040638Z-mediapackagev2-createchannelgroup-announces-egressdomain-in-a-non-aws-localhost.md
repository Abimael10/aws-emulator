Issue-Source: trace-sweep-divergence
Issue-Trace: mediapackagev2-error-paths
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mediapackagev2-error-paths mediapackagev2-tagging-workflow
Issue-Shape-Id: com.amazonaws.mediapackagev2#mediapackagev2
Issue-Key: mediapackagev2|CreateChannelGroup|2xx->2xx|$.EgressDomain
Issue-Commit: 8d86b70a1e1105bf86bfbd322bfad436233da8ec

# CreateChannelGroup announces EgressDomain in a non-AWS .localhost DNS zone

> _Note: this issue was identified against the code base at commit `8d86b70a1e1105bf86bfbd322bfad436233da8ec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChannelGroup

## Description
Cause 1 of 1: CreateChannelGroup returns an EgressDomain whose DNS zone is the RFC 6761 loopback zone (`.localhost`) instead of this service's regional AWS zone, so the required member carries a name that is neither AWS's published egress domain nor an address the emulator is reachable on.

AWS: Smithy: CreateChannelGroup -> CreateChannelGroupResponse$EgressDomain, target String, @required; doc "The output domain where the source stream should be sent. Integrate the egress domain with a downstream CDN (such as Amazon CloudFront) or playback device." HTTP POST /channelGroup, success 200. The member is a DOMAIN NAME only (no scheme, no port, no path). Recorded behavior shows the name AWS mints for a channel group: two opaque labels, then the plane label, the signing name, the Region and the service's regional zone — `<group-label>.egress.<account-label>.mediapackagev2.<region>.amazonaws.com` (the recorded example is `cq8go8.egress.z4xeig.mediapackagev2.us-west-2.amazonaws.com`; `cq8go8` and `z4xeig` are per-account/per-group opaque labels and `us-west-2` comes from the request's credential scope — only the trailing zone `mediapackagev2.<region>.amazonaws.com` and the `.egress.` plane label are the fixed text, the two leading labels are values). The same domain is what GetChannelGroup and ListChannelGroups must read back, because AWS guarantees all channels and origin endpoints in a group share this DNS name.

Emulator: the step is answered 200 with every other member matching, but `$.EgressDomain` comes back as `096530b1.egress.4a962433403d.mediapackagev2.us-west-2.localhost` — correct label structure and Region, wrong zone: `localhost` where AWS publishes `amazonaws.com`. The replay classifies this `host_not_own`: the announced name is not a host the emulator answers on either (the data listener is reached on its own address/port, which `announced_data_plane_domain` resolves separately into the group's data-plane authority and which this member, being portless, cannot carry). So the one member a client is told to dial or hand to a CDN names nothing that resolves to the service.

Why: a client that does what the member documents — resolve EgressDomain, or configure it as a CDN origin domain name / playback host — gets a name in the loopback zone that does not reach the emulator's data plane, so egress playback against the announced domain fails outright. Code that parses the domain to recover the Region or to recognise the host as a MediaPackageV2 egress host (anything matching on the `mediapackagev2.<region>.amazonaws.com` suffix) also stops recognising it, and the value is persisted at create time, so every later GetChannelGroup/ListChannelGroups read reproduces the wrong zone.

Fix: the channel group's egress-domain announcement in this service's state — `announced_egress_domain` in services/service-com-amazonaws-mediapackagev2-mediapackagev2/src/state/data_plane_addressing.rs, which falls through to `egress_domain_in_zone(region, group, announced_zone_of_authority(authority))` and thereby substitutes the loopback zone whenever the group was created through a request that reached the emulator's own address. The required member must be minted in the service's AWS zone (`egress_domain(region, group)`, i.e. `AWS_DNS_ZONE` in services/service-com-amazonaws-mediapackagev2-mediapackagev2/src/state.rs) for every create, independently of which address the creating request arrived on; the reachable `host[:port]` stays where it already is, on the group's `data_plane_authority` used to root ingest and manifest URLs. The host readers that claim these names (`data_plane_of_host` / `addressed_data_plane`) must keep accepting the announced name so the emulator still serves the plane it announces.

Traces: mediapackagev2-error-paths#task#5 (representative) and mediapackagev2-tagging-workflow#task#1, both CreateChannelGroup, same divergence key `service=mediapackagev2|CreateChannelGroup|status=200->200|keys=$.EgressDomain`.

## Evidence
Fixture mediapackagev2-error-paths, step task#5, operation CreateChannelGroup.
Expected status 200, actual status 200 — body member diverges.
Replay detail: `$.EgressDomain: host_not_own (expected 'cq8go8.egress.z4xeig.mediapackagev2.us-west-2.amazonaws.com', got '096530b1.egress.4a962433403d.mediapackagev2.us-west-2.localhost')`.
Expected (AWS): domain ending in the service's regional zone `mediapackagev2.<region>.amazonaws.com`, with `.egress.` as the plane label; the two leading labels are opaque per-account/per-group values and are NOT part of the requirement.
Actual (emulator): identical label structure but zone `localhost`, a name that resolves to neither AWS's egress host nor the address/port the emulator's data listener is reached on.
Cluster members: mediapackagev2-error-paths#task#5, mediapackagev2-tagging-workflow#task#1 (affected_count 2).
Source of the substitution: `announced_egress_domain` -> `announced_zone_of_authority(authority)` in src/state/data_plane_addressing.rs (lines 93-105).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Replay fixture `mediapackagev2-error-paths`, step `task#5`: POST /channelGroup (CreateChannelGroup) with a ChannelGroupName matching ^[a-zA-Z0-9_-]+$, signed for us-west-2, sent to the emulator's local endpoint.
3. Read `EgressDomain` from the 200 response body: it ends in `.mediapackagev2.us-west-2.localhost` instead of `.mediapackagev2.us-west-2.amazonaws.com`.
4. Same divergence via fixture `mediapackagev2-tagging-workflow`, step `task#1`.
5. Follow up with GetChannelGroup on the same group to confirm the wrong zone was persisted at create time and is read back unchanged.
