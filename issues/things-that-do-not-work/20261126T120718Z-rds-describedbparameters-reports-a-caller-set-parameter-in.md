Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-parameter-group-memory-formula-value-range
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-parameter-group-memory-formula-value-range
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters
Issue-Commit: 43586243266d19b209b15f2de40e433673053310

# rds DescribeDBParameters reports a caller-set parameter in a read AWS answers with an empty Parameters list

> _Note: this issue was identified against the code base at commit `43586243266d19b209b15f2de40e433673053310`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBParameters

## Description
DescribeDBParameters does not narrow the group's caller-set parameters to the selection the request asks for: the read this step makes is answered by AWS with no parameters at all, while the emulator reports the parameter the fixture earlier set to the InnoDB memory formula. 1 cause in this bundle.

Cause 1 of 1: the parameter report carries a user-sourced parameter the recorded report does not.
AWS: Smithy: DescribeDBParameters -> DBParameterGroupDetails$Parameters (ParametersList of Parameter); DescribeDBParametersMessage$Source — "The parameter types to return. Default: All parameter types returned. Valid Values: user | system | engine-default"; DescribeDBParametersMessage$Filters — "A filter that specifies one or more DB parameters to describe. The only supported filter is parameter-name. The results list only includes information about the DB parameters with these names."; DescribeDBParametersMessage$Marker pages the report. The model states no values for what a group actually holds, so: recorded behavior only — at this step the recording shows AWS answering HTTP 200 with the Parameters list EMPTY (an empty Parameters element carrying no Parameter member) for this DB parameter group, after the fixture's earlier steps set innodb_buffer_pool_size to the memory formula '{DBInstanceClassMemory*3/4}'. In other words the selection this read asks for (its Source / parameter-name filter / Marker window) matches NO parameter of the group, and every other step of the fixture — including the modify that set the formula — replays identically, so the divergence is this read's selection and not the write that preceded it.
Emulator: answers HTTP 200 with a one-member Parameters list carrying the caller-set parameter — ParameterName 'innodb_buffer_pool_size', ParameterValue '{DBInstanceClassMemory*3/4}', Source 'user', ApplyType 'dynamic', DataType 'integer', AllowedValues '5242880-18446744073709551615', IsModifiable 'true', ApplyMethod 'immediate'. The replay reports type_mismatch at $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters: expected '' (the empty element), got a Parameter member. Status is 200 on both sides; only the body diverges. Hypothesis: the group's caller-set entries are injected into the report before the request's selection is honoured — in the handler, the Customer arm of the Source match maps every key of `group.parameters` and `every_parameter_type` re-inserts every such key over the family defaults, with the parameter-name filter and the Marker window applied only afterwards — or the setting was expected to have been cleared from `group.parameters` by the fixture's preceding step and the clear did not reach the stored record.
Why: this read is how a client learns which parameters it has overridden in a group versus which still run the engine/family default. A client told an override exists that AWS reports as absent will skip a modify it must still perform, or keep re-resetting a parameter that is already at its default; a drift-detection or infrastructure-as-code client reports a permanent, unresolvable difference; and because the extra member changes the report's length and name order, the Marker a paging client carries forward lands on a different record than it does against AWS.
Fix: the DescribeDBParameters handler's selection step in services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_parameters.rs — the Source / Filters / Marker narrowing applied to the group's caller-set parameters (`every_parameter_type`, the `ParameterSourceFilter::Customer` arm) — and, if the entry should no longer be stored at all, the DB-parameter-group state it reads (`crate::state::DbParameterGroupRecord::parameters`). Not the serializer: the list wrapping itself matches.
Traces: rds-db-parameter-group-memory-formula-value-range#task#5.

Open issue 20261126T074046Z-rds-reserved-offering-and-db-parameter-catalog-reads.md also touches DescribeDBParameters, but only the Description / MinimumEngineVersion catalog text under a different replay key (rds-db-parameters-pagination#task#4); it does not carry this fixture/step and does not cover this finding.

## Evidence
DescribeDBParameters, fixture rds-db-parameter-group-memory-formula-value-range step task#5 (status 200 -> 200), divergence key rds|DescribeDBParameters|status=200->200|keys=$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters:

Expected (recording): $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters = '' — an empty Parameters element, no Parameter members.
Actual (emulator): $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters = {'Parameter': {'ParameterName': 'innodb_buffer_pool_size', 'ParameterValue': '{DBInstanceClassMemory*3/4}', 'Description': 'The size in bytes of the memory buffer innodb uses to cache data and indexes of its tables', 'Source': 'user', 'ApplyType': 'dynamic', 'DataType': 'integer', 'AllowedValues': '5242880-18446744073709551615', 'IsModifiable': 'true', 'ApplyMethod': 'immediate'}} — replay verdict type_mismatch.

The parameter name, the formula value, the AllowedValues range and the group name are values this fixture's request and state supply; the requirement is that the report carry exactly the parameters the request's Source/parameter-name filter/Marker window selects out of what the group holds — here, none.

## Steps to Reproduce
1. Replay fixture rds-db-parameter-group-memory-formula-value-range against the emulator.
2. At step task#5 (DescribeDBParameters on the fixture's DB parameter group) compare $.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters with the recording: the recorded response carries an empty Parameters element, the emulator carries one Parameter member for innodb_buffer_pool_size with Source 'user'.
3. Replay the fixture's earlier steps in order and, after each one, re-issue the same task#5 request: the user-sourced entry is reported regardless of the Source value, parameter-name filter and Marker the request names, and regardless of the preceding step that should have left nothing for this read to select.
4. Confirm the other steps of the fixture (including the ModifyDBParameterGroup that sets the memory formula) still replay identically, so the fix is confined to this read.
