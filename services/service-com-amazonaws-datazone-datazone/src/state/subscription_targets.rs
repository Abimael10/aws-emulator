// SPDX-License-Identifier: CC0-1.0

//! Subscription targets: the stored record, and the lookup that decides whether a
//! subscription to a published listing has a target behind it.
//!
//! A subscription target is "the details of the subscription target" an environment
//! publishes data into (smithy_query operation="CreateSubscriptionTarget"), and it
//! declares the asset types that can be included in it (`applicableAssetTypes`). A
//! subscription to a published entity is therefore FULFILLED through a target whose
//! applicable asset types include that entity's own type — and a listing no target
//! applies to has no fulfilment path at all, which is what
//! [`super::created_subscription_request_status`] reads this store for.
//!
//! `CreateSubscriptionTarget` is still a scaffold (it is addressed by
//! `environmentIdentifier`, and no environment resource exists in this store yet), so
//! nothing on the wire writes [`Domain::subscription_targets`] today. The lookup reads
//! the store either way, so the day a target can be created the decision follows it
//! without another handler changing.

use super::{Domain, ListingRevision};

/// One stored subscription target.
#[derive(Debug, Clone)]
pub struct SubscriptionTarget {
    /// The target's own `SubscriptionTargetId` — the identifier `GetSubscriptionTarget`
    /// addresses one by.
    pub id: String,
    pub domain_id: String,
    /// The environment this target publishes into, and the project that environment
    /// belongs to: `environmentId` and `projectId` are both REQUIRED on a reported
    /// target (smithy_query shape="SubscriptionTargetSummary").
    pub environment_id: String,
    pub project_id: String,
    /// The asset types that can be included in this target
    /// (`applicableAssetTypes`), each an asset type identifier — the same string an
    /// asset's `typeIdentifier` carries.
    pub applicable_asset_types: Vec<String>,
}

/// The subscription target of `domain` that a subscription to the entity recorded by
/// `revision` would be fulfilled through, when the domain holds one.
///
/// A target applies to an entity exactly when its applicable asset types include the
/// entity's own type, which the listing revision records as `entity_type_name`. A linear
/// scan over a `BTreeMap`, so the target reported for a domain holding several is the
/// same one on every call — the trade [`Domain::listing_for_entity`] makes.
pub fn fulfilling_subscription_target<'a>(
    domain: &'a Domain,
    revision: &ListingRevision,
) -> Option<&'a SubscriptionTarget> {
    domain.subscription_targets.values().find(|target| {
        target
            .applicable_asset_types
            .iter()
            .any(|asset_type| asset_type == &revision.entity_type_name)
    })
}
