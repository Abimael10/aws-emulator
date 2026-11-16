Issue-Source: trace-sweep-divergence
Issue-Trace: route53-catalog-list-geo-locations
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53-catalog-list-geo-locations route53-list-hosted-zones-membership-and-by-vpc
Issue-Shape-Id: com.amazonaws.route53#AWSDnsV20130401
Issue-Key: route53|ListGeoLocations|2xx->2xx|$.ListGeoLocationsResponse.GeoLocationDetailsList.GeoLocationDetails[*]
Issue-Key: route53|ListHostedZones|2xx->2xx|$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhza-77693f83.com."].Features
Issue-Key: route53|ListHostedZones|2xx->2xx|$.ListHostedZonesResponse.HostedZones.HostedZone[Name="sim2real-probe-route53-lhzb-aee69059.com."].Features
Issue-Commit: cdc3646ad611eb2cc2d5bdd6e209586bd93b16e2

# Route 53 list projections diverge: ListGeoLocations subdivision catalog and ListHostedZones Features

> _Note: this issue was identified against the code base at commit `cdc3646ad611eb2cc2d5bdd6e209586bd93b16e2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListGeoLocations

## Description
Two net-new causes, both in Route 53's read/list projection path: a static reference catalog whose rows do not match AWS's, and a hosted-zone list projection that emits a structure member as an empty element. Both answer HTTP 200, so the divergence is purely in the serialized body and a client cannot detect it.

Cause 1 of 2: ListGeoLocations serves a geo-location subdivision catalog whose rows differ from AWS's (fixture route53-catalog-list-geo-locations, step task#1).
AWS: Smithy: ListGeoLocations -> ListGeoLocationsResponse.GeoLocationDetailsList (required, list of GeoLocationDetails{ContinentCode, ContinentName, CountryCode, CountryName, SubdivisionCode @length 1-3, SubdivisionName @length 1-64}), GET /2013-04-01/geolocations, 200. The model does not enumerate the catalog's contents, so this is recorded behavior only: the recording shows AWS's full unfiltered catalog, ordered by continent/country/subdivision code, and for CountryCode UA it contains subdivision rows that the emulator's catalog does not -- among them SubdivisionCode '11' SubdivisionName 'Crimea' and SubdivisionCode '20' SubdivisionName 'Sevastopol'. The catalog is a fixed reference dataset, identical on every call; it is a property of the service, not of the account or the run.
Emulator: at the same ordinal positions it returns different subdivision rows for UA, so the recorded rows are absent and rows AWS does not place there appear instead: expected GeoLocationDetails[230] {'CountryCode':'UA','SubdivisionCode':'11','SubdivisionName':'Crimea'} but got {'CountryCode':'UA','SubdivisionCode':'12','SubdivisionName':'Dnipropetrovska oblast'}; expected [234] {'SubdivisionCode':'20','SubdivisionName':'Sevastopol'} but got {'SubdivisionCode':'23','SubdivisionName':'Zaporizka oblast'}; and it emits extra rows at [239] {'SubdivisionCode':'40','SubdivisionName':'Sevastopol'} and [240] {'SubdivisionCode':'43','SubdivisionName':'Avtonomna Respublika Krym'} where AWS has {'SubdivisionCode':'32','SubdivisionName':'Kyivska oblast'} and {'SubdivisionCode':'35','SubdivisionName':'Kirovohradska oblast'}. So the emulator carries a differently-sourced UA subdivision list (different codes for the same names, and names AWS does not publish under those codes).
Why: a client that resolves a geolocation routing target by listing the catalog and selecting a SubdivisionCode cannot find codes AWS does publish, and will construct GeoLocation routing policies with codes AWS would reject. Because GetGeoLocation and geolocation resource record sets key on exactly these codes, every downstream call built from the listing is wrong, and the catalog's ordering also shifts the StartSubdivisionCode pagination cursor so a paged scan lands on different rows than against AWS.
Fix: the ListGeoLocations handler's backing reference catalog (the static geo-location dataset it projects, and the continent/country/subdivision ordering it emits it in).
Traces: route53-catalog-list-geo-locations#task#1.

Cause 2 of 2: ListHostedZones projects HostedZone.Features as an empty element instead of the zone's feature-status structure (fixture route53-list-hosted-zones-membership-and-by-vpc, step task#6).
AWS: Smithy: ListHostedZones -> ListHostedZonesResponse.HostedZones (required) of HostedZone, whose optional member Features targets the structure HostedZoneFeatures ('Represents the features configuration for a hosted zone, including the status of various features and any associated failure reasons'); GET /2013-04-01/hostedzone, 200. The on-disk model's HostedZoneFeatures declares AcceleratedRecoveryStatus and FailureReasons; the member observed on the wire, PublicDNSFallbackStatus, is recorded behavior only. The recording shows AWS projecting Features for each listed zone as a nested structure carrying a feature status -- Features: {'PublicDNSFallbackStatus': 'DISABLED'} for a zone with no accelerated-recovery configuration. The requirement is that Features, when present, is a structure of feature-status members; the member value is drawn from the zone's own feature state, never a fixed literal.
Emulator: for every zone in the listing it serializes Features as an empty element, which deserializes to the empty string rather than a structure -- type_mismatch (expected {'PublicDNSFallbackStatus': 'DISABLED'}, got '') on both listed zones in the fixture. The defect is the shape, not one zone's value: an empty XML element is emitted where either a populated HostedZoneFeatures structure or no Features element at all is correct.
Why: an SDK deserializing HostedZone.Features expects a structure; a text node there either fails deserialization or yields a Features object whose status members are all absent, so a client cannot read any zone's feature state from a listing and cannot tell a zone with features disabled from one whose state is unknown. Serializing an empty element is also strictly worse than omitting the member, since the member's presence signals that feature state was reported.
Fix: the ListHostedZones response serializer's HostedZone projection -- omit Features when the zone has no feature state, and otherwise serialize the HostedZoneFeatures structure from zone state; the same projection is shared with GetHostedZone.
Traces: route53-list-hosted-zones-membership-and-by-vpc#task#6.

Related open issues, none of which carries this step's replay key (the replay reports no owner for it), so this cause is filed net-new and is not covered by them: 20261110T235626Z-route53-route-53-listhostedzones-projects-hostedzone-features-as-an.md, 20261111T045951Z-route53-route-53-list-projections-cannot-be-matched-to.md, 20261112T225243Z-route53-route-53-listcidrcollections-rows-and-listhostedzones-features-cannot.md, 20261114T032429Z-route53-route-53-list-responses-misreport-membership-cidr-collection.md and 20261115T171142Z-route53-route-53-list-projections-unmatchable-cidr-collection-rows.md. The GetHostedZone variant of the same serializer defect is owned by 20261108T200652Z-route53-route-53-gethostedzone-serializes-an-empty-hostedzone-features.md.

## Evidence
Cause 1 - ListGeoLocations, fixture route53-catalog-list-geo-locations step task#1, expected status 200, actual status 200:
  $.ListGeoLocationsResponse.GeoLocationDetailsList.GeoLocationDetails[230]: missing (expected {'CountryCode': 'UA', 'CountryName': 'Ukraine', 'SubdivisionCode': '11', 'SubdivisionName': 'Crimea'}, got {'CountryCode': 'UA', 'CountryName': 'Ukraine', 'SubdivisionCode': '12', 'SubdivisionName': 'Dnipropetrovska oblast'})
  $.ListGeoLocationsResponse.GeoLocationDetailsList.GeoLocationDetails[234]: missing (expected {'CountryCode': 'UA', 'CountryName': 'Ukraine', 'SubdivisionCode': '20', 'SubdivisionName': 'Sevastopol'}, got {'CountryCode': 'UA', 'CountryName': 'Ukraine', 'SubdivisionCode': '23', 'SubdivisionName': 'Zaporizka oblast'})
  $.ListGeoLocationsResponse.GeoLocationDetailsList.GeoLocationDetails[239]: extra (expected {'CountryCode': 'UA', 'CountryName': 'Ukraine', 'SubdivisionCode': '32', 'SubdivisionName': 'Kyivska oblast'}, got {'CountryCode': 'UA', 'CountryName': 'Ukraine', 'SubdivisionCode': '40', 'SubdivisionName': 'Sevastopol'})
  $.ListGeoLocationsResponse.GeoLocationDetailsList.GeoLocationDetails[240]: extra (expected {'CountryCode': 'UA', 'CountryName': 'Ukraine', 'SubdivisionCode': '35', 'SubdivisionName': 'Kirovohradska oblast'}, got {'CountryCode': 'UA', 'CountryName': 'Ukraine', 'SubdivisionCode': '43', 'SubdivisionName': 'Avtonomna Respublika Krym'})

Cause 2 - ListHostedZones, fixture route53-list-hosted-zones-membership-and-by-vpc step task#6, expected status 200, actual status 200:
  $.ListHostedZonesResponse.HostedZones.HostedZone[Name="<zone-a>"].Features: type_mismatch (expected {'PublicDNSFallbackStatus': 'DISABLED'}, got '')
  $.ListHostedZonesResponse.HostedZones.HostedZone[Name="<zone-b>"].Features: type_mismatch (expected {'PublicDNSFallbackStatus': 'DISABLED'}, got '')
(The two zone names are per-run generated values; the requirement is that every listed zone's Features be a structure or absent, not those particular names.)

Model citations: Smithy ListGeoLocations output GeoLocationDetailsList (required) of GeoLocationDetails{ContinentCode, ContinentName, CountryCode, CountryName, SubdivisionCode @length 1-3, SubdivisionName @length 1-64}; Smithy ListHostedZones output HostedZones (required) of HostedZone, member Features -> structure HostedZoneFeatures.

## Steps to Reproduce
Cause 1: replay fixture route53-catalog-list-geo-locations and inspect step task#1 -- GET /2013-04-01/geolocations with no Start* filters, listing the whole catalog. Walk GeoLocationDetailsList to the CountryCode=UA rows (around index 230-240) and compare SubdivisionCode/SubdivisionName against the recorded response: the recorded 'Crimea' (code 11) and 'Sevastopol' (code 20) rows are absent and other codes/names appear in their place.

Cause 2: replay fixture route53-list-hosted-zones-membership-and-by-vpc and inspect step task#6 -- GET /2013-04-01/hostedzone. For each returned HostedZone element, inspect Features: the emulator emits <Features></Features> (deserializing to '') where the recording carries a HostedZoneFeatures structure with a feature-status member. Any two hosted zones created in the account reproduce it; the values are not zone-specific.
