Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: opensearch.start-migration-invalid-source-version-rejected
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch|CreateDomain|InvalidTypeException
Issue-Commit: d295b4c3fda6f64da36c1fa9c1853a5bae45da9f

# opensearch: CreateDomain refuses the T2 burstable family on every OpenSearch release (InvalidTypeException "Invalid instance type: t2.small.search"), so an OpenSearch_2.x domain on t2 cannot be created at all

> _Note: this issue was identified against the code base at commit `d295b4c3fda6f64da36c1fa9c1853a5bae45da9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDomain

## Description
One net-new root cause in the opensearch control plane.

Cause 1 of 1: the emulator's instance-type catalog publishes the T2 burstable family as ELASTICSEARCH-ONLY, so CreateDomain refuses any OpenSearch_X.Y domain whose ClusterConfig$InstanceType is a t2 type, even though AWS provisions it. Failing check: opensearch.start-migration-invalid-source-version-rejected.

AWS: Smithy: CreateDomain input ClusterConfig$InstanceType targets OpenSearchPartitionInstanceType, and that enum — the OpenSearch (not the legacy Elasticsearch) service model — lists t2.micro.search, t2.small.search and t2.medium.search alongside the t3/t4g sizes; nothing in the model scopes those three values to the Elasticsearch engine, and CreateDomain's InvalidTypeException is documented as "An exception for trying to create or access a sub-resource that's either invalid or not supported". For the engine pairing this is recorded behavior only: the recording creates two managed domains in sequence with the identical ClusterConfig {InstanceType: t2.small.search, InstanceCount: 1, DedicatedMasterEnabled: false, ZoneAwarenessEnabled: false} and EBSOptions {EBSEnabled: true, VolumeType: gp2, VolumeSize: 10} — one on EngineVersion Elasticsearch_1.5 and one on EngineVersion OpenSearch_2.11 — and BOTH are accepted, each returning a DomainStatus with an ARN, which the check then feeds to StartMigration. So a t2 instance type is a valid cluster configuration on an OpenSearch release, not only on the legacy engine.

Emulator: the first (Elasticsearch_1.5) CreateDomain is accepted; the byte-identical call with EngineVersion=OpenSearch_2.11 is refused with InvalidTypeException and the message "Invalid instance type: t2.small.search" (fixed text "Invalid instance type: " plus the ClusterConfig$InstanceType value taken verbatim from the request). The cause is the catalog row for the family: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/instance_types.rs lines 764-769 declare the t2 family with releases: OfferedReleases::ElasticsearchOnly, whose offered_on() (same file, lines 230-237) answers !version.is_opensearch(); CreateDomain's cluster_instance_type_refusal gate (src/ops/create_domain.rs lines 537-544) therefore rejects every t2 type on every OpenSearch_X.Y version. The same catalog feeds DescribeInstanceTypeLimits, ListInstanceTypeDetails and UpdateDomainConfig, so all of them report the T2 family as unavailable to an OpenSearch domain too.

Why: a client that provisions the cheapest burstable dev/test domain — t2.small.search is the classic single-node sandbox size, and is still what plenty of templates, tutorials and CI fixtures pin — cannot create that domain at all against the emulator: CreateDomain hands back a terminal 4xx InvalidTypeException naming its instance type as invalid, so the caller concludes its configuration is wrong when AWS would have provisioned it. Everything downstream of such a domain is unreachable: in this check the entire StartMigration surface (the datasourceArn collection-ARN pattern refusal and the assertion that ListMigrations stays empty after a rejected StartMigration) is never exercised, because the fixture dies at domain setup.

Fix: the instance-type catalog's T2 row and the engine-release gate it carries (the CatalogFamily entry for "t2" in src/state/instance_types.rs) — the T2 family must be offered on OpenSearch releases as well as Elasticsearch ones, so CreateDomain's cluster_instance_type_refusal, UpdateDomainConfig's copy of it, and the per-release listings (offered_instance_types / DescribeInstanceTypeLimits / ListInstanceTypeDetails) all report t2.micro.search, t2.small.search and t2.medium.search as offered for an OpenSearch domain. The family's other published properties stay as they are (no encryption at rest / node-to-node encryption / FGAC via UNENCRYPTED_FAMILIES, no UltraWarm via BURSTABLE_FAMILIES, previous-generation catalog rank); only the engine-release scoping is wrong.

Traces: opensearch.start-migration-invalid-source-version-rejected (Cause 1). Code: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/instance_types.rs lines 190-237 (OfferedReleases / offered_on) and lines 759-769 (the t2 CatalogFamily row); src/ops/create_domain.rs lines 529-544 (the CreateDomain instance-type gate).

## Evidence
Behavioral suite replay against the emulator built from this checkout: 298/364 passed (81.9%), 58 mismatched, 8 errored. All but two of the failures are marked OWNED by an already-filed issue; this one is unowned.

### opensearch
[error] opensearch.start-migration-invalid-source-version-rejected
  botocore.errorfactory.InvalidTypeException: An error occurred (InvalidTypeException) when calling the CreateDomain operation: Invalid instance type: t2.small.search

  Expected (recorded against AWS): CreateDomain(DomainName=<compliant name>, EngineVersion="OpenSearch_2.11", ClusterConfig={InstanceType: "t2.small.search", InstanceCount: 1, DedicatedMasterEnabled: false, ZoneAwarenessEnabled: false}, EBSOptions={EBSEnabled: true, VolumeType: "gp2", VolumeSize: 10}, TagList=[...]) is ACCEPTED, HTTP 200, and returns DomainStatus with an ARN — exactly as the identical call with EngineVersion="Elasticsearch_1.5" (which the emulator does accept) is.
  Actual: HTTP 4xx InvalidTypeException with message "Invalid instance type: t2.small.search". Fixed text of that message: "Invalid instance type: "; variable part: the ClusterConfig$InstanceType value from the request. No domain is created, so the operation actually under test (StartMigration with a managed-domain ARN as migrationOptions.source.datasourceArn, which AWS refuses with ValidationException and the message "1 validation error detected: Value '<arn>' at 'migrationOptions.source.datasourceArn' failed to satisfy constraint: Member must satisfy regular expression pattern: arn:aws[a-z\-]*:aoss:[a-z0-9\-]+:[0-9]{12}:collection/[a-z0-9]+") and the follow-up ListMigrations assertion are never reached and remain unverified.
  Model support: OpenSearchPartitionInstanceType in services/service-com-amazonaws-opensearch-amazonopensearchservice/smithy-model/com-amazonaws-opensearch-amazonopensearchservice.json lists "t2.micro.search", "t2.small.search" and "t2.medium.search" as values of the enum ClusterConfig$InstanceType targets, with no engine-release qualification.

## Steps to Reproduce
1. git clone the repo and let the behavioral suite build and boot the emulator from it.
2. Call CreateDomain(DomainName="sim2real-t2-es15", EngineVersion="Elasticsearch_1.5", ClusterConfig={"InstanceType": "t2.small.search", "InstanceCount": 1, "DedicatedMasterEnabled": false, "ZoneAwarenessEnabled": false}, EBSOptions={"EBSEnabled": true, "VolumeType": "gp2", "VolumeSize": 10}) — the emulator ACCEPTS it and returns a DomainStatus/ARN.
3. Repeat the byte-identical call with a fresh DomainName and EngineVersion="OpenSearch_2.11" (any OpenSearch_X.Y release behaves the same). AWS accepts it; the emulator answers InvalidTypeException "Invalid instance type: t2.small.search" and creates nothing.
4. Confirm the source of the gate: services/service-com-amazonaws-opensearch-amazonopensearchservice/src/state/instance_types.rs lines 764-769 declare the "t2" family with releases: OfferedReleases::ElasticsearchOnly; offered_on() (lines 230-237) answers !version.is_opensearch() for it, and src/ops/create_domain.rs lines 537-544 turns that into the InvalidTypeException. ListInstanceTypeDetails/DescribeInstanceTypeLimits for an OpenSearch version likewise omit every t2 type.
5. After the fix, re-replay opensearch.start-migration-invalid-source-version-rejected — the domain setup must succeed on both engine versions and the check must then exercise StartMigration's datasourceArn pattern refusal and the empty ListMigrations assertion.
