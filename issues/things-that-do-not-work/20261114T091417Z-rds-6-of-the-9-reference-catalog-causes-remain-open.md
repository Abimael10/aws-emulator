Issue-Source: trace-sweep-divergence
Issue-Trace: rds-catalog-describe-orderable-db-instance-options-db-c6gd-12xlarge-db-r5d-4xlarge
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-catalog-describe-orderable-db-instance-options-db-c6gd-12xlarge-db-r5d-4xlarge rds-catalog-describe-orderable-db-instance-options-db-r5d-8xlarge-db-x2iedn-xlarge rds-catalog-describe-db-major-engine-versions rds-cluster-parameter-group-mutation-envelopes rds-db-parameter-group-lifecycle rds-db-parameter-group-lifecycle-and-engine-defaults
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeOrderableDBInstanceOptions|2xx->2xx|$.DescribeOrderableDBInstanceOptionsResponse.DescribeOrderableDBInstanceOptionsResult.OrderableDBInstanceOptions
Issue-Key: rds|DescribeDBMajorEngineVersions|2xx->2xx|$.DescribeDBMajorEngineVersionsResponse.DescribeDBMajorEngineVersionsResult.DBMajorEngineVersions.DBMajorEngineVersion[*]
Issue-Key: rds|DescribeDBClusterParameters|2xx->2xx|$.DescribeDBClusterParametersResponse.DescribeDBClusterParametersResult.Parameters.Parameter[*]
Issue-Key: rds|DescribeDBParameters|2xx->2xx|$.DescribeDBParametersResponse.DescribeDBParametersResult.Parameters.Parameter[*]
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*]
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Marker
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].AllowedValues
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].ApplyType
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].DataType
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].Description
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].ParameterName
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].ParameterValue
Issue-Key: rds|DescribeEngineDefaultParameters|2xx->2xx|$.DescribeEngineDefaultParametersResponse.DescribeEngineDefaultParametersResult.EngineDefaults.Parameters.Parameter[*].Source
Issue-Key: rds|DescribeEngineDefaultClusterParameters|2xx->2xx|$.DescribeEngineDefaultClusterParametersResponse.DescribeEngineDefaultClusterParametersResult.EngineDefaults.Marker
Issue-Key: rds|DescribeEngineDefaultClusterParameters|2xx->2xx|$.DescribeEngineDefaultClusterParametersResponse.DescribeEngineDefaultClusterParametersResult.EngineDefaults.Parameters.Parameter[*]
Issue-Key: rds|DescribeEngineDefaultClusterParameters|2xx->2xx|$.DescribeEngineDefaultClusterParametersResponse.DescribeEngineDefaultClusterParametersResult.EngineDefaults.Parameters.Parameter[*].AllowedValues
Issue-Key: rds|DescribeEngineDefaultClusterParameters|2xx->2xx|$.DescribeEngineDefaultClusterParametersResponse.DescribeEngineDefaultClusterParametersResult.EngineDefaults.Parameters.Parameter[*].ApplyType
Issue-Key: rds|DescribeEngineDefaultClusterParameters|2xx->2xx|$.DescribeEngineDefaultClusterParametersResponse.DescribeEngineDefaultClusterParametersResult.EngineDefaults.Parameters.Parameter[*].DataType
Issue-Key: rds|DescribeEngineDefaultClusterParameters|2xx->2xx|$.DescribeEngineDefaultClusterParametersResponse.DescribeEngineDefaultClusterParametersResult.EngineDefaults.Parameters.Parameter[*].Description
Issue-Key: rds|DescribeEngineDefaultClusterParameters|2xx->2xx|$.DescribeEngineDefaultClusterParametersResponse.DescribeEngineDefaultClusterParametersResult.EngineDefaults.Parameters.Parameter[*].MinimumEngineVersion
Issue-Key: rds|DescribeEngineDefaultClusterParameters|2xx->2xx|$.DescribeEngineDefaultClusterParametersResponse.DescribeEngineDefaultClusterParametersResult.EngineDefaults.Parameters.Parameter[*].ParameterName
Issue-Key: rds|ModifyDBClusterParameterGroup|4xx->2xx|$.ErrorResponse
Issue-Key: rds|ModifyDBClusterParameterGroup|4xx->2xx|status
Issue-Related: 20261114T091417Z-rds-reference-catalogs-and-parameter-metadata-are-emulator.md

# RDS: 6 of the 9 reference-catalog causes are still open — the parameter, major-version and option catalogs need AWS's published content, which no rule over the model, the request or stored state yields

> _Split out of `issues/things-that-now-work/20261114T091417Z-rds-reference-catalogs-and-parameter-metadata-are-emulator.md`, whose cause 1 (the orderable-options list shape) is fixed and whose causes 3 and 9 replay clean. This report carries the replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what that fix established about them; a later fixer starts from here rather than from scratch._

## Operation
DescribeEngineDefaultParameters

## Description
What the parent fix landed, so a later fixer does not redo it:
`DescribeOrderableDBInstanceOptions` no longer answers an empty
`<OrderableDBInstanceOptions/>` for a release the engine-version catalogue samples no row
for. The version resolution now lives in `described_versions`
(`services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_orderable_db_instance_options.rs`):
an exact release is described as before, a deprecated one and a line name describe nothing,
and a plain release number inside a published line whose releases are plain numbers is
described with that line's capability profile — because this catalogue is a documented
SAMPLE of the live version list and absence from it is no denial
(`crate::state::check_engine_version_form` states that rule for the whole service). The
engines of the sibling control planes keep the strict reading, since their version list is
one the service itself publishes.

Six causes remain. Five of them are ONE problem — the CONTENT of the reference tables this
emulator publishes — and the sixth is a validation that cannot be implemented against a
sampled table without refusing requests AWS accepts.

Cause 1 of 6 (parent cause 2): `DescribeDBMajorEngineVersions` reports the wrong major
versions past index 12, with the wrong support calendars.
AWS: the `(Engine, MajorEngineVersion)` pairs, their order and each
`SupportedEngineLifecycle`'s name and start/end dates are AWS catalog facts.
Emulator: the first 13 entries now match the recording unchanged (the parent report's
claim that index 1 should carry aurora-mysql 8.0 `2021-11-18 -> 2028-04-30` is wrong: the
recording carries the `2022-01-27 -> 2026-10-31` / `2029-10-31` this emulator serves, and
that step passes). Divergence begins at index 13, the first MariaDB family, and runs over
the MariaDB, MySQL and PostgreSQL rows: which major versions AWS still lists, in what order,
and each one's release/standard-support-end/extended-support-end days.
Where: `LIFECYCLE_CALENDAR` and `families()` in
`src/ops/describe_db_major_engine_versions.rs`, which derives the pairs from the
engine-version catalogue, so a family added or dropped there moves this report too.
Traces: rds-catalog-describe-db-major-engine-versions#task#1.

Cause 2 of 6 (parent cause 5): `DescribeDBParameters` publishes a different MySQL parameter
set from index 10 on, with different `Source`, `IsModifiable`, `Description` and
`ParameterValue` columns.
AWS: which parameters a family has, and each one's published columns, are engine catalog
facts; `Parameter$Source` takes `engine-default`, `system` and `user`, and this emulator
never reports `system` at all — a value the SERVICE imposes (an installation path) is
reported as an engine default instead.
Emulator: the recorded page has `bind_address` (value `*`) at index 11 where this emulator
has `binlog_cache_size`, and `big_tables` is reported modifiable with an explanatory
description where the recording reports it unmodifiable with none.
Where: `MYSQL_PARAMETERS` in `src/ops/describe_engine_default_parameters.rs` (the catalog a
family's report is a window over) and `engine_default_parameter_to_wire` in
`src/state/parameter_groups.rs` (the projection, which has no `system` source).
Traces: rds-db-parameter-group-lifecycle#task#3.

Cause 3 of 6 (parent cause 6): `DescribeEngineDefaultParameters` publishes different
descriptions, `MinimumEngineVersion`s and defaults for the MySQL engine defaults.
AWS: an engine default's `Description`, `ParameterValue` and `MinimumEngineVersion` are the
engine catalog's values, and a default AWS does not pin carries NO `ParameterValue`.
Emulator: the paraphrases the parent report quoted are already corrected; what remains is
the parameter SET and the remaining columns of it.
Where: the same `MYSQL_PARAMETERS` table.
Traces: rds-db-parameter-group-lifecycle#task#4.

Cause 4 of 6 (parent cause 7): `DescribeEngineDefaultParameters` returns a `Marker` naming
an earlier parameter than the recorded page boundary.
This is NOT a pagination defect: `crate::state::paginate_parameters` already derives the
marker from the LAST record emitted in the page, and a test pins the head of each MySQL
family's page (`tests/all/engine_default_catalog_head.rs`). The marker differs only because
the catalog's membership does, so it closes with cause 2/3 and not before.
Traces: rds-db-parameter-group-lifecycle-and-engine-defaults#task#10.

Cause 5 of 6 (parent cause 8): `DescribeEngineDefaultClusterParameters` omits the `Marker`
and publishes a different Aurora MySQL cluster parameter set.
The `Marker` is absent because the published cluster catalog is SHORTER than one page, not
because the boundary is miscomputed: the handler emits the token whenever records remain,
and `SupportedEngineModes` is already `provisioned` alone. The recorded page is 20
parameters ending at `aws_default_sagemaker_role`, i.e. the engine's `aurora_*` and
`aws_default_*` cluster parameters, of which this emulator publishes two.
Where: `MYSQL_CLUSTER_PARAMETERS` in
`src/ops/describe_engine_default_cluster_parameters.rs`.
Traces: rds-db-parameter-group-lifecycle-and-engine-defaults#task#11.

Cause 6 of 6 (parent cause 4, second half): `ModifyDBClusterParameterGroup` stores a
parameter name the family publishes nothing for, and `DescribeDBClusterParameters` then
reports it as a user-sourced parameter with no metadata.
AWS: 400 `InvalidParameterValue`, `Could not find parameter with name: <name>` — the
recording confirms it, and the two description divergences the parent report cites for the
same step already match.
Why it is BLOCKED, and on what: the refusal needs the family's COMPLETE parameter list. It
was implemented during the parent fix (every `ParameterName` of a modify or reset resolved
against `published_cluster_parameter_at_any_level`, refused with
`service_common::rds_cluster_parameters::parameter_not_found_message`) and reverted,
because the published catalogs are documented samples of hundreds of engine variables: it
refused `aws_default_s3_role` — a real Aurora MySQL cluster parameter this emulator's own
S3-integration data plane sets — along with `time_zone` and `autovacuum_naptime`, breaking
14 of the RDS crate's own tests. A 400 on a modify AWS accepts is a worse divergence than a
stored phantom parameter, so this closes with the parameter catalogs (cause 2/3/5) and not
separately.
Traces: rds-cluster-parameter-group-mutation-envelopes#task#5 and #task#6.

## Also open, and established by the parent fix

* `DescribeOrderableDBInstanceOptions` now returns the member-wrapped option list, but the
  recorded option's CONTENT still diverges inside `OrderableDBInstanceOption[0]`. The parent
  report quotes `MinStorageSize` `5` for mysql/`gp2` there, which contradicts the published
  "Amazon RDS DB instance storage" table (General Purpose SSD from 20 GiB) and the
  doc-sourced table this crate's `tests/all/orderable_db_instance_options.rs` pins, so it
  was not written in. A fixer with the recorded body should settle that one member first.
* A report-versus-create asymmetry the parent fix introduced deliberately: this report
  describes a plain release inside a published line, while
  `describe_db_engine_versions::resolve_version` still refuses that same string for
  `CreateDBInstance` ("Cannot find version <v> for <engine>"), because a create has to name
  a release this emulator can stand up. Closing it either way — extending the sampled-line
  reading to the create, or narrowing the report — belongs with whichever recording settles
  what AWS does for a create naming a release outside its catalog.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout.
2. Replay trace set simparity-wire-aws-v1.
3. Cause 1: `rds-catalog-describe-db-major-engine-versions#task#1` — compare
   `DBMajorEngineVersion` from index 13 on, including each lifecycle's name and dates.
4. Causes 2 and 3: `rds-db-parameter-group-lifecycle#task#3` and `#task#4` — compare the
   parameter names from index 10 on and each one's `Source`, `IsModifiable`, `Description`
   and `ParameterValue`.
5. Causes 4 and 5: `rds-db-parameter-group-lifecycle-and-engine-defaults#task#10` and
   `#task#11` — the page boundary follows from the catalog membership, so compare the
   parameter sets first and the `Marker` after.
6. Cause 6: `rds-cluster-parameter-group-mutation-envelopes#task#5` — the modify must answer
   400 `InvalidParameterValue` for a name the family publishes nothing for, without refusing
   a name the engine really has.
