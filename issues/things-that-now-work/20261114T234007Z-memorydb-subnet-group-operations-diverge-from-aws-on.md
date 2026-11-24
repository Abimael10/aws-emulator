Issue-Source: trace-sweep-divergence
Issue-Trace: memorydb-subnet-group-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: memorydb-subnet-group-lifecycle memorydb-describe-maxresults-bounds
Issue-Shape-Id: com.amazonaws.memorydb#AmazonMemoryDB
Issue-Key: memorydb|CreateSubnetGroup|2xx->2xx|$.SubnetGroup.Subnets[*].SupportedNetworkTypes
Issue-Key: memorydb|CreateSubnetGroup|2xx->2xx|$.SubnetGroup.SupportedNetworkTypes
Issue-Key: memorydb|DescribeSubnetGroups|2xx->2xx|$.SubnetGroups[*].Subnets[*].SupportedNetworkTypes
Issue-Key: memorydb|DescribeSubnetGroups|2xx->2xx|$.SubnetGroups[*].SupportedNetworkTypes
Issue-Key: memorydb|UpdateSubnetGroup|2xx->2xx|$.SubnetGroup.Subnets[*]
Issue-Key: memorydb|UpdateSubnetGroup|2xx->2xx|$.SubnetGroup.SupportedNetworkTypes
Issue-Key: memorydb|DescribeSubnetGroups|2xx->2xx|$.SubnetGroups[*].Subnets[*]
Issue-Key: memorydb|CreateSubnetGroup|4xx->2xx|$.__type
Issue-Key: memorydb|CreateSubnetGroup|4xx->2xx|$.message
Issue-Key: memorydb|CreateSubnetGroup|4xx->2xx|status
Issue-Key: memorydb|DescribeSubnetGroups|4xx->4xx|$.__type
Issue-Key: memorydb|DescribeSubnetGroups|4xx->4xx|$.message
Issue-Commit: 5bc7b474c3edd4c3c4a63267d20aa9334a54ae6b

# memorydb subnet group operations diverge from AWS on network types, subnet validation and error shape

> _Note: this issue was identified against the code base at commit `5bc7b474c3edd4c3c4a63267d20aa9334a54ae6b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateSubnetGroup

## Description
Replaying the wire corpus against memorydb produced 6 divergence clusters across the subnet-group surface, which cluster into THREE distinct root causes. All three live in the memorydb subnet-group write/read path (its handlers, its subnet resolution and its `SubnetGroup` serialization). A fixer must close ALL THREE before submitting.

Note: the open issue `20261104T053017Z-memorydb-createsubnetgroup-stub.md` ("Implement memorydb CreateSubnetGroup: unimplemented scaffold stub") does NOT cover any of these: CreateSubnetGroup is now implemented and answers 200 in this replay, and that issue is about the 501 scaffold refusal only. Nothing in it addresses the three faults below.

Cause 1 of 3: `SupportedNetworkTypes` is never populated on `SubnetGroup` or on its `Subnets`. Fixture memorydb-subnet-group-lifecycle, step task#4 (CreateSubnetGroup); also task#7 and task#10 (DescribeSubnetGroups) and task#9 (UpdateSubnetGroup).
  AWS: Smithy (com.amazonaws.memorydb) — `SubnetGroup$SupportedNetworkTypes` targets `NetworkTypeList` (member enum `NetworkType`, values `ipv4`, `ipv6`, `dual_stack`), documented as "The network types supported by this subnet group ... an array of strings that can include 'ipv4', 'ipv6', or both"; `Subnet$SupportedNetworkTypes` targets the same list, documented as the network types supported by that subnet. AWS returns the member populated on every `SubnetGroup` and on every element of its `Subnets` list, for CreateSubnetGroup, UpdateSubnetGroup and DescribeSubnetGroups alike — for an IPv4-only subnet, the single-element list `["ipv4"]`. The value is derived from the IP address types the underlying VPC subnet supports, not from the request.
  Emulator: HTTP 200, but `$.SubnetGroup.SupportedNetworkTypes` and `$.SubnetGroup.Subnets[*].SupportedNetworkTypes` are absent (null) on CreateSubnetGroup (task#4) and UpdateSubnetGroup (task#9), and `$.SubnetGroups[0].SupportedNetworkTypes` / `$.SubnetGroups[0].Subnets[*].SupportedNetworkTypes` are absent on DescribeSubnetGroups (task#7, task#10). Every other member of the group and of each subnet (Identifier, AvailabilityZone.Name) is serialized correctly, so only this member is missing.
  Why: a client that selects a subnet group by addressing capability — e.g. refusing to place a dual-stack or IPv6 cluster in a group whose subnets are IPv4-only, or rendering the group's network types in a console/IaC plan — reads an empty/absent list and either treats every group as supporting nothing or crashes dereferencing the list. SDKs that model the member as a non-optional list of enums surface it as empty rather than `[ipv4]`, silently inverting the capability check.
  Fix: the memorydb subnet-group state projection and the subnet resolution it reads — `StoredSubnetGroup`/`StoredSubnet` must carry the supported network types resolved for each subnet and the group, and `to_api()` must serialize them onto both `SubnetGroup$SupportedNetworkTypes` and `Subnet$SupportedNetworkTypes`, so all three of CreateSubnetGroup, UpdateSubnetGroup and DescribeSubnetGroups report them from one place.
  Traces: memorydb-subnet-group-lifecycle#task#4, #task#7, #task#9, #task#10.

Cause 2 of 3: CreateSubnetGroup accepts subnet ids that name no existing subnet. Fixture memorydb-subnet-group-lifecycle, step task#6.
  AWS: a well-formed but nonexistent subnet id is refused, not persisted. The recorded response is HTTP 400 with error `InvalidParameterValueException` (Smithy: `com.amazonaws.memorydb#InvalidParameterValueException`, client error, httpError 400, awsQueryError code `InvalidParameterValue`, documented "The specified parameter value is not valid.") and message `The provided input subnet ids are invalid : [<subnet-ids>].` — fixed text `The provided input subnet ids are invalid : [` + `].`, with the bracketed part being the offending subnet identifiers taken verbatim from the request's `SubnetIds` (in the recording, a single id of EC2 form that no subnet exists for). Note AWS uses `InvalidParameterValueException` here rather than the operation's modeled `InvalidSubnet`, so the thrown error type matters as much as the status.
  Emulator: HTTP 200 with a fully populated `SubnetGroup` — the group is created and persisted over subnet ids that resolve to nothing. No `__type` and no `message` are emitted. The handler does check the EC2 *form* of each id (`state::subnet_id_rejection`) and does call `state::subnet_placement::resolve_subnets`, but that resolution does not refuse an id that is syntactically valid and simply does not exist, so it falls through to the insert.
  Why: a client gets a success and a subnet group ARN for a group that can never place a node. The failure surfaces far later, at CreateCluster time or not at all, so test suites and IaC runs that assert "bad subnet ids are rejected at subnet-group creation" pass against the emulator and fail against AWS; and emulator state now holds a group AWS would never have created, corrupting every later Describe.
  Fix: the CreateSubnetGroup handler's subnet validation — subnet resolution must treat "no such subnet" as a refusal (alongside the existing Invalid/NotAllowed refusals) and the handler must answer it as `InvalidParameterValueException` (HTTP 400) with the message above, before anything is written to state. UpdateSubnetGroup's subnet replacement needs the same rule.
  Traces: memorydb-subnet-group-lifecycle#task#6.

Cause 3 of 3: DescribeSubnetGroups raises the wrong error type and wrong message for an out-of-range `MaxResults`. Fixture memorydb-describe-maxresults-bounds, step task#11.
  AWS: an out-of-bounds `MaxResults` is refused with HTTP 400 and error `InvalidParameterValueException` (Smithy: `com.amazonaws.memorydb#InvalidParameterValueException`, httpError 400) and message `Invalid value for max records. Must be between 20 and 100` — all of it fixed text for this error class except the two bounds, which are the service's documented MaxResults range. The memorydb model declares NO `ValidationException` shape at all (closest shapes are `InvalidParameterCombinationException`, `InvalidCredentialsException`, `InvalidParameterValueException`), so `ValidationException` is not a wire-valid error for this service.
  Emulator: HTTP 400 (status correct) but `$.__type` is `ValidationException` instead of `InvalidParameterValueException`, and `$.message` is `Invalid value for max results. Must be between 20 and 100` instead of `Invalid value for max records. Must be between 20 and 100` — AWS words the bounded quantity as "max records", not "max results".
  Why: SDK error handling dispatches on the error code, so a client catching `InvalidParameterValueException` (the only code the memorydb model exposes for a bad parameter value) never matches and the failure escapes as an unmodeled/unknown error; retry and error-classification logic keyed to the modeled error never fires. Code or operators matching the documented message text also miss it.
  Fix: the DescribeSubnetGroups handler's MaxResults validation — raise the modeled `InvalidParameterValueException` rather than a generic validation error, and use AWS's wording ("max records") for the message. Any shared MaxResults-bounds helper used by the other memorydb paginated describes must be corrected in the same place so they do not emit `ValidationException` either.
  Traces: memorydb-describe-maxresults-bounds#task#11.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, score 76.4%, 6 divergence clusters — all memorydb, none with stub_path or owned_by).

Cause 1 — SupportedNetworkTypes absent:
- memorydb-subnet-group-lifecycle#task#4 (CreateSubnetGroup), expected 200 / actual 200: `$.SubnetGroup.Subnets[0].SupportedNetworkTypes: missing (expected ['ipv4'], got None)`; `$.SubnetGroup.SupportedNetworkTypes: missing (expected ['ipv4'], got None)`.
- memorydb-subnet-group-lifecycle#task#7 (DescribeSubnetGroups), 200/200: `$.SubnetGroups[0].Subnets[0].SupportedNetworkTypes: missing (expected ['ipv4'], got None)`; `$.SubnetGroups[0].SupportedNetworkTypes: missing (expected ['ipv4'], got None)`.
- memorydb-subnet-group-lifecycle#task#9 (UpdateSubnetGroup), 200/200: each element of `$.SubnetGroup.Subnets` expected `{'AvailabilityZone': {'Name': ...}, 'Identifier': ..., 'SupportedNetworkTypes': ['ipv4']}` but got the same object WITHOUT `SupportedNetworkTypes`; `$.SubnetGroup.SupportedNetworkTypes: missing (expected ['ipv4'], got None)`.
- memorydb-subnet-group-lifecycle#task#10 (DescribeSubnetGroups), 200/200: same omission on `$.SubnetGroups[0].Subnets[*]` and `$.SubnetGroups[0].SupportedNetworkTypes`.
(The subnet identifiers and availability-zone names in those records are per-run values from the recording and are not themselves the requirement; the missing member is.)

Cause 2 — unknown subnet ids accepted:
- memorydb-subnet-group-lifecycle#task#6 (CreateSubnetGroup), expected 400, actual 200: `$.__type: missing (expected 'InvalidParameterValueException', got None)`; `$.message: missing (expected 'The provided input subnet ids are invalid : [<subnet-id from the request>].', got None)`. The emulator instead answered 200 and persisted the group.

Cause 3 — wrong error type/message on MaxResults bounds:
- memorydb-describe-maxresults-bounds#task#11 (DescribeSubnetGroups), expected 400, actual 400: `$.__type: value_mismatch (expected 'InvalidParameterValueException', got 'ValidationException')`; `$.message: value_mismatch (expected 'Invalid value for max records. Must be between 20 and 100', got 'Invalid value for max results. Must be between 20 and 100')`.

Model evidence (smithy_query on services/service-com-amazonaws-memorydb-amazonmemorydb/smithy-model/com-amazonaws-memorydb-amazonmemorydb.json):
- `SubnetGroup$SupportedNetworkTypes` -> `NetworkTypeList` of enum `NetworkType` {ipv4, ipv6, dual_stack}; `Subnet$SupportedNetworkTypes` -> same list.
- `InvalidParameterValueException`: client error, httpError 400, awsQueryError code `InvalidParameterValue`.
- `ValidationException`: not found in the memorydb model.
- CreateSubnetGroup / UpdateSubnetGroup / DescribeSubnetGroups all return `SubnetGroup`(s); DescribeSubnetGroups is paginated with pageSize `MaxResults`.
Handler context: services/service-com-amazonaws-memorydb-amazonmemorydb/src/ops/create_subnet_group.rs lines 199-258 validate subnet id form and call `state::subnet_placement::resolve_subnets`, whose refusal set covers only Invalid/NotAllowed; lines 291-320 build `StoredSubnetGroup` and serialize via `to_api()`, which emits no network types.

## Steps to Reproduce
1. Clone the repo and boot the memorydb emulator built from this checkout.
2. Replay fixture `memorydb-subnet-group-lifecycle` and inspect steps task#4, task#7, task#9, task#10: CreateSubnetGroup / DescribeSubnetGroups / UpdateSubnetGroup all answer 200 with `SupportedNetworkTypes` absent on both the subnet group and each of its subnets, where AWS returns `["ipv4"]` for IPv4-only subnets (Cause 1).
3. In the same fixture, inspect step task#6: CreateSubnetGroup is called with a well-formed subnet id that names no existing subnet. The emulator answers 200 and persists the group; AWS answers 400 `InvalidParameterValueException` with message `The provided input subnet ids are invalid : [<the offending SubnetIds>].` (Cause 2).
4. Replay fixture `memorydb-describe-maxresults-bounds` and inspect step task#11: DescribeSubnetGroups with `MaxResults` outside 20..100 returns 400 `ValidationException` / `Invalid value for max results. Must be between 20 and 100`, where AWS returns 400 `InvalidParameterValueException` / `Invalid value for max records. Must be between 20 and 100` (Cause 3).
5. Re-replay both fixtures after fixing: all 6 clusters must pass.
