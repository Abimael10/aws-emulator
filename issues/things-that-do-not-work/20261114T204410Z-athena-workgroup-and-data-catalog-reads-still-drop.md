Issue-Source: trace-sweep-divergence
Issue-Trace: athena-createworkgroup-required-and-optional-members
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: athena-createworkgroup-required-and-optional-members athena-get-work-group-configuration-and-missing-error athena-workgroup-lifecycle-and-query-execution athena-data-catalog-crud-lifecycle
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena|GetWorkGroup|2xx->2xx|$.WorkGroup.Configuration.EngineVersion.Category
Issue-Key: athena|GetWorkGroup|2xx->2xx|$.WorkGroup.Configuration.PublishCloudWatchMetricsEnabled
Issue-Key: athena|GetWorkGroup|2xx->2xx|$.WorkGroup.Configuration.QuerySchedulingType
Issue-Key: athena|GetDataCatalog|2xx->2xx|$.DataCatalog.Parameters.catalog
Issue-Key: athena|GetDataCatalog|2xx->2xx|$.DataCatalog.Parameters.metadata-function
Issue-Key: athena|GetDataCatalog|2xx->2xx|$.DataCatalog.Parameters.record-function
Issue-Related: 20261109T161941Z-athena-getworkgroup-omits-systemtablestatuses-and-enforceworkgroupconfiguration-and-over.md
Issue-Commit: a96ded234c891227942f65156e56a199ba16999f

# Athena workgroup and data-catalog reads still drop created configuration and server-side defaults (4 causes)

> _Note: this issue was identified against the code base at commit `a96ded234c891227942f65156e56a199ba16999f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetWorkGroup

## Description
Athena control-plane resources (workgroups, data catalogs) are still serialized from a lossy record: members AWS always serves in WorkGroup.Configuration are absent, two boolean workgroup defaults are materialized with the wrong value, and the Parameters map submitted to CreateDataCatalog is not readable back. This reproduces across every workgroup shape the corpus exercises (created with required members only, created with an explicit Configuration, and read after UpdateWorkGroup), so it is the GetWorkGroup/GetDataCatalog serializers and the create-time defaults, not one request's values. 4 causes.

NOTE ON RELATED ISSUES. (a) The RESOLVED issue 20261104T063259Z-athena-workgroup-and-data-catalog-reads-omit-modeled.md covered Configuration.EngineVersion.Category, Configuration.QuerySchedulingType, Configuration.PublishCloudWatchMetricsEnabled and the GetDataCatalog Parameters keys; it is no longer open and those members still diverge on replay, so causes 1, 2 and 4 below are live and unowned - a resolved issue covers nothing. (b) The OPEN issue 20261109T161941Z-athena-getworkgroup-omits-systemtablestatuses-and-enforceworkgroupconfiguration-and-over.md owns Configuration.SystemTableStatuses, the EnforceWorkGroupConfiguration omission and the WorkGroup.CreationTime over-precision, but these clusters replay with NO owner because their key sets have changed, so cause 3 is restated here for completeness and the CreationTime precision defect is deliberately NOT re-filed - it stays with that issue.

Cause 1 of 4: GetWorkGroup never serves Configuration.EngineVersion.Category or Configuration.QuerySchedulingType.
AWS: recorded behavior only - the on-disk Smithy model (com.amazonaws.athena#EngineVersion) declares only SelectedEngineVersion and EffectiveEngineVersion, and com.amazonaws.athena#WorkGroupConfiguration declares no QuerySchedulingType member, so the model says nothing about either. The recorded AWS responses return Configuration.EngineVersion.Category = 'Presto' and Configuration.QuerySchedulingType = 'DEFAULT' on every GetWorkGroup, for a workgroup created with required members only, for one created with an explicit Configuration, and for one read back after UpdateWorkGroup. Both are server-side effective values; neither is supplied by the request.
Emulator: both members are absent from the Configuration object entirely (expected 'Presto' / 'DEFAULT', got None), while the EngineVersion object itself is otherwise served.
Why: a client that reads effective workgroup settings cannot discover which engine family the workgroup runs on or how its queries are scheduled; code that branches on the engine Category (Presto vs Spark) treats the workgroup as untyped and takes the wrong branch, and a read-back of a workgroup is not equal to what AWS reports.
Fix: the athena crate's GetWorkGroup WorkGroupConfiguration serializer plus the workgroup configuration defaults that should materialize the engine Category and the scheduling type at create time.
Traces: athena-createworkgroup-required-and-optional-members#task#2 (representative), the same fixture's #task#4 and #task#6, athena-get-work-group-configuration-and-missing-error#task#2, athena-workgroup-lifecycle-and-query-execution#task#2 and #task#7.

Cause 2 of 4: a workgroup created with required members only defaults EnforceWorkGroupConfiguration and PublishCloudWatchMetricsEnabled to false, where AWS reports true.
AWS: Smithy: WorkGroupConfiguration.EnforceWorkGroupConfiguration targets BoxedBoolean and is documented as the workgroup-settings override ('If set to "true", the settings for the workgroup override client-side settings'); WorkGroupConfiguration.PublishCloudWatchMetricsEnabled targets BoxedBoolean and is documented as 'Indicates that the Amazon CloudWatch metrics are enabled for the workgroup'. The recorded response for a workgroup created by CreateWorkGroup WITHOUT a Configuration returns both members as true - these are AWS's create-time defaults, not request values.
Emulator: both members are present but false (expected True, got False).
Why: EnforceWorkGroupConfiguration decides whether a client's own ResultConfiguration/encryption settings are honoured or overridden by the workgroup, and PublishCloudWatchMetricsEnabled decides whether AWS/Athena metrics exist for it. A client reading false where AWS states true concludes its per-query overrides will be respected and that no workgroup metrics are published - the exact opposite of the real service - so enforcement-sensitive and metric-polling logic takes the wrong branch.
Fix: the athena crate's CreateWorkGroup configuration defaults (default both flags to enabled when the request supplies no Configuration), not the GetWorkGroup serializer, which already emits the members.
Traces: athena-createworkgroup-required-and-optional-members#task#2.

Cause 3 of 4: GetWorkGroup never serves Configuration.SystemTableStatuses.
AWS: recorded behavior only - com.amazonaws.athena#WorkGroupConfiguration declares no SystemTableStatuses member. The recorded AWS responses return Configuration.SystemTableStatuses as a one-element list carrying the QUERY_EXECUTIONS system table with Enabled false for a workgroup created without enabling it, on both recorded create shapes.
Emulator: the member is absent from the Configuration object (expected the one-entry QUERY_EXECUTIONS list with Enabled false, got None).
Why: a client reading effective workgroup settings cannot tell whether the workgroup's system tables are enabled; code that branches on the presence of the list treats the feature as unavailable rather than disabled.
Fix: the athena crate's CreateWorkGroup configuration defaults (materialize each system table's status with its default disabled state) and the GetWorkGroup serializer. This cause is also described by the open issue 20261109T161941Z-...-getworkgroup-omits-systemtablestatuses-...md; it is listed here because these clusters replay unowned, and that issue additionally owns the WorkGroup.CreationTime millisecond-precision defect which is NOT re-filed here.
Traces: athena-createworkgroup-required-and-optional-members#task#2 and #task#4 (and #task#6).

Cause 4 of 4: GetDataCatalog does not return the Parameters keys submitted to CreateDataCatalog.
AWS: Smithy: DataCatalog.Parameters targets ParametersMap and is documented as 'Specifies the Lambda function or functions to use for the data catalog'; for a LAMBDA catalog the documented parameter sets are metadata-function + record-function, or the composite function key. CreateDataCatalog accepts the map and AWS round-trips every key it was given, so GetDataCatalog returns the catalog's full Parameters map (the recorded catalog returns the catalog, metadata-function and record-function keys, the two function values being the connector Lambda ARN the request supplied).
Emulator: the keys the request supplied are missing from DataCatalog.Parameters (expected each submitted key present with the submitted value, got None for catalog, metadata-function and record-function), both on the read directly after create and on the read after UpdateDataCatalog.
Why: a client reading back a federated/LAMBDA catalog cannot discover which Lambda function serves metadata and which serves records, so connector wiring is unresolvable and any config-drift or idempotent-reconcile flow that compares desired Parameters with the read-back map diverges forever.
Fix: the athena crate's CreateDataCatalog/UpdateDataCatalog handlers and the data-catalog state that stores Parameters, plus the GetDataCatalog serializer that emits the map.
Traces: athena-data-catalog-crud-lifecycle#task#4 (representative) and #task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (37 fixtures replayed, emulator_up true). All four clusters are status 200 expected and 200 actual - the divergence is entirely in the response body - and all four replay with an EMPTY owned_by and an empty stub_path, i.e. no open issue carries their replay key.

Cluster A - athena-createworkgroup-required-and-optional-members#task#2 (workgroup created with required members only), divergence key service=athena|GetWorkGroup|status=200->200:
  $.WorkGroup.Configuration.EnforceWorkGroupConfiguration: value_mismatch (expected True, got False)
  $.WorkGroup.Configuration.PublishCloudWatchMetricsEnabled: value_mismatch (expected True, got False)
  $.WorkGroup.Configuration.EngineVersion.Category: missing (expected 'Presto', got None)
  $.WorkGroup.Configuration.QuerySchedulingType: missing (expected 'DEFAULT', got None)
  $.WorkGroup.Configuration.SystemTableStatuses: missing (expected [{'TableName': 'QUERY_EXECUTIONS', 'Enabled': False}], got None)
  $.WorkGroup.CreationTime: value_mismatch (expected the asserted epoch-seconds timestamp shape, got an epoch-seconds float with seven fractional digits) - owned by the open issue 20261109T161941Z-...; the instant itself is a per-run value and is not a requirement.

Cluster B - athena-createworkgroup-required-and-optional-members#task#4 (representative; also #task#6, workgroup created with an explicit Configuration), divergence key service=athena|GetWorkGroup|status=200->200:
  $.WorkGroup.Configuration.EngineVersion.Category: missing (expected 'Presto', got None)
  $.WorkGroup.Configuration.QuerySchedulingType: missing (expected 'DEFAULT', got None)
  $.WorkGroup.Configuration.SystemTableStatuses: missing (expected the same one-entry QUERY_EXECUTIONS list with Enabled false, got None)
  $.WorkGroup.CreationTime: same over-precision as cluster A (owned elsewhere).
  Note the two booleans of cause 2 do NOT diverge here, which is what shows the fault is the create-time default and not the serializer.

Cluster C - athena-get-work-group-configuration-and-missing-error#task#2 (representative; also athena-workgroup-lifecycle-and-query-execution#task#2 and #task#7, the latter a read after UpdateWorkGroup), divergence key service=athena|GetWorkGroup|status=200->200:
  $.WorkGroup.Configuration.EngineVersion.Category: missing (expected 'Presto', got None)
  $.WorkGroup.Configuration.QuerySchedulingType: missing (expected 'DEFAULT', got None)

Cluster D - athena-data-catalog-crud-lifecycle#task#4 (representative; also #task#6, the read after UpdateDataCatalog), divergence key service=athena|GetDataCatalog|status=200->200:
  $.DataCatalog.Parameters.catalog: missing (expected the submitted catalog name, got None)
  $.DataCatalog.Parameters.metadata-function: missing (expected the submitted connector Lambda ARN, got None)
  $.DataCatalog.Parameters.record-function: missing (expected the submitted connector Lambda ARN, got None)
  (The recorded ARN/account values are per-environment; the requirement is that each submitted key round-trips with the value the request gave.)

Model basis: smithy_query on services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json - WorkGroupConfiguration declares EnforceWorkGroupConfiguration and PublishCloudWatchMetricsEnabled (BoxedBoolean) but declares neither QuerySchedulingType nor SystemTableStatuses; EngineVersion declares only SelectedEngineVersion/EffectiveEngineVersion (no Category); DataCatalog.Parameters targets ParametersMap and CreateDataCatalog accepts Parameters.

## Steps to Reproduce
1. git clone the repository and boot the emulator built from this checkout.
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
3. Inspect fixture athena-createworkgroup-required-and-optional-members steps task#2 (representative), task#4 and task#6; athena-get-work-group-configuration-and-missing-error step task#2; athena-workgroup-lifecycle-and-query-execution steps task#2 and task#7; athena-data-catalog-crud-lifecycle steps task#4 and task#6.
4. Minimal manual repro (workgroups): CreateWorkGroup with only the required Name, then GetWorkGroup on that name - WorkGroup.Configuration must contain EngineVersion.Category 'Presto', QuerySchedulingType 'DEFAULT', a SystemTableStatuses list carrying the QUERY_EXECUTIONS entry with Enabled false, and both EnforceWorkGroupConfiguration and PublishCloudWatchMetricsEnabled true. Repeat with CreateWorkGroup supplying an explicit Configuration, and again after UpdateWorkGroup: Category and QuerySchedulingType must still be present.
5. Minimal manual repro (data catalogs): CreateDataCatalog with Type LAMBDA and Parameters {catalog=<name>, metadata-function=<lambda arn>, record-function=<lambda arn>}, then GetDataCatalog on that name - every submitted key must come back with the submitted value. Repeat after UpdateDataCatalog with the same Parameters map.
