Issue-Source: trace-sweep-divergence
Issue-Trace: gamelift-catalog-list-locations
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: gamelift-catalog-list-locations
Issue-Shape-Id: com.amazonaws.gamelift#GameLift
Issue-Key: gamelift|ListLocations|2xx->2xx|$.Locations[*]
Issue-Key: gamelift|ListLocations|2xx->2xx|$.Locations[*].PingBeacon.UDPEndpoint.Domain
Issue-Commit: f219ac3df78b3a0d7de8e592fcbd60f707d44291

# GameLift ListLocations managed-location catalog is incomplete and region-substituted

> _Note: this issue was identified against the code base at commit `f219ac3df78b3a0d7de8e592fcbd60f707d44291`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListLocations

## Description
One cause. Residual divergence on ListLocations after the earlier catalog gap was partly closed: the open issue 20261029T183132Z-gamelift-responses-omit-modeled-fields-matchmaking-defaults-terminationtime.md (its Cause 4) was filed when ListLocations returned a single entry {"LocationName": "us-west-2"} with no PingBeacon. The emulator now serves a multi-entry catalog with PingBeacon.UDPEndpoint populated, but the catalog it serves is still not the GameLift managed-location set: the tail of the list diverges and it is short by entries. That residual is this issue's cause; the replay reports it as unowned (its divergence key no longer matches the keys that issue carries), so it is filed net-new here and the earlier issue is named for context, not as cover.

Cause 1 of 1: ListLocations serves a truncated, region-substituted managed-location catalog.

AWS: ListLocations with no Filters returns the account's full set of GameLift-supported locations — every AWS Region in which GameLift Servers hosting is available plus the supported local zones — and each managed location carries PingBeacon.UDPEndpoint with a Domain naming a beacon for THAT location and Port 7770. The beacon domain follows the service form gamelift-ping.<region>.api.aws for a Region and gamelift-ping-<zone>.<region>.api.aws for a local zone, i.e. the domain is derived from the location's OWN region/zone, never from the region the caller's endpoint happens to be in. Smithy: ListLocations output Locations: LocationModelList, paginated (inputToken/outputToken NextToken, items Locations, pageSize Limit with range 1-200); LocationModel members LocationName (LocationStringModel, length 1-64, pattern ^[A-Za-z0-9\-]+$), LocationArn (custom locations only, pattern ^arn:.*:location\/custom-\S+$) and PingBeacon; PingBeacon.UDPEndpoint.Port documented as "typically port 7770". The specific membership of the catalog is service-published data, so for the exact roster the recording is the reference: recorded behavior only — the recorded response carries Regions including ap-east-1, ap-northeast-1, ap-northeast-2 and ap-south-2 and local zones including us-east-1-nyc-2, 46 entries in all.

Emulator: from the tail of the list onward the catalog does not match. At the index where the recording has ap-east-1 the emulator serves a us-west-2 entry (PingBeacon domain gamelift-ping.us-west-2.api.aws instead of gamelift-ping.ap-east-1.api.aws); where the recording has ap-northeast-1 and ap-northeast-2 the emulator serves us-west-2 local zones (gamelift-ping-lax.us-west-2.api.aws, gamelift-ping-phx.us-west-2.api.aws); where the recording has the us-east-1-nyc-2 local zone the emulator serves us-west-2-phx-2; and the recording's final ap-south-2 entry has no counterpart at all (got None), so the emulator's list is shorter than AWS's. In short: the out-of-region Regions and the non-us-west-2 local zones are absent, and their slots are filled by further us-west-2 entries. Hypothesis: the catalog is generated from the emulator's own configured region rather than carried as the service's published location roster, so every location — and therefore every beacon domain — collapses onto us-west-2.

Why: a client enumerating ListLocations to pick fleet locations can only ever build single-region fleets — a multi-location fleet in ap-northeast-1 or ap-south-2 looks unsupported, and CreateFleet/CreateLocation flows that validate a desired location against this list reject locations AWS accepts. A client reading PingBeacon to measure player latency per location gets the same us-west-2 beacon for several distinct locations, so latency-based placement ranks every region identically. The count is also wrong, so pagination over the catalog (Limit/NextToken) yields a different number of pages than AWS.

Fix: the ListLocations handler's catalog source — serve the GameLift managed-location roster as fixed service data (all supported Regions plus local zones), with each entry's PingBeacon.UDPEndpoint.Domain derived from that entry's own region/zone and Port 7770, rather than synthesizing entries from the emulator's configured region; keep the account's custom locations (with LocationArn) appended, and keep Limit/NextToken paging over the full roster. Note that one recorded beacon domain already matched exactly (the us-west-2 local-zone entry gamelift-ping-den.us-west-2.api.aws), which is consistent with the shape being right and only the roster being wrong. Per-environment values — hosts/ports of the emulator itself, the caller's region — are not the requirement here; the beacon domain's derivation from the location and the roster's membership are.

Traces: gamelift-catalog-list-locations#task#1 (ListLocations, 200/200).

## Evidence
Fixture gamelift-catalog-list-locations step task#1, ListLocations, expected status 200, actual status 200 — body diverges:
- $.Locations[43]: missing — expected {'LocationName': 'us-east-1-nyc-2', 'PingBeacon': {'UDPEndpoint': {'Domain': 'gamelift-ping-nyc.us-east-1.api.aws', 'Port': 7770}}}, got {'LocationName': 'us-west-2-phx-2', 'PingBeacon': {'UDPEndpoint': {'Domain': 'gamelift-ping-phx.us-west-2.api.aws', 'Port': 7770}}}
- $.Locations[44]: missing — expected {'LocationName': 'ap-south-2', 'PingBeacon': {'UDPEndpoint': {'Domain': 'gamelift-ping.ap-south-2.api.aws', 'Port': 7770}}}, got None (emulator's list ends short)
- $.Locations[39].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping.ap-east-1.api.aws', got 'gamelift-ping.us-west-2.api.aws'
- $.Locations[41].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping.ap-northeast-1.api.aws', got 'gamelift-ping-lax.us-west-2.api.aws'
- $.Locations[42].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping.ap-northeast-2.api.aws', got 'gamelift-ping-phx.us-west-2.api.aws'
- $.Locations[40].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping-den.us-west-2.api.aws', got the same value — that entry is NOT part of the defect and is listed only to show the beacon shape is already right where the location itself is right.
No status or error-code divergence: the operation answers 200 as AWS does; the defect is in the Locations roster and the per-location beacon derivation. Recorded roster length 46 entries; emulator stops at 44. Indices and the particular region names are the recording's values filling a roster template — the requirement is that the roster is GameLift's published location set and that each PingBeacon domain is derived from its own location.

## Steps to Reproduce
1. Replay the wire corpus simparity-wire-aws-v1 against the emulator.
2. Call gamelift ListLocations with no Filters and no Limit.
3. Count the returned Locations and list their LocationName values: AWS returns the full GameLift managed-location roster (46 entries in the recording, including Regions ap-east-1, ap-northeast-1, ap-northeast-2, ap-south-2 and the local zone us-east-1-nyc-2); the emulator returns a shorter list whose tail entries are all us-west-2 Regions/local zones.
4. For each entry, check PingBeacon.UDPEndpoint.Domain: it must be gamelift-ping.<that location's region>.api.aws (or gamelift-ping-<zone>.<region>.api.aws for a local zone) on Port 7770; the emulator serves us-west-2-derived domains for locations that are not us-west-2.
5. Re-replay gamelift-catalog-list-locations#task#1 to confirm the roster, its length and every beacon domain match.
