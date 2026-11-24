Issue-Source: trace-sweep-divergence
Issue-Trace: route53-cidr-collection-basic-lifecycle
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-cidr-collection-basic-lifecycle route53-cidr-collection-lifecycle route53-cidr-collection-lifecycle-and-list route53-list-hosted-zones-membership-and-by-vpc
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|ListCidrCollections|2xx->2xx|$.ListCidrCollectionsResponse.CidrCollections.member[Arn="arn:aws:route53:::cidrcollection/c4f80f50-628d-680f-8a97-bf20ee2d911f"]
Issue-Key: route53|ListCidrCollections|2xx->2xx|$.ListCidrCollectionsResponse.CidrCollections.member[Arn="arn:aws:route53:::cidrcollection/f6c868cc-4d4c-6dfd-7f68-54435fbe2a66"]
Issue-Key: route53|ListCidrCollections|2xx->2xx|$.ListCidrCollectionsResponse.CidrCollections.member[Arn="arn:aws:route53:::cidrcollection/c93c355b-c8a1-b223-8aa6-a0e752161987"]
Issue-Key: route53|ListCidrCollections|2xx->2xx|$.ListCidrCollectionsResponse.CidrCollections.member[Arn="arn:aws:route53:::cidrcollection/3fd53796-eced-9e6b-d3ab-36e236b0bf97"]
Issue-Key: route53|ListHostedZones|2xx->2xx|$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhza-fbd5b98d.com."].Features
Issue-Key: route53|ListHostedZones|2xx->2xx|$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhzb-3bf9a85f.com."].Features
Issue-Commit: 332deeb05333b69e850545a66f3071a5605eafe8

# Route 53 list projections unmatchable: CIDR collection rows keyed by an account-stamped Arn, ListHostedZones Features emitted empty

> _Note: this issue was identified against the code base at commit `332deeb05333b69e850545a66f3071a5605eafe8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListCidrCollections

## Description
FOUR causes in this bundle. All four are route53 list projections whose rows/members cannot be matched against what AWS returns: three on ListCidrCollections (a collection the emulator itself just created is not findable in the listing by its AWS-shaped Arn) and one on ListHostedZones (HostedZone.Features is serialized as an empty element instead of the feature-status structure). A fixer must close all four before submitting.

Cause 1 of 4 — ListCidrCollections row for a freshly created collection is not matchable (fixture route53-cidr-collection-basic-lifecycle, step task#3; cluster also covers task#4).
AWS: Smithy: ListCidrCollections -> ListCidrCollectionsResponse.CidrCollections : CollectionSummaries of CollectionSummary{Arn (ARN, @length 20..2048, @pattern \S), Id (UUID, @pattern ^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$), Name (CollectionName), Version (CollectionVersion, @range min 1)}; HTTP GET /2013-04-01/cidrcollection, success 200. Route 53 is a global (account-less in this ARN family) service: the recorded response lists each collection with Arn of the form arn:aws:route53:::cidrcollection/<collection-id> — partition field populated, region AND account fields EMPTY — plus the Id, the Name the caller supplied to CreateCidrCollection and Version starting at 1.
Emulator: status 200, but no member whose Arn is the AWS-shaped arn:aws:route53:::cidrcollection/<id> exists in the body (replay: '$.ListCidrCollectionsResponse.CidrCollections.member[Arn="arn:aws:route53:::cidrcollection/<id>"]: missing (expected {Arn, Id, Name, Version:'1'}, got None)'). Hypothesis: the row IS present but keyed by an account-stamped Arn, so it cannot be matched — the same corpus records CreateCidrCollection returning Collection.Arn as arn:aws:route53::<account>:cidrcollection/<id> where AWS leaves the account field empty (that create-path divergence is already owned by open issue 20261030T161909Z-route53-route-53-cidr-collection-arns-embed-an-account.md, which does NOT carry this listing step's replay key). If the Arn is not the cause, then the collection is simply absent from the listing, which is the stronger defect; both read on the same projection.
Why: a client that enumerates CIDR collections and then addresses one by the Arn the listing gave it (IAM policy resource, cross-account reference, correlation against a create response) finds either no row at all or a row whose Arn is unusable against real AWS, so an inventory/reconcile loop concludes the collection it just created does not exist.
Fix: the ListCidrCollections handler / its response serializer, together with the Arn the collection state stores at create time — the collection's ARN must be rendered with empty region and account fields, and every created collection must appear in the listing.
Traces: route53-cidr-collection-basic-lifecycle#task#3 (cluster also affects route53-cidr-collection-basic-lifecycle#task#4).

Cause 2 of 4 — same ListCidrCollections projection defect on a single-collection lifecycle (fixture route53-cidr-collection-lifecycle, step task#2).
AWS: as Cause 1. The listing after CreateCidrCollection contains one CollectionSummary with Arn arn:aws:route53:::cidrcollection/<id>, the caller's Name and Version 1.
Emulator: status 200 with no member matching that account-less Arn (replay: 'member[Arn="arn:aws:route53:::cidrcollection/<id>"]: missing ... got None'), Version 1 row unmatchable.
Why: as Cause 1 — create-then-list round-trips cannot be correlated.
Fix: as Cause 1.
Traces: route53-cidr-collection-lifecycle#task#2.

Cause 3 of 4 — same ListCidrCollections projection defect after an update bumped Version (fixture route53-cidr-collection-lifecycle-and-list, step task#3).
AWS: as Cause 1, and CollectionSummary.Version is the sequential counter Route 53 sets to 1 at create and increments by 1 per settings update, so after one ChangeCidrCollection the row reads Version 2.
Emulator: status 200 with no member matching Arn arn:aws:route53:::cidrcollection/<id> (expected row carried Version '2'), got None.
Why: a client that reads Version from the listing to detect concurrent modification cannot locate the row at all, so optimistic-concurrency logic over CIDR collections never sees the bump.
Fix: as Cause 1.
Traces: route53-cidr-collection-lifecycle-and-list#task#3.

Cause 4 of 4 — ListHostedZones serializes HostedZone.Features as an empty element (fixture route53-list-hosted-zones-membership-and-by-vpc, step task#6).
AWS: Smithy: ListHostedZones -> ListHostedZonesResponse.HostedZones : HostedZones of HostedZone, whose member Features targets the HostedZoneFeatures STRUCTURE ('the features configuration for the hosted zone, including accelerated recovery settings and status information'); HTTP GET /2013-04-01/hostedzone, success 200. Recorded behavior: each listed zone carries Features as a nested structure holding a feature status — the recording shows the member PublicDNSFallbackStatus with value DISABLED for a plain zone (that member name is not in the on-disk model, whose HostedZoneFeatures declares AcceleratedRecoveryStatus and FailureReasons, so the member list is recorded behavior only; what is modeled and recorded alike is that Features is a structure of feature statuses, never a scalar). The exact zone names are per-run values, not requirements; the requirement is that every listed zone's Features is a structure whose feature-status members report that zone's configured state.
Emulator: status 200, but Features is serialized as an empty element for every listed zone (replay: 'HostedZones.HostedZone[Name=...].Features: type_mismatch (expected {PublicDNSFallbackStatus: DISABLED}, got \'\')' for both zones in the step).
Why: an XML/SDK client deserializing HostedZone.Features gets an empty structure where AWS reports feature statuses, so code that reads a zone's feature state from the listing sees it as unset/None and cannot distinguish 'feature disabled' from 'field absent'; strict deserializers can also fault on a scalar where a structure is modeled.
Fix: the ListHostedZones response serializer / the hosted-zone projection it shares with GetHostedZone — Features must be omitted entirely when there is nothing to report, or serialized as a populated structure, never as an empty element.
Traces: route53-list-hosted-zones-membership-and-by-vpc#task#6.

Related open issues that do NOT carry these steps' replay keys (so they do not cover these causes; named here for the fixer): 20261030T161909Z-route53-route-53-cidr-collection-arns-embed-an-account.md (CreateCidrCollection Arn), 20261111T041251Z-route53-route-53-cidr-collection-read-paths-diverge-listing.md, 20261112T225243Z-route53-route-53-listcidrcollections-rows-and-listhostedzones-features-cannot.md, 20261114T032429Z-route53-route-53-list-responses-misreport-membership-cidr-collection.md, 20261110T235626Z-route53-route-53-listhostedzones-projects-hostedzone-features-as-an.md, and 20261108T200652Z-route53-route-53-gethostedzone-serializes-an-empty-hostedzone-features.md (which owns the GetHostedZone Features cluster only).

## Evidence
Cause 1 (route53-cidr-collection-basic-lifecycle#task#3, ListCidrCollections): expected 200 with CidrCollections member keyed Arn=arn:aws:route53:::cidrcollection/<id> {Arn, Id, Name, Version:'1'}; actual 200 with that member missing (got None) for two collections in the same response. Cluster also affects task#4.
Cause 2 (route53-cidr-collection-lifecycle#task#2, ListCidrCollections): expected member[Arn="arn:aws:route53:::cidrcollection/<id>"] with Version '1'; actual None.
Cause 3 (route53-cidr-collection-lifecycle-and-list#task#3, ListCidrCollections): expected member[Arn="arn:aws:route53:::cidrcollection/<id>"] with Version '2'; actual None.
Supporting (already-owned CreateCidrCollection cluster in the same replay): expected Collection.Arn arn:aws:route53:::cidrcollection/<id> (empty region AND account fields); actual arn:aws:route53::<account>:cidrcollection/<id>.
Cause 4 (route53-list-hosted-zones-membership-and-by-vpc#task#6, ListHostedZones): expected HostedZone.Features = structure {PublicDNSFallbackStatus: DISABLED} for each listed zone; actual Features = '' (empty element), reported as type_mismatch for both zones.
Status codes matched in all four causes (200 expected, 200 actual); the divergence is purely in the projected body.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay fixture route53-cidr-collection-basic-lifecycle through step task#3 — CreateCidrCollection twice, then GET /2013-04-01/cidrcollection; look for a CidrCollections member whose Arn is arn:aws:route53:::cidrcollection/<created-id> (no account field). None matches.
3. Cause 2: replay route53-cidr-collection-lifecycle step task#2 (create one collection, then list) and look for the same account-less Arn with Version 1.
4. Cause 3: replay route53-cidr-collection-lifecycle-and-list step task#3 (create, change, then list) and look for the same account-less Arn with Version 2.
5. Cause 4: replay route53-list-hosted-zones-membership-and-by-vpc step task#6 — GET /2013-04-01/hostedzone and inspect each HostedZones/HostedZone/Features element: the emulator emits it empty instead of a structure carrying the zone's feature status.
