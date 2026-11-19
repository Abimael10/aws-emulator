// SPDX-License-Identifier: CC0-1.0

//! The LEGACY global-table surface: what the 2017.11.29 operations answer, and why.
//!
//! `CreateGlobalTable`, `DescribeGlobalTable`, `UpdateGlobalTable`,
//! `DescribeGlobalTableSettings` and `UpdateGlobalTableSettings` are all documented the
//! same way — "This documentation is for version 2017.11.29 (Legacy) of global tables,
//! which should be avoided for new global tables. Customers should use Global Tables
//! version 2019.11.21 (Current) when possible" (the `CreateGlobalTable` doc) — so they are
//! one surface, addressing global tables OF THAT VERSION.
//!
//! That version is no longer offered: a global table is created by adding replicas to a
//! table ([`super::replicas`], which establishes a group under
//! [`GlobalTablesVersion::Current`]), and `CreateGlobalTable` — the only operation that
//! could create one of the legacy version — refuses the call as the client error it is
//! ([`legacy_version_withdrawn`]). So no global table of the legacy version exists, and
//! every legacy operation that ADDRESSES one answers the modelled
//! `GlobalTableNotFoundException` ([`global_table_does_not_exist`]) — the error a client
//! catches by name, rather than an unmodelled server-class answer it can neither retry nor
//! fix.
//!
//! `ListGlobalTables` is the one operation of the surface that addresses no single global
//! table: it "lists all global tables that have a replica in the specified Region", which
//! is the enumeration of this surface's own version, so it reports the groups this store
//! holds that were established UNDER that version ([`GlobalTableGroup::version`]) and
//! leaves the ones the current version established to the readers of that version
//! (`DescribeTable`'s `Replicas`), exactly as the legacy readers above refuse to address
//! them.

use std::collections::BTreeSet;

use super::GLOBAL_TABLE_VERSION;

/// The global tables version these operations document themselves for, and which is no
/// longer offered.
pub const LEGACY_GLOBAL_TABLE_VERSION: &str = "2017.11.29";

/// Which global tables VERSION established a replication group — the question that decides
/// which surface the group belongs to, and so which operations enumerate and address it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalTablesVersion {
    /// 2017.11.29, the version `CreateGlobalTable` establishes a group under and the one
    /// the legacy surface addresses.
    Legacy,
    /// 2019.11.21, the version adding replicas to a table establishes a group under
    /// ([`GLOBAL_TABLE_VERSION`], which `TableDescription$GlobalTableVersion` reports).
    Current,
}

impl GlobalTablesVersion {
    /// The version as every description states it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => LEGACY_GLOBAL_TABLE_VERSION,
            Self::Current => GLOBAL_TABLE_VERSION,
        }
    }
}

/// One replication group this store holds: the Regions that hold a replica of the global
/// table, and the version the group was established under.
///
/// The version travels with the group because the two global-table surfaces address
/// DIFFERENT groups: the legacy operations address (and `ListGlobalTables` enumerates) the
/// groups of version 2017.11.29, while a group established by adding replicas to a table is
/// one of version 2019.11.21 and is addressed through `UpdateTable`/`DescribeTable`.
#[derive(Debug, Clone)]
pub struct GlobalTableGroup {
    /// "The Regions where the global table has replicas" (`GlobalTable$ReplicationGroup`),
    /// in ascending order, so every reader reports one group the same way.
    pub regions: BTreeSet<String>,
    /// The version that established this group.
    pub version: GlobalTablesVersion,
}

impl GlobalTableGroup {
    /// An empty group of `version`, for the call that is about to record its first Regions.
    pub fn established_under(version: GlobalTablesVersion) -> Self {
        Self {
            regions: BTreeSet::new(),
            version,
        }
    }
}

/// The message a call that would CREATE a global table of the legacy version is refused
/// with.
///
/// A permanently invalid request rather than a gap or a conflict: the version it asks for
/// is not on offer, so no retry and no change of state can make the same call succeed —
/// the caller has to create the global table the current way instead. AWS's own sentence
/// for it, inside the clause it opens every invalid-parameter refusal with: fixed text
/// stating the version that is not supported and the one to use instead, with nothing taken
/// from the request.
pub fn legacy_version_withdrawn() -> String {
    format!(
        "One or more parameter values were invalid: DynamoDB global tables version \
         {LEGACY_GLOBAL_TABLE_VERSION} is not supported. We recommend using DynamoDB global \
         tables version {GLOBAL_TABLE_VERSION}, instead of version \
         {LEGACY_GLOBAL_TABLE_VERSION} (Legacy)."
    )
}

/// The message the modelled `GlobalTableNotFoundException` ("The specified global table
/// does not exist") carries for a legacy operation addressing `name`: AWS's sentence for
/// that error, whose only variable is the global table name the request asked about, in
/// single quotes.
pub fn global_table_does_not_exist(name: &str) -> String {
    format!("Global table with name: '{name}' does not exist.")
}

/// The same error as `DescribeGlobalTable` words it: that operation states the sentence
/// above behind a clause of its own naming what was not found.
pub fn describe_global_table_not_found(name: &str) -> String {
    format!("Global table not found: {}", global_table_does_not_exist(name))
}
