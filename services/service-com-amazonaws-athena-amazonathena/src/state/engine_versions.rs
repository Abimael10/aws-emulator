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
//! Each entry pairs a selectable version with the version a query then runs on, through
//! the SAME resolution the create and update paths apply to a workgroup's selection
//! ([`resolved_engine_version`]) — so the effective version this catalog publishes for a
//! selection is the one a workgroup that made that selection reports.

use super::*;

use crate::ops::types::EngineVersion;

/// The PySpark engine version a Spark enabled workgroup selects.
///
/// Athena's Spark family is named by the same scheme as its SQL one (see
/// [`PYSPARK_ENGINE_VERSION_PREFIX`], which is what identifies the family), and Athena's
/// documentation names PySpark engine version 3 as the version a Spark enabled workgroup
/// runs its sessions and notebooks on — so that is the member of the family this emulator
/// offers, beside [`DEFAULT_EFFECTIVE_ENGINE_VERSION`] for SQL.
pub const PYSPARK_ENGINE_VERSION: &str = "PySpark engine version 3";

/// Every engine version a request may select, in the order a listing reports them.
///
/// The Auto option first — `ListEngineVersions` documents its answer as "a list of engine
/// versions that are available to choose from, including the Auto option" — then the
/// Athena SQL version ([`DEFAULT_EFFECTIVE_ENGINE_VERSION`]) and the PySpark one
/// ([`PYSPARK_ENGINE_VERSION`]), one per engine family Athena runs. `EngineVersionsList`
/// holds 0 to 10 entries, which this catalog is within, so a listing of it is one page
/// wide at the modelled maximum page size.
pub const SELECTABLE_ENGINE_VERSIONS: &[&str] = &[
    ENGINE_VERSION_AUTO,
    DEFAULT_EFFECTIVE_ENGINE_VERSION,
    PYSPARK_ENGINE_VERSION,
];

/// The catalog as the modelled `EngineVersion` entries a listing reports: each selectable
/// version beside the version it resolves to.
///
/// Built through [`resolved_engine_version`], the one place this service decides what a
/// selection means, so the `EffectiveEngineVersion` published here cannot drift from the
/// one a workgroup selecting that version is created with.
pub fn selectable_engine_versions() -> Vec<EngineVersion> {
    SELECTABLE_ENGINE_VERSIONS
        .iter()
        .map(|selected| {
            resolved_engine_version(Some(&EngineVersion {
                selected_engine_version: Some((*selected).to_string()),
                // Read only, and ignored on the way in: the resolution decides it.
                effective_engine_version: None,
            }))
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
