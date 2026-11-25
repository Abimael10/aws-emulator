// SPDX-License-Identifier: CC0-1.0

//! The engine versions a caller may select, and the version each one resolves to.
//!
//! `ListEngineVersions` is documented "Returns a list of engine versions that are
//! available to choose from, including the Auto option", and
//! `EngineVersion$SelectedEngineVersion` names that same set from the other side:
//! "Possible values are determined by the output of `ListEngineVersions`, including AUTO."
//! So the set this module holds IS the catalog of selectable versions, and it is the
//! service's own rather than anything a request wrote: no operation adds to it or takes
//! from it, which is why two listings of it report the same entries.
//!
//! Each entry of the catalog names ONE offer: the value a request may select and the
//! engine family that value belongs to ([`engine_version_category`], the one place this
//! service decides a family). What a selection then RESOLVES to is a different question,
//! answered where a selection is actually made ([`resolved_engine_version`]) — a catalog
//! row stands for no selection, so it carries no resolved version.

use super::*;

use crate::ops::types::EngineVersion;

/// The PySpark engine version a Spark enabled workgroup selects.
///
/// Athena's Spark family is named by the same scheme as its SQL one (see
/// [`PYSPARK_ENGINE_VERSION_PREFIX`], which is what identifies the family), and Athena's
/// documentation names PySpark engine version 3 as the version a Spark enabled workgroup
/// runs its sessions and notebooks on.
pub const PYSPARK_ENGINE_VERSION: &str = "PySpark engine version 3";

/// The Apache Spark version a Spark enabled workgroup selects, the other offer of Athena's
/// Spark family beside [`PYSPARK_ENGINE_VERSION`].
///
/// Athena's documentation names Apache Spark version 3.5 among the engine versions a Spark
/// enabled workgroup or a session may run on, so it is one of the values this catalog
/// publishes as selectable.
pub const APACHE_SPARK_ENGINE_VERSION: &str = "Apache Spark version 3.5";

/// Every engine version a request may select, in the order a listing reports them.
///
/// One entry per offer Athena publishes: the Spark family's Apache Spark
/// ([`APACHE_SPARK_ENGINE_VERSION`]) and PySpark ([`PYSPARK_ENGINE_VERSION`]) versions, the
/// Athena SQL version ([`DEFAULT_EFFECTIVE_ENGINE_VERSION`]), and the Auto option
/// ([`ENGINE_VERSION_AUTO`]) — `ListEngineVersions` documents its answer as "a list of
/// engine versions that are available to choose from, including the Auto option", and
/// `EngineVersion$SelectedEngineVersion` names the same set from the other side. The
/// version names are the ones Athena's own documentation gives each family; the order is
/// the one the catalog is published in. `EngineVersionsList` holds 0 to 10 entries, which
/// this catalog is within, so a listing of it is one page wide at the modelled maximum page
/// size.
pub const SELECTABLE_ENGINE_VERSIONS: &[&str] = &[
    APACHE_SPARK_ENGINE_VERSION,
    ENGINE_VERSION_AUTO,
    DEFAULT_EFFECTIVE_ENGINE_VERSION,
    PYSPARK_ENGINE_VERSION,
];

/// The engine family of Athena's SQL engine versions: the Presto-derived engine the
/// `Athena engine version N` line runs queries on, and the family the Auto option
/// resolves into.
pub const ENGINE_CATEGORY_PRESTO: &str = "Presto";

/// The engine family of Athena's Spark engine versions, the one a Spark enabled workgroup
/// runs its sessions and notebooks on.
pub const ENGINE_CATEGORY_SPARK: &str = "Spark";

/// What a version name carries when it names a member of the Spark family: Athena names
/// every one of them after Spark (`PySpark engine version 3`, `Apache Spark version 3.5`)
/// and no SQL version after anything but Athena's own engine line, so this one word is
/// what tells the two families apart — the PySpark line's own name
/// ([`PYSPARK_ENGINE_VERSION_PREFIX`]) carries it too. Matched without regard to ASCII
/// case, for the reason the catalog's other name comparisons are.
const SPARK_FAMILY_MARKER: &str = "spark";

/// The engine family `version` belongs to: [`ENGINE_CATEGORY_SPARK`] for a version naming
/// Athena's Spark engine ([`SPARK_FAMILY_MARKER`]), and [`ENGINE_CATEGORY_PRESTO`] for
/// every other one — the Athena SQL line and the Auto option, which resolves into it.
///
/// The ONE place this service decides a family, so the family a listing publishes for a
/// version cannot drift from the family reported for a workgroup or a session running that
/// same version, nor from the family that decides whether a workgroup holds notebooks
/// ([`is_spark_enabled`]).
pub fn engine_version_category(version: &str) -> &'static str {
    if version.to_ascii_lowercase().contains(SPARK_FAMILY_MARKER) {
        ENGINE_CATEGORY_SPARK
    } else {
        ENGINE_CATEGORY_PRESTO
    }
}

/// The catalog as the modelled `EngineVersion` entries a listing reports: one entry per
/// offer, naming the value a request may select and the engine family it belongs to.
///
/// No entry carries `EffectiveEngineVersion`: that member is "the engine version on which
/// the query runs" for a version somebody SELECTED, and a catalog row is an offer nobody
/// has selected yet, so there is no query and no resolved version to report. What a
/// selection resolves to is decided where one is made ([`resolved_engine_version`]).
pub fn selectable_engine_versions() -> Vec<EngineVersion> {
    SELECTABLE_ENGINE_VERSIONS
        .iter()
        .map(|selected| EngineVersion {
            category: Some(engine_version_category(selected).to_string()),
            selected_engine_version: Some((*selected).to_string()),
            // Nothing is selected on an offer, so nothing has resolved.
            effective_engine_version: None,
        })
        .collect()
}

/// The cursor a `ListEngineVersions` page hands back: the catalog it is paging — one the
/// whole account shares, since the operation models no member that narrows it — and the
/// position to resume at.
fn engine_versions_cursor(position: usize) -> String {
    format!("engineversions{CURSOR_SEPARATOR}{ACCOUNT_SCOPE}{CURSOR_SEPARATOR}{position}")
}

impl ComAmazonawsAthenaAmazonathenaState {
    /// The `NextToken` a `ListEngineVersions` page resumed at `position` hands back.
    pub fn engine_versions_page_token(&self, position: usize) -> String {
        ::hex::encode(self.page_token_key.mint(&engine_versions_cursor(position)))
    }

    /// The position `token` resumes an engine-version listing at, or `None` for a token
    /// this store did not mint for THAT listing.
    pub fn engine_versions_page_position(&self, token: &str) -> Option<usize> {
        let cursor = self.page_cursor(token)?;
        cursor_position(&cursor, "engineversions", ACCOUNT_SCOPE)
    }
}
