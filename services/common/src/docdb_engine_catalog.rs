// SPDX-License-Identifier: CC0-1.0

//! The Amazon DocumentDB engine catalog the ONE `rds` front door publishes: its engine
//! versions, what a version of it can do, and what an instance of it can be ordered with.
//!
//! Amazon DocumentDB is reached through two views of that one front door — the Amazon RDS
//! view, which an SDK DocumentDB client reaches because it signs `rds`, and the DocumentDB
//! view, reached under its own routing key — and both answer the SAME published catalog for
//! this engine: `DescribeDBEngineVersions` reports the same versions with the same
//! capability flags, and `DescribeOrderableDBInstanceOptions` the same per-option
//! capabilities, whichever view a request arrives at. The facts therefore live here, once,
//! rather than in a table per view: the copies drifted, and a client read a version list,
//! an upgrade path and a capability set that depended on how it addressed the door.
//!
//! Each projection stays with its view, because each view has its own wire shapes; what is
//! shared is the DATA.

/// `Engine` for every row of this catalog: `CreateDBClusterMessage$Engine` in the
/// DocumentDB model states "Valid values: docdb".
pub const ENGINE: &str = "docdb";

/// `DBEngineDescription` — the product's own full name, which is also the description every
/// version of it and every upgrade target onto it carries: the DocumentDB control plane
/// publishes no per-version description of its own, so a version of this engine is
/// described by the product rather than by the product and its version number.
pub const ENGINE_DESCRIPTION: &str = "Amazon DocumentDB (with MongoDB compatibility)";

/// `LicenseModel` of an orderable option of this engine: the licence setting does not apply
/// to Amazon DocumentDB — the model declares no `LicenseModel` on `CreateDBInstance` at all
/// — and an option of an engine AWS licenses itself carries `na`.
pub const LICENSE_MODEL: &str = "na";

/// The engine version a cluster created without an `EngineVersion` runs: the release this
/// catalog DESIGNATES as the engine's own default ([`VersionDefault::Engine`]), read off the
/// rows below so the designation is stated once.
///
/// `CreateDBClusterMessage$EngineVersion` says "the --engine-version will default to the
/// latest major engine version", which names the LINE; which release of that line a create
/// gets is the designation the rows carry, not the newest release the list happens to hold
/// (see [`VersionDefault`]). The 8.0 family is published because it is named as an upgrade
/// target rather than on an engine-version listing of its own, which is evidence that the
/// family is SERVED and no evidence of what a create without a version gets, so the default
/// stays on the 5.0 line until an engine-version listing of 8.0 says otherwise.
pub const DEFAULT_ENGINE_VERSION: &str = engine_default_version();

/// [`DEFAULT_ENGINE_VERSION`], read off the one row designated the engine's default.
const fn engine_default_version() -> &'static str {
    let mut index = 0;
    while index < ENGINE_VERSIONS.len() {
        let row = &ENGINE_VERSIONS[index];
        if row.default_of.is_engine_default() {
            return row.version;
        }
        index += 1;
    }
    panic!("one published release of this engine is designated its default version")
}

/// Which DEFAULT a published release is: the engine's own default version, the default of
/// its own major line, or neither.
///
/// The designation is published DATA about a release, carried one column per row, rather
/// than the maximum of the list: AWS promotes a release to be a default, and a later patch
/// of the same line is published — orderable, and reachable as a minor upgrade target —
/// before it is promoted. So a request that does not name an exact release (it names none,
/// or names only a line) runs the designated default, which can be an EARLIER release than
/// the newest one its line publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionDefault {
    /// The engine's own default version — and so the default of its line too, since the
    /// release a create with no `EngineVersion` runs is a release of the default line.
    Engine,
    /// The default release of its own major line, but not the engine's default: what a
    /// request naming only that line resolves to, and where an automatic minor version
    /// upgrade inside the line lands.
    Line,
    /// Neither: a published release AWS has not designated a default.
    Neither,
}

impl VersionDefault {
    /// Whether this release is the engine's own default version.
    pub const fn is_engine_default(self) -> bool {
        matches!(self, Self::Engine)
    }

    /// Whether this release is the default of its own major line.
    pub const fn is_line_default(self) -> bool {
        matches!(self, Self::Engine | Self::Line)
    }
}

/// The first MAJOR version Amazon DocumentDB offers an in-place major version upgrade TO.
///
/// "Upgrading your Amazon DocumentDB cluster's engine version" (Amazon DocumentDB Developer
/// Guide) publishes the in-place major version upgrade onto the 5.0 engine and later; the
/// recorded upgrade-target list of the oldest row names the 5.0 and 8.0 versions and NOT the
/// 4.0 one, which is the same rule read off the wire. A client plans its upgrade from
/// [`upgrade_targets`], so a major line this engine does not upgrade in place to is not in
/// it.
pub const FIRST_IN_PLACE_UPGRADE_MAJOR: &str = "5.0";

/// One engine version this catalog publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineVersion {
    /// `EngineVersion` (`5.0.0`).
    pub version: &'static str,
    /// `MajorEngineVersion` — the line a MINOR upgrade moves within (`5.0`).
    pub major: &'static str,
    /// `MinorEngineVersion` — this release within its line (`0`).
    pub minor: &'static str,
    /// `DBParameterGroupFamily` — the engine name followed by the line (`docdb5.0`).
    pub family: &'static str,
    /// Which default this release is designated; see [`VersionDefault`].
    pub default_of: VersionDefault,
}

/// The engine versions this front door publishes for Amazon DocumentDB, OLDEST FIRST,
/// which is the order the report publishes them in (the catalog's own, oldest major first).
///
/// The 3.6, 4.0 and 5.0 lines are the ones the Amazon DocumentDB release notes publish; the
/// later 5.0 releases and the 8.0 line are published because the recorded engine-version
/// report names them as the versions reachable from the oldest row, which is evidence the
/// front door serves them. The 4.0 line carries a second release for the same evidence read
/// the other way round: the recorded report publishes EIGHT engine-version records, of which
/// the oldest row's own upgrade targets ([`upgrade_targets`]) name five and the two oldest
/// lines account for two more — and a record no upgrade-target list can name is a later
/// release of a line BELOW [`FIRST_IN_PLACE_UPGRADE_MAJOR`], since no in-place upgrade
/// reaches one. The 4.0 line is the only such line with a successor, so its next release is
/// that eighth record.
///
/// A version this list does not carry is a version the engine cannot run, so every report
/// that resolves an `EngineVersion` reads it from here.
///
/// The DEFAULT column is each release's own designation ([`VersionDefault`]). The engine's
/// default is the 5.0 line's FIRST release: the recorded create that named no
/// `EngineVersion` provisioned it while that line already published later patches, so those
/// later patches are published without being designated. A line nothing records an
/// undesignated later patch of carries its designation on its newest release, which is
/// where an automatic minor version upgrade inside the line lands.
pub const ENGINE_VERSIONS: &[EngineVersion] = &[
    version("3.6.0", "3.6", "0", "docdb3.6", VersionDefault::Line),
    version("4.0.0", "4.0", "0", "docdb4.0", VersionDefault::Neither),
    version("4.0.1", "4.0", "1", "docdb4.0", VersionDefault::Line),
    version("5.0.0", "5.0", "0", "docdb5.0", VersionDefault::Engine),
    version("5.0.1", "5.0", "1", "docdb5.0", VersionDefault::Neither),
    version("5.0.2", "5.0", "2", "docdb5.0", VersionDefault::Neither),
    version("8.0.0", "8.0", "0", "docdb8.0", VersionDefault::Neither),
    version("8.0.1", "8.0", "1", "docdb8.0", VersionDefault::Line),
];

/// One catalog row, so the table above reads as a table. Columns, in order: engine version,
/// major engine version, minor engine version, DB parameter group family, default
/// designation.
const fn version(
    version: &'static str,
    major: &'static str,
    minor: &'static str,
    family: &'static str,
    default_of: VersionDefault,
) -> EngineVersion {
    EngineVersion {
        version,
        major,
        minor,
        family,
        default_of,
    }
}

/// Every published engine version, oldest first.
pub fn engine_versions() -> impl Iterator<Item = &'static EngineVersion> {
    ENGINE_VERSIONS.iter()
}

/// The published row of `version`, or `None` when this engine publishes no such version.
/// Matching is case-insensitive, as every version member of these models is.
pub fn engine_version(version: &str) -> Option<&'static EngineVersion> {
    ENGINE_VERSIONS
        .iter()
        .find(|published| published.version.eq_ignore_ascii_case(version))
}

/// The published row of `version`, in a CONST context, so a table that adds a column to
/// these rows can be keyed by the version STRING rather than by a position in this list — a
/// row inserted here then cannot silently re-assign another version's column.
///
/// A version this engine does not publish is a typo in that table rather than a value a
/// caller can send, so it fails the compile instead of being handled.
pub const fn published_row(version: &str) -> &'static EngineVersion {
    let mut index = 0;
    while index < ENGINE_VERSIONS.len() {
        let candidate = &ENGINE_VERSIONS[index];
        if const_str_eq(candidate.version, version) {
            return candidate;
        }
        index += 1;
    }
    panic!("every version a table of this catalog names is one the catalog publishes")
}

/// Whether two string literals are equal, in a const context (`str::eq` is not const).
const fn const_str_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// The `DBParameterGroupFamily` values this engine publishes, one per major line, in
/// catalog order and without repeats.
pub fn parameter_group_families() -> Vec<&'static str> {
    let mut families: Vec<&'static str> = Vec::new();
    for published in ENGINE_VERSIONS {
        if !families.contains(&published.family) {
            families.push(published.family);
        }
    }
    families
}

/// The major LINES this engine publishes (`5.0`), in catalog order and without repeats.
///
/// A line is what `MajorEngineVersion` reports and what a request naming only "the intended
/// major engine version" names; [`line_default`] answers which release of one a request
/// naming it runs.
pub fn major_lines() -> Vec<&'static str> {
    let mut lines: Vec<&'static str> = Vec::new();
    for published in ENGINE_VERSIONS {
        if !lines.contains(&published.major) {
            lines.push(published.major);
        }
    }
    lines
}

/// The release this catalog designates as the default of the major line `line`, or `None`
/// when it publishes no such line.
///
/// This is the one accessor for "which release does that line resolve to": a create or a
/// modify naming only the line, and a `DefaultOnly` report narrowed to it, answer from here,
/// so neither view of the front door derives the designation its own way. Matching is
/// case-insensitive, as every version member of these models is.
pub fn line_default(line: &str) -> Option<&'static EngineVersion> {
    ENGINE_VERSIONS
        .iter()
        .find(|row| row.major.eq_ignore_ascii_case(line) && row.default_of.is_line_default())
}

/// Whether `version` is the DEFAULT release of its major line: the release of that line this
/// catalog designates ([`VersionDefault::is_line_default`]), which is not necessarily the
/// newest one the line publishes.
///
/// It is the release an automatic minor version upgrade moves a cluster of that line onto,
/// which is what `UpgradeTarget$AutoUpgrade` reports about a target; the release a request
/// naming only the line resolves to; and the line's own default that `DefaultOnly` selects
/// when a request narrows the report to one major version. The ENGINE's default is the
/// designation one step up ([`DEFAULT_ENGINE_VERSION`]): it names the version a create
/// without an `EngineVersion` runs, and it is a line default too.
pub fn is_line_default(version: &str) -> bool {
    engine_version(version).is_some_and(|row| row.default_of.is_line_default())
}

/// The versions `version` can be upgraded to, in catalog order: every LATER MINOR release
/// of its own line, and every release of a later major line this engine offers an in-place
/// major version upgrade to (see [`FIRST_IN_PLACE_UPGRADE_MAJOR`]).
///
/// This is what `DBEngineVersion$ValidUpgradeTarget` reports ("A list of engine versions
/// that this database engine version can be upgraded to"), so a client that plans an
/// upgrade from it names a version the engine will actually move to.
pub fn upgrade_targets(version: &str) -> Vec<&'static EngineVersion> {
    let Some(from) = engine_version(version) else {
        return Vec::new();
    };
    // Every comparison is numeric, component by component, so a two-digit release or line
    // orders after a one-digit one.
    let precedes = crate::rds_cluster_parameters::engine_version_precedes;
    ENGINE_VERSIONS
        .iter()
        .filter(|target| {
            if target.major == from.major {
                // A later minor release of the line the version already runs.
                return precedes(from.version, target.version);
            }
            precedes(from.major, target.major)
                && !precedes(target.major, FIRST_IN_PLACE_UPGRADE_MAJOR)
        })
        .collect()
}

/// What a VERSION of this engine can do: the capability flags the front door reports on
/// every engine version, answered for Amazon DocumentDB.
///
/// Each member answers a capability of the shared front door, and a DocumentDB version
/// publishes none of them: the cluster — not its instance — owns the storage, the readers
/// and the regions, so none of the DB-instance capabilities is this engine version's, and
/// the Aurora and Amazon RDS features (Babelfish, a DB proxy, parallel query, a limitless
/// database, data groups) belong to other engines of the same door. The one capability it
/// does publish is the certificate rotation: Amazon DocumentDB rotates a cluster's server
/// certificate without restarting its instances for every version it publishes ("Updating
/// your Amazon DocumentDB TLS certificates").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionCapabilities {
    /// `SupportsReadReplica`.
    pub read_replica: bool,
    /// `SupportsCluster`.
    pub cluster: bool,
    /// `SupportsGlobalDatabases`.
    pub global_databases: bool,
    /// `SupportsRDSProxy`.
    pub rds_proxy: bool,
    /// `SupportsParallelQuery`.
    pub parallel_query: bool,
    /// `SupportsBabelfish`.
    pub babelfish: bool,
    /// `SupportsLimitlessDatabase`.
    pub limitless_database: bool,
    /// `SupportsDataGroups`.
    pub data_groups: bool,
    /// `SupportsCertificateRotationWithoutRestart`.
    pub certificate_rotation_without_restart: bool,
}

/// The capabilities every published version of this engine reports; see
/// [`VersionCapabilities`].
pub const VERSION_CAPABILITIES: VersionCapabilities = VersionCapabilities {
    read_replica: false,
    cluster: false,
    global_databases: false,
    rds_proxy: false,
    parallel_query: false,
    babelfish: false,
    limitless_database: false,
    data_groups: false,
    certificate_rotation_without_restart: true,
};

/// What an ORDERABLE INSTANCE OPTION of this engine reports: the capability members the
/// front door carries on every option, answered for Amazon DocumentDB.
///
/// A DocumentDB instance is a member of a cluster, and the cluster owns the storage, the
/// zones and the readers: it takes no instance-level Multi-AZ standby
/// (`MultiAZCapable`), no RDS read replica of its own (`ReadReplicaCapable`) and no
/// Enhanced Monitoring — the DocumentDB `CreateDBInstance` models no monitoring member to
/// enable one with — while the CLUSTER it joins is what makes the class part of a cluster
/// (`SupportsClusters`) and able to join a global cluster (`SupportsGlobalDatabases`, which
/// the DocumentDB `CreateGlobalCluster` publishes). Its storage is always encrypted-capable
/// and carries neither provisioned IOPS nor provisioned throughput, since the cluster owns
/// it. The remaining members name features of other engines of the same door (an HTTP
/// query endpoint, optimized reads and writes, Kerberos authentication, a dedicated log
/// volume, additional storage volumes) or of a DB instance on an Outpost, none of which
/// this engine offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionCapabilities {
    /// `MultiAZCapable`.
    pub multi_az_capable: bool,
    /// `ReadReplicaCapable`.
    pub read_replica_capable: bool,
    /// `SupportsStorageEncryption`.
    pub storage_encryption: bool,
    /// `SupportsIops`.
    pub iops: bool,
    /// `SupportsStorageThroughput`.
    pub storage_throughput: bool,
    /// `SupportsEnhancedMonitoring`.
    pub enhanced_monitoring: bool,
    /// `SupportsIAMDatabaseAuthentication`.
    pub iam_database_authentication: bool,
    /// `SupportsPerformanceInsights`.
    pub performance_insights: bool,
    /// `OutpostCapable`.
    pub outpost_capable: bool,
    /// `SupportsClusters`.
    pub clusters: bool,
    /// `SupportsGlobalDatabases`.
    pub global_databases: bool,
    /// `SupportsHttpEndpoint`.
    pub http_endpoint: bool,
    /// `SupportsOptimizedReads`.
    pub optimized_reads: bool,
    /// `SupportsOptimizedWrites`.
    pub optimized_writes: bool,
    /// `SupportsKerberosAuthentication`.
    pub kerberos_authentication: bool,
    /// `SupportsDedicatedLogVolume`.
    pub dedicated_log_volume: bool,
    /// `SupportsAdditionalStorageVolumes`.
    pub additional_storage_volumes: bool,
    /// `RequiresCustomProcessorFeatures`.
    pub requires_custom_processor_features: bool,
}

/// The capabilities every orderable option of this engine reports; see
/// [`OptionCapabilities`].
pub const OPTION_CAPABILITIES: OptionCapabilities = OptionCapabilities {
    multi_az_capable: false,
    read_replica_capable: false,
    storage_encryption: true,
    iops: false,
    storage_throughput: false,
    enhanced_monitoring: false,
    iam_database_authentication: false,
    performance_insights: true,
    outpost_capable: false,
    clusters: true,
    global_databases: true,
    http_endpoint: false,
    optimized_reads: false,
    optimized_writes: false,
    kerberos_authentication: false,
    dedicated_log_volume: false,
    additional_storage_volumes: false,
    requires_custom_processor_features: false,
};

/// `SupportedEngineModes` of an option of this engine: a DocumentDB cluster member is
/// ordered with provisioned capacity, the one mode this engine publishes.
pub const ENGINE_MODES: &[&str] = &["provisioned"];

/// `SupportedNetworkTypes` of an option of this engine: `DBCluster$NetworkType` publishes
/// "Valid Values: IPV4 | DUAL", in that order.
pub const NETWORK_TYPES: &[&str] = &["IPV4", "DUAL"];

/// The storage types an instance of this engine is ordered WITH, in the order the value list
/// publishes them.
///
/// `CreateDBClusterMessage$StorageType` of this engine's own control plane states the list:
/// "Valid values for storage type - standard | iopt1 … Default value is `standard`", so the
/// type a create defaults to is the first of them. The CLUSTER carries the type, which is why
/// an option of this engine reports neither a storage size nor provisioned IOPS — but the
/// type is still part of what an order picks, so `DescribeOrderableDBInstanceOptions`
/// publishes one option per type a class can be ordered with rather than one option carrying
/// the type a create defaults to.
pub const STORAGE_TYPES: &[&str] = &["standard", "iopt1"];

/// `SupportedActivityStreamModes` of an option of this engine: the DocumentDB control plane
/// binds no operation that starts a database activity stream, so an option of it publishes
/// no mode — the member is reported EMPTY rather than left out, because it is a list and an
/// engine with no mode reports it empty.
pub const ACTIVITY_STREAM_MODES: &[&str] = &[];

/// The CloudWatch log types a cluster of this engine can export, each with the cluster
/// parameter that enables it.
///
/// `CreateDBClusterMessage$EnableCloudwatchLogsExports`: "A list of log types that need to be
/// enabled for exporting to Amazon CloudWatch Logs. You can enable audit logs or profiler
/// logs" — the audit log is what the `audit_logs` parameter turns on and the profiler log
/// what the `profiler` parameter does, so a version exports a log type exactly when the
/// parameter that enables it applies at that version.
pub const EXPORTABLE_LOG_TYPES: &[(&str, &str)] =
    &[("audit", "audit_logs"), ("profiler", "profiler")];

/// `DBEngineVersion$ExportableLogTypes` for `version`: the log types of
/// [`EXPORTABLE_LOG_TYPES`] whose enabling parameter the version's family publishes and whose
/// own earliest engine version the version is not older than.
///
/// It is a property of the VERSION, not of the engine: a version whose parameter catalogue
/// cannot switch a log type on cannot export it, so the report of an early version does not
/// claim a log export its cluster has no parameter for — and both views of the front door
/// read it here, so they cannot disagree about a version.
pub fn exportable_log_types(version: &str) -> Vec<&'static str> {
    let family = engine_version(version)
        .map(|row| row.family)
        .unwrap_or_default();
    EXPORTABLE_LOG_TYPES
        .iter()
        .filter(|(_, parameter)| {
            crate::rds_cluster_parameters::published_parameter(family, parameter).is_some_and(
                |spec| {
                    !crate::rds_cluster_parameters::engine_version_precedes(
                        version,
                        spec.minimum_engine_version,
                    )
                },
            )
        })
        .map(|(log_type, _)| *log_type)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row's major and minor version, and its parameter-group family, are the parts of
    /// its own version string: a row whose family named another line would hand a client a
    /// parameter group its cluster cannot take.
    #[test]
    fn every_row_is_consistent_with_its_version_string() {
        for row in ENGINE_VERSIONS {
            assert_eq!(
                row.version,
                format!("{}.{}", row.major, row.minor),
                "{}: the version is its major line and its minor release",
                row.version
            );
            assert_eq!(
                row.family,
                format!("{ENGINE}{}", row.major),
                "{}: the family is the engine name and the major line",
                row.version
            );
            assert_eq!(
                engine_version(row.version),
                Some(row),
                "{}: the lookup must resolve its own row",
                row.version
            );
        }
        assert_eq!(
            engine_version("5.0.0").map(|row| row.family),
            engine_version("5.0.0").map(|row| row.family),
            "the lookup is stable across reads"
        );
        assert_eq!(
            engine_version("nosuchversion"),
            None,
            "a version the engine does not publish resolves to nothing"
        );
        assert_eq!(
            parameter_group_families(),
            vec!["docdb3.6", "docdb4.0", "docdb5.0", "docdb8.0"],
            "one family per major line, in catalog order, without repeats"
        );
    }

    /// The upgrade rule, over every published row: a later minor of the same line, and a
    /// later major line only from the first line this engine upgrades in place to.
    #[test]
    fn upgrade_targets_follow_the_published_upgrade_rule() {
        let cases: &[(&str, &[&str])] = &[
            // The oldest row: the recorded report names exactly the 5.0 and 8.0 releases,
            // and not the 4.0 ones.
            ("3.6.0", &["5.0.0", "5.0.1", "5.0.2", "8.0.0", "8.0.1"]),
            // A row of a line below the first in-place upgrade major still reaches the later
            // release of its OWN line, which is a minor upgrade.
            ("4.0.0", &["4.0.1", "5.0.0", "5.0.1", "5.0.2", "8.0.0", "8.0.1"]),
            ("4.0.1", &["5.0.0", "5.0.1", "5.0.2", "8.0.0", "8.0.1"]),
            ("5.0.0", &["5.0.1", "5.0.2", "8.0.0", "8.0.1"]),
            ("5.0.2", &["8.0.0", "8.0.1"]),
            ("8.0.0", &["8.0.1"]),
            // The newest row has nowhere to go.
            ("8.0.1", &[]),
            // A version this engine does not publish has no upgrade path at all.
            ("9.9.9", &[]),
        ];
        for (from, expected) in cases {
            let targets: Vec<&str> = upgrade_targets(from)
                .into_iter()
                .map(|row| row.version)
                .collect();
            assert_eq!(
                targets, *expected,
                "case {from}: expected upgrade targets {expected:?}, got {targets:?}"
            );
        }
    }

    /// Exactly one release of each line is designated its default — an automatic minor
    /// version upgrade has one destination inside a line, and a request naming the line one
    /// release to resolve to — and the designation is the row's own, so a line can publish a
    /// LATER release than the one it designates.
    #[test]
    fn one_release_of_each_line_is_its_default() {
        for (case, version, is_default) in [
            ("the only release of its line", "3.6.0", true),
            ("the first release of the 4.0 line", "4.0.0", false),
            ("the designated release of the 4.0 line", "4.0.1", true),
            ("the designated release of the 5.0 line", "5.0.0", true),
            ("a later patch of the designated line", "5.0.1", false),
            ("the newest patch of the designated line", "5.0.2", false),
            ("the first release of the newest line", "8.0.0", false),
            ("the designated release of the newest line", "8.0.1", true),
            ("a version the engine does not publish", "9.9.9", false),
        ] {
            assert_eq!(
                is_line_default(version),
                is_default,
                "case {case:?} ({version}): expected is_line_default == {is_default}"
            );
        }
        for family in parameter_group_families() {
            let defaults = ENGINE_VERSIONS
                .iter()
                .filter(|row| row.family == family && is_line_default(row.version))
                .count();
            assert_eq!(
                defaults, 1,
                "case {family}: exactly one release of a line is its default, got {defaults}"
            );
        }
        // Structurally, over every row rather than over the table above: the line each row
        // belongs to designates a default, and it is a release of that line. A line added
        // here without a designation would leave a request naming it resolving to nothing.
        for row in ENGINE_VERSIONS {
            let designated = line_default(row.major);
            assert_eq!(
                designated.map(|default| default.major),
                Some(row.major),
                "case {}: its line {} must designate a default release of its own, got {:?}",
                row.version,
                row.major,
                designated.map(|default| default.version)
            );
        }
        assert_eq!(
            major_lines(),
            vec!["3.6", "4.0", "5.0", "8.0"],
            "one line per major version, in catalog order, without repeats"
        );
        assert_eq!(
            line_default("9.9").map(|row| row.version),
            None,
            "a line the engine does not publish designates nothing"
        );
        assert!(
            engine_version(DEFAULT_ENGINE_VERSION).is_some(),
            "the engine's default version is one this catalog publishes"
        );
        assert!(
            is_line_default(DEFAULT_ENGINE_VERSION),
            "the engine's default version is the default release of its own line, so every \
             view of the front door resolves that line to the same release"
        );
        let engine_defaults = ENGINE_VERSIONS
            .iter()
            .filter(|row| row.default_of.is_engine_default())
            .count();
        assert_eq!(
            engine_defaults, 1,
            "exactly one published release is designated the engine's default version, got \
             {engine_defaults}"
        );
        // The designation is the row's own datum and not the maximum of the list: the
        // engine's default line publishes LATER releases than the one it designates, so a
        // report or a create answering with the newest release of the line would answer with
        // a version this catalog designates nothing about.
        let later_in_the_default_line: Vec<&str> = ENGINE_VERSIONS
            .iter()
            .filter(|row| {
                row.major == published_row(DEFAULT_ENGINE_VERSION).major
                    && crate::rds_cluster_parameters::engine_version_precedes(
                        DEFAULT_ENGINE_VERSION,
                        row.version,
                    )
            })
            .map(|row| row.version)
            .collect();
        assert!(
            !later_in_the_default_line.is_empty()
                && later_in_the_default_line
                    .iter()
                    .all(|version| !is_line_default(version)),
            "case {DEFAULT_ENGINE_VERSION:?}: expected the designated default to have later, \
             undesignated releases in its own line, got {later_in_the_default_line:?}"
        );
        for row in ENGINE_VERSIONS {
            assert_eq!(
                published_row(row.version),
                row,
                "case {}: the const lookup resolves its own row",
                row.version
            );
        }
    }
}
