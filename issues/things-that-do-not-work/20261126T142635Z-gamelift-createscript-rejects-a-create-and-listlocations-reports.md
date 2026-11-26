Issue-Source: trace-sweep-divergence
Issue-Trace: gamelift-script-tagging-roundtrip
Issue-Step: setup#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: gamelift-script-tagging-roundtrip gamelift-catalog-list-locations
Issue-Shape-Id: com.amazonaws.gamelift#GameLift
Issue-Key: gamelift|CreateScript|2xx->4xx|$.Script
Issue-Key: gamelift|CreateScript|2xx->4xx|status
Issue-Key: gamelift|ListLocations|2xx->2xx|$.Locations[*].PingBeacon.UDPEndpoint.Domain
Issue-Commit: a650362e0e15805c1963e4149e23a73be0a73a90

# GameLift CreateScript rejects a create and ListLocations reports wrong per-position ping beacons

> _Note: this issue was identified against the code base at commit `a650362e0e15805c1963e4149e23a73be0a73a90`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateScript

## Description
Two net-new wire divergences in the emulator's GameLift service (2 causes; both must be closed). Neither is covered by the open gamelift issues 20261029T183132Z-gamelift-responses-omit-modeled-fields-matchmaking-defaults-terminationtime.md or 20261101T221208Z-gamelift-upload-credentials-accesskeyid-is-not-an-aws.md, which own the DescribeFleetAttributes/CreateAlias and UploadCredentials clusters respectively.

Cause 1 of 2: CreateScript refuses a create that AWS accepts (fixture gamelift-script-tagging-roundtrip, step setup#1).
AWS: Smithy model com.amazonaws.gamelift#CreateScript — input members Name, Version, StorageLocation (S3Location), ZipFile (ZipBlob, len 0..5000000), Tags, NodeJsVersion (NodeJsVersion, len 0..16, pattern ^[0-9]+\.(?:x|[0-9]+)$); output Script; errors ConflictException/InvalidRequestException/TaggingFailedException/UnauthorizedException (all HTTP 400) and InternalServiceException (500). NO input member is @required, and NodeJsVersion's own doc states the service's default: "The valid values are 10.x | 24.x. By default, NodeJsVersion is 10.x." The recorded step is a plain script create that provides the zip file locally (the recorded response carries a service-owned StorageLocation and a non-zero SizeOnDisk, i.e. the "If the script file is provided as a local file, the file is uploaded to an Amazon GameLift Servers-owned S3 bucket" path) and AWS answers HTTP 200 with a complete Script: ScriptId (^script-\S+$), ScriptArn (^arn:.*:script\/script-\S+$), Name, Version, SizeOnDisk, CreationTime, StorageLocation{Bucket,Key} and NodeJsVersion "10.x" — the DEFAULT applied by the service, not a value echoed from a required input.
Emulator: answers HTTP 400 and no Script member at all, so the script is never created and the whole tagging round-trip behind it cannot run.
Hypothesis: the only refusal path reachable for a ZipFile-only create whose recorded response carries the default "10.x" is the handler's required-NodeJsVersion gate (services/service-com-amazonaws-gamelift-gamelift/src/ops/create_script.rs, the `let Some(node_js_version) = input.node_js_version.clone() else { return Err(invalid(NODE_JS_VERSION_REQUIRED_MESSAGE)) }` branch, whose message text lives in src/state.rs as NODE_JS_VERSION_REQUIRED_MESSAGE: "The NodeJsVersion parameter is required. See the GameLift API Reference for CreateScript() for a list of supported runtime versions."). That constant's own comment concedes the member documents a default and asserts the service applies none; this wire recording shows the direct CreateScript API DOES apply it. The requirement: a CreateScript that names no NodeJsVersion is accepted and the created script records NodeJsVersion "10.x"; InvalidRequestException (HTTP 400, error code InvalidRequestException, message "One or more parameter values in the request are invalid..."-class wording with the offending member named) stays reserved for genuinely invalid input (e.g. a NodeJsVersion outside 10.x | 24.x, or neither/both of ZipFile and StorageLocation). Only the fixed text of such a message is a requirement; the script id, ARN, timestamp, bucket and key in the recording are per-run values that fill the response template, not literals to reproduce.
Why: a client that creates a Realtime script without pinning a runtime version — the documented default path, and what the CLI/SDK sends when --node-js-version is omitted — gets a 400 instead of a script. Every downstream call (TagResource/ListTagsForResource/DescribeScript/CreateFleet on that script) then fails for want of a ScriptId, so the emulator cannot run a scripted-Realtime flow at all.
Fix: CreateScript's input validation and defaulting in its handler (services/service-com-amazonaws-gamelift-gamelift/src/ops/create_script.rs) plus the NODE_JS_VERSION_REQUIRED_MESSAGE rationale in src/state.rs: default the member to 10.x instead of refusing, keeping the catalog check for a version that is present and unsupported.
Traces: gamelift-script-tagging-roundtrip#setup#1 (expected 200 with $.Script, got 400 with $.Script missing).

Cause 2 of 2: ListLocations' AWS-managed location catalog reports a different location at each list position than AWS does, so the PingBeacon domain at those positions names the wrong hosting location (fixture gamelift-catalog-list-locations, step task#1).
AWS: Smithy model com.amazonaws.gamelift#ListLocations — output Locations (LocationModelList), paginated with inputToken/outputToken NextToken, items Locations, pageSize Limit; LocationModel carries LocationName, LocationArn and PingBeacon, and PingBeacon.UDPEndpoint has Domain (NonZeroAndMaxString) and Port ("for Amazon GameLift Servers ping beacons, this is typically port 7770"). The value set of the AWS-managed catalog and its order are not in the model — recorded behavior only: the recording shows AWS returning the published service-location list in ITS OWN order, with each entry's beacon domain derived from that entry's location (gamelift-ping.<region>.api.aws for a Region, gamelift-ping-<city code>.<parent region>.api.aws for a Local Zone). In the recorded page, positions 41..45 carry the beacons gamelift-ping.ap-northeast-1.api.aws, gamelift-ping.ap-northeast-2.api.aws, gamelift-ping-nyc.us-east-1.api.aws, gamelift-ping.ap-south-2.api.aws and gamelift-ping.us-east-1.api.aws.
Emulator: at the same positions it serves gamelift-ping.us-west-2.api.aws, gamelift-ping-den.us-west-2.api.aws, gamelift-ping-lax.us-west-2.api.aws, gamelift-ping-phx.us-west-2.api.aws and gamelift-ping-phx.us-west-2.api.aws — the tail of a catalog table sorted by location name (services/service-com-amazonaws-gamelift-gamelift/src/state/managed_locations.rs, MANAGED_LOCATIONS, emitted in table order by ListLocations' handler). The domain FORMAT is right; the catalog's membership/ordering is not, so the Nth entry describes a different Region or Local Zone than AWS's Nth entry does. These are AWS's own published reference-data host names, not the emulator's listening address, so they are part of the response contract rather than an environment-specific value.
Why: a client reads this catalog positionally and by page — it picks latency-probe targets from it, and a paginated walk with Limit/NextToken slices it at fixed offsets. With a different ordering the client probes the wrong beacon for a location, caches a beacon under the wrong location name, and a page boundary falls in a different place than against AWS, so multi-page catalog walks and latency-based fleet placement diverge.
Fix: the managed-location catalog state (services/service-com-amazonaws-gamelift-gamelift/src/state/managed_locations.rs) — the catalog's contents and its emission order under the AWS filter — and the ordering ListLocations' handler pages over (src/ops/list_locations.rs); the per-location domain derivation (ping_beacon_domain) already matches AWS's form and needs no change.
Traces: gamelift-catalog-list-locations#task#1 ($.Locations[41..45].PingBeacon.UDPEndpoint.Domain, verdict host_not_own).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200; 213 steps, 158 passed, score 74.2%).

Cause 1 — fixture gamelift-script-tagging-roundtrip, step setup#1, operation CreateScript, verdict mismatch:
  expected_status 200, actual_status 400
  detail: "status: expected 200, got 400" and "$.Script: missing (expected {'CreationTime': ..., 'Name': 'sim2real-probe-gamelift-script-tag-...', 'NodeJsVersion': '10.x', 'ScriptArn': '{{script_arn}}', 'ScriptId': '{{script_id}}', 'SizeOnDisk': 146, 'StorageLocation': {'Bucket': 'prod-gamescale-scripts-us-west-2', 'Key': '000000000000/{{script_id}}'}, 'Version': '1.0.0'}, got None)"
  Expected: HTTP 200 with a Script whose NodeJsVersion is the service default 10.x (names/ids/timestamps in the recording are per-run values).
  Actual: HTTP 400, no Script; the handler's InvalidRequestException path fires for the absent NodeJsVersion.

Cause 2 — fixture gamelift-catalog-list-locations, step task#1, operation ListLocations, verdict mismatch (status 200 both ways):
  $.Locations[41].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping.ap-northeast-1.api.aws', got 'gamelift-ping.us-west-2.api.aws'
  $.Locations[42].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping.ap-northeast-2.api.aws', got 'gamelift-ping-den.us-west-2.api.aws'
  $.Locations[43].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping-nyc.us-east-1.api.aws', got 'gamelift-ping-lax.us-west-2.api.aws'
  $.Locations[44].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping.ap-south-2.api.aws', got 'gamelift-ping-phx.us-west-2.api.aws'
  $.Locations[45].PingBeacon.UDPEndpoint.Domain: expected 'gamelift-ping.us-east-1.api.aws', got 'gamelift-ping-phx.us-west-2.api.aws'
  The emulator's values are exactly the last five entries of MANAGED_LOCATIONS in name-sorted order (us-west-2, us-west-2-den-1, us-west-2-lax-1, us-west-2-phx-1, us-west-2-phx-2), confirming the catalog order rather than the domain formatter is wrong.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1 (replay_wire_corpus).
2. Cause 1: replay fixture gamelift-script-tagging-roundtrip, step setup#1 — POST a GameLift CreateScript (X-Amz-Target GameLift.CreateScript) with Name, Version '1.0.0', a ZipFile and Tags, and NO NodeJsVersion member. Observe HTTP 400 InvalidRequestException ("The NodeJsVersion parameter is required...") instead of HTTP 200 with a Script whose NodeJsVersion is '10.x'; every later step of that fixture (tag/list-tags/describe on the script) is then unreachable.
3. Cause 2: replay fixture gamelift-catalog-list-locations, step task#1 — POST GameLift.ListLocations with no Filters and read $.Locations[41..45].PingBeacon.UDPEndpoint.Domain. Observe the us-west-2 Region/Local-Zone beacons where the recorded response has gamelift-ping.ap-northeast-1 / ap-northeast-2 / -nyc.us-east-1 / ap-south-2 / us-east-1, i.e. the Nth catalog entry is a different location than AWS's Nth.
