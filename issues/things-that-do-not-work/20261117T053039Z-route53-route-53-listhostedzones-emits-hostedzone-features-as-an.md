Issue-Source: trace-sweep-divergence
Issue-Trace: route53-list-hosted-zones-membership-and-by-vpc
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-list-hosted-zones-membership-and-by-vpc
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|ListHostedZones|2xx->2xx|$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhza-2241b501.com."].Features
Issue-Key: route53|ListHostedZones|2xx->2xx|$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhzb-0e34f49c.com."].Features
Issue-Commit: 143a08759a03c33e811cd9d512509dbf0e312af3

# Route 53 ListHostedZones emits HostedZone.Features as an empty element instead of a feature-status structure

> _Note: this issue was identified against the code base at commit `143a08759a03c33e811cd9d512509dbf0e312af3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListHostedZones

## Description
Cause 1 of 1: the shared hosted-zone projection used by ListHostedZones opens the optional HostedZone.Features container unconditionally and writes nothing into it, so a structure member is serialized as an empty XML element (which deserializes to the empty string) for every zone in the listing. Fixture route53-list-hosted-zones-membership-and-by-vpc, step task#6, ListHostedZones, 200 vs 200 — the divergence is entirely in the response body, so no status or error code signals it.

AWS: Smithy (services/service-com-amazonaws-route53-awsdnsv20130401, com.amazonaws.route53#HostedZone) declares member `Features` targeting the structure `com.amazonaws.route53#HostedZoneFeatures`, optional (only Id, Name, CallerReference are required), documented "The features configuration for the hosted zone, including accelerated recovery settings and status information"; HostedZoneFeatures is documented "Represents the features configuration for a hosted zone, including the status of various features and any associated failure reasons" and declares AcceleratedRecoveryStatus (enum) and FailureReasons (list). ListHostedZones is GET /2013-04-01/hostedzone returning ListHostedZonesResponse.HostedZones (required) of HostedZone, restXml, 200. The specific wire member the recording carries inside Features is recorded behavior only: the recording shows AWS serializing, for every listed hosted zone, a Features element containing a feature-status child element whose value is the per-zone status of that feature — in the recording a public-DNS-fallback status reported as DISABLED on a zone where the feature has not been enabled. The requirement is structural and per-zone: Features, when serialized, is a STRUCTURE whose child elements are feature statuses read from that zone's own feature state; it is never an empty container and never a fixed literal. The particular zone names and the single recorded status value are per-run/per-zone values, not requirements; the requirement is that each listed zone's Features deserializes as a structure carrying that zone's feature statuses (and, where no feature state exists to report, that the optional member be omitted entirely rather than emitted empty).

Emulator: ListHostedZones serializes `<Features></Features>` for every zone in the listing. Replay detail for both zones of the step: `$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhza-2241b501.com."].Features: type_mismatch (expected {'PublicDNSFallbackStatus': 'DISABLED'}, got '')` and the same for the lhzb zone. A structure is expected and a text node is produced — a type mismatch, not a merely-absent child — and it is uniform across zones, i.e. no feature state is carried in hosted-zone state at all.

Why: a client deserializing HostedZone.Features against a strict restXml/structure deserializer either fails to parse the listing or obtains a HostedZoneFeatures object whose status members are all null. Code written against AWS reads a zone's feature posture straight from the listing — to assert the DISABLED baseline before toggling a feature, to decide whether a feature must be enabled, or to render per-zone posture for an account — and against the emulator gets "unknown" for every hosted zone or dereferences a null status. Because the member is present but empty, the presence test that AWS gives as the "feature state was reported" signal is also always true, so a caller cannot fall back to treating it as unreported. Golden-response and strict shape comparators fail on the type change as well.

Fix: the ListHostedZones response serializer's HostedZone projection in the route53 service crate (the shared hosted-zone projection, since the other hosted-zone read paths serialize the same structure), plus the hosted-zone state it reads: carry per-zone feature statuses in zone state and emit them as child elements of Features so a zone whose feature is not enabled serializes that feature's status as DISABLED; when a zone has no feature state to report, omit the optional Features member instead of opening an empty container.

Related but NOT covering this: the replay reports NO owner for this step's replay key, so it is filed net-new. Open issues 20261110T235626Z-route53-route-53-listhostedzones-projects-hostedzone-features-as-an.md, 20261112T225243Z-route53-route-53-listcidrcollections-rows-and-listhostedzones-features-cannot.md, 20261115T171142Z-route53-route-53-list-projections-unmatchable-cidr-collection-rows.md and 20261116T202850Z-route53-route-53-list-projections-diverge-listgeolocations-subdivision-catalog.md describe the same ListHostedZones Features symptom but key their replay pointers on earlier runs' generated zone names, so none of them carries this step's key. Open issue 20261108T200652Z-route53-route-53-gethostedzone-serializes-an-empty-hostedzone-features.md covers GetHostedZone, where AWS omits Features entirely — closing that one by globally suppressing an empty Features would NOT fix this listing divergence, because here AWS does send Features and the emulator's container is wrong for being empty, not for being present.

Traces: route53-list-hosted-zones-membership-and-by-vpc#task#6 (ListHostedZones, 200 vs 200, two HostedZone[].Features entries type_mismatch).

## Evidence
Replay divergence (verdict mismatch, cluster size 1, owned_by: none):
  fixture_id: route53-list-hosted-zones-membership-and-by-vpc
  step_key: task#6
  service: route53, operation: ListHostedZones
  expected_status 200, actual_status 200
  detail:
    $.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhza-2241b501.com."].Features: type_mismatch (expected {'PublicDNSFallbackStatus': 'DISABLED'}, got '')
    $.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhzb-0e34f49c.com."].Features: type_mismatch (expected {'PublicDNSFallbackStatus': 'DISABLED'}, got '')

Expected (AWS, recorded): each HostedZone in ListHostedZonesResponse carries a Features element containing a feature-status child reporting the zone's own status for that feature (recorded: the public-DNS-fallback status DISABLED on a zone where the feature is not enabled). The zone names above are per-run generated values and are not part of the requirement.
Actual (emulator): each HostedZone carries Features as an empty element — a text node where a structure is expected, identically for every zone.

Model evidence: Smithy com.amazonaws.route53#HostedZone member Features -> com.amazonaws.route53#HostedZoneFeatures, optional (required: Id, Name, CallerReference), doc "The features configuration for the hosted zone, including accelerated recovery settings and status information"; HostedZoneFeatures declares AcceleratedRecoveryStatus (enum) and FailureReasons (list) and has no public-DNS-fallback member, so the recorded wire member is model drift and the recording is the authority for it. ListHostedZones: GET /2013-04-01/hostedzone, output HostedZones (required) of HostedZone, restXml, 200.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire fixture `route53-list-hosted-zones-membership-and-by-vpc` and stop at step `task#6`.
   Equivalent by hand: run the fixture's setup to create two public hosted zones in the account, then call route53 ListHostedZones.
3. Inspect the restXml body of ListHostedZonesResponse and the `<Features>` element of every `<HostedZone>`.
4. Observed: `<Features></Features>` on every zone — an empty container, deserializing to '' where a HostedZoneFeatures structure is expected.
   Expected: `<Features>` containing the zone's feature statuses (the not-enabled feature reported as DISABLED), or no Features element at all when the zone has no feature state to report. Status is 200 either way.
5. Any two hosted zones in the account reproduce it; the behavior is not zone-specific.
