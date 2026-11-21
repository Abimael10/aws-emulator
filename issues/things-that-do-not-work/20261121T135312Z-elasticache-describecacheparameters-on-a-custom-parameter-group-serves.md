Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-cache-parameter-group-workflow
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-cache-parameter-group-workflow
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*]
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].Source
Issue-Related: 20261116T175951Z-elasticache-reference-catalogs-are-emulator-authored-engine-defaults.md
Issue-Commit: 8cbf0dcd0020846f13fa76c0319ab04fca2e29a2

# ElastiCache DescribeCacheParameters on a custom parameter group serves an emulator-authored parameter catalog (2 causes)

> _Note: this issue was identified against the code base at commit `8cbf0dcd0020846f13fa76c0319ab04fca2e29a2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCacheParameters

## Description
TWO net-new root causes in the elasticache crate, both in the same place: the seeded cache-parameter catalog that DescribeCacheParameters projects for a customer-created cache parameter group. A fixer must close TWO causes before submitting.

AWS: Smithy (services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json) DescribeCacheParameters(CacheParameterGroupName clientOptional/required, Source, MaxRecords, Marker) -> CacheParameterGroupDetails{Marker, Parameters: ParametersList, CacheNodeTypeSpecificParameters: CacheNodeTypeSpecificParametersList}; errors CacheParameterGroupNotFoundFault (404/CacheParameterGroupNotFound), InvalidParameterValueException (400/InvalidParameterValue), InvalidParameterCombinationException (400/InvalidParameterCombination); paginated {inputToken Marker, outputToken Marker, items Parameters, pageSize MaxRecords}. Shape Parameter{ParameterName, ParameterValue, Description ('A description of the parameter.'), Source ('The source of the parameter.'), DataType, AllowedValues ('The valid range of values for the parameter.'), IsModifiable, MinimumEngineVersion, ChangeType}. The model fixes the SHAPE but says nothing about catalog CONTENT, so the content requirement is recorded behavior only: a group created from a default family returns that family's whole published parameter catalog - the same parameter names, in the service's order, each row carrying the published ParameterName/ParameterValue/AllowedValues/DataType/Description/IsModifiable/ChangeType and Source 'system' until the row is modified, after which that row's Source becomes 'user'. These are catalog facts about the family, not per-run values: the same request returns the same rows on every call.

Cause 1 of 2 - the post-create read of the group's parameters (fixture elasticache-cache-parameter-group-workflow, step task#5).
Emulator: HTTP 200 (status agrees) but the Parameters list is a DIFFERENT set in a DIFFERENT order, and the rows it does emit carry the emulator's own prose and bounds. The list is shifted by one from index 0 onwards because a published row is absent: Parameters.Parameter[0] is expected to be the ACL-log-length parameter (ParameterName 'acllog-max-len', DataType 'integer', AllowedValues '1-10000', ParameterValue '128', Description 'The maximum length of the ACL Log') and the emulator serves the pub/sub-default row there instead (ParameterName 'acl-pubsub-default', DataType 'string', AllowedValues 'allchannels,resetchannels', ParameterValue 'resetchannels', Description 'The default pub/sub channel permissions a newly created user is given.'); Parameter[1] is expected to be that pub/sub row with the published Description 'Default pubsub channel permissions for ACL users deployed to this cluster', AllowedValues 'resetchannels,allchannels' and ParameterValue 'allchannels', and the emulator serves an active-defragmentation row there ('active-defrag-cycle-max', integer, '1-75', '75'). The shift runs the length of the list and the two bodies also differ in membership, not only order: the recording has Parameter[18] and Parameter[19] rows for which the emulator emits nothing.
Why: a client that reads a group's parameters to decide what it may tune gets a catalog that is not AWS's - a parameter AWS publishes is missing, the AllowedValues it validates against are another parameter's bounds, DataType disagrees with the value it must send, and the default ParameterValue it reads back is wrong. Tooling that diffs a group against the family default, or that renders the catalog, cannot reproduce AWS output; a value accepted against the emulator can be refused by AWS and vice versa.
Fix: the elasticache crate's seeded cache-parameter reference catalog plus the DescribeCacheParameters handler's projection of Parameters - the row set, the row order and each row's published metadata must come from the family catalog instead of being composed in the handler.
Traces: elasticache-cache-parameter-group-workflow#task#5.

Cause 2 of 2 - the read taken after the group's parameters have been modified in the same fixture (step task#7).
Emulator: HTTP 200 again, with the whole Cause-1 shift and prose divergence still present, and in addition the Source member of the modified rows disagrees: the cluster's key list names $...Parameters.Parameter[41].Source and $...Parameters.Parameter[42].Source as diverging, i.e. after ModifyCacheParameterGroup the emulator does not report the modified rows the way AWS does (AWS flips a modified row's Source from 'system' to 'user' and leaves every other row 'system'). Membership also differs at the tail here - the recording carries Parameter[64], Parameter[65] and Parameter[66] rows the emulator does not emit.
Why: Source is how a client tells which parameters an operator has actually overridden (it is also the filter value accepted by the Source input member), so a wrong Source makes 'show me what was customised on this group' answer wrongly, and the missing tail rows hide published parameters from any client that enumerates the group.
Fix: same component - the seeded catalog and the DescribeCacheParameters projection, specifically the per-row Source bookkeeping applied after a modify, and the completeness of the emitted row set.
Traces: elasticache-cache-parameter-group-workflow#task#7.

Related open issues that do NOT cover these two steps (replay reports owned_by empty and stub_path empty for both clusters): 20261116T175951Z-elasticache-reference-catalogs-are-emulator-authored-engine-defaults.md owns the DescribeEngineDefaultParameters cluster of this same fixture (step task#3) but carries neither of these replay keys; 20261119T140017Z-elasticache-describecacheparameters-serves-emulator-authored-metadata-for-cluster.md is about a single parameter's Description/MinimumEngineVersion in a different fixture. The issue that previously covered the family catalog, 20261030T130325Z-elasticache-parameter-catalog-is-incomplete-describecacheparameters-omits-cachenodetypespecificparameters.md, is RESOLVED and therefore covers nothing here.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true ('/__admin/health answered 200 and the :44689 data listener accepts'), 35 fixtures replayed, 333 steps total, 282 passed, 51 mismatched, 0 errored, 0 blocked, score 84.7%, bootstrap_signal=false, 28 divergence clusters (all service=elasticache). 11 clusters are net-new (owned_by empty, stub_path empty); the two below are this issue's.

Cluster A - divergence_key 'service=elasticache|DescribeCacheParameters|status=200->200|keys=$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[0].AllowedValues,...' , fixture elasticache-cache-parameter-group-workflow, representative step task#5, affected_count 1, expected_status 200, actual_status 200, verdict mismatch:
  $...Parameters.Parameter[0].ParameterName: value_mismatch (expected 'acllog-max-len', got 'acl-pubsub-default')
  $...Parameters.Parameter[0].DataType: value_mismatch (expected 'integer', got 'string')
  $...Parameters.Parameter[0].AllowedValues: value_mismatch (expected '1-10000', got 'allchannels,resetchannels')
  $...Parameters.Parameter[0].ParameterValue: value_mismatch (expected '128', got 'resetchannels')
  $...Parameters.Parameter[0].Description: value_mismatch (expected 'The maximum length of the ACL Log', got 'The default pub/sub channel permissions a newly created user is given.')
  $...Parameters.Parameter[1].ParameterName: value_mismatch (expected 'acl-pubsub-default', got 'active-defrag-cycle-max')
  $...Parameters.Parameter[1].AllowedValues: value_mismatch (expected 'resetchannels,allchannels', got '1-75')
  $...Parameters.Parameter[1].DataType: value_mismatch (expected 'string', got 'integer')
  $...Parameters.Parameter[1].Description: value_mismatch (expected 'Default pubsub channel permissions for ACL users deployed to this cluster', got 'The largest share of CPU, as a percentage, active defragmentation may use.')
  $...Parameters.Parameter[1].ParameterValue: value_mismatch (expected 'allchannels', got '75')
  ... the same shift continues through the list; the key list also names whole elements $...Parameters.Parameter[18] and $...Parameters.Parameter[19] (present in the recording, absent from the emulator body).

Cluster B - same divergence_key family, fixture elasticache-cache-parameter-group-workflow, representative step task#7, affected_count 1, expected_status 200, actual_status 200, verdict mismatch: the same index-0/index-1 shift and prose divergences, plus $...Parameters.Parameter[41].Source and $...Parameters.Parameter[42].Source in the key list and whole elements $...Parameters.Parameter[64], [65], [66] reported missing.

The quoted names, values, bounds and sentences are the recording's fill-ins for the family under test; the requirement is that the row set, its order and each row's metadata be the family's published catalog rather than handler-composed text and bounds. No per-run value (host, port, id, timestamp, request id) is involved in either cluster, and both reads in the fixture diverge identically, before and after the modify.

Model evidence: smithy_query(query='operation', operation='DescribeCacheParameters', model_path='services/service-com-amazonaws-elasticache-amazonelasticachev9/smithy-model/com-amazonaws-elasticache-amazonelasticachev9.json') -> output CacheParameterGroupDetails{Marker, Parameters, CacheNodeTypeSpecificParameters}, errors CacheParameterGroupNotFoundFault/InvalidParameterCombinationException/InvalidParameterValueException, paginated on Marker/Parameters; smithy_query(query='shape', shape='Parameter') -> ParameterName, ParameterValue, Description, Source, DataType, AllowedValues, IsModifiable, MinimumEngineVersion, ChangeType. The model documents the members' meaning but not their values, hence 'recorded behavior only' for content.

## Steps to Reproduce
1. Build and boot the emulator from this checkout (health answered 200 and the data listener accepted during this replay).
2. Replay trace set simparity-wire-aws-v1 and watch fixture elasticache-cache-parameter-group-workflow, steps task#5 and task#7.
3. Create the fixture's cache parameter group from its default family, then call DescribeCacheParameters(CacheParameterGroupName=<that group>) (step task#5).
4. Expect Parameters to be the family's published catalog: Parameter[0] the ACL-log-length row ('acllog-max-len', integer, AllowedValues '1-10000', ParameterValue '128', Description 'The maximum length of the ACL Log') and Parameter[1] the pub/sub-default row ('acl-pubsub-default', string, 'resetchannels,allchannels', 'allchannels'), with every published row present (the recording includes rows at indexes 18 and 19).
5. Observe instead that the list starts at the pub/sub-default row with emulator-authored Description/AllowedValues, that every subsequent index is shifted, and that the rows at indexes 18 and 19 are absent.
6. Modify a parameter in the group and read it back (step task#7): the same shift persists and the Source member of the modified rows (key list names Parameter[41].Source and Parameter[42].Source) does not match AWS's system/user bookkeeping, with rows 64-66 absent.
7. Re-replay elasticache-cache-parameter-group-workflow after the fix and confirm task#5 and task#7 both pass.
