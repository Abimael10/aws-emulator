Issue-Source: trace-sweep-divergence
Issue-Trace: docdb-catalog-describe-db-engine-versions
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: docdb-catalog-describe-db-engine-versions docdb-catalog-describe-orderable-db-instance-options
Issue-Shape-Id: com.amazonaws.docdb#AmazonRDSv19
Issue-Key: docdb|DescribeDBEngineVersions|2xx->2xx|$.DescribeDBEngineVersionsResponse.DescribeDBEngineVersionsResult.DBEngineVersions.DBEngineVersion[*]
Issue-Key: docdb|DescribeOrderableDBInstanceOptions|2xx->2xx|$.DescribeOrderableDBInstanceOptionsResponse.DescribeOrderableDBInstanceOptionsResult.OrderableDBInstanceOptions.OrderableDBInstanceOption[*]
Issue-Commit: c3a7d511a5670169ecde182d84092cb0c8e6535e

# DocDB published reference catalogs diverge from AWS: engine-version list and orderable-instance-option list

> _Note: this issue was identified against the code base at commit `c3a7d511a5670169ecde182d84092cb0c8e6535e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBEngineVersions

## Description
The two read-only reference reports DocumentDB publishes — the engine-version catalog and the orderable-instance-option catalog — are both served from static tables in the emulator (`crate::state::engine_versions`, backed by `::service_common::docdb_engine_catalog`, and `crate::state::DOCDB_INSTANCE_FAMILIES` / `STORAGE_TYPES`), and both tables hold a different set of rows than AWS publishes for the `docdb` engine. Because every other DocDB report resolves against the same tables (the ops' own module docs say the projections exist so "no two reports can disagree"), the divergence is not cosmetic: a version or instance class AWS offers but the table omits is a version or class the emulator refuses to create with. 2 causes.

Cause 1 of 2: DescribeDBEngineVersions omits the docdb 3.6 engine-version family, so the whole published list diverges.
AWS: Smithy: DescribeDBEngineVersions -> DBEngineVersionMessage.DBEngineVersions (list of DBEngineVersion, paginated items=DBEngineVersions). The model defines the member set of each row (Engine, EngineVersion, MajorEngineVersion, MinorEngineVersion, DBParameterGroupFamily, DBEngineVersionDescription, ValidUpgradeTarget, SupportedCACertificateIdentifiers, SupportsCertificateRotationWithoutRestart, and the Supports* flags) but not WHICH rows exist, so the row set is recorded behavior only. The recording (HTTP 200) shows AWS publishing a list whose first row is the 3.6 family: Engine `docdb`, EngineVersion `3.6.0`, MajorEngineVersion `3.6`, MinorEngineVersion `0`, DBParameterGroupFamily `docdb3.6`, DBEngineVersionDescription `Amazon DocumentDB (with MongoDB compatibility)`, SupportsCertificateRotationWithoutRestart `true`, SupportedCACertificateIdentifiers `rds-ca-rsa4096-g1` and `rds-ca-rsa2048-g1`, every Supports* capability flag `false`, and a ValidUpgradeTarget list of major-version upgrade rows (5.0.0, 5.0.1, 5.0.2, 8.0.0, 8.0.1, each IsMajorVersionUpgrade `true`, AutoUpgrade `false`).
Emulator: answers 200 but that row is absent — `DBEngineVersion[0]: missing`. The replay reports every index of the list as divergent (DBEngineVersion[0] through DBEngineVersion[7]), i.e. dropping the 3.6 family shifts and changes the entire published catalog, not just one entry.
Why: a client that enumerates engine versions to pick one, or that pins the 3.6 family (a legitimate, still-offered DocumentDB major version), sees a catalog that does not contain it; and because the same table is what `resolve_engine_version` answers from, CreateDBCluster with EngineVersion `3.6.0` is refused as a version the engine cannot run, which on AWS succeeds. Upgrade-planning clients also lose the 3.6 -> 5.0/8.0 ValidUpgradeTarget edges.
Fix: the shared engine-version catalog table the DescribeDBEngineVersions handler reads (`::service_common::docdb_engine_catalog` engine rows, projected by `crate::state::engine_version_to_shape`) must publish the full set of versions AWS offers for `docdb`, including the `docdb3.6` family and its ValidUpgradeTarget edges, in AWS's order.
Traces: docdb-catalog-describe-db-engine-versions#task#1.

Cause 2 of 2: DescribeOrderableDBInstanceOptions publishes a different set of instance classes, so the published option list diverges from index 4 onwards.
AWS: Smithy: DescribeOrderableDBInstanceOptions (Engine required) -> OrderableDBInstanceOptionsMessage.OrderableDBInstanceOptions (list of OrderableDBInstanceOption, paginated items=OrderableDBInstanceOptions). As above, the model fixes each row's members (DBInstanceClass, Engine, EngineVersion, LicenseModel, StorageType, AvailabilityZones, Vpc, SupportsClusters, SupportsGlobalDatabases, SupportedEngineModes, SupportedNetworkTypes, OutpostCapable, SupportsKerberosAuthentication, SupportsDedicatedLogVolume, SupportsAdditionalStorageVolumes, SupportsHttpEndpoint, SupportedActivityStreamModes, ...) but not the row set, so the row set is recorded behavior only. The recording (HTTP 200) publishes, at the position in question, the option for DBInstanceClass `db.r5.24xlarge` on EngineVersion `5.0.0` with StorageType `standard`, LicenseModel `na`, Vpc `true`, SupportsClusters `true`, SupportsGlobalDatabases `true`, SupportedEngineModes `provisioned` and SupportedNetworkTypes `IPV4`/`DUAL`.
Emulator: answers 200 but the option at that position is a different class — `OrderableDBInstanceOption[4]: value_mismatch`, expected DBInstanceClass `db.r5.24xlarge`, got `db.r5.2xlarge` — and the replay reports indices 4 and 28 through 65 as divergent, i.e. the cross product of instance classes (and storage types) the emulator enumerates is not AWS's set in AWS's order. (The replay's `got` record is clipped, so the per-member delta beyond DBInstanceClass is not fully visible in the output; the certain divergence is the row set/ordering.)
Why: a client that selects a member instance class from this catalog cannot discover classes AWS offers (e.g. db.r5.24xlarge), and capacity-planning or validation code that cross-checks a requested class against the orderable catalog rejects orders AWS accepts.
Fix: the orderable-option catalog the DescribeOrderableDBInstanceOptions handler crosses (`crate::state::DOCDB_INSTANCE_FAMILIES` and `crate::state::STORAGE_TYPES`, projected by `crate::state::orderable_option_to_shape`) must enumerate the instance classes AWS publishes for `docdb`, in AWS's order.
Traces: docdb-catalog-describe-orderable-db-instance-options#task#1.

Neither cause is covered by an existing open issue: the open DocDB catalog-adjacent issue `20261117T135409Z-docdb-createdbcluster-defaults-engineversion-to-the-highest-minor.md` is about which version CreateDBCluster picks as a default, not about which rows the two reference reports publish.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 90.8%, 11 mismatched steps of 120).

Cause 1 — fixture docdb-catalog-describe-db-engine-versions, step task#1, DescribeDBEngineVersions, expected 200 / actual 200:
  $.DescribeDBEngineVersionsResponse.DescribeDBEngineVersionsResult.DBEngineVersions.DBEngineVersion[0]: missing
  expected: {Engine: 'docdb', EngineVersion: '3.6.0', MajorEngineVersion: '3.6', MinorEngineVersion: '0', DBParameterGroupFamily: 'docdb3.6', DBEngineVersionDescription: 'Amazon DocumentDB (with MongoDB compatibility)', SupportsCertificateRotationWithoutRestart: 'true', SupportedCACertificateIdentifiers: ['rds-ca-rsa4096-g1','rds-ca-rsa2048-g1'], SupportsReadReplica/SupportsCluster/SupportsGlobalDatabases/SupportsRDSProxy/SupportsParallelQuery/SupportsBabelfish/SupportsLimitlessDatabase/SupportsDataGroups: 'false', ValidUpgradeTarget: [5.0.0, 5.0.1, 5.0.2, 8.0.0, 8.0.1 ... IsMajorVersionUpgrade 'true']}
  actual: entry absent. Divergent keys: DBEngineVersion[0] .. DBEngineVersion[7] (the whole list).

Cause 2 — fixture docdb-catalog-describe-orderable-db-instance-options, step task#1, DescribeOrderableDBInstanceOptions, expected 200 / actual 200:
  $.DescribeOrderableDBInstanceOptionsResponse.DescribeOrderableDBInstanceOptionsResult.OrderableDBInstanceOptions.OrderableDBInstanceOption[4]: value_mismatch
  expected DBInstanceClass 'db.r5.24xlarge' (Engine 'docdb', EngineVersion '5.0.0', StorageType 'standard', LicenseModel 'na', Vpc 'true', SupportsClusters 'true', SupportsGlobalDatabases 'true', SupportedEngineModes 'provisioned', SupportedNetworkTypes ['IPV4','DUAL'])
  got DBInstanceClass 'db.r5.2xlarge'. Divergent keys: OrderableDBInstanceOption[4] and [28]..[65].

Both clusters carry empty stub_path and empty owned_by in the replay report, so neither is filed for us nor owned by an open issue.

Model evidence (services/service-com-amazonaws-docdb-amazonrdsv19/smithy-model/com-amazonaws-docdb-amazonrdsv19.json): DescribeDBEngineVersions output DBEngineVersions: DBEngineVersionList, paginated items=DBEngineVersions; DescribeOrderableDBInstanceOptions output OrderableDBInstanceOptions: OrderableDBInstanceOptionsList, paginated items=OrderableDBInstanceOptions; shapes DBEngineVersion and OrderableDBInstanceOption define the members quoted above. Emulator source: services/service-com-amazonaws-docdb-amazonrdsv19/src/ops/describe_db_engine_versions.rs lines 5-10 (reads crate::state::engine_versions, projected by engine_version_to_shape) and services/service-com-amazonaws-docdb-amazonrdsv19/src/ops/describe_orderable_db_instance_options.rs lines 5-9 (crosses crate::state::DOCDB_INSTANCE_FAMILIES with crate::state::STORAGE_TYPES); services/service-com-amazonaws-docdb-amazonrdsv19/src/state.rs lines 205-216 shows engine_versions() delegating to ::service_common::docdb_engine_catalog::engine_versions().

## Steps to Reproduce
Cause 1:
1. Boot the emulator from this checkout and replay fixture docdb-catalog-describe-db-engine-versions, step task#1 (POST to the docdb endpoint, Action=DescribeDBEngineVersions with Engine=docdb, awsQuery wire).
2. Read $.DescribeDBEngineVersionsResponse.DescribeDBEngineVersionsResult.DBEngineVersions.DBEngineVersion: no member has EngineVersion '3.6.0' / DBParameterGroupFamily 'docdb3.6', and the list's rows do not match the recorded list position for position.
3. Follow on: Action=CreateDBCluster with EngineVersion=3.6.0 is refused as an unknown engine version, because resolve_engine_version answers from the same table.

Cause 2:
1. Replay fixture docdb-catalog-describe-orderable-db-instance-options, step task#1 (Action=DescribeOrderableDBInstanceOptions, Engine=docdb).
2. Read $.DescribeOrderableDBInstanceOptionsResponse.DescribeOrderableDBInstanceOptionsResult.OrderableDBInstanceOptions.OrderableDBInstanceOption[4]: DBInstanceClass is 'db.r5.2xlarge' where the recording has 'db.r5.24xlarge'; the published class set contains no db.r5.24xlarge option, and indices 28..65 likewise do not line up with the recorded catalog.
