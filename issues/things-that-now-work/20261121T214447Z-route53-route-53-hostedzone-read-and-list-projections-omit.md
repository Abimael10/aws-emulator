Issue-Source: trace-sweep-divergence
Issue-Trace: route53-create-private-zone-preserves-vpc-association
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-create-private-zone-preserves-vpc-association route53-hosted-zone-comment-tags-and-not-found route53-list-hosted-zones-membership-and-by-vpc
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|GetHostedZone|2xx->2xx|$.GetHostedZoneResponse.HostedZone.Features
Issue-Key: route53|UpdateHostedZoneComment|2xx->2xx|$.UpdateHostedZoneCommentResponse.HostedZone.Features
Issue-Key: route53|ListHostedZones|2xx->2xx|$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhza-274b8e0f.com."].Features
Issue-Key: route53|ListHostedZones|2xx->2xx|$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhzb-23dc87fb.com."].Features
Issue-Related: 20261118T054050Z-route53-route-53-hostedzone-projection-omits-config-hostedzonetier-and.md
Issue-Commit: 6ee685494fcb976e5e853eb07eb6238c0bcf2fe0

# Route 53 HostedZone read and list projections omit Features and Config.HostedZoneTier

> _Note: this issue was identified against the code base at commit `6ee685494fcb976e5e853eb07eb6238c0bcf2fe0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetHostedZone

## Description
FOUR net-new causes, one shared fault: the emulator's HostedZone element is a hand-written projection that carries no Features element at all (and no Config.HostedZoneTier), on every READ/LIST path that returns a HostedZone - GetHostedZone, UpdateHostedZoneComment and ListHostedZones. AWS always serializes a Features element for a zone; what it contains depends on the zone (an empty element for a private zone, feature statuses for a public one), but it is never absent. The emulator omits the element entirely, so a client cannot distinguish "no features configured" from "this response has no feature block".

Cause 1 of 4: GetHostedZone on a PRIVATE hosted zone omits HostedZone.Features entirely (fixture route53-create-private-zone-preserves-vpc-association, step task#4).
AWS: GET /2013-04-01/hostedzone/{Id} answers 200 with the zone's HostedZone element, and for a private hosted zone the recorded body carries HostedZone.Features as an EMPTY element - the element is present with no member inside it (no PublicDNSFallbackStatus, no AcceleratedRecoveryStatus). Smithy: GetHostedZone output member HostedZone is @required and targets com.amazonaws.route53#HostedZone; HostedZone.Features targets HostedZoneFeatures{AcceleratedRecoveryStatus, FailureReasons, PublicDNSFallbackStatus}, all optional, documented as "The features configuration for the hosted zone". The model does not state that the element is always serialized, so the "always present, possibly empty" shape is recorded behavior only.
Emulator: answers 200 with HostedZone.Features absent from the body (read back as None).
Why: a client that reads zone.Features to inspect feature posture gets a null object rather than an empty structure, so feature-aware code takes a null branch (or throws on dereference) where against AWS it would walk an empty-but-present structure; strict comparison against recorded AWS traffic fails on a missing element.
Fix: the shared HostedZone serializer used by the GetHostedZone handler - always emit the Features element, with the members the zone's stored state actually has (none for a private zone).

Cause 2 of 4: GetHostedZone on a PUBLIC hosted zone omits Config.HostedZoneTier and the whole Features block (fixture route53-hosted-zone-comment-tags-and-not-found, step task#3; the same cluster also covers step task#13 of that fixture).
AWS: GET /2013-04-01/hostedzone/{Id} answers 200 with Config.HostedZoneTier set (STANDARD for an ordinary zone) and Features carrying AcceleratedRecoveryStatus (DISABLED on a zone that has not enabled accelerated recovery) plus a ChangeGuardrails element holding one ChangeGuardrail entry per guardrail Route 53 defines, each with Name and Status (the recording shows NS_OCCLUSION_CHECK, NS_DELEGATION_AUTHORITY_CHECK, NS_FORMAT_CHECK, ALIAS_TARGET_CHECK, ACTIVE_RECORD_CHECK, ACTIVE_DELEGATION_CHECK, each Status DISABLED on a fresh zone). Smithy: HostedZone.Config targets HostedZoneConfig{Comment, PrivateZone} and HostedZone.Features targets HostedZoneFeatures{AcceleratedRecoveryStatus, FailureReasons, PublicDNSFallbackStatus} - the on-disk model has NO HostedZoneTier member and NO ChangeGuardrails member, so both are recorded behavior only: the recording is the authority that AWS serializes HostedZoneTier inside Config and ChangeGuardrails inside Features.
Emulator: answers 200 with Config.HostedZoneTier absent and Features absent (both None).
Why: the zone's tier and its guardrail/accelerated-recovery posture are unreadable for the whole lifetime of the zone, so tier-aware and guardrail-aware logic silently takes its not-configured branch.
Fix: the shared HostedZone projection behind the GetHostedZone handler, and the zone state record it projects from (which must carry a tier and a per-guardrail status set from creation).

Cause 3 of 4: UpdateHostedZoneComment echoes the same truncated HostedZone (fixture route53-hosted-zone-comment-tags-and-not-found, step task#12).
AWS: POST /2013-04-01/hostedzone/{Id} with a new Comment answers 200 echoing the updated zone, and a comment update changes only Config.Comment: Config.HostedZoneTier (STANDARD) and the Features block (AcceleratedRecoveryStatus DISABLED plus the per-guardrail ChangeGuardrails set) are returned unchanged. Smithy: UpdateHostedZoneComment output member HostedZone is @required and targets the same HostedZone shape; HostedZoneTier/ChangeGuardrails are recorded behavior only, as in Cause 2.
Emulator: answers 200 with Config.HostedZoneTier and Features both absent.
Why: a client that keeps the returned HostedZone as its cached view of the zone after a comment update loses the tier and the feature block, so any later decision made from that cached object is made on nulls.
Fix: the same shared HostedZone projection, reached from the UpdateHostedZoneComment handler - not a per-operation patch.

Cause 4 of 4: ListHostedZones omits HostedZone.Features on every listed zone (fixture route53-list-hosted-zones-membership-and-by-vpc, step task#6).
AWS: GET /2013-04-01/hostedzone answers 200 with HostedZones.HostedZone[*], and each listed zone carries a Features element; in this recording each listed public zone's Features carries PublicDNSFallbackStatus with status DISABLED (the zones in the cluster are two distinct listed zones, so this is the per-row projection, not one zone's quirk). Smithy: ListHostedZones output member HostedZones is @required and targets a list of the same HostedZone shape, whose Features targets HostedZoneFeatures{..., PublicDNSFallbackStatus: String, "The status of public DNS fallback for the hosted zone"}.
Emulator: answers 200 with Features absent on every listed zone row.
Why: a client enumerating zones to audit feature posture sees nothing for every zone, so an audit over the list reports "unknown" (or crashes on a null) for a fleet AWS reports as explicitly DISABLED, and the list projection is inconsistent with what a per-zone Get would have to return.
Fix: the ListHostedZones handler's per-row HostedZone serializer - it must use the same zone-state-derived projection as the single-zone paths.

Related open issues, none of which covers these four: 20261118T054050Z-route53-route-53-hostedzone-projection-omits-config-hostedzonetier-and.md files the CreateHostedZone projection and an earlier form of the private-zone Features divergence (at that time the emulator EMITTED Features with PublicDNSFallbackStatus where AWS sent an empty element; it now omits the element outright, and the Get/Update/List paths replay as unowned); 20261117T053039Z-route53-route-53-listhostedzones-emits-hostedzone-features-as-an.md and 20261115T171142Z / 20261112T225243Z describe ListHostedZones emitting Features as an EMPTY element, not omitting it. All four causes below must be closed on their own fixtures.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (score 95.4%, 17 mismatched steps). Four mismatch clusters, status matched in every one - only the body diverged:

1) GetHostedZone, fixture route53-create-private-zone-preserves-vpc-association#task#4, expected 200 / actual 200:
   $.GetHostedZoneResponse.HostedZone.Features: missing (expected '', got None)

2) GetHostedZone, fixture route53-hosted-zone-comment-tags-and-not-found#task#3 (cluster also covers #task#13), expected 200 / actual 200:
   $.GetHostedZoneResponse.HostedZone.Config.HostedZoneTier: missing (expected 'STANDARD', got None)
   $.GetHostedZoneResponse.HostedZone.Features: missing (expected {'AcceleratedRecoveryStatus': 'DISABLED', 'ChangeGuardrails': {'ChangeGuardrail': [{'Name': 'NS_OCCLUSION_CHECK', 'Status': 'DISABLED'}, {'Name': 'NS_DELEGATION_AUTHORITY_CHECK', 'Status': 'DISABLED'}, {'Name': 'NS_FORMAT_CHECK', 'Status': 'DISABLED'}, {'Name': 'ALIAS_TARGET_CHECK', 'Status': 'DISABLED'}, {'Name': 'ACTIVE_RECORD_CHECK', 'Status': 'DISABLED'}, {'Name': 'ACTIVE_DELEGATION_CHECK', 'Status': 'DISABLED'}]}}, got None)

3) UpdateHostedZoneComment, fixture route53-hosted-zone-comment-tags-and-not-found#task#12, expected 200 / actual 200:
   $.UpdateHostedZoneCommentResponse.HostedZone.Config.HostedZoneTier: missing (expected 'STANDARD', got None)
   $.UpdateHostedZoneCommentResponse.HostedZone.Features: missing (expected the same AcceleratedRecoveryStatus + six-entry ChangeGuardrails structure, got None)

4) ListHostedZones, fixture route53-list-hosted-zones-membership-and-by-vpc#task#6, expected 200 / actual 200:
   $.ListHostedZonesResponse.HostedZones.HostedZone[Name="<zone-a>"].Features: missing (expected {'PublicDNSFallbackStatus': 'DISABLED'}, got None)
   $.ListHostedZonesResponse.HostedZones.HostedZone[Name="<zone-b>"].Features: missing (expected {'PublicDNSFallbackStatus': 'DISABLED'}, got None)
   (the two row keys are the generated zone names of this run; the requirement is per-row, not those names)

Model evidence: smithy_query on services/service-com-amazonaws-route53-awsdnsv20130401/smithy-model/com-amazonaws-route53-awsdnsv20130401.json - HostedZone{Id, Name, CallerReference, Config, ResourceRecordSetCount, LinkedService, Features}; HostedZoneFeatures{AcceleratedRecoveryStatus, FailureReasons, PublicDNSFallbackStatus}; HostedZoneConfig{Comment, PrivateZone}. Shapes/members 'HostedZoneTier' and 'ChangeGuardrails' do not exist in the model - recorded behavior only. GetHostedZone/UpdateHostedZoneComment output member HostedZone is @required; ListHostedZones output member HostedZones is @required.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture route53-create-private-zone-preserves-vpc-association step task#4 - GET /2013-04-01/hostedzone/{Id} for the private zone created earlier in the fixture - and look for the HostedZone.Features element in the 200 body (AWS: present and empty; emulator: absent).
3. Cause 2: replay fixture route53-hosted-zone-comment-tags-and-not-found step task#3 (and task#13) - GET /2013-04-01/hostedzone/{Id} for the public zone - and read HostedZone.Config.HostedZoneTier and HostedZone.Features.
4. Cause 3: replay the same fixture's step task#12 - POST /2013-04-01/hostedzone/{Id} with a new Comment - and read the same two members in the 200 body.
5. Cause 4: replay fixture route53-list-hosted-zones-membership-and-by-vpc step task#6 - GET /2013-04-01/hostedzone - and read Features on each HostedZone row.
6. All four steps must match the recorded AWS bodies before this issue is closed.
