// SPDX-License-Identifier: CC0-1.0

//! The ENGINE defaults a DB instance or DB cluster is created with when the request names
//! none: the documented default port of each engine family and the engine-shaped values a
//! create resolves beside it.
//!
//! Split out of [`super`] so that module keeps the store, the records and their
//! projections; every name here is re-exported from it, so a handler still reads them as
//! `crate::state::<name>`.

use super::*;

/// The port a DB instance accepts connections on when the request names none.
///
/// From `CreateDBInstanceMessage$Port`: "Default: RDS for Db2 - 50000, RDS for
/// MariaDB - 3306, RDS for Microsoft SQL Server - 1433, RDS for MySQL - 3306, RDS
/// for Oracle - 1521, RDS for PostgreSQL - 5432". The engine names are the prefixes
/// the `Engine` member's own value list uses (`custom-sqlserver-ee`, `oracle-se2`,
/// `aurora-postgresql`, …), so the family is matched on the prefix.
///
/// The two engines of the sibling control planes this door also serves are matched FIRST
/// and exactly: the default port of such a resource is the one THAT service publishes
/// ([`DOCDB_DEFAULT_PORT`], [`NEPTUNE_DEFAULT_PORT`]), not a value from this model's list,
/// and neither name carries any of the substrings below.
pub fn default_port(engine: &str) -> i32 {
    let engine = engine.to_ascii_lowercase();
    if engine == DOCDB_ENGINE {
        DOCDB_DEFAULT_PORT
    } else if engine == NEPTUNE_ENGINE {
        NEPTUNE_DEFAULT_PORT
    } else if engine.contains("db2") {
        50000
    } else if engine.contains("sqlserver") {
        1433
    } else if engine.contains("oracle") {
        1521
    } else if engine.contains("postgres") {
        5432
    } else {
        // mysql, mariadb, aurora-mysql and the Aurora MySQL aliases.
        3306
    }
}

/// Whether a DB cluster of `engine` takes automatic minor engine upgrades when the request
/// that made it named nothing.
///
/// `AutoMinorVersionUpgrade` is a property of the ENGINE's own control plane, so the default
/// follows the engine rather than this front door:
///
/// * For the engines the member applies to — `CreateDBClusterMessage$AutoMinorVersionUpgrade`
///   is "Valid for Cluster Type: Aurora DB clusters and Multi-AZ DB cluster" and "by default,
///   minor engine upgrades are applied automatically" — a cluster takes them
///   ([`DEFAULT_CLUSTER_AUTO_MINOR_VERSION_UPGRADE`]).
/// * The graph control plane this door also serves publishes no such member on its own
///   `CreateDBCluster` at all
///   (`smithy_query(query="operation",operation="CreateDBCluster",model_path="services/service-com-amazonaws-neptune-amazonrdsv19/smithy-model/com-amazonaws-neptune-amazonrdsv19.json")`),
///   and a cluster its front door creates does not take minor engine upgrades
///   automatically, so one of that engine is born with the setting off.
///
/// It is the DEFAULT only: a request that names the member gets the value it asked for,
/// whatever the engine, as does a cluster a later modification changes the setting on.
pub fn cluster_auto_minor_version_upgrade_default(engine: &str) -> bool {
    if engine.eq_ignore_ascii_case(NEPTUNE_ENGINE) {
        false
    } else {
        DEFAULT_CLUSTER_AUTO_MINOR_VERSION_UPGRADE
    }
}
