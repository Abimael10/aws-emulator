// SPDX-License-Identifier: CC0-1.0

//! Subscription targets: the stored record of where an environment publishes data into.
//!
//! A subscription target is "the details of the subscription target" an environment
//! publishes data into (smithy_query operation="CreateSubscriptionTarget"), and it
//! declares the asset types that can be included in it (`applicableAssetTypes`). It is
//! what a GRANT of a subscription is fulfilled through; whether a domain holds one does
//! NOT decide anything about a subscription request, which is an ask a reviewer decides.
//!
//! `CreateSubscriptionTarget` is still a scaffold (it is addressed by
//! `environmentIdentifier`, and no environment resource exists in this store yet), so
//! nothing on the wire writes [`super::Domain::subscription_targets`] today. The record
//! is exactly what that handler will write.

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
