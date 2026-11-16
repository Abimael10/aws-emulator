Issue-Source: trace-sweep-divergence
Issue-Trace: mediapackagev2-multiple-origin-endpoints-isolation
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mediapackagev2-multiple-origin-endpoints-isolation
Issue-Shape-Id: com.amazonaws.mediapackagev2#mediapackagev2
Issue-Key: mediapackagev2|CreateChannelGroup|2xx->2xx|$.EgressDomain
Issue-Commit: e6b8bf484126909ac54e08c66383f30f771b4a2d

# CreateChannelGroup EgressDomain is serialized as a host:port authority instead of a DNS domain name

> _Note: this issue was identified against the code base at commit `e6b8bf484126909ac54e08c66383f30f771b4a2d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChannelGroup

## Description
Wire replay of simparity-wire-aws-v1 (96 steps, 93 passed, 3 mismatched, score 96.9%, emulator_up=true, bootstrap_signal=false) leaves ONE unowned mediapackagev2 divergence. The other two mismatched steps (TagResource#task#3, UntagResource#task#5) are OWNED by 20261109T115329Z-mediapackagev2-tagresource-and-untagresource-204-responses-omit-the-content.md and are skipped here. This is a ONE-cause issue.

Related open issues, named explicitly because neither covers this cluster: 20261109T115329Z-mediapackagev2-origin-endpoint-manifest-urls-advertise-aws-owned-egress.md and 20261110T042138Z-mediapackagev2-data-plane-urls-channel-ingest-and-origin.md describe the inverse fault (data-plane URLs minted on PUBLIC amazonaws.com hosts) on CreateOriginEndpoint/CreateChannel members HlsManifests[].Url and IngestEndpoints[].Url, and carry only those replay keys (mediapackagev2|CreateOriginEndpoint|2xx->2xx|$.HlsManifests[*].Url etc.). Neither carries a CreateChannelGroup key nor the $.EgressDomain member path, so this cluster is not gated by them; the present defect is the opposite over-correction on a different member and survives in the current checkout (those manifest/ingest clusters no longer diverge, this one does).

Cause 1 of 1: CreateChannelGroup's required EgressDomain member is emitted as an IP-literal authority with a port rather than as a domain name.
  AWS: Smithy mediapackagev2 CreateChannelGroup -> POST /channelGroup, code 200; output member EgressDomain (target String, @required) is documented as "The output domain where the source stream should be sent. Integrate the egress domain with a downstream CDN (such as Amazon CloudFront) or playback device." The member is a DOMAIN, not a URL and not an authority: it carries no scheme, no path and no port, and it is consumed as a DNS name (a CloudFront origin domain name, or the host a player resolves). GetChannelGroup, UpdateChannelGroup and ListChannelGroups re-report the same member with the same shape. The model states no pattern on it, so the FORMAT requirement beyond "domain name" is recorded behavior only: the recording's value for this member is a dotted, port-less, scheme-less hostname of the form <label>.mediapackagev2.<region>.amazonaws.com (the recorded value is a scrubbed placeholder, so the generated label, the account label and the region in it are per-environment values and are NOT the requirement). The requirement is the SHAPE — a dotted DNS hostname with no port and no scheme — and that the name be one the emulator answers on.
  Emulator: CreateChannelGroup returns $.EgressDomain as a bare IPv4 loopback literal with a port appended, i.e. the authority form <host>:<port> (observed '127.0.0.1:37713'). Status 200 and every other member of the response matched. The value is not a domain name at all: it is not dotted-DNS-resolvable, and it carries a port component that this member is never allowed to carry. Hypothesis: the field is being filled from the same endpoint-authority derivation that correctly produces the full URLs in HlsManifests[].Url / IngestEndpoints[].Url, which legitimately include a port, instead of from a domain-only derivation; note also that the announced port differs from the data listener the replay was served on (the emulator reported '/__admin/health answered 200 and the :45383 data listener accepts' while announcing :37713), so unless a dedicated per-resource egress listener is bound on the announced port the emulator is also announcing an address other than the one it is reached on.
  Why: a client that does what the member is documented for breaks. Configuring a CloudFront (or any CDN) origin from EgressDomain fails, because an origin domain name must be a DNS name without a port. A client that resolves EgressDomain, or that composes a subdomain/host header from it, or that builds 'https://' + EgressDomain + path and then relies on TLS SNI/certificate matching against a hostname, gets a malformed or unresolvable target — while the control plane reported 200 with a required member present, giving no indication the value is unusable. A client that merely concatenates it into a URL authority happens to work, which makes the defect silent until a CDN integration is attempted.
  Fix: the CreateChannelGroup handler's EgressDomain minting and the channel-group state record that persists it, plus the shared endpoint derivation used by the GetChannelGroup / UpdateChannelGroup / ListChannelGroups response serializers so persisted records stop re-serving the authority form. EgressDomain must be derived as a host-only DNS name for an address the emulator answers on (no port, no scheme, no path); the port, if one is needed to reach the emulator, belongs only in the full-URL members (HlsManifests[].Url, IngestEndpoints[].Url), not in this member.
  Traces: mediapackagev2-multiple-origin-endpoints-isolation#setup#1.

## Evidence
Replay: trace_set_id simparity-wire-aws-v1, emulator_up=true, bootstrap_signal=false, steps_total 96, steps_passed 93, steps_mismatched 3, steps_errored 0, steps_blocked 0, not_implemented 0, score_pct 96.9, divergence_count 3. emulator_detail: "/__admin/health answered 200 and the :45383 data listener accepts".

Cause 1 — fixture mediapackagev2-multiple-origin-endpoints-isolation, step setup#1, operation CreateChannelGroup, service mediapackagev2, verdict mismatch, expected_status 200 / actual_status 200, stub_path empty, owned_by empty, affected_count 1 (affected: mediapackagev2-multiple-origin-endpoints-isolation#setup#1).
  detail: "$.EgressDomain: value_mismatch (expected 'placeholder.mediapackagev2.us-west-2.amazonaws.com', got '127.0.0.1:37713')".
  divergence_key: service=mediapackagev2|CreateChannelGroup|status=200->200|keys=$.EgressDomain.
  The recorded value is a scrubbed placeholder, so the literal label 'placeholder', the account label and the region are NOT the requirement and neither is the emulator's literal host or the literal port 37713 — those are per-environment values. What the recording does establish, and what the emulator violates, is the member's FORMAT: a dotted, scheme-less, PORT-LESS DNS hostname. The emulator's value supplies an IP literal plus ':<port>', i.e. a URL authority, for a member the model documents as a domain to hand to a CDN.
  The HTTP status (200) and all other members of the CreateChannelGroup response (ChannelGroupName, Arn, CreatedAt, ModifiedAt, ETag, Tags) matched; the divergence is confined to the $.EgressDomain member's form.
  The two remaining mismatched steps in this replay (mediapackagev2-tagging-workflow#task#3 TagResource and #task#5 UntagResource, both '$headers.content-length: missing') each reported owned_by 20261109T115329Z-mediapackagev2-tagresource-and-untagresource-204-responses-omit-the-content.md and are therefore not filed here.

## Steps to Reproduce
Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus (fixture mediapackagev2-multiple-origin-endpoints-isolation, step setup#1), or reproduce directly with botocore pointed at the emulator endpoint:

1. mp = boto3.client('mediapackagev2', endpoint_url=<emulator endpoint>)
2. created = mp.create_channel_group(ChannelGroupName=g)
3. Read created['EgressDomain']. Observed: an IP-literal authority of the form '<host>:<port>' (e.g. '127.0.0.1:37713') — it contains a ':' and a port and is not a dotted DNS hostname.
4. Call mp.get_channel_group(ChannelGroupName=g), mp.update_channel_group(ChannelGroupName=g, Description='x') and mp.list_channel_groups() and read EgressDomain from each; confirm the persisted record does not re-serve the authority form on the read/update/list paths either.

Pass criterion: EgressDomain on every path that reports it (CreateChannelGroup, GetChannelGroup, UpdateChannelGroup, ListChannelGroups Items[]) must be a host-only DNS domain name — no scheme, no path, and no ':<port>' component — naming an address the emulator answers on, such that it can be used directly as a CDN origin domain name. The generated label, account label, region and the concrete host the emulator derives may differ freely between environments; the absence of a port and the dotted-hostname form are the requirement, and the value must stay consistent between the create response and the subsequent read/update/list responses for the same channel group.
