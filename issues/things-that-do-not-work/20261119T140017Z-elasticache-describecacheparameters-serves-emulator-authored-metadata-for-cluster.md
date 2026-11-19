Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-cluster-enabled-parameter-validation
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-cluster-enabled-parameter-validation
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter.Description
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter.MinimumEngineVersion
Issue-Commit: 365042aa5aff1c761f3edc665c19518d88f112ee

# ElastiCache DescribeCacheParameters serves emulator-authored metadata for cluster-enabled: wrong Description and MinimumEngineVersion

> _Note: this issue was identified against the code base at commit `365042aa5aff1c761f3edc665c19518d88f112ee`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCacheParameters

## Description
ONE net-new root cause in the elasticache crate. A fixer must close ONE cause before submitting.

Cause 1 of 1: a per-parameter DescribeCacheParameters read answers HTTP 200 whose Parameter record for the cluster-mode parameter carries the emulator's own description prose and its own MinimumEngineVersion instead of the metadata AWS publishes for that parameter on the group's family (fixture elasticache-cluster-enabled-parameter-validation, step task#5; the same cluster also covers task#7, task#11 and task#13, i.e. the reads taken before and after the parameter is modified).

AWS: Smithy (services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json): DescribeCacheParameters(CacheParameterGroupName clientOptional/required, Source, MaxRecords, Marker) -> CacheParameterGroupDetails{Marker, Parameters: ParametersList, CacheNodeTypeSpecificParameters}; errors CacheParameterGroupNotFoundFault (404/CacheParameterGroupNotFound), InvalidParameterValueException (400/InvalidParameterValue), InvalidParameterCombinationException (400/InvalidParameterCombination). Shape Parameter{ParameterName, ParameterValue, Description ('A description of the parameter.'), Source, DataType, AllowedValues, IsModifiable, MinimumEngineVersion ('The earliest cache engine version to which the parameter can apply.'), ChangeType}. The model does not state catalog CONTENT, so the required values are recorded behavior only: for the cluster-mode parameter on the family the fixture's group belongs to, AWS returns the published one-line Description for that parameter and the published MinimumEngineVersion of the family in which the parameter first applies (the recording shows Description 'Enable cluster mode' and MinimumEngineVersion '7.0.0' for the family under test). Those are catalog facts about the parameter, not per-run values: the same request returns the same pair on every call.

Emulator: status 200 (status matches), but the single returned Parameter record diverges on two members. $.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter.Description is the emulator's own sentence-cased prose 'Whether cluster mode is enabled for the cluster.' where AWS returns the published short form 'Enable cluster mode', and $...Parameters.Parameter.MinimumEngineVersion is '3.2.4' where AWS returns '7.0.0'. The same two mismatches recur on all four reads in the fixture (task#5, task#7, task#11, task#13), i.e. both before and after the parameter value is changed, which rules out any request-derived explanation: this is seeded reference data, not state.

Why: MinimumEngineVersion is the gate a client checks before tuning a parameter on a group it intends to attach to a cluster. Reporting a much older minimum (a 3.x value where AWS reports 7.0.0) tells a client the parameter is settable on engine versions on which AWS would refuse it, so a deployment that validates against the emulator and then runs against AWS fails at ModifyCacheParameterGroup or at cluster creation; conversely tooling that pins the published minimum sees the emulator disagree about which engine versions support cluster mode. The Description is the text operators and generated consoles/docs surface for the parameter, so emulator-authored prose makes the parameter catalog non-reproducible against AWS output.

Fix: the elasticache crate's seeded cache-parameter reference catalog (the per-family parameter metadata the DescribeCacheParameters handler reads) and that handler's projection of each Parameter's Description and MinimumEngineVersion — the metadata must come from the family's published catalog rather than being composed in the handler.

Related open issues that do NOT cover this step (replay reports owned_by empty and stub_path empty for this cluster): 20261116T175951Z-elasticache-reference-catalogs-are-emulator-authored-engine-defaults.md covers the same seeded catalog but only via fixture elasticache-cache-parameter-group-workflow and the elasticache-catalog-* fixtures, and carries none of this fixture's replay keys; 20261030T130325Z-elasticache-parameter-catalog-is-incomplete-describecacheparameters-omits-cachenodetypespecificparameters.md names DescribeCacheParameters for fixture elasticache-catalog-describe-cache-parameters only.

Traces: elasticache-cluster-enabled-parameter-validation#task#5 (cluster also covers #task#7, #task#11, #task#13).

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true ('/__admin/health answered 200 and the :44209 data listener accepts'), 35 fixtures replayed, 333 steps total, 273 passed, 60 mismatched, 0 errored, 0 blocked, score 82.0%, bootstrap_signal=false, 37 divergence clusters. Exactly ONE cluster is net-new (verdict=mismatch, service=elasticache, stub_path empty, owned_by empty); the other 36 are owned by open issues.

Cluster (verdict=mismatch, divergence_key 'service=elasticache|DescribeCacheParameters|status=400->400'... reported as status=200->200 for this key: 'service=elasticache|DescribeCacheParameters|status=200->200|keys=$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter.Description,$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter.MinimumEngineVersion'), fixture elasticache-cluster-enabled-parameter-validation, representative step task#5, affected_count 4 (task#5, task#7, task#11, task#13), expected_status 200, actual_status 200:
  $.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter.MinimumEngineVersion: value_mismatch (expected '7.0.0', got '3.2.4')
  $.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter.Description: value_mismatch (expected 'Enable cluster mode', got 'Whether cluster mode is enabled for the cluster.')

The two quoted expected values are the recording's fill-ins for the family under test; the requirement is that both members are the published catalog metadata for that parameter on that family rather than text and a version the handler composes. Neither member is a per-run value (no id, host, timestamp or request id is involved), and both diverge identically on all four reads in the fixture.

Model evidence: smithy_query(query='operation', operation='DescribeCacheParameters', model_path='services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json') -> output CacheParameterGroupDetails{Marker, Parameters, CacheNodeTypeSpecificParameters}, errors CacheParameterGroupNotFoundFault/InvalidParameterValueException/InvalidParameterCombinationException; smithy_query(query='shape', shape='Parameter') -> members ParameterName, ParameterValue, Description, Source, DataType, AllowedValues, IsModifiable, MinimumEngineVersion, ChangeType. The model documents what the members mean but not their values, hence 'recorded behavior only' for the content.

## Steps to Reproduce
1. Build and boot the emulator from this checkout (health answered 200 and the data listener accepted during this replay).
2. Replay trace set simparity-wire-aws-v1 and watch fixture elasticache-cluster-enabled-parameter-validation, steps task#5, task#7, task#11 and task#13.
3. Create the fixture's cache parameter group, then call DescribeCacheParameters(CacheParameterGroupName=<that group>) and locate the cluster-mode parameter in Parameters.
4. Expect that Parameter's Description to be the family's published short description for the parameter ('Enable cluster mode' in the recording) and its MinimumEngineVersion to be the published earliest applicable engine version ('7.0.0' in the recording).
5. Observe instead Description 'Whether cluster mode is enabled for the cluster.' and MinimumEngineVersion '3.2.4'.
6. Modify the parameter and read it back (the fixture's task#11/task#13 reads): both members still diverge, confirming the values come from seeded reference data rather than request state.
7. Re-replay elasticache-cluster-enabled-parameter-validation after the fix and confirm task#5, task#7, task#11 and task#13 all pass.
