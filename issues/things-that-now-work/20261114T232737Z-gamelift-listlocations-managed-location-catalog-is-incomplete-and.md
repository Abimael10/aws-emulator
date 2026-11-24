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
Issue-Commit: 9c2516539d875aca1c093d5d29d5c463295fb404

# GameLift ListLocations managed-location catalog is incomplete and derives Local Zone ping-beacon domains with the zone index

> _Note: this issue was identified against the code base at commit `9c2516539d875aca1c093d5d29d5c463295fb404`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListLocations

## Description
ListLocations now serves a managed-location catalog with ping beacons (the earlier "single us-west-2 entry, no PingBeacon" state described as Cause 4 of open issue 20261029T183132Z-gamelift-responses-omit-modeled-fields-matchmaking-defaults-terminationtime.md), but the catalog it serves still diverges from the recorded AWS response in two independent ways. That open issue does NOT carry this replay key (the replay reports owned_by empty for this cluster), so this is the residual, net-new divergence. Two causes, one shared fault: the managed-location reference table and the beacon host name derived from it. A fixer must close BOTH before submitting.

Cause 1 of 2 - the managed-location set omits Regions and Local Zones AWS publishes.
AWS: ListLocations with no Filters returns the full Amazon GameLift Servers service-location list for the account - every Region the service offers plus their Local Zones - each managed location carrying PingBeacon.UDPEndpoint {Domain, Port}. Smithy: ListLocations output Locations: LocationModelList, paginated (Limit range 1..200, inputToken/outputToken NextToken, items Locations); LocationModel members LocationName (LocationStringModel, len 1..64, pattern ^[A-Za-z0-9\-]+$), LocationArn ("assigned to a custom location resource", so absent for managed ones) and PingBeacon. The model does not enumerate the value set, so the membership itself is recorded behavior only: the recording's catalog contains locations the emulator's table does not carry at all, including the Regions mx-central-1, ap-southeast-7, eu-south-1 and eu-south-2 and the Local Zones us-east-1-iah-2, us-east-1-atl-2 and af-south-1-los-1.
Emulator: the catalog is a fixed 47-entry table that has no entry for any of those, so each recorded location is reported missing from the served list. The requirement is that the published service-location set is served, not one recording's exact row count or ordering (the response order is not asserted here).
Why: a client that enumerates locations to decide where a multi-location fleet may deploy, or that validates a user-supplied location against ListLocations before CreateFleet, concludes that a Region or Local Zone AWS does offer is unavailable - and a later CreateFleet/CreateGameSession for it is then never attempted, or is attempted and the client has no way to have known it was legal.
Fix: the ListLocations catalog source - the managed-location reference table the handler reads (state's managed-location table, re-exported as the AWS-filter source) must carry the Regions and Local Zones the service publishes, each with its beacon.
Traces: gamelift-catalog-list-locations#task#1.

Cause 2 of 2 - a Local Zone's PingBeacon.UDPEndpoint.Domain is built with the zone's numeric index instead of the bare city code.
AWS: a managed location's beacon host name is published per hosting location in the service's api.aws zone. For a Region the form is gamelift-ping.<region>.api.aws; for a Local Zone it is gamelift-ping-<city code>.<parent region>.api.aws, where <city code> is the Local Zone's city/airport code WITHOUT the trailing numeric index - so the zones of one city share one beacon. Recorded behavior only (Smithy types UDPEndpoint.Domain as NonZeroAndMaxString len 1..1024 and documents only "the domain name of the UDP endpoint"), and the recording shows exactly that rule: us-east-1-iah-2 -> gamelift-ping-iah.us-east-1.api.aws, us-east-1-atl-2 -> gamelift-ping-atl.us-east-1.api.aws, af-south-1-los-1 -> gamelift-ping-los.af-south-1.api.aws. Port is 7770, which the emulator already matches.
Emulator: it appends the WHOLE name remainder after the parent Region, numeric index included, so it serves gamelift-ping-<city>-<n>.<region>.api.aws - e.g. gamelift-ping-dfw-1.us-east-1.api.aws, gamelift-ping-dfw-2.us-east-1.api.aws, gamelift-ping-iah-1.us-east-1.api.aws, gamelift-ping-bos-1.us-east-1.api.aws, gamelift-ping-lim-1.us-east-1.api.aws. This is wrong for every Local Zone in the catalog, including the ones whose LocationName the emulator does get right, so it is a defect independent of Cause 1. The Region form (gamelift-ping.<region>.api.aws, e.g. gamelift-ping.eu-south-2.api.aws) is correct and must not change.
Why: a game client reads PingBeacon.UDPEndpoint to measure player latency to a hosting location. The emulator hands it a host name that is not the one AWS publishes, so latency probing resolves nothing (or the wrong beacon) and the latency-based placement a GameSessionQueue depends on is driven by garbage; a client that caches beacons per city also sees one beacon per zone where AWS publishes one per city.
Why these are not environment values: the host name is derived from the location name by a fixed rule, so the rule is the requirement even though the individual domains are reference data. No per-run value (host, port, id, timestamp, request id) is asserted by this cause.
Fix: the beacon-domain derivation used by the ListLocations serializer - strip the Local Zone's trailing numeric index before formatting gamelift-ping-<city>.<region>.api.aws, leaving the Region form untouched.
Traces: gamelift-catalog-list-locations#task#1.

## Evidence
Fixture gamelift-catalog-list-locations step task#1, ListLocations, expected HTTP 200 / actual HTTP 200 - body mismatch on the Locations list (cluster divergence_key service=gamelift|ListLocations|status=200->200, affected_count 1, owned_by empty, stub_path empty).

Cause 1 (expected location absent from the served list):
- $.Locations[11]: expected {'LocationName': 'mx-central-1', 'PingBeacon': {'UDPEndpoint': {'Domain': 'gamelift-ping.mx-central-1.api.aws', 'Port': 7770}}} - not served (emulator's list carries 'eu-central-1' there).
- $.Locations[23]: expected {'LocationName': 'ap-southeast-7', 'PingBeacon': {'UDPEndpoint': {'Domain': 'gamelift-ping.ap-southeast-7.api.aws', 'Port': 7770}}} - not served.
- $.Locations[27]: expected {'LocationName': 'us-east-1-iah-2', 'PingBeacon': {'UDPEndpoint': {'Domain': 'gamelift-ping-iah.us-east-1.api.aws', 'Port': 7770}}} - not served.
- $.Locations[28]: expected {'LocationName': 'us-east-1-atl-2', 'PingBeacon': {'UDPEndpoint': {'Domain': 'gamelift-ping-atl.us-east-1.api.aws', 'Port': 7770}}} - not served.
- $.Locations[29]: expected {'LocationName': 'eu-south-2', ... 'gamelift-ping.eu-south-2.api.aws'} - not served.
- $.Locations[30]: expected {'LocationName': 'af-south-1-los-1', ... 'gamelift-ping-los.af-south-1.api.aws'} - not served.
- $.Locations[38]: expected {'LocationName': 'eu-south-1', ... 'gamelift-ping.eu-south-1.api.aws'} - not served.

Cause 2 (beacon host name differs at indices whose LocationName is NOT itself reported absent, so the difference is the domain alone): the cluster reports PingBeacon.UDPEndpoint.Domain mismatches at $.Locations[12], [18], [22], [23], [24], [25], [26], [27], [28], [29], [30], [31], [32], [33], [34], [35], [36], [37], [38], [41], [42], [43], [44], [45] and [46]. Recorded Local Zone beacons use the bare city code (gamelift-ping-iah / -atl / -los); every Local Zone beacon the emulator serves carries the zone index (gamelift-ping-bos-1.us-east-1.api.aws, gamelift-ping-dfw-1.us-east-1.api.aws, gamelift-ping-dfw-2.us-east-1.api.aws, gamelift-ping-iah-1.us-east-1.api.aws, gamelift-ping-lim-1.us-east-1.api.aws). Port 7770 matches throughout; no error code or message is involved (status 200 on both sides).

## Steps to Reproduce
1. Replay the wire corpus simparity-wire-aws-v1 against the emulator.
2. Cause 1: call ListLocations with no Filters and no Limit, collect every LocationName, and compare the set against the recorded catalog for gamelift-catalog-list-locations#task#1 - mx-central-1, ap-southeast-7, eu-south-1, eu-south-2, us-east-1-iah-2, us-east-1-atl-2 and af-south-1-los-1 are absent from the emulator's response.
3. Cause 2: in the same response, read PingBeacon.UDPEndpoint.Domain for any Local Zone entry (e.g. us-east-1-atl-1, us-east-1-dfw-1, us-east-1-dfw-2, us-east-1-iah-1): the emulator returns gamelift-ping-<city>-<n>.<region>.api.aws where AWS publishes gamelift-ping-<city>.<region>.api.aws. Confirm the Region entries (e.g. eu-central-1) still return gamelift-ping.<region>.api.aws.
4. Re-replay gamelift-catalog-list-locations#task#1 and confirm BOTH causes are closed: the published location set is served AND every Local Zone beacon domain uses the bare city code.
