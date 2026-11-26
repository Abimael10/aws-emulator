Issue-Source: trace-sweep-divergence
Issue-Trace: elasticache-cache-parameter-group-workflow
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: elasticache-cache-parameter-group-workflow elasticache-catalog-describe-reserved-cache-nodes-offerings elasticache-catalog-describe-service-updates
Issue-Shape-Id: com.amazonaws.elasticache#AmazonElastiCacheV9
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Marker
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*]
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].AllowedValues
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].ChangeType
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].DataType
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].Description
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].IsModifiable
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].ParameterName
Issue-Key: elasticache|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].ParameterValue
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].AllowedValues
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].ChangeType
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].DataType
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].Description
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].IsModifiable
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].MinimumEngineVersion
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].ParameterName
Issue-Key: elasticache|DescribeCacheParameters|2xx->2xx|$.DescribeCacheParametersResponse.DescribeCacheParametersResult.Parameters.Parameter[*].ParameterValue
Issue-Key: elasticache|DescribeReservedCacheNodesOfferings|2xx->2xx|$.DescribeReservedCacheNodesOfferingsResponse.DescribeReservedCacheNodesOfferingsResult.ReservedCacheNodesOfferings.ReservedCacheNodesOffering[*]
Issue-Key: elasticache|DescribeServiceUpdates|2xx->2xx|$.DescribeServiceUpdatesResponse.DescribeServiceUpdatesResult.ServiceUpdates.ServiceUpdate[*]
Issue-Related: 20261121T135312Z-elasticache-describecacheparameters-on-a-custom-parameter-group-serves.md
Issue-Commit: 01f237389f2ba129bcfe6809f1b810fd15e9edea

# ElastiCache built-in reference catalogs serve emulator-authored parameter, offering and service-update data

> _Note: this issue was identified against the code base at commit `01f237389f2ba129bcfe6809f1b810fd15e9edea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEngineDefaultParameters

## Description
Four distinct causes with one shared fault: the reference datasets the ElastiCache emulator serves for its read-only catalog operations are emulator-authored rather than the engine/catalog data AWS returns, so every element of the lists differs in name, ordering, value and documentation. All are 200-vs-200 body divergences. THIS BUNDLE CARRIES 4 CAUSES (Cause 1 of 4 .. Cause 4 of 4); all four must be closed before submitting.

Cause 1 of 4 - DescribeEngineDefaultParameters serves a different parameter set, different parameter documentation and a differently built Marker (fixture elasticache-cache-parameter-group-workflow#task#3).
AWS: Smithy com.amazonaws.elasticache#EngineDefaults (members CacheParameterGroupFamily, Marker 'Provides an identifier to allow retrieval of paginated results', Parameters, CacheNodeTypeSpecificParameters) and com.amazonaws.elasticache#Parameter (ParameterName, ParameterValue, Description, Source, DataType, AllowedValues, IsModifiable, MinimumEngineVersion, ChangeType). The model does not enumerate the engine defaults, so for the dataset itself this is recorded behavior only: the recording shows, for the requested family, a specific ordered parameter list in which e.g. index 10 is ParameterName 'appendfsync' / ParameterValue 'everysec' / AllowedValues 'always,everysec,no' / ChangeType 'immediate' / Description 'fsync policy for AOF persistence', index 2's Description is 'Enabled active memory defragmentation' and index 9's is 'The amount of effort that redis uses to expire items in the active expiration job'. The Marker is a base64 continuation token naming the parameter at the page boundary (recorded value decodes to the parameter name 'client-output-buffer-limit-replica-soft-limit'); the fixed part is the base64 encoding of a parameter name, the variable part is which parameter ends the page.
Emulator: a different list in a different order with its own prose - at index 10 it serves ParameterName 'activerehashing' / ParameterValue 'yes' / AllowedValues 'yes,no' / ChangeType 'requires-reboot' / Description 'Apply rehashing or not.', at index 2 'Enabled active defragmentation', at index 9 'How much effort the node spends expiring keys, from the least to the most eager.', and it returns extra trailing Parameter[18]/[19] entries AWS does not return. The Marker is a hex-encoded internal composite key (the recorded actual decodes to 'redis7|1:clusterallowreadswhendown:cluster-allow-reads-when-down') rather than a base64 parameter name.
Why: a client that reads engine defaults to pre-populate or validate a parameter group gets parameter names that do not exist in AWS for that family, wrong AllowedValues/ChangeType (so it mis-predicts whether a change needs a reboot), and a pagination token in an encoding AWS never produces, so a paging client cannot round-trip the Marker against the real service.
Fix: the elasticache engine-defaults catalog (the static parameter dataset behind DescribeEngineDefaultParameters) and the Marker encoder in that handler.
Traces: elasticache-cache-parameter-group-workflow#task#3.

Cause 2 of 4 - DescribeCacheParameters on a just-created custom parameter group serves that same emulator-authored list (fixture elasticache-cache-parameter-group-workflow#task#5 and, after a ModifyCacheParameterGroup, #task#7).
AWS: recorded behavior only for the dataset - a custom group created from a family inherits exactly that family's parameter set, with the same names, order, Descriptions, AllowedValues, DataTypes and ChangeTypes as the engine defaults (Smithy shape com.amazonaws.elasticache#Parameter as above); modifying one parameter changes only that parameter's ParameterValue and Source, not the catalogue.
Emulator: the same substitutions as Cause 1 appear here (index 10 'activerehashing' instead of 'appendfsync', index 11 DataType 'integer' and Description 'Normal client output buffer hard limit in bytes.' instead of DataType 'string' / 'Enable Redis persistence.' for 'appendonly', with AllowedValues dropped entirely for that member), and the divergence persists on the post-modify read, running to Parameter[67].
Why: a client that creates a parameter group and then reads it back cannot find the parameter it intends to modify, and reads wrong DataType/AllowedValues for the ones it does find. This is NOT covered by the open issue 20261121T135312Z-elasticache-describecacheparameters-on-a-custom-parameter-group-serves (which owns the elasticache-catalog-describe-cache-parameters fixture): these two steps are reported with no owning issue and must be re-replayed separately.
Fix: the DescribeCacheParameters handler/state must project the family's real parameter set onto a custom group instead of the emulator's own list.
Traces: elasticache-cache-parameter-group-workflow#task#5, elasticache-cache-parameter-group-workflow#task#7.

Cause 3 of 4 - DescribeReservedCacheNodesOfferings serves invented offerings (fixture elasticache-catalog-describe-reserved-cache-nodes-offerings#task#1, cluster also covers task#2 and task#3).
AWS: recorded behavior only - the recording shows, per node type and product description, a set of offerings each identified by a stable ReservedCacheNodesOfferingId, with OfferingType, Duration, FixedPrice, UsagePrice and RecurringCharges that are consistent with that OfferingType: a 'No Upfront' offering has FixedPrice '0.0' and a non-zero Hourly RecurringChargeAmount, an 'All Upfront' offering has a non-zero FixedPrice and RecurringChargeAmount '0.0'.
Emulator: it returns its own offering ids and swaps the economics - the first element comes back as OfferingType 'All Upfront' with FixedPrice '1261.44' and RecurringChargeAmount '0' where the recording has OfferingType 'No Upfront' with FixedPrice '0.0' and RecurringChargeAmount '0.141', and the second is the mirror image; every offering id differs.
Why: a client that lists offerings and then calls PurchaseReservedCacheNodesOffering with an id it just read, or that compares pricing/offering types, is working from a catalogue that does not exist in AWS, and the id it passes back is not a real offering id.
Fix: the static reserved-cache-node-offering catalog behind DescribeReservedCacheNodesOfferings (ids, OfferingType and the price/recurring-charge pairing).
Traces: elasticache-catalog-describe-reserved-cache-nodes-offerings#task#1 (cluster also covers #task#2, #task#3).

Cause 4 of 4 - DescribeServiceUpdates serves an invented service-update list (fixture elasticache-catalog-describe-service-updates#task#1).
AWS: recorded behavior only - the recording returns the service's published update list, e.g. a ServiceUpdate with ServiceUpdateName 'elasticache-redis-oss-v6-eol-replication-groups', ServiceUpdateType 'engine-major-version-update', ServiceUpdateSeverity 'important', ServiceUpdateStatus 'available', Engine 'redis', EngineVersion 'redis 6.x' and AutoUpdateAfterRecommendedApplyByDate 'false', with release/recommended-apply-by/end dates in that order of time. Names, types and statuses are the catalogue's; the dates are the catalogue's too, not per-run values.
Emulator: an entirely different first element - ServiceUpdateName 'elasticache-memcached-security-update-001', ServiceUpdateType 'security-update', ServiceUpdateSeverity 'critical', ServiceUpdateStatus 'expired', Engine 'memcached' - and a different list length (it serves entries at indices the recording does not have, up to ServiceUpdate[72] diverging).
Why: a client that lists service updates and then applies one by ServiceUpdateName (BatchApplyUpdateAction) names an update that AWS does not have, and anything filtering on status/severity/engine sees a different population.
Fix: the static service-update catalog behind DescribeServiceUpdates.
Traces: elasticache-catalog-describe-service-updates#task#1.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up): 5 divergence clusters, every one expected status 200 / actual status 200, verdict mismatch, body-only, none owned by an open issue and none blocked by a stub.

Cause 1 - elasticache-cache-parameter-group-workflow#task#3, DescribeEngineDefaultParameters:
- $...EngineDefaults.Marker: value_mismatch (expected 'Y2xpZW50LW91dHB1dC1idWZmZXItbGltaXQtcmVwbGljYS1zb2Z0LWxpbWl0' = base64 'client-output-buffer-limit-replica-soft-limit', got '7265646973377c...' = hex 'redis7|1:clusterallowreadswhendown:cluster-allow-reads-when-down')
- Parameter[2].Description: expected 'Enabled active memory defragmentation', got 'Enabled active defragmentation'
- Parameter[9].Description: expected 'The amount of effort that redis uses to expire items in the active expiration job', got 'How much effort the node spends expiring keys, from the least to the most eager.'
- Parameter[10].ParameterName: expected 'appendfsync', got 'activerehashing'; ParameterValue expected 'everysec', got 'yes'; AllowedValues expected 'always,everysec,no', got 'yes,no'; ChangeType expected 'immediate', got 'requires-reboot'; Description expected 'fsync policy for AOF persistence', got 'Apply rehashing or not.'
- Parameter[18], Parameter[19]: extra elements served that the recording does not contain

Cause 2 - elasticache-cache-parameter-group-workflow#task#5 and #task#7, DescribeCacheParameters:
- Parameter[2].Description / Parameter[9].Description / Parameter[10].* : identical substitutions to Cause 1
- Parameter[11].AllowedValues: missing (expected 'yes,no', got None); Parameter[11].DataType: expected 'string', got 'integer'; Parameter[11].Description: expected 'Enable Redis persistence.', got 'Normal client output buffer hard limit in bytes.'; Parameter[11].ParameterName: expected 'appendonly', got 'clie...'
- divergent keys run to Parameter[67] on #task#5 and include Parameter[41].Source / Parameter[43].Source on #task#7

Cause 3 - elasticache-catalog-describe-reserved-cache-nodes-offerings#task#1 (affected also #task#2, #task#3), DescribeReservedCacheNodesOfferings; 6 offerings diverge, e.g.:
- ReservedCacheNodesOffering[0]: expected {OfferingType 'No Upfront', FixedPrice '0.0', UsagePrice '0.0', RecurringChargeAmount '0.141', Duration '31536000', CacheNodeType 'cache.r6g.large', ProductDescription 'redis', ReservedCacheNodesOfferingId <catalog id>} got {OfferingType 'All Upfront', FixedPrice '1261.44', UsagePrice '0', RecurringChargeAmount '0', ReservedCacheNodesOfferingId <emulator id>}
- ReservedCacheNodesOffering[1]: expected {OfferingType 'All Upfront', FixedPrice '1146.0', RecurringChargeAmount '0.0'} got {OfferingType 'No Upfront', FixedPrice '0', RecurringChargeAmount '0.18'}

Cause 4 - elasticache-catalog-describe-service-updates#task#1, DescribeServiceUpdates; ServiceUpdate[0]..ServiceUpdate[72] diverge, e.g.:
- ServiceUpdate[0]: expected {ServiceUpdateName 'elasticache-redis-oss-v6-eol-replication-groups', ServiceUpdateType 'engine-major-version-update', ServiceUpdateSeverity 'important', ServiceUpdateStatus 'available', Engine 'redis', EngineVersion 'redis 6.x', AutoUpdateAfterRecommendedApplyByDate 'false'} got {ServiceUpdateName 'elasticache-memcached-security-update-001', ServiceUpdateType 'security-update', ServiceUpdateSeverity 'critical', ServiceUpdateStatus 'expired', Engine 'memcached', EngineVersion '1.6.22', AutoUpdateAfterRecommendedApplyByDate 'true'}

Model evidence: Smithy com.amazonaws.elasticache#EngineDefaults{CacheParameterGroupFamily, Marker, Parameters, CacheNodeTypeSpecificParameters} and com.amazonaws.elasticache#Parameter{ParameterName, ParameterValue, Description, Source, DataType, AllowedValues, IsModifiable, MinimumEngineVersion, ChangeType}; the model does not enumerate catalog contents, hence 'recorded behavior only' for the datasets.

## Steps to Reproduce
1. git clone the project repo and build/boot the emulator from that checkout.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay elasticache-cache-parameter-group-workflow step task#3 (DescribeEngineDefaultParameters) and compare EngineDefaults.Parameters element-by-element plus EngineDefaults.Marker against the recorded response - the parameter set, their Descriptions/AllowedValues/ChangeTypes and the Marker encoding all differ.
4. Cause 2: in the same fixture, replay task#5 (DescribeCacheParameters on the newly created custom group) and task#7 (the read after ModifyCacheParameterGroup) - the same emulator-authored parameter list is served, with 'appendonly'/'appendfsync' absent and wrong DataType/AllowedValues.
5. Cause 3: replay elasticache-catalog-describe-reserved-cache-nodes-offerings task#1 (and task#2/task#3) and compare each ReservedCacheNodesOffering's id, OfferingType, FixedPrice and RecurringCharges against the recording.
6. Cause 4: replay elasticache-catalog-describe-service-updates task#1 and compare the ServiceUpdates list (names, types, severities, statuses, engines) against the recording.
