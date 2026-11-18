Issue-Source: trace-sweep-divergence
Issue-Trace: route53-hosted-zone-comment-tags-and-not-found
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-hosted-zone-comment-tags-and-not-found route53-create-private-zone-preserves-vpc-association
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|CreateHostedZone|2xx->2xx|$.CreateHostedZoneResponse.HostedZone.Config.HostedZoneTier
Issue-Key: route53|CreateHostedZone|2xx->2xx|$.CreateHostedZoneResponse.HostedZone.Features.AcceleratedRecoveryStatus
Issue-Key: route53|CreateHostedZone|2xx->2xx|$.CreateHostedZoneResponse.HostedZone.Features.ChangeGuardrails
Issue-Key: route53|CreateHostedZone|2xx->2xx|$.CreateHostedZoneResponse.HostedZone.Features
Issue-Key: route53|GetHostedZone|2xx->2xx|$.GetHostedZoneResponse.HostedZone.Config.HostedZoneTier
Issue-Key: route53|GetHostedZone|2xx->2xx|$.GetHostedZoneResponse.HostedZone.Features.AcceleratedRecoveryStatus
Issue-Key: route53|GetHostedZone|2xx->2xx|$.GetHostedZoneResponse.HostedZone.Features.ChangeGuardrails
Issue-Key: route53|UpdateHostedZoneComment|2xx->2xx|$.UpdateHostedZoneCommentResponse.HostedZone.Config.HostedZoneTier
Issue-Key: route53|UpdateHostedZoneComment|2xx->2xx|$.UpdateHostedZoneCommentResponse.HostedZone.Features.AcceleratedRecoveryStatus
Issue-Key: route53|UpdateHostedZoneComment|2xx->2xx|$.UpdateHostedZoneCommentResponse.HostedZone.Features.ChangeGuardrails
Issue-Commit: e731816d9d754023c43f20a6999b56eec2c1047e

# Route 53 HostedZone projection omits Config.HostedZoneTier and feature statuses

> _Note: this issue was identified against the code base at commit `e731816d9d754023c43f20a6999b56eec2c1047e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateHostedZone

## Description
FOUR net-new causes, all one shared fault: the emulator's HostedZone element is a fixed, hand-written projection rather than one derived from the zone's stored configuration. It emits a feature member AWS does not emit for a private zone, and omits the tier and feature members AWS does emit for a public zone, on every operation that returns a HostedZone (CreateHostedZone, GetHostedZone, UpdateHostedZoneComment).

Cause 1 of 4: CreateHostedZone omits HostedZone.Config.HostedZoneTier and HostedZone.Features.{AcceleratedRecoveryStatus,ChangeGuardrails} (fixture route53-hosted-zone-comment-tags-and-not-found, step task#1).
AWS: CreateHostedZone answers 201 with a HostedZone element whose Config carries HostedZoneTier (value STANDARD for an ordinary newly-created zone) and whose Features carries AcceleratedRecoveryStatus (DISABLED on a zone that has not enabled accelerated recovery) plus a ChangeGuardrails element holding one ChangeGuardrail entry per guardrail Route 53 defines, each with a Name and a Status (the recording shows the six guardrail names NS_OCCLUSION_CHECK, NS_DELEGATION_AUTHORITY_CHECK, NS_FORMAT_CHECK, ALIAS_TARGET_CHECK, ACTIVE_RECORD_CHECK, ACTIVE_DELEGATION_CHECK, each Status DISABLED on a fresh zone). Smithy: HostedZone.Config -> HostedZoneConfig and HostedZone.Features -> HostedZoneFeatures (members AcceleratedRecoveryStatus, FailureReasons, PublicDNSFallbackStatus); the on-disk model has NO HostedZoneTier member and NO ChangeGuardrails member, so those two are recorded behavior only - the recording is the authority that AWS serializes them inside Config and Features respectively.
Emulator: answers 201 but Config.HostedZoneTier is absent (read back as None), Features.AcceleratedRecoveryStatus is absent, and Features.ChangeGuardrails is absent.
Why: a client that reads the zone's tier or its guardrail/accelerated-recovery posture from the create response gets null for all three, so tier-aware and guardrail-aware logic silently takes its not-configured branch and a round-trip of the create response back into a zone description loses the feature block entirely.
Fix: the CreateHostedZone handler's HostedZone serializer, and the zone state record it projects from (which must carry a tier and a per-guardrail status set from creation).

Cause 2 of 4: CreateHostedZone on a PRIVATE zone emits a Features element AWS leaves empty (fixture route53-create-private-zone-preserves-vpc-association, step task#2).
AWS: for the private hosted zone created in this fixture the recorded 201 response carries HostedZone.Features as an empty element - no PublicDNSFallbackStatus member at all. Smithy: HostedZoneFeatures.PublicDNSFallbackStatus is an optional String member documented as 'The status of public DNS fallback for the hosted zone'; nothing in the model requires it to be serialized, and the recording shows AWS omitting it here. Hypothesis: public DNS fallback is not a concept that applies to a private zone, so Route 53 serializes no status for it.
Emulator: emits Features as a structure containing PublicDNSFallbackStatus = DISABLED, a type mismatch against the empty element AWS returns.
Why: a client that tests for the presence of a feature member to decide whether a feature is configured sees public DNS fallback as configured-and-disabled on a private zone where AWS reports nothing, and strict response comparison against recorded AWS traffic fails on an element AWS never sends.
Fix: the CreateHostedZone handler's HostedZone.Features serializer - emit only the feature members the zone actually has state for, instead of a constant default block.

Cause 3 of 4: GetHostedZone omits the same tier and feature members (fixture route53-hosted-zone-comment-tags-and-not-found, step task#3; also reproduced at step task#13 of the same fixture).
AWS: GetHostedZone answers 200 with the same HostedZone element as the create response - Config.HostedZoneTier STANDARD, Features.AcceleratedRecoveryStatus DISABLED, and Features.ChangeGuardrails with one Name/Status entry per guardrail. Smithy: GetHostedZone output member HostedZone is required and targets the same HostedZone shape; HostedZoneTier/ChangeGuardrails are recorded behavior only, as in Cause 1.
Emulator: answers 200 with Config.HostedZoneTier, Features.AcceleratedRecoveryStatus and Features.ChangeGuardrails all absent.
Why: the zone's tier and feature posture is unreadable for the whole lifetime of the zone, not just at create time, so no client can observe or verify it.
Fix: the GetHostedZone handler's HostedZone serializer (same projection as Cause 1 - the fix belongs in the shared HostedZone projection, not per operation).
Note: the open issue 'Route 53 GetHostedZone serializes an empty HostedZone.Features element when no features are configured' (20261108T200652Z-route53-route-53-gethostedzone-serializes-an-empty-hostedzone-features.md) and 'Route 53 ListHostedZones emits HostedZone.Features as an empty element instead of a feature-status structure' (20261117T053039Z-...) describe the empty-Features case on GetHostedZone/ListHostedZones; neither covers the missing Config.HostedZoneTier, the missing ChangeGuardrails set, or the CreateHostedZone/UpdateHostedZoneComment projections filed here.

Cause 4 of 4: UpdateHostedZoneComment omits the same tier and feature members (fixture route53-hosted-zone-comment-tags-and-not-found, step task#12).
AWS: UpdateHostedZoneComment answers 200 echoing the updated HostedZone, including Config.HostedZoneTier STANDARD, Features.AcceleratedRecoveryStatus DISABLED and the ChangeGuardrails set - a comment update changes only Config.Comment and leaves every other projected member intact. Smithy: UpdateHostedZoneComment output member HostedZone is required and targets HostedZone; HostedZoneTier/ChangeGuardrails are recorded behavior only.
Emulator: answers 200 with Config.HostedZoneTier, Features.AcceleratedRecoveryStatus and Features.ChangeGuardrails all absent.
Why: a client that updates a comment and keeps the returned HostedZone as its cached view of the zone drops the tier and feature block, so a subsequent decision made from that cached object is made on nulls.
Fix: the UpdateHostedZoneComment handler's HostedZone serializer (shared projection, as above).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator, four mismatch clusters (status matched in every one; only the body diverged):

1) CreateHostedZone, fixture route53-hosted-zone-comment-tags-and-not-found#task#1, expected 201 / actual 201:
   $.CreateHostedZoneResponse.HostedZone.Config.HostedZoneTier: missing (expected 'STANDARD', got None)
   $.CreateHostedZoneResponse.HostedZone.Features.AcceleratedRecoveryStatus: missing (expected 'DISABLED', got None)
   $.CreateHostedZoneResponse.HostedZone.Features.ChangeGuardrails: missing (expected {'ChangeGuardrail': [{'Name': 'NS_OCCLUSION_CHECK', 'Status': 'DISABLED'}, {'Name': 'NS_DELEGATION_AUTHORITY_CHECK', 'Status': 'DISABLED'}, {'Name': 'NS_FORMAT_CHECK', 'Status': 'DISABLED'}, {'Name': 'ALIAS_TARGET_CHECK', 'Status': 'DISABLED'}, {'Name': 'ACTIVE_RECORD_CHECK', 'Status': 'DISABLED'}, {'Name': 'ACTIVE_DELEGATION_CHECK', 'Status': 'DISABLED'}]}, got None)

2) CreateHostedZone, fixture route53-create-private-zone-preserves-vpc-association#task#2, expected 201 / actual 201:
   $.CreateHostedZoneResponse.HostedZone.Features: type_mismatch (expected '', got {'PublicDNSFallbackStatus': 'DISABLED'})

3) GetHostedZone, fixture route53-hosted-zone-comment-tags-and-not-found#task#3 (cluster also covers #task#13), expected 200 / actual 200:
   $.GetHostedZoneResponse.HostedZone.Config.HostedZoneTier: missing (expected 'STANDARD', got None)
   $.GetHostedZoneResponse.HostedZone.Features.AcceleratedRecoveryStatus: missing (expected 'DISABLED', got None)
   $.GetHostedZoneResponse.HostedZone.Features.ChangeGuardrails: missing (expected the six-entry ChangeGuardrail list above, got None)

4) UpdateHostedZoneComment, fixture route53-hosted-zone-comment-tags-and-not-found#task#12, expected 200 / actual 200:
   $.UpdateHostedZoneCommentResponse.HostedZone.Config.HostedZoneTier: missing (expected 'STANDARD', got None)
   $.UpdateHostedZoneCommentResponse.HostedZone.Features.AcceleratedRecoveryStatus: missing (expected 'DISABLED', got None)
   $.UpdateHostedZoneCommentResponse.HostedZone.Features.ChangeGuardrails: missing (expected the six-entry ChangeGuardrail list above, got None)

Model evidence: smithy_query on services/service-com-amazonaws-route53-awsdnsv20130401/smithy-model/com-amazonaws-route53-awsdnsv20130401.json - HostedZone.Features -> HostedZoneFeatures{AcceleratedRecoveryStatus, FailureReasons, PublicDNSFallbackStatus}; HostedZone.Config -> HostedZoneConfig{Comment, PrivateZone}; shapes 'HostedZoneTier' and 'ChangeGuardrails' are not present in the model (recorded behavior only).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture route53-hosted-zone-comment-tags-and-not-found step task#1 - POST /2013-04-01/hostedzone creating a public zone - and read HostedZone.Config.HostedZoneTier and HostedZone.Features in the 201 body.
3. Cause 2: replay fixture route53-create-private-zone-preserves-vpc-association step task#2 - POST /2013-04-01/hostedzone with a VPC (private zone) - and observe the Features element is a structure carrying PublicDNSFallbackStatus instead of being empty.
4. Cause 3: replay fixture route53-hosted-zone-comment-tags-and-not-found step task#3 (and task#13) - GET /2013-04-01/hostedzone/{Id} - and read the same three members.
5. Cause 4: replay fixture route53-hosted-zone-comment-tags-and-not-found step task#12 - POST /2013-04-01/hostedzone/{Id} with a new Comment - and read the same three members in the 200 body.
6. All four steps must match the recorded AWS bodies before this issue is closed.
