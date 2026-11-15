// SPDX-License-Identifier: CC0-1.0

//! The data sources an application attaches that ANOTHER service holds, resolved where they
//! are held.
//!
//! An OpenSearch UI application reads its data from an OpenSearch Service domain or direct
//! query data source — this service's own resources, resolved in this service's own store —
//! and from an OpenSearch Serverless COLLECTION, which it does not own
//! ([`super::serverless_collections`]).
//!
//! What the resolution answers is [`DataSourceActivation`]: an attachment completes once its
//! data source is active, so a collection that is already active lets the attachment
//! complete at once, one still being created lets it complete when the collection's own
//! transition falls due, and an ARN that store holds nothing live under names no data source
//! this account can read at all.
//!
//! Resolved as a SNAPSHOT, taken before this service's own store is locked — so no call
//! holds two service stores at once — and read from there by every decision the call then
//! makes, so the attach that decides an attachment's outcome and the settlement that lands
//! it cannot disagree about what the account holds.

use ::std::collections::BTreeMap;

use super::serverless_collections;
use super::DataSourceActivation;

/// Where every data source another service holds stands, as one snapshot.
#[derive(Debug, Clone, Default)]
pub struct ForeignDataSources {
    /// Where each OpenSearch Serverless collection of this account stands, by its ARN, or
    /// `None` when that store was not reachable at all.
    collections: Option<BTreeMap<String, DataSourceActivation>>,
}

impl ForeignDataSources {
    /// Where the data source `arn` names stands, or [`DataSourceActivation::Absent`] where
    /// this snapshot holds none under it — which is also the answer for a value naming no
    /// kind of resource this resolution owns the lookup of, since every other kind is
    /// resolved where that kind is held, and for every value when the collection store was
    /// not reachable: a collection the service that owns it does not report is not one this
    /// account holds, so the attachment of it settles FAILED rather than being reported
    /// ATTACHED against a resource nothing provisioned.
    #[must_use]
    pub fn activation(&self, arn: &str) -> DataSourceActivation {
        if !serverless_collections::names_a_collection(arn) {
            return DataSourceActivation::Absent;
        }
        self.collections
            .as_ref()
            .and_then(|collections| collections.get(arn).copied())
            .unwrap_or(DataSourceActivation::Absent)
    }
}

/// Where every data source another service holds stands, read from that service's own store.
///
/// Called BEFORE this service's store is locked, so no call holds two service stores at
/// once, and read from the snapshot it answers with for the rest of the call.
pub fn foreign_data_sources<Eng: Send + Sync + 'static>(engine: &Eng) -> ForeignDataSources {
    ForeignDataSources {
        collections: serverless_collections::collections(engine),
    }
}
