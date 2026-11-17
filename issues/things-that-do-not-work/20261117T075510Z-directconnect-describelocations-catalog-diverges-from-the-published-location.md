Issue-Source: trace-sweep-divergence
Issue-Trace: directconnect-catalog-describe-locations
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: directconnect-catalog-describe-locations
Issue-Shape-Id: com.amazonaws.directconnect#OvertureService
Issue-Key: directconnect|DescribeLocations|2xx->2xx|$.locations[*]
Issue-Commit: 9fab9846750cdfe9e2888499cf2790a38cb445d8

# directconnect: DescribeLocations catalog diverges from the published location list for a Region

> _Note: this issue was identified against the code base at commit `9fab9846750cdfe9e2888499cf2790a38cb445d8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLocations

## Description
Cause 1 of 1: DescribeLocations answers a us-west-2 location catalog whose membership, locationCode/locationName spelling, provider roster and availableBillingModes do not match the location list AWS publishes for the signed Region, so every element from the third onward is a different location than the recorded one.

AWS: Smithy: DescribeLocations takes smithy.api#Unit input and returns Locations$locations (LocationList of Location{locationCode, locationName, region, availablePortSpeeds, availableProviders, availableMacSecPortSpeeds, availableBillingModes}); modeled errors DirectConnectClientException (400) and DirectConnectServerException (500). Location$locationCode is documented as "The code for the location" and Location$locationName as "The name of the location. This includes the name of the colocation partner and the physical site of the building"; Location$availableBillingModes is "The billing modes available at the location, including the port speeds and Amazon Web Services Regions supported by each mode", with billingMode drawn from the BillingMode enum (PayAsYouGo, FlatRateTier1..FlatRateTier5, PortPairFlatRateTier1..5). The CONTENT of the catalog is recorded behavior only - the model fixes the shape, not the facility list. The recording for us-west-2 shows, at index 2, a location whose locationCode is a short facility code with no partner suffix ('SNAP') and whose locationName is the building alone ('Switch SUPERNAP 8, Las Vegas, NV'), publishing availablePortSpeeds ['100G','1G','10G'], availableMacSecPortSpeeds ['100G','10G'], a nine-name availableProviders roster that includes delivery partners such as 'Crown Castle', 'IX Reach', 'Cox Business' and 'XO Communications', and availableBillingModes carrying PayAsYouGo plus FlatRateTier1 through FlatRateTier5, each tier's includedRegions enumerating the Regions that tier's flat fee covers (Tier1 ['us-west-1'], Tier2 the US East/EU-West group, Tiers 3-5 progressively wider Region groups spanning every commercial Region). Status 200.

Emulator: the same step returns 200, but the response body diverges at $.locations[2] through $.locations[9] - the whole tail of the list. Where the recording has locationCode 'SNAP' / locationName 'Switch SUPERNAP 8, Las Vegas, NV', the emulator serves locationCode 'EqSe3-CL' / locationName 'Equinix SE3, Seattle, WA | Cologix, SEA1, Seattle, WA' with availablePortSpeeds ['1G','10G'], availableMacSecPortSpeeds [] and a different availableProviders roster. The emulator's catalog is a built-in facility table whose entries are synthesized one per (facility, partner) pair: a locationCode is DERIVED as '<facility code>-<partner code>' and a locationName as '<building> | <partner site>', the provider roster is narrowed from a single thirty-name list, and availableBillingModes is derived from a per-row flat-rate tier COUNT against a six-entry Region-area table. That derivation cannot produce the published values: it emits no 'SNAP'-style unsuffixed code, no building-only name, no provider outside its roster, and no includedRegions set covering the Regions the recorded tiers list. Because the list is keyed and ordered by locationCode, the differing membership also shifts every subsequent element, which is why eight consecutive indices diverge from a catalog difference.

Why: DescribeLocations is the discovery call a client makes before CreateConnection/CreateInterconnect, and the locationCode it returns is the exact token those calls take. A client that reads the emulator's catalog picks codes ('EqSe3-CL', 'SUPERNAP8-SW') that do not exist in AWS and never sees the ones that do, so a provisioning flow validated against the emulator fails on AWS at the first CreateConnection. Clients that filter on availableProviders, availableMacSecPortSpeeds or availableBillingModes/includedRegions (MACsec capability checks, flat-rate cost selection) also branch on values AWS does not publish for that facility, and any client that indexes or diffs the list sees a wholly different ordering.

Fix: the DescribeLocations catalog state - the built-in facility table and the derivation that renders a Location from it, in the service's state module (the seed consumed by locations_in_region / location_output), not the handler, which is a pure read of that state. The catalog's locationCode and locationName must be able to carry the published value for a facility (including an unsuffixed code and a building-only name) instead of always being synthesized from facility+partner, the provider roster must admit the partners the published locations name, and availableBillingModes/includedRegions must be able to carry the published tier-to-Region mapping rather than being derived from a tier count against a fixed six-area table.

Traces: directconnect-catalog-describe-locations#task#1 (DescribeLocations, expected 200 / actual 200, $.locations[2]..$.locations[9] mismatched). Related but NOT covering this: open issues 20261109T130740Z-directconnect-direct-connect-gateway-records-omit-arn-type.md and 20261109T130740Z-directconnect-gateway-and-resiliency-group-error-responses-diverge.md are about Direct Connect gateway records and gateway/resiliency-group error responses, and neither touches the location catalog.

## Evidence
Step directconnect-catalog-describe-locations#task#1, operation DescribeLocations, verdict mismatch, expected status 200, actual status 200.

Diverging keys: $.locations[2], $.locations[3], $.locations[4], $.locations[5], $.locations[6], $.locations[7], $.locations[8], $.locations[9].

$.locations[2]: missing - expected an element with locationCode 'SNAP', locationName 'Switch SUPERNAP 8, Las Vegas, NV', region 'us-west-2', availablePortSpeeds ['100G','1G','10G'], availableMacSecPortSpeeds ['100G','10G'], availableProviders ['Verizon','Megaport','Zayo Group','CenturyLink','AT&T','Crown Castle','IX Reach','Cox Business','XO Communications'] and availableBillingModes PayAsYouGo + FlatRateTier1..FlatRateTier5 with per-tier includedRegions (Tier1 ['us-west-1']; Tier2 ['us-east-2','eu-west-1','us-east-1','us-west-2']; Tiers 3-5 the wider Region groups).

Got instead at that index: locationCode 'EqSe3-CL', locationName 'Equinix SE3, Seattle, WA | Cologix, SEA1, Seattle, WA', region 'us-west-2', availablePortSpeeds ['1G','10G'], availableMacSecPortSpeeds [], availableProviders ['Telstra','Colt','Verizon','Console Connect','Internet2','Megaport','GTT','Allstream','Equinix, Inc.','Zayo Group','CenturyLink','AT&T','Comcast','PacketFabric','Telus','Arelion'], availableBillingModes beginning with PayAsYouGo over ['1G','10G'].

The catalog the emulator serves is the built-in facility table in the directconnect service's state module: facility 'EqSe3' (Equinix SE3, Seattle, WA) with partner rows EQ and CL, and facility 'SUPERNAP8' (Switch SUPERNAP 8, Las Vegas, NV) with a single 'SW' partner row - which renders as locationCode 'SUPERNAP8-SW' and locationName 'Switch SUPERNAP 8, Las Vegas, NV | Switch, SUPERNAP 8, Las Vegas, NV', never the published 'SNAP' / 'Switch SUPERNAP 8, Las Vegas, NV'. Only $.locations[0] and $.locations[1] agree; the differing membership shifts the rest of the list.

The fixed part of this requirement is the SHAPE and the published catalog semantics (one Location per published location in the signed Region, its own code/name/speeds/providers/billing modes); the specific codes, names, speeds, provider names and includedRegions above are the published facility data for us-west-2, not per-run values - no id, timestamp or request id is involved.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture directconnect-catalog-describe-locations.
2. At step task#1 the fixture calls directconnect DescribeLocations signed for us-west-2 (the operation takes no input; the signed Region selects the locations).
3. Compare the returned $.locations array element-by-element with the recorded response. Elements 0 and 1 match; from $.locations[2] on, every element is a different location - the recorded index 2 is locationCode 'SNAP' / locationName 'Switch SUPERNAP 8, Las Vegas, NV', the emulator serves locationCode 'EqSe3-CL' / locationName 'Equinix SE3, Seattle, WA | Cologix, SEA1, Seattle, WA'.
4. Observe that no catalog entry can produce an unsuffixed locationCode or a building-only locationName, and that availableProviders / availableBillingModes.includedRegions for the diverging entries are drawn from the emulator's fixed roster and six-area Region table rather than the published values.
