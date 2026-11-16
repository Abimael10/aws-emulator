// SPDX-License-Identifier: CC0-1.0

//! The hub V2 subscription: the "service resource" the V2 API family is enabled,
//! described and opted into features through.
//!
//! Security Hub's V2 API family is subscribed per account **and** Region, exactly as
//! the CSPM hub of [`super::Hub`] is: `EnableSecurityHubV2` "enables the service in
//! account for the current Amazon Web Services Region", `DescribeSecurityHubV2`
//! "returns details about the service resource in your account" and every V2
//! operation that needs a subscription answers the ARN of that one resource
//! (`smithy_query(query="operation",
//! operation="EnableSecurityHubV2,DescribeSecurityHubV2")`). The record below is
//! therefore keyed on the same [`super::HubKey`] the CSPM hub is, and holds what a
//! read of the subscription reports: the ARN, the instant it was created, the tags it
//! was created with, and the status of each opt-in feature.

use std::collections::{BTreeMap, HashMap};

use super::*;
use crate::ops::types::{FeatureDetail, FEATURE_NAME_VALUES};

/// The resource part of a hub V2 ARN.
///
/// The ARN's resource TYPE is `hubv2`: the one place an Amazon Web Services document
/// in this checkout spells a Security Hub V2 resource ARN spells it that way —
/// "For Security Hub, use `arn:aws:securityhub:us-east-1:111122223333:hubv2/*`"
/// (`PutDeliverySource`'s `resourceArn`, `smithy_query(query="doc",
/// shape="PutDeliverySourceRequest", member="resourceArn",
/// model_path="services/service-com-amazonaws-cloudwatchlogs-logs-20140328/smithy-model/com-amazonaws-cloudwatchlogs-logs-20140328.json")`).
/// The name part is `default`, as the CSPM hub's [`super::HUB_RESOURCE`] is: the
/// account holds exactly one such resource per Region, so nothing distinguishes one
/// from another.
pub const HUB_V2_RESOURCE: &str = "hubv2/default";

/// `FeatureStatus$ENABLED`: the feature is turned on for the account in this Region
/// (`smithy_query(query="shape", shape="FeatureStatus")`).
pub const FEATURE_STATUS_ENABLED: &str = "ENABLED";

/// `FeatureStatus$DISABLED`: the feature is turned off — the status of every opt-in
/// feature nobody has opted into yet (`smithy_query(query="shape",
/// shape="FeatureStatus")`).
pub const FEATURE_STATUS_DISABLED: &str = "DISABLED";

/// One opt-in feature's current state, as the modelled `FeatureDetail` reports it.
#[derive(Debug, Clone)]
pub struct FeatureRecord {
    /// A `FeatureStatus`: whether the account has opted into the feature.
    pub feature_status: String,
    /// `FeatureDetail$UpdatedAt`: "The date and time when the feature status was last
    /// updated" — the instant the subscription was created until an opt-in operation
    /// moves the status.
    pub updated_at: ::aws_smithy_types::DateTime,
}

/// The hub V2 resource `EnableSecurityHubV2` creates and `DescribeSecurityHubV2`
/// reports.
#[derive(Debug, Clone)]
pub struct HubV2 {
    /// The ARN this subscription answers to, [`hub_v2_arn`] of its key.
    pub hub_v2_arn: String,
    /// `DescribeSecurityHubV2Response$SubscribedAt`, an `IsoString`: when the service
    /// was enabled in the account. Stable for the subscription's lifetime.
    pub subscribed_at: String,
    /// The tags `EnableSecurityHubV2`'s `Tags` carried.
    pub tags: HashMap<String, String>,
    /// Every opt-in feature the model publishes, with the status it currently has for
    /// this account and Region. Ordered so one read never reports the map in a
    /// different order than the next.
    pub features: BTreeMap<String, FeatureRecord>,
}

/// `arn:aws:securityhub:<region>:<account>:hubv2/default` for a hub key.
pub fn hub_v2_arn(key: &HubKey) -> String {
    // `emulator_arn` stamps the canonical emulator account id; the key's account is
    // that same id, so the two cannot drift.
    ::engine::emulator_arn(ARN_NAMESPACE, &key.1, HUB_V2_RESOURCE)
}

/// The subscription `EnableSecurityHubV2` creates for `key` with `tags`.
///
/// Every opt-in feature the model declares (`FeatureName`, whose values the generated
/// [`FEATURE_NAME_VALUES`] carries) starts `DISABLED`: a feature is opt-in, so until
/// `EnableSecurityHubFeatureV2` turns one on its status was last set when the
/// subscription itself was created.
pub fn new_hub_v2(key: &HubKey, tags: HashMap<String, String>) -> HubV2 {
    let created = now_date_time();
    HubV2 {
        hub_v2_arn: hub_v2_arn(key),
        subscribed_at: created
            .fmt(::aws_smithy_types::date_time::Format::DateTime)
            .unwrap_or_default(),
        tags,
        features: FEATURE_NAME_VALUES
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    FeatureRecord {
                        feature_status: FEATURE_STATUS_DISABLED.to_string(),
                        updated_at: created,
                    },
                )
            })
            .collect(),
    }
}

/// The one projection a read reports the subscription's features with, so every
/// operation that reports them describes the same stored state identically.
pub fn features_view(hub: &HubV2) -> HashMap<String, FeatureDetail> {
    hub.features
        .iter()
        .map(|(name, record)| {
            (
                name.clone(),
                FeatureDetail {
                    feature_status: Some(record.feature_status.clone()),
                    updated_at: Some(record.updated_at),
                },
            )
        })
        .collect()
}

/// Set `feature_name`'s status on `hub` to `status`, answering whether that CHANGED
/// anything.
///
/// Both opt-in operations are documented idempotent — "The operation is idempotent.
/// If the feature is already enabled, no changes are made" (`smithy_query(query="doc",
/// shape="EnableSecurityHubFeatureV2")`, and the same sentence for the disable) — so a
/// call that asks for the status the feature already has leaves its `UpdatedAt` where
/// it is: that member reports when the status last changed, not when it was last
/// asked for.
pub fn set_feature_status(hub: &mut HubV2, feature_name: &str, status: &str) -> bool {
    let record = hub
        .features
        .entry(feature_name.to_string())
        .or_insert_with(|| FeatureRecord {
            feature_status: FEATURE_STATUS_DISABLED.to_string(),
            updated_at: now_date_time(),
        });
    if record.feature_status == status {
        return false;
    }
    record.feature_status = status.to_string();
    record.updated_at = now_date_time();
    true
}

/// How many distinct Regions this store holds a hub V2 subscription for.
///
/// The Region a subscription is filed under comes from the request's own SigV4
/// credential scope, which the caller chooses and nothing authenticates, so the
/// per-Region singleton above bounds this map only together with a cap on how many
/// Regions it holds subscriptions in at all. CHOSEN, not published: Amazon Web
/// Services operates on the order of thirty-odd commercial Regions, so a caller
/// enabling the service everywhere it exists stays far below this, while a caller
/// signing for invented Region names cannot grow the store without end.
pub const MAX_HUB_V2_REGIONS: usize = 64;

// The cap is a small finite figure and stays one: this fails to COMPILE otherwise, so a
// later edit cannot turn the bound into one a caller can fill the store under.
const _: () = assert!(MAX_HUB_V2_REGIONS <= 64);

/// The message `EnableSecurityHubV2` refuses a subscription with when it would be the
/// first in a Region past [`MAX_HUB_V2_REGIONS`]: the bound this emulator holds and
/// the Region that asked for one more.
pub fn hub_v2_region_ceiling_message(region: &str) -> String {
    format!(
        "this emulator holds hub V2 subscriptions in at most {MAX_HUB_V2_REGIONS} Regions at \
         once, and {region} would be one more"
    )
}

/// Whether a subscription for `key` would take this store past
/// [`MAX_HUB_V2_REGIONS`] — the one comparison the create makes before it inserts.
///
/// A Region the store already holds a subscription in needs no room for a further
/// one, so a repeated enable is never refused here. The account is the single
/// emulator account, so the number of subscriptions IS the number of Regions held.
pub fn hub_v2_region_ceiling_reached(
    store: &ComAmazonawsSecurityhubSecurityhubapiserviceState,
    key: &HubKey,
) -> bool {
    !store.hubs_v2.contains_key(key) && store.hubs_v2.len() >= MAX_HUB_V2_REGIONS
}

/// The message a V2 operation refuses a caller with when the account holds no hub V2
/// subscription in the request's Region — the resource those operations address, so
/// its absence is the modelled `ResourceNotFoundException`, "The request was rejected
/// because we can't find the specified resource" (`smithy_query(query="operation",
/// operation="DescribeSecurityHubV2")`).
///
/// The account id is the one variable part. The Region is NOT interpolated: Security
/// Hub answers through per-Region endpoints, so the Region a refusal is about is the
/// endpoint the caller dialled rather than anything the request stated, and the
/// service states only the precondition that failed — which is also what the replayed
/// recording of this refusal carries.
pub fn hub_v2_not_found_message(account_id: &str) -> String {
    format!("Account {account_id} is not subscribed to AWS Security Hub")
}

/// The message a feature opt-in operation refuses an unknown `FeatureName` with: the
/// value the request carried, followed by the values `FeatureName` declares.
///
/// Reachable only on a protocol that does not refuse an unmodelled enum value at the
/// boundary; the published set is the model's own, never a hand-written list.
pub fn unknown_feature_message(feature_name: &str) -> String {
    format!(
        "Feature {feature_name} is not one of: {}",
        FEATURE_NAME_VALUES.join(", ")
    )
}
