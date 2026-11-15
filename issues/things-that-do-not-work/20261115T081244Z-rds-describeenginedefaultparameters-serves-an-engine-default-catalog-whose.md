Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-parameter-group-lifecycle-and-engine-defaults
Issue-Step: task#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-parameter-group-lifecycle-and-engine-defaults
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].MinimumEngineVersion
Issue-Related: 20261114T091417Z-rds-reference-catalogs-and-parameter-metadata-are-emulator.md
Issue-Commit: 51f17b774124d07cc1970a907a5af4e8c96232ea

# rds: DescribeEngineDefaultParameters serves an engine-default catalog whose parameter set, Source values and pagination Marker diverge from AWS

> _Note: this issue was identified against the code base at commit `51f17b774124d07cc1970a907a5af4e8c96232ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEngineDefaultParameters

## Description
One cause. The replay reported no blocking stub and no owning issue for this step.

Cause 1 of 1: DescribeEngineDefaultParameters for a DB parameter group family returns a parameter catalog that diverges from AWS's engine defaults from the ninth entry onward — a different parameter set and ordering, 'engine-default' where AWS reports 'system', omitted ParameterValue for parameters that have one, and consequently a different pagination Marker.

AWS: Smithy (com.amazonaws.rds#AmazonRDSv19) models DescribeEngineDefaultParameters with required input DBParameterGroupFamily and output EngineDefaults, paginated with inputToken Marker, outputToken EngineDefaults.Marker, items EngineDefaults.Parameters and pageSize MaxRecords. Each Parameter carries ParameterName, ParameterValue, Description, Source ('The source of the parameter value'), ApplyType, DataType, AllowedValues and MinimumEngineVersion. The engine defaults are a fixed, engine-family-specific catalog: for a given family AWS returns the same parameters, in the same (name-ordered) sequence, with the same descriptions, data types and allowed-value ranges, each parameter's Source naming where its value comes from — 'system' for a parameter whose default is supplied by the engine installation (for example the MySQL-family 'basedir', whose ParameterValue is the engine's install path) and 'engine-default' only for parameters that take the engine's own compiled-in default. The recorded response shows this family's page containing, in order, '...', 'basedir' (Source 'system', ParameterValue present), then 'binlog_cache_size' (DataType integer, AllowedValues '4096-18446744073709547520', ParameterValue '32768', Source 'system'), and ending with a Marker that is the base64 of the last parameter name in the page ('binlog_rows_query_log_events'). AWS's catalog content itself is recorded behavior only (the model does not enumerate parameters); the structural rules — one fixed catalog per family, name-ordered paging, Marker derived from the page's last entry, Source reflecting the value's origin, ParameterValue present when a default exists — are what the recording establishes.

Emulator: for the same family and page the emulator agrees for the first eight entries and then diverges. At the 'basedir' entry it reports Source 'engine-default' instead of 'system' and omits ParameterValue entirely (expected the engine install path, got None). From the next entry on the catalogs disagree outright: the emulator lists 'big_tables' (DataType boolean, AllowedValues '0,1', ParameterValue '0', Description about internal temporary tables) where AWS lists 'binlog_cache_size' (DataType integer, AllowedValues '4096-18446744073709547520', ParameterValue '32768'), and every subsequent index 10..19 mismatches on ParameterName, Description, DataType, AllowedValues, ApplyType, MinimumEngineVersion and Source the same way — the emulator's set includes parameters AWS does not list at those positions and so is shifted against it. Because the page content differs, the pagination cursor differs too: EngineDefaults.Marker came back as base64 of 'binlog_row_image' instead of base64 of 'binlog_rows_query_log_events'.

Why: clients use the engine defaults as the authority for what is tunable in a family and for the baseline value of each parameter. A shifted catalog makes a parameter lookup return the wrong parameter's metadata, so validation of a ModifyDBParameterGroup value against AllowedValues/DataType is performed against the wrong constraint; reporting 'engine-default' for a system-sourced parameter and dropping its ParameterValue makes a drift comparison against a modified group report a false difference; and because the Marker is derived from the page, a caller that pages the catalog and re-enters with a recorded Marker resumes at a different point than on AWS.

Fix: the engine-default parameter catalog backing the DescribeEngineDefaultParameters handler (the rds engine-defaults data/state the handler reads, not the paging code) must carry the same parameters per family as AWS with the same name ordering and per-parameter metadata, each parameter's Source set to the origin of its value ('system' for engine-installation-supplied defaults) and ParameterValue populated whenever a default exists; the handler's Marker must then fall out of the corrected page's last entry.

Traces: rds-db-parameter-group-lifecycle-and-engine-defaults#task#10. The one open DescribeEngineDefaultParameters issue, 20261104T130124Z-neptune-describeenginedefaultparameters-serves-an-empty-engine-default-parameter.md, is for the neptune service and carries no replay key for this rds step.

## Evidence
Replay of trace set simparity-wire-aws-v1 (99 fixtures, 993 steps, 539 passed, score 54.3%).

Fixture rds-db-parameter-group-lifecycle-and-engine-defaults, step task#10, rds DescribeEngineDefaultParameters (status 200 expected and returned; the body diverges):
  $...EngineDefaults.Parameters.Parameter[8].Source: expected 'system', got 'engine-default'
  $...EngineDefaults.Parameters.Parameter[8].ParameterValue: expected '/rdsdbbin/mysql', got None (member missing)
  $...EngineDefaults.Parameters.Parameter[9].ParameterName: expected 'binlog_cache_size', got 'big_tables'
  $...EngineDefaults.Parameters.Parameter[9].DataType: expected 'integer', got 'boolean'
  $...EngineDefaults.Parameters.Parameter[9].AllowedValues: expected '4096-18446744073709547520', got '0,1'
  $...EngineDefaults.Parameters.Parameter[9].ParameterValue: expected '32768', got '0'
  $...EngineDefaults.Parameters.Parameter[9].Description: expected 'The size of the cache to hold the SQL statements for the binary log during a transaction.', got 'Whether all internal temporary tables are stored on disk rather than in memory.'
  $...EngineDefaults.Parameters.Parameter[9].Source: expected 'system', got 'engine-default'
  $...EngineDefaults.Parameters.Parameter[10..19].{ParameterName,Description,DataType,AllowedValues,ApplyType,MinimumEngineVersion,ParameterValue,Source}: the same shifted-catalog mismatch repeats for every remaining entry in the page
  $...EngineDefaults.Marker: expected 'YmlubG9nX3Jvd3NfcXVlcnlfbG9nX2V2ZW50cw==' (base64 'binlog_rows_query_log_events'), got 'YmlubG9nX3Jvd19pbWFnZQ==' (base64 'binlog_row_image')

Expected: the family's engine-default catalog, name-ordered, with each parameter's own metadata and Source, and a Marker derived from the last entry of that page.
Actual: the first eight entries agree, then the emulator's own catalog takes over — a different parameter at each index, 'engine-default' in place of 'system', a missing ParameterValue on a system-sourced parameter, and a Marker pointing at a different parameter name.
The specific parameter names/values above are the recording's example of the catalog; the requirement is that the served catalog match the family's AWS defaults in set, order and per-parameter metadata, with the Marker following from the page.

## Steps to Reproduce
1. git_clone(), then replay_wire_corpus() against the emulator built from that checkout.
2. Replay fixture rds-db-parameter-group-lifecycle-and-engine-defaults to step task#10, which calls rds DescribeEngineDefaultParameters for the fixture's DBParameterGroupFamily with the recorded MaxRecords page size.
3. Compare the returned EngineDefaults.Parameters list against the recording: entries 0..7 match, entry 8 ('basedir') reports Source 'engine-default' with no ParameterValue, entry 9 onward is a different parameter at every index, and EngineDefaults.Marker decodes to a different parameter name than AWS returned.
