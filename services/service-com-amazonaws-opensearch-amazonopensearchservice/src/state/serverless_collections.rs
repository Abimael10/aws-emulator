// SPDX-License-Identifier: CC0-1.0

//! The OpenSearch Serverless COLLECTION a data source ARN can name, read in the store that
//! service holds its collections in.
//!
//! A collection is not this service's resource: it is minted by `aoss:CreateCollection` and
//! reported by `aoss:BatchGetCollection`, so what this module does is read it — through the
//! SAME shared engine this service's handlers reach their own store through, the way
//! [`super::log_destinations`] reads a CloudWatch Logs log group and [`super::packages`] an
//! Amazon S3 object. [`super::foreign_data_sources`] is what calls it, and what states when.
//!
//! Reads ONLY: nothing here writes, so resolving a data source cannot move another service's
//! record.

use ::std::collections::BTreeMap;
use ::std::time::{Duration, Instant, SystemTime};

use ::service_com_amazonaws_opensearchserverless_opensearchserverless::state as serverless;
use ::service_common::arn;

use super::DataSourceActivation;

/// OpenSearch Serverless' own `arnNamespace`: a collection ARN carries it rather than this
/// service's namespace, which is what tells a collection's ARN from a domain's.
pub const COLLECTION_ARN_NAMESPACE: &str = "aoss";

/// The resource TYPE field of a collection's ARN
/// (`arn:<partition>:aoss:<region>:<account-id>:collection/<id>`).
pub const COLLECTION_RESOURCE_TYPE: &str = "collection";

/// Whether `arn` names an OpenSearch Serverless collection — the SHAPE half of the rule,
/// read off the value alone, so which kinds of ARN name a data source at all
/// ([`super::data_source_arns::names_a_data_source`]) is decided without reaching a store.
#[must_use]
pub fn names_a_collection(arn: &str) -> bool {
    // A collection is addressed by its id, so an ARN of that type carrying none names no
    // collection.
    arn::vendor(arn) == Some(COLLECTION_ARN_NAMESPACE)
        && arn::resource_id_of_type(arn, COLLECTION_RESOURCE_TYPE).is_some_and(|id| !id.is_empty())
}

/// Where every collection that store holds stands, by the ARN that names it, or `None` when
/// the engine hands out no collection store at all.
///
/// Reads ONLY, and reads ANOTHER service's store, so it is called before this service's own
/// store is locked: no call holds two service stores at once
/// ([`super::packages::imported_content`] states the same rule).
pub fn collections<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<BTreeMap<String, DataSourceActivation>> {
    let state = serverless::service_state(engine)?;
    let store = state.lock();
    Some(
        store
            .collections
            .values()
            .map(|collection| (collection.arn.clone(), activation(collection)))
            .collect(),
    )
}

/// When `collection` becomes active, read off the record the collection store holds.
///
/// A collection whose deletion has been accepted is on its way out and never becomes active
/// again, so it counts as absent — an attachment of it could never complete. One that is
/// still being created carries the instant its own store owes it the status it settles into,
/// so an attachment waits for THAT instant rather than for a span chosen here, and a
/// transition already due is read as the active collection it leaves behind, since that is
/// what the collection's own next reader reports.
fn activation(collection: &serverless::CollectionRecord) -> DataSourceActivation {
    if collection.purge_at.is_some() {
        return DataSourceActivation::Absent;
    }
    if collection.status == serverless::COLLECTION_ACTIVE {
        return DataSourceActivation::Active;
    }
    match collection
        .pending_status
        .as_ref()
        .filter(|pending| pending.status == serverless::COLLECTION_ACTIVE)
    {
        Some(pending) => match pending.due_at.checked_duration_since(Instant::now()) {
            Some(remaining) if remaining > Duration::ZERO => {
                DataSourceActivation::ActiveAt(SystemTime::now() + remaining)
            }
            _ => DataSourceActivation::Active,
        },
        // A collection that is neither active nor on its way to active — one whose creation
        // failed, or one being deleted — has nothing that becomes active.
        None => DataSourceActivation::Absent,
    }
}
