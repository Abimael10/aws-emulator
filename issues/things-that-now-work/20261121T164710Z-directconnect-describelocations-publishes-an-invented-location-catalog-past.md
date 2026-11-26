Issue-Source: trace-sweep-divergence
Issue-Trace: directconnect-catalog-describe-locations
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: directconnect-catalog-describe-locations
Issue-Shape-Id: com.amazonaws.directconnect#OvertureService
Issue-Key: directconnect|DescribeLocations|2xx->2xx|$.locations[*]
Issue-Commit: 75e93429d3ea083e11be0578db4814848b1cd3ff

# directconnect: DescribeLocations publishes an invented location catalog past the recorded rows

> _Note: this issue was identified against the code base at commit `75e93429d3ea083e11be0578db4814848b1cd3ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLocations

## Description
The Direct Connect location catalog the emulator serves is only faithful for the first two rows of the signed Region; everything after them — the rest of the Region's locations and the wider flat-rate billing tiers — is emulator-authored data that disagrees with the recorded DescribeLocations response. This bundle has 1 cause.

Cause 1 of 1: DescribeLocations' listing diverges from the third element onward (fixture directconnect-catalog-describe-locations, step task#1).
AWS: Smithy: DescribeLocations takes smithy.api#Unit and returns Locations.locations : LocationList of Location {locationCode, locationName, region, availablePortSpeeds, availableProviders, availableMacSecPortSpeeds, availableBillingModes}; AvailableBillingMode carries billingMode (the BillingMode enum: PayAsYouGo, FlatRateTier1..5, PortPairFlatRateTier1..5), availablePortSpeeds and includedRegions. The model constrains neither which locations a Region publishes, nor which modes a location offers, nor which Regions a mode's fee includes, so this is recorded behavior only: the recorded 200 response for the signed Region publishes ten elements, and its third element (the whole-building location, locationCode 'SNAP') publishes the usage-based mode plus a five-tier flat-rate ladder whose tiers are DISJOINT distance bands — each Region named in exactly one tier, the higher tiers naming the farther Regions (including Regions such as ca-west-1, eu-west-3, eu-central-2, eu-south-2, ap-south-2, ap-southeast-3/4/5/6/7, ap-east-2, me-central-1, il-central-1 and mx-central-1) rather than repeating the nearer ones. The individual Region names and facility rows are published catalog data read off the recording, not values derivable from any rule.
Emulator: answers 200, and the first two elements match, but every element from index 2 to index 9 of the listing diverges from the recorded one. For the 'SNAP' element the first two flat-rate tiers match and tiers 3, 4 and 5 do not: they are built as CUMULATIVE supersets of the lower tiers from a short Region roster (services/service-com-amazonaws-directconnect-overtureservice/src/state/published_locations.rs, constants US_WEST_2_TIER_3 / US_WEST_2_TIER_4 / US_WEST_2_TIER_5), so a tier repeats Regions the recording puts in a lower tier and no tier names any of the Regions above. The remaining elements are catalogued facility rows (their codes, names, partner rosters, port speeds and tier ladders) that the recording does not publish — the module's own header says every row past the recorded ones is 'CATALOGUED: written from the public location and delivery-partner lists', i.e. invented.
Why: DescribeLocations is the discovery call a client makes before CreateConnection/CreateInterconnect: the locationCode it returns is passed straight back as CreateConnection$location, and availableBillingModes/includedRegions is what a client uses to decide which billing mode to order and which Regions a flat-rate fee covers. A client that enumerates locations therefore sees facilities AWS does not publish and misses ones it does, and a client that checks a Region against a tier's includedRegions gets the wrong answer in both directions (a Region listed in a tier AWS puts higher, and newer Regions listed in no tier at all).
Fix: the catalog seed data this read projects — the location rows and the per-Region flat-rate tier constants in src/state/published_locations.rs. The rows the recording covers must be written as published (one row per published Location, each tier's includedRegions as recorded, tiers disjoint rather than nested) instead of being derived from a cumulative ladder over a partial Region roster; src/ops/describe_locations.rs and the rendering in src/state/location_catalog.rs are a faithful read of whatever the seed holds and need no change.
Traces: directconnect-catalog-describe-locations#task#1.

No open issue covers this: the DescribeLocations catalog is named only in passing in 20261109T130740Z-directconnect-direct-connect-gateway-records-omit-arn-type.md, which carries no replay key for this fixture step.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout.

directconnect-catalog-describe-locations#task#1, DescribeLocations, expected 200 / actual 200. The divergence cluster keys are $.locations[2], $.locations[3], $.locations[4], $.locations[5], $.locations[6], $.locations[7], $.locations[8] and $.locations[9] - 8 of the 10 published elements.

For $.locations[2] (locationCode 'SNAP') the code, name, region, availablePortSpeeds, availableProviders, availableMacSecPortSpeeds, the PayAsYouGo mode and the FlatRateTier1/FlatRateTier2 includedRegions agree, and the three wider tiers do not:
  expected FlatRateTier3.includedRegions: the Europe/Canada/South-America/Asia-Pacific band including eu-north-1, eu-west-3, eu-central-1, sa-east-1, ap-northeast-3, eu-west-2, eu-south-1, ap-northeast-1, ap-southeast-1, ca-central-1, ca-west-1, eu-south-2, eu-central-2
  expected FlatRateTier4.includedRegions: ap-south-1, af-south-1, ap-east-1, ap-south-2, ap-northeast-2, ap-southeast-4, mx-central-1, ap-southeast-2, il-central-1
  expected FlatRateTier5.includedRegions: ap-southeast-6, me-south-1, ap-southeast-7, ap-southeast-3, me-central-1, ap-southeast-5, ap-east-2
  got: tier 3 = tier 2's Regions plus us-west-1/ca-central-1/eu-west-2/eu-central-1/eu-north-1; tier 4 = tier 3 plus four ap-* Regions and ap-south-1; tier 5 = tier 4 plus ap-northeast-3/ap-east-1/sa-east-1/me-south-1/af-south-1/eu-south-1 - i.e. nested tiers, with ca-west-1, eu-west-3, eu-central-2, eu-south-2, ap-south-2, ap-southeast-3/4/5/6/7, ap-east-2, me-central-1, il-central-1 and mx-central-1 named by no tier at all.

Seed under test: src/state/published_locations.rs BUILT_IN_LOCATIONS (rows past the two recorded Seattle demarcations are marked 'catalogued') and the US_WEST_2_TIER_3/4/5 constants (commented 'catalogued - the tier-N group with ...', i.e. built by accumulation).

## Steps to Reproduce
1. Replay fixture directconnect-catalog-describe-locations against the emulator.
2. Step task#1: call DescribeLocations (no input) signed for the fixture's Region.
3. Compare the returned $.locations list with the recorded one element by element: elements 0 and 1 match; from $.locations[2] on the elements diverge.
4. On $.locations[2] (locationCode 'SNAP'), inspect availableBillingModes: FlatRateTier3/FlatRateTier4/FlatRateTier5 includedRegions are cumulative supersets of the lower tiers drawn from a partial Region roster, not the recorded disjoint distance bands, and they name none of the newer Regions the recording lists.
