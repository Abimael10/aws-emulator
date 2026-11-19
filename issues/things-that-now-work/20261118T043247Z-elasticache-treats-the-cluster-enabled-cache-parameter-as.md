Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-cluster-enabled-parameter-validation
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-cluster-enabled-parameter-validation
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|ModifyCacheParameterGroup|2xx->4xx|$.ModifyCacheParameterGroupResponse
Issue-Key: elasticache|ModifyCacheParameterGroup|2xx->4xx|status
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters
Issue-Key: elasticache|ModifyCacheParameterGroup|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: elasticache|ResetCacheParameterGroup|2xx->4xx|$.ResetCacheParameterGroupResponse
Issue-Key: elasticache|ResetCacheParameterGroup|2xx->4xx|status
Issue-Commit: 5c103ae721112cfc59443e8c0b52708204a7cb08

# ElastiCache treats the cluster-enabled cache parameter as unmodifiable, so modify and reset are refused and the user-source read is empty

> _Note: this issue was identified against the code base at commit `5c103ae721112cfc59443e8c0b52708204a7cb08`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyCacheParameterGroup

## Description
Four net-new causes, one shared root cause: the emulator's cache-parameter-group state treats 'cluster-enabled' as a non-modifiable parameter, so a valid modify is refused, the reset of that parameter is refused, the invalid-value refusal carries the 'cannot be modified' message instead of AWS's allowed-values message, and the parameter never appears in the user-source read-back.

Cause 1 of 4: a valid ModifyCacheParameterGroup setting cluster-enabled is refused.
AWS: Smithy ModifyCacheParameterGroup takes CacheParameterGroupName + ParameterNameValues and returns CacheParameterGroupNameMessage {CacheParameterGroupName} with HTTP 200. 'cluster-enabled' is a modifiable parameter of a Redis 7 cache parameter group with allowed values yes,no and ChangeType requires-reboot, so setting it to an allowed value succeeds and the response echoes the group name.
Emulator: HTTP 400 with 'The parameter cluster-enabled cannot be modified.' and no ModifyCacheParameterGroupResponse at all.
Why: a client cannot enable cluster mode on a parameter group, so the whole cluster-mode configuration path is unreachable; the modeled success shape never arrives.
Fix: the cache-parameter-group state's parameter catalog/modifiability flags plus the ModifyCacheParameterGroup handler — cluster-enabled must be modifiable with allowed values yes,no.
Traces: elasticache-cluster-enabled-parameter-validation#task#4.

Cause 2 of 4: DescribeCacheParameters with Source=user returns no parameters after a modify.
AWS: DescribeCacheParameters returns CacheParameterGroupDetails {Parameters, CacheNodeTypeSpecificParameters, Marker}; a parameter the caller has set is projected under Source 'user' with its catalog metadata — ParameterName 'cluster-enabled', the value the caller set, AllowedValues 'yes,no', DataType 'string', ChangeType 'requires-reboot', IsModifiable 'true', Description 'Enable cluster mode', MinimumEngineVersion for the group's engine family.
Emulator: Parameters is empty ('') — the user-set parameter is not recorded or not projected.
Why: a client that reads back its own parameter settings (the normal way to confirm a modify took) sees an empty list and concludes nothing was set.
Fix: the cache-parameter-group state (persist user-set parameter values with Source 'user') and the DescribeCacheParameters Source filter projection.
Traces: elasticache-cluster-enabled-parameter-validation#task#5.

Cause 3 of 4: an out-of-range cluster-enabled value is refused with the wrong message.
AWS: HTTP 400 InvalidParameterValue (ModifyCacheParameterGroup models InvalidParameterValueException, awsQueryError code InvalidParameterValue) with the fixed text 'invalid parameter value, allowed values are:yes,no' — the allowed-values list comes from the parameter's catalog entry.
Emulator: HTTP 400 with 'The parameter cluster-enabled cannot be modified.' — it refuses on modifiability instead of on the value, so the message names the wrong rule.
Why: the client is told the parameter can never be changed rather than which values are accepted, so a correcting retry is never attempted.
Fix: the ModifyCacheParameterGroup handler's per-parameter value validation — refuse out-of-range values with AWS's 'invalid parameter value, allowed values are:<AllowedValues>' text once cluster-enabled is modifiable.
Traces: elasticache-cluster-enabled-parameter-validation#task#6.

Cause 4 of 4: ResetCacheParameterGroup for cluster-enabled is refused.
AWS: ResetCacheParameterGroup returns CacheParameterGroupNameMessage {CacheParameterGroupName} with HTTP 200 when resetting a user-set parameter back to its engine default.
Emulator: HTTP 400 and no ResetCacheParameterGroupResponse body.
Why: a client cannot roll a parameter group back to defaults, so the modify/verify/reset cycle cannot complete.
Fix: the ResetCacheParameterGroup handler plus the same modifiability data in cache-parameter-group state.
Traces: elasticache-cluster-enabled-parameter-validation#task#8.

No open issue owns these steps (replay reported owned_by empty and stub_path empty for all four clusters). The nearest open issues — 20261030T130325Z-elasticache-parameter-catalog-is-incomplete-describecacheparameters-omits-cachenodetypespecificparameters and 20261116T175951Z-elasticache-reference-catalogs-are-emulator-authored-engine-defaults — concern the reference catalog contents, not the modifiability of cluster-enabled or the user-source read-back.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture elasticache-cluster-enabled-parameter-validation, service=elasticache, all verdict=mismatch, owned_by empty, stub_path empty.

1) task#4 ModifyCacheParameterGroup: expected_status 200, actual_status 400. detail: 'status: expected 200, got 400' and '$.ModifyCacheParameterGroupResponse: missing (expected {ModifyCacheParameterGroupResult: {CacheParameterGroupName: <group>}, ResponseMetadata: {RequestId: ...}}, got None)'. Cluster also covers task#10.
2) task#5 DescribeCacheParameters (Source=user): '$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters: type_mismatch (expected {Parameter: {AllowedValues: "yes,no", MinimumEngineVersion: "7.0.0", DataType: "string", ChangeType: "requires-reboot", Description: "Enable cluster mode", Source: "user", ParameterName: "cluster-enabled", IsModifiable: "true", ParameterValue: "yes"}}, got "")'. Cluster also covers task#7, task#11, task#13.
3) task#6 ModifyCacheParameterGroup (bad value), 400->400: '$.ErrorResponse.Error.Message: value_mismatch (expected "invalid parameter value, allowed values are:yes,no", got "The parameter cluster-enabled cannot be modified.")'. Cluster also covers task#12.
4) task#8 ResetCacheParameterGroup: expected_status 200, actual_status 400; '$.ResetCacheParameterGroupResponse: missing (expected {ResetCacheParameterGroupResult: {CacheParameterGroupName: <group>}, ResponseMetadata: {RequestId: ...}}, got None)'. Cluster also covers task#14.

(The group name and RequestId in the expected bodies are per-run values; the requirements are the 200 status with the echoed CacheParameterGroupName, the user-source projection of the parameter, and the allowed-values refusal text.)

## Steps to Reproduce
1. Clone the repo, boot the emulator, replay the bound wire corpus.
2. Fixture elasticache-cluster-enabled-parameter-validation: create a Redis 7 cache parameter group, then task#4 — ModifyCacheParameterGroup with ParameterNameValues cluster-enabled=yes. Expect HTTP 200 with the group name echoed; the emulator answers 400 'The parameter cluster-enabled cannot be modified.'
3. task#5 — DescribeCacheParameters on that group with Source=user. Expect the cluster-enabled parameter with Source 'user' and the value just set; the emulator returns an empty Parameters list.
4. task#6 — ModifyCacheParameterGroup with cluster-enabled set to a value outside yes,no. Expect 400 InvalidParameterValue 'invalid parameter value, allowed values are:yes,no'.
5. task#8 — ResetCacheParameterGroup for cluster-enabled. Expect HTTP 200 with the group name echoed; the emulator answers 400.
