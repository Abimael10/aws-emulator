Issue-Source: trace-sweep-divergence
Issue-Trace: directconnect-catalog-describe-locations
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: directconnect-catalog-describe-locations
Issue-Shape-Id: com.amazonaws.directconnect#OvertureService
Issue-Key: directconnect|DescribeLocations|2xx->2xx|$.locations[*]
Issue-Commit: e35babe621893e5087518213f3a8eab658fbcbae

# directconnect: DescribeLocations catalog diverges from AWS for nine of ten locations

> _Note: this issue was identified against the code base at commit `e35babe621893e5087518213f3a8eab658fbcbae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLocations

## Description
Cause 1 of 1: the emulator's built-in DescribeLocations catalog is not the AWS catalog. The response shape and status are right, but the per-location CONTENT of nine of the ten returned Location entries diverges from what AWS serves for the same region, so the whole list beyond the first element compares unequal element-by-element.

AWS: Smithy (com.amazonaws.directconnect, DescribeLocations) takes no input and returns output shape Locations with member `locations: LocationList`. Each `Location` carries `locationCode`, `locationName` (documented as "The name of the location. This includes the name of the colocation partner and the physical site of the building."), `region`, `availablePortSpeeds`, `availableProviders`, `availableMacSecPortSpeeds` and `availableBillingModes: AvailableBillingModeList`, where each `AvailableBillingMode` carries `billingMode` (enum BillingMode), `availablePortSpeeds` and `includedRegions`. The model constrains the SHAPE only; the catalog VALUES are a server-side data set, so for those this is recorded behavior only. The recording shows, for the same request, locations whose `locationName` carries the full colocation-partner plus physical-site designation (e.g. the second entry's name ends with the building's specific site/FMMR designation after the ' | ' separator, not a repeat of the building name), whose `availableProviders` list is the AWS provider set for that location, and whose `availableBillingModes` includes FlatRate tiers with a populated `includedRegions` list in addition to PayAsYouGo.

Emulator: returns status 200 and a well-formed `locations` list, but every entry from index 1 through index 9 differs from the recorded AWS entry for the same `locationCode`. Concretely, at the second entry (locationCode 'EqSe2-WBE', region 'us-west-2') the emulator's `locationName` repeats the building name after the ' | ' separator instead of naming the physical site, and its `availableProviders` list carries an extra provider that AWS does not list for that location; subsequent entries diverge in `availableBillingModes`, where the recording has additional FlatRate billing modes with `includedRegions` populated and the emulator does not. The replay reports these as `$.locations[1]` .. `$.locations[9]` 'missing', i.e. no emulator element matched the expected element at those positions.

Why: DescribeLocations is a read-only catalog lookup that clients use to pick a colocation facility, a port speed and a billing mode before creating a connection or a LAG. A client that reads this list gets the wrong facility description, an available-provider set that does not match the real one (so it offers a provider that cannot be ordered there, or omits one that can), and a billing-mode set missing the FlatRate tiers and their `includedRegions`. Any configuration, UI picker, or validation that is driven off this catalog therefore selects values AWS will reject, and any test that asserts on a location's providers or billing modes passes against the emulator and fails against AWS.

Fix: the DescribeLocations handler's static location catalog in the directconnect service (the seed data the operation serves, not the serializer — the serialized shape already matches the model). Each catalog row needs its `locationName` carrying the colocation-partner plus physical-site form, its `availableProviders` set, and its `availableBillingModes` entries including the FlatRate tiers with `includedRegions`, for every location beyond the first.

Traces: directconnect-catalog-describe-locations#task#1 (DescribeLocations, expected 200 / actual 200, nine diverging list elements).

Note: this is not covered by the two open directconnect issues (20261109T130740Z-directconnect-direct-connect-gateway-records-omit-arn-type.md and 20261109T130740Z-directconnect-gateway-and-resiliency-group-error-responses-diverge.md); both are about gateway/resiliency-group records and error wording, not the location catalog.

## Evidence
Replay cluster: service=directconnect | DescribeLocations | status=200->200 | keys=$.locations[1],$.locations[2],$.locations[3],$.locations[4],$.locations[5],$.locations[6],$.locations[7],$.locations[8],$.locations[9]

Expected (recorded AWS), locations[1]:
  locationCode 'EqSe2-WBE', region 'us-west-2'
  locationName 'Westin Building Exchange, Seattle, WA | Westin Building Exchange - SEA10 - Main FMMR, Seattle, WA'
  availableProviders: 30-entry provider set WITHOUT 'Tata Communications'
  availableMacSecPortSpeeds ['100G','10G','400G'], availablePortSpeeds ['100G','1G','10G','400G']
  availableBillingModes [{billingMode 'PayAsYouGo', availablePortSpeeds ['100G','1G','10G','400G']}]

Actual (emulator), locations[1]:
  locationCode 'EqSe2-WBE', region 'us-west-2'
  locationName 'Westin Building Exchange, Seattle, WA | Westin Building Exchange, Seattle, WA'  <-- site designation replaced by a repeat of the building name
  availableProviders: same set PLUS a trailing 'Tata Communications'

Expected locations[2] additionally carries billing modes the emulator omits:
  availableBillingModes [{billingMode 'PayAsYouGo', availablePortSpeeds ['100G','1G','10G']},
                         {billingMode 'FlatRateTier1', availablePortSpeeds ['10G','100G'], includedRegions ['us-west-1']},
                         {billingMode 'FlatRateTier2', availablePortSpeeds ['10G','100G'], includedRegions ['us-east...']}]

Status and response shape agree (200, `locations` list of Location); only the catalog values diverge. Model reference: Location.locationName / .availableProviders / .availableBillingModes; AvailableBillingMode.billingMode (enum BillingMode) / .availablePortSpeeds / .includedRegions.

## Steps to Reproduce
1. Boot the emulator from this checkout (directconnect data listener).
2. Replay fixture `directconnect-catalog-describe-locations`, step `task#1`: an unsigned-body DescribeLocations call (OvertureService.DescribeLocations, empty input) against the us-west-2 directconnect endpoint.
3. Compare the returned `locations` array element-by-element against the recorded response. Elements 1..9 do not match: element 1 differs in `locationName` (text after the ' | ' separator) and in `availableProviders` (one extra provider), and later elements differ in `availableBillingModes` (missing FlatRateTier entries and their `includedRegions`).
4. Only `locations[0]` matches, which is why status and shape assertions pass while the list content does not.

## Resolution

Three rules were implemented in the catalog and its renderer
(`services/service-com-amazonaws-directconnect-overtureservice/src/state.rs`):

1. `locationName`'s second half names the colocation partner's physical site inside the
   building (its cage, suite or meet-me-room designation), never a repeat of the
   building half. Two rows carried the repeat and were corrected.
2. `availableProviders` draws on a roster holding only partner names a recorded
   location publishes, so the carrier hotel's demarcations no longer publish an extra,
   unrecorded delivery partner.
3. `availableBillingModes` is derived per location: the usage-based mode on every port
   speed the location offers, then one `AvailableBillingMode` per flat-rate tier
   offered there, in tier order, each on that location's own 10G/100G ports and naming
   in `includedRegions` one area of Regions further out from the location's own
   (tier 1 the rest of its own area).

Replay state when this was closed: `$.locations[1]`, the element whose expected content
this report spells out, now matches. `$.locations[2]`..`$.locations[9]` still diverge:
they are eight further us-west-2 facilities whose catalog rows (location code, building,
partner site, partner list, port speeds) the recording withholds — publishing the same
flat-rate tier set at every non-Seattle us-west-2 location matched none of them, so the
gap is undisclosed facility DATA rather than a missing rule. The tier counts per row and
the Region areas each tier includes are catalogued, not recorded, beyond the two Seattle
demarcations (which publish the usage-based mode alone) and the one us-west-2 element
whose two tiers this report quotes; a later fixer holding a recording for one of these
facilities should refine that row rather than read it as fact.
