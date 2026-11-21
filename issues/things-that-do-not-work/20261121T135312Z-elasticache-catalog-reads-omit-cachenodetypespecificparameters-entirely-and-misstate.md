Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-catalog-describe-cache-parameters
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-catalog-describe-cache-parameters
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.CacheNodeTypeSpecificParameters.CacheNodeTypeSpecificParameter[*]
Issue-Related: 20261121T135312Z-elasticache-describecacheparameters-on-a-custom-parameter-group-serves.md
Issue-Commit: 8cbf0dcd0020846f13fa76c0319ab04fca2e29a2

# ElastiCache catalog reads omit CacheNodeTypeSpecificParameters entirely and misstate every family's Parameters set (9 causes)

> _Note: this issue was identified against the code base at commit `8cbf0dcd0020846f13fa76c0319ab04fca2e29a2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCacheParameters

## Description
NINE net-new divergence clusters in the elasticache crate, one per engine family the catalog fixture walks. A fixer must close NINE causes before submitting; they share one root cause in the seeded parameter catalog, so each cause is the same defect observed on a different family and each must be re-replayed.

AWS: Smithy (services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json) DescribeCacheParameters(CacheParameterGroupName clientOptional/required, Source, MaxRecords, Marker) -> CacheParameterGroupDetails{Marker, Parameters: ParametersList, CacheNodeTypeSpecificParameters: CacheNodeTypeSpecificParametersList}; errors CacheParameterGroupNotFoundFault (404/CacheParameterGroupNotFound), InvalidParameterValueException (400/InvalidParameterValue), InvalidParameterCombinationException (400/InvalidParameterCombination). Shape CacheNodeTypeSpecificParameter{ParameterName, Description, Source, DataType, AllowedValues, IsModifiable, MinimumEngineVersion, CacheNodeTypeSpecificValues: list<CacheNodeTypeSpecificValue{CacheNodeType, Value}>, ChangeType}, documented as 'A parameter that has a different value for each cache node type it is applied to'; CacheParameterGroupDetails documents CacheNodeTypeSpecificParameters as 'A list of parameters specific to a particular cache node type'. The model fixes the shape, not the catalog CONTENT, so content is recorded behavior only: for a default parameter group of any published family, AWS answers 200 with (a) that family's complete Parameters catalog and (b) a NON-EMPTY CacheNodeTypeSpecificParameters list whose rows carry the family's MinimumEngineVersion, the parameter's AllowedValues, and one CacheNodeTypeSpecificValue per supported cache node type (the recordings show rows with MinimumEngineVersion '1.4.5', '5.0.0', '6.0.5', '7.0.0' and '9.0.0', AllowedValues '1-100000' / '0-', and per-node-type values for cache.m4/m5/m6g/m7g/r4/c7gn/c8gn/caspianr1g sizes). Both lists are catalog facts, identical on every call.

Emulator (all nine causes, identical fault): HTTP 200 (status agrees) but CacheNodeTypeSpecificParameters is absent from the body altogether - every expected CacheNodeTypeSpecificParameter element is reported 'missing (expected {...}, got None)' - and the Parameters list does not agree with the family's catalog either: whole Parameter[i] elements are reported missing across the list's entire index range, so the emulator's row set and row order are its own rather than the family's.

Cause 1 of 9: the memcached family read (MinimumEngineVersion '1.4.5' in the recording) - fixture elasticache-catalog-describe-cache-parameters, step task#1 (the cluster also covers task#2 and task#3). Expected CacheNodeTypeSpecificParameter[0] and [1] (e.g. a row with AllowedValues '1-100000' and per-node-type values such as cache.m4.large '6573', cache.m5.xlarge '13248') are missing entirely, and Parameter[0]..Parameter[29] are reported missing as whole elements.
Cause 2 of 9: the next family read in the same fixture - step task#4. Same fault: CacheNodeTypeSpecificParameter[0] missing, Parameters rows missing/divergent.
Cause 3 of 9: step task#5. Same fault.
Cause 4 of 9: step task#6. Same fault.
Cause 5 of 9: step task#8. Same fault.
Cause 6 of 9: the Redis 5.0.0 family read - step task#10 (cluster also covers task#11). CacheNodeTypeSpecificParameter[0]..[2] missing (the recording's row 0 has AllowedValues '0-', MinimumEngineVersion '5.0.0' and a value per cache node type, e.g. cache.m5.large '685454275'), Parameter[0]..[56] missing as whole elements.
Cause 7 of 9: the 6.0.5 family read - step task#12 (cluster also covers task#13). CacheNodeTypeSpecificParameter[0]..[2] missing (row 0 MinimumEngineVersion '6.0.5'), Parameter[0]..[61] missing.
Cause 8 of 9: the 7.0.0 family read - step task#14 (cluster also covers task#15..task#19). CacheNodeTypeSpecificParameter[0]..[3] missing (row 0 MinimumEngineVersion '7.0.0'), Parameter[0]..[66] missing.
Cause 9 of 9: the 9.0.0 (Valkey) family read - step task#20 (cluster also covers task#21). CacheNodeTypeSpecificParameter[0]..[3] missing (row 0 MinimumEngineVersion '9.0.0', including cache.c8gn.* node-type values), Parameter[0]..[67] missing.

Why: CacheNodeTypeSpecificParameters is the only place AWS publishes a parameter's value per cache node type - it is how a client learns the maxmemory-class default for the node type it is about to launch, and how sizing and capacity tooling computes a budget before creating a cluster. Serving the member not at all makes that computation impossible against the emulator (a client reading it gets nothing where AWS always returns rows), while the divergent Parameters set means even the non-node-specific catalog cannot be diffed against AWS: a parameter AWS publishes for the family is absent or sits at another index with another row's metadata. Any client that enumerates a family's tunables, validates a value against AllowedValues, or renders the catalog therefore behaves differently against the emulator than against AWS - and it does so for every engine family, including the current Valkey one.

Fix: the elasticache crate's seeded cache-parameter reference catalog (the per-family parameter metadata, which must include the node-type-specific rows and their per-node-type values) and the DescribeCacheParameters handler's response projection, which must serialize CacheNodeTypeSpecificParameters from that catalog alongside Parameters instead of emitting Parameters only.

Traces: elasticache-catalog-describe-cache-parameters#task#1 (also #task#2, #task#3), #task#4, #task#5, #task#6, #task#8, #task#10 (also #task#11), #task#12 (also #task#13), #task#14 (also #task#15 .. #task#19), #task#20 (also #task#21).

Related issues that do NOT cover these clusters (replay reports owned_by empty and stub_path empty for all nine): 20261116T175951Z-elasticache-reference-catalogs-are-emulator-authored-engine-defaults.md is open and owns the DescribeEngineDefaultParameters cluster of fixture elasticache-cache-parameter-group-workflow, carrying none of these replay keys; 20261119T140017Z-elasticache-describecacheparameters-serves-emulator-authored-metadata-for-cluster.md is open but is about one parameter's Description/MinimumEngineVersion in fixture elasticache-cluster-enabled-parameter-validation. 20261030T130325Z-elasticache-parameter-catalog-is-incomplete-describecacheparameters-omits-cachenodetypespecificparameters.md describes this same shape of fault but is RESOLVED, so it covers nothing here - the fault is present again (or still) on every family in this replay.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true ('/__admin/health answered 200 and the :44689 data listener accepts'), 35 fixtures replayed, 333 steps total, 282 passed, 51 mismatched, 0 errored, 0 blocked, score 84.7%, bootstrap_signal=false, 28 divergence clusters (all service=elasticache); 11 net-new, of which these nine (fixture elasticache-catalog-describe-cache-parameters, operation DescribeCacheParameters, verdict mismatch, expected_status 200, actual_status 200, stub_path empty, owned_by empty) are this issue's:

- step task#1 (affected_count 3: task#1, task#2, task#3). detail begins: '$.DescribeCacheParametersResponse.DescribeCacheParametersResult.CacheNodeTypeSpecificParameters.CacheNodeTypeSpecificParameter[0]: missing (expected {\'AllowedValues\': \'1-100000\', \'MinimumEngineVersion\': \'1.4.5\', \'CacheNodeTypeSpecificValues\': {\'CacheNodeTypeSpecificValue\': [{\'CacheNodeType\': \'cache.m4.10xlarge\', \'Value\': \'158355\'}, ... {\'CacheNodeType\': \'cache.r4.large\', \'Value\': \'12590\'}, ...]}}, got None)'. Its key list additionally names CacheNodeTypeSpecificParameter[1] and the whole elements Parameters.Parameter[0] .. Parameters.Parameter[29].
- steps task#4, task#5, task#6, task#8 (one cluster each). Each detail begins with the same '...CacheNodeTypeSpecificParameters.CacheNodeTypeSpecificParameter[0]: missing (...), got None)' and each key list names the family's Parameters.Parameter[i] elements as missing.
- step task#10 (affected_count 2: task#10, task#11): CacheNodeTypeSpecificParameter[0] missing, expected {'AllowedValues': '0-', 'MinimumEngineVersion': '5.0.0', 'CacheNodeTypeSpecificValues': [... cache.m5.large '685454275', cache.r4.16xlarge '43702157312' ...]}; key list names CacheNodeTypeSpecificParameter[0..2] and Parameters.Parameter[0..56].
- step task#12 (affected_count 2: task#12, task#13): same, MinimumEngineVersion '6.0.5'; key list names CacheNodeTypeSpecificParameter[0..2] and Parameters.Parameter[0..61].
- step task#14 (affected_count 6: task#14 .. task#19): same, MinimumEngineVersion '7.0.0'; key list names CacheNodeTypeSpecificParameter[0..3] and Parameters.Parameter[0..66].
- step task#20 (affected_count 2: task#20, task#21): same, MinimumEngineVersion '9.0.0' with cache.c8gn.* node types; key list names CacheNodeTypeSpecificParameter[0..3] and Parameters.Parameter[0..67].

In every cluster the verdict is a payload mismatch at HTTP 200 -> 200: the response is well-formed but CacheNodeTypeSpecificParameters is absent ('got None' for each expected element) and the Parameters rows the recording carries are not the rows the emulator emits. The quoted node types, values, AllowedValues and MinimumEngineVersion strings are the recordings' fill-ins for each family; the requirement is that both lists be the family's published catalog. No per-run value (host, port, generated id, timestamp, request id) appears in any of these clusters.

Model evidence: smithy_query(query='operation', operation='DescribeCacheParameters', model_path='services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json') -> output CacheParameterGroupDetails{Marker, Parameters, CacheNodeTypeSpecificParameters}; smithy_query(query='shape', shape='CacheNodeTypeSpecificParameter,CacheNodeTypeSpecificValue,CacheParameterGroupDetails') -> CacheNodeTypeSpecificParameter{ParameterName, Description, Source, DataType, AllowedValues, IsModifiable, MinimumEngineVersion, CacheNodeTypeSpecificValues, ChangeType} and CacheNodeTypeSpecificValue{CacheNodeType, Value}. The model documents the members but not their values, hence 'recorded behavior only' for the catalog content.

## Steps to Reproduce
1. Build and boot the emulator from this checkout (health answered 200 and the data listener accepted during this replay).
2. Replay trace set simparity-wire-aws-v1 and watch fixture elasticache-catalog-describe-cache-parameters, steps task#1, task#4, task#5, task#6, task#8, task#10, task#12, task#14 and task#20 (the clusters also cover task#2, task#3, task#11, task#13, task#15-task#19 and task#21).
3. For each engine family the fixture walks (the memcached 1.4.5 family through the Redis 5.0.0 / 6.0.5 / 7.0.0 and Valkey 9.0.0 families), call DescribeCacheParameters(CacheParameterGroupName=<that family's default group>).
4. Expect a 200 whose body carries BOTH lists: the family's complete Parameters catalog and a non-empty CacheNodeTypeSpecificParameters list whose rows carry the family's MinimumEngineVersion, the parameter's AllowedValues and one CacheNodeTypeSpecificValue per supported cache node type.
5. Observe instead that CacheNodeTypeSpecificParameters is absent from the body for every family (every expected element reports 'got None'), and that the Parameters rows differ from the family's published catalog across the whole index range.
6. Re-replay elasticache-catalog-describe-cache-parameters after the fix and confirm task#1, task#4, task#5, task#6, task#8, task#10, task#12, task#14 and task#20 all pass (along with the other steps in their clusters).
