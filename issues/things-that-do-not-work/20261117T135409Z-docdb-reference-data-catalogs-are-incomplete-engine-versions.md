Issue-Source: trace-sweep-divergence
Issue-Trace: docdb-catalog-describe-db-engine-versions
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: docdb-catalog-describe-db-engine-versions docdb-catalog-describe-event-categories docdb-catalog-describe-orderable-db-instance-options
Issue-Shape-Id: com.amazonaws.docdb#AmazonRDSv19
Issue-Key: docdb|DescribeDBEngineVersions|2xx->2xx|$.DescribeDBEngineVersionsResponse.DescribeDBEngineVersionsResult.DBEngineVersions.DBEngineVersion[*]
Issue-Key: docdb|DescribeEventCategories|2xx->2xx|$.DescribeEventCategoriesResponse.DescribeEventCategoriesResult.EventCategoriesMapList.EventCategoriesMap[*]
Issue-Key: docdb|DescribeOrderableDBInstanceOptions|2xx->2xx|$.DescribeOrderableDBInstanceOptionsResponse.DescribeOrderableDBInstanceOptionsResult.OrderableDBInstanceOptions.OrderableDBInstanceOption[*]
Issue-Commit: f8316590bcf97b65fd5e64b3c988acfd983e0aed

# DocDB reference-data catalogs are incomplete: engine versions, event categories and orderable instance options all diverge

> _Note: this issue was identified against the code base at commit `f8316590bcf97b65fd5e64b3c988acfd983e0aed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBEngineVersions

## Description
Three DocDB reference-data ("catalog") reports are built from static tables in the service crate that do not match the catalog AWS publishes. All three answer 200 with a well-formed envelope, so the divergence is purely in the contents of the published list: entries AWS publishes are absent, and the entries that are present are not the ones at those list positions. This issue bundles 3 distinct causes, one per catalog table. ALL THREE must be closed.

=== Cause 1 of 3: DescribeDBEngineVersions publishes an incomplete engine-version catalog (fixture docdb-catalog-describe-db-engine-versions, step task#1) ===
AWS: Smithy: DescribeDBEngineVersions -> DBEngineVersionMessage.DBEngineVersions (list of DBEngineVersion), no modeled errors; DBEngineVersion carries Engine, EngineVersion, MajorEngineVersion, MinorEngineVersion, DBParameterGroupFamily, DBEngineVersionDescription, SupportedCACertificateIdentifiers, SupportsCertificateRotationWithoutRestart, SupportsGlobalDatabases, SupportsReadReplica, SupportsCluster, SupportsRDSProxy, SupportsParallelQuery, SupportsBabelfish, SupportsLimitlessDatabase, SupportsDataGroups and ValidUpgradeTarget. The published set of versions itself is recorded behavior only: the recording shows AWS enumerating every DocumentDB engine version the account can order, each of the older major versions carrying ValidUpgradeTarget rows for the newer majors it can be upgraded to (the recorded response starts with the docdb 3.6 family — DBParameterGroupFamily docdb3.6, MajorEngineVersion 3.6, MinorEngineVersion 0 — whose ValidUpgradeTarget enumerates the 5.0.x and 8.0.x targets).
Emulator: the catalog it serves is shorter and differently ordered, so eight list positions diverge: $.DescribeDBEngineVersionsResponse.DescribeDBEngineVersionsResult.DBEngineVersions.DBEngineVersion[0] through [7]. Position [0] is simply absent in the emulator's answer (the whole older-major record AWS publishes there, including its ValidUpgradeTarget list, is missing), and the following positions are therefore occupied by the wrong records.
Why: a client that enumerates engine versions to choose one, or that reads ValidUpgradeTarget to plan a major-version upgrade, sees a catalog in which the older major family does not exist. Version-selection logic picks a version that is not the one AWS would offer, and upgrade-path discovery finds no upgrade targets at all, so an upgrade workflow cannot be modelled against the emulator.
Fix: the engine-version catalog table that backs the operation's handler — `crate::state::engine_versions`, projected by `crate::state::engine_version_to_shape` — must publish the same set of engine-version records (and, per record, the same ValidUpgradeTarget set) that AWS publishes, in AWS's order.
Traces: docdb-catalog-describe-db-engine-versions#task#1. Handler and catalog: services/service-com-amazonaws-docdb-amazonrdsv19/src/ops/describe_db_engine_versions.rs (header lines 3-10 name `crate::state::engine_versions` / `engine_version_to_shape`).

=== Cause 2 of 3: DescribeEventCategories omits whole event source types (fixture docdb-catalog-describe-event-categories, step task#1) ===
AWS: Smithy: DescribeEventCategories -> EventCategoriesMessage.EventCategoriesMapList (list of EventCategoriesMap{SourceType, EventCategories}), no modeled errors. Which source types are published is recorded behavior only: the recording shows AWS publishing a map for every event source type on this (shared RDS) wire, each with that source type's category names — including the newer source types blue-green-deployment (creation, failure, deletion, notification), db-shard-group (creation, deletion, availability, maintenance, failure, notification) and zero-etl (creation, deletion, failure, notification).
Emulator: its EventCategoriesMapList stops short — positions [8], [9] and [10] are absent entirely (got None), i.e. the three source types above are not published at all.
Why: a client that validates a requested event category against the categories published for a source type, or that builds a subscription UI/plan from this report, concludes those source types do not exist and cannot subscribe to them; validation code keyed on this catalog rejects source types AWS accepts.
Fix: the event-category catalog table that backs the operation's handler — `crate::state::EVENT_CATEGORIES` — must carry one map per event source type AWS publishes, with that source type's full category list.
Traces: docdb-catalog-describe-event-categories#task#1. Handler and catalog: services/service-com-amazonaws-docdb-amazonrdsv19/src/ops/describe_event_categories.rs (header lines 3-8 name `crate::state::EVENT_CATEGORIES`).

=== Cause 3 of 3: DescribeOrderableDBInstanceOptions publishes a different option matrix (fixture docdb-catalog-describe-orderable-db-instance-options, step task#1) ===
AWS: Smithy: DescribeOrderableDBInstanceOptions (Engine required) -> OrderableDBInstanceOptionsMessage.OrderableDBInstanceOptions (list of OrderableDBInstanceOption), no modeled errors. Each option carries Engine, EngineVersion, DBInstanceClass, LicenseModel, AvailabilityZones, Vpc, StorageType plus the capability members (SupportsStorageEncryption, SupportsIops, SupportsStorageThroughput, SupportsEnhancedMonitoring, SupportsIAMDatabaseAuthentication, SupportsPerformanceInsights, SupportsClusters, SupportsGlobalDatabases, SupportsHttpEndpoint, SupportsOptimizedReads/Writes, SupportsKerberosAuthentication, SupportsDedicatedLogVolume, SupportsAdditionalStorageVolumes, OutpostCapable, RequiresCustomProcessorFeatures, SupportedEngineModes, SupportedActivityStreamModes, SupportedNetworkTypes, MultiAZCapable, ReadReplicaCapable, AvailableProcessorFeatures). Which rows exist is recorded behavior only: the recording shows AWS crossing each engine version with each orderable instance class AND each supported storage type (the recorded row at position [1] is an option whose StorageType is the IO-optimized one and whose SupportsGlobalDatabases/SupportsClusters are true), so the matrix is larger than one row per instance class.
Emulator: its matrix is built as engine versions crossed with a fixed instance-class list only, so ~58 list positions diverge (keys [1], [3]..[5], [7], [9], [11], [13]..[27], [29]..[59], [61]..[65]). At position [1] the emulator serves a different instance class and the default storage type where AWS serves the IO-optimized storage-type row, i.e. the rows are a different set in a different order rather than one field being wrong.
Why: a client that enumerates orderable options to pick a valid (instance class, storage type) pair before CreateDBInstance/CreateDBCluster never sees the storage-type variants AWS offers and sees instance classes at positions AWS does not, so capacity-planning and any "is this class orderable" pre-check disagree with AWS and a request the emulator's catalog says is impossible is in fact valid (and vice versa).
Fix: the orderable-option catalog in the operation's handler — the cross product of `crate::state::engine_versions` with `crate::state::DOCDB_INSTANCE_FAMILIES`, projected by `crate::state::orderable_option_to_shape` — must enumerate the same rows AWS does, including one row per supported storage type, in AWS's order.
Traces: docdb-catalog-describe-orderable-db-instance-options#task#1. Handler and catalog: services/service-com-amazonaws-docdb-amazonrdsv19/src/ops/describe_orderable_db_instance_options.rs (header lines 3-8 name `crate::state::DOCDB_INSTANCE_FAMILIES` / `orderable_option_to_shape`).

No open issue covers these three catalog tables: the open DocDB issues concern endpoints, DBInstance projection, 400 message text and DocDB Elastic, and 20261111T131231Z-service-quotas-... is about Service Quotas' own default-quota catalog, not DocDB's engine/event/option catalogs.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator (score 90.0%, 12 mismatched steps of 120):

Cause 1 — docdb-catalog-describe-db-engine-versions#task#1, DescribeDBEngineVersions, expected 200 / actual 200:
  $.DescribeDBEngineVersionsResponse.DescribeDBEngineVersionsResult.DBEngineVersions.DBEngineVersion[0]: missing (expected a docdb 3.6 record — DBParameterGroupFamily 'docdb3.6', MajorEngineVersion '3.6', MinorEngineVersion '0', DBEngineVersionDescription 'Amazon DocumentDB (with MongoDB compatibility)', SupportedCACertificateIdentifiers [rds-ca-rsa4096-g1, rds-ca-rsa2048-g1], SupportsCertificateRotationWithoutRestart 'true', ValidUpgradeTarget rows for 5.0.0/5.0.1/5.0.2/8.0.0/8.0.1..., got None)
  diverging keys: DBEngineVersion[0] .. DBEngineVersion[7] (8 positions)

Cause 2 — docdb-catalog-describe-event-categories#task#1, DescribeEventCategories, expected 200 / actual 200:
  EventCategoriesMap[8]: missing (expected SourceType 'blue-green-deployment', EventCategories [creation, failure, deletion, notification], got None)
  EventCategoriesMap[9]: missing (expected SourceType 'db-shard-group', EventCategories [creation, deletion, availability, maintenance, failure, notification], got None)
  EventCategoriesMap[10]: missing (expected SourceType 'zero-etl', EventCategories [creation, deletion, failure, notification], got None)

Cause 3 — docdb-catalog-describe-orderable-db-instance-options#task#1, DescribeOrderableDBInstanceOptions, expected 200 / actual 200:
  OrderableDBInstanceOption[1]: expected an option with StorageType 'iopt1', DBInstanceClass 'db.r5.12xlarge', SupportsClusters 'true', SupportsGlobalDatabases 'true', SupportedEngineModes ['provisioned'], SupportedNetworkTypes ['IPV4','DUAL'], OutpostCapable 'false'; got an option with DBInstanceClass 'db.r5.16xlarge', StorageType 'standard'
  diverging keys: positions [1], [3], [4], [5], [7], [9], [11], [13]-[27], [29]-[59], [61]-[65] (~58 positions)

(Instance-class names, version strings and AZ names above are the recorded run's values filling the catalog template; the requirement is that the emulator publish the same set and order of rows AWS publishes, not these literals.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
2. Cause 1: replay fixture docdb-catalog-describe-db-engine-versions step task#1 — POST the docdb query-protocol Action=DescribeDBEngineVersions with Engine=docdb and compare DBEngineVersions entry-by-entry with the recorded AWS response; entry [0] (the older major family and its ValidUpgradeTarget list) is absent.
3. Cause 2: replay fixture docdb-catalog-describe-event-categories step task#1 — Action=DescribeEventCategories; the EventCategoriesMapList lacks the blue-green-deployment, db-shard-group and zero-etl source types at positions [8]-[10].
4. Cause 3: replay fixture docdb-catalog-describe-orderable-db-instance-options step task#1 — Action=DescribeOrderableDBInstanceOptions&Engine=docdb; the published matrix has no per-storage-type rows, so ~58 positions hold different options than AWS's.
5. Each step answers HTTP 200 with a valid envelope; the divergence is only in the list contents.
