// SPDX-License-Identifier: CC0-1.0

//! The organization's Security Hub delegated administrator accounts — the designation
//! `EnableOrganizationAdminAccount` makes and `ListOrganizationAdminAccounts` reports.
//!
//! The designation is NOT a hub-scoped resource, which is why it lives beside
//! [`super::Hub`] rather than inside it: it "designates the Security Hub CSPM
//! administrator account for an organization" and "can only be called by the
//! organization management account" (`smithy_query(query="operation",
//! operation="EnableOrganizationAdminAccount")`), so it belongs to the organization
//! the calling account manages, not to that account's own subscription — a management
//! account designates a delegated administrator whether or not it has subscribed
//! itself.
//!
//! What a designation is keyed on follows from the two members both operations carry.
//! Security Hub answers through per-Region endpoints and each of these calls states no
//! Region, so the Region a designation is filed under is the one the request was
//! signed for; `Feature` is "the feature for which the delegated admin account is
//! enabled … Defaults to Security Hub CSPM if not specified"
//! (`smithy_query(query="operation", operation="EnableOrganizationAdminAccount")`), so
//! the two features hold their designations independently. An organization has one
//! management account, which here is the single emulated account, so the Region and
//! the feature are the whole identity of a designation and each pair holds at most
//! one: `ListOrganizationAdminAccounts` reports "the Security Hub CSPM administrator
//! accounts" of the feature it is asked about, and a second administrator for one
//! feature in one Region is not something that read could describe.

use std::collections::HashSet;

use super::*;
use crate::ops::types::{AdminAccount, SECURITY_HUB_FEATURE_VALUES};

/// A designation's identity: the Region the designating call was signed for, plus the
/// `SecurityHubFeature` the delegated administrator is enabled for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OrganizationAdminKey {
    pub region: String,
    pub feature: String,
}

/// The delegated administrator designation `EnableOrganizationAdminAccount` writes.
#[derive(Debug, Clone)]
pub struct OrganizationAdminRecord {
    /// The account the organization's management account designated.
    pub admin_account_id: String,
    /// An `AdminStatus` value: "the current status of the Security Hub CSPM
    /// administrator account. Indicates whether the account is currently enabled as a
    /// Security Hub CSPM administrator" (`smithy_query(query="shape",
    /// shape="AdminAccount")`, member `Status`).
    pub status: String,
}

/// `SecurityHubFeature$SecurityHub` — Security Hub CSPM, the feature a call that names
/// none is about: `Feature` "defaults to Security Hub CSPM if not specified"
/// (`smithy_query(query="operation", operation="EnableOrganizationAdminAccount")`).
pub const SECURITY_HUB_FEATURE_CSPM: &str = "SecurityHub";

/// `AdminStatus$ENABLED`: the account "is currently enabled as a Security Hub CSPM
/// administrator" (`smithy_query(query="shape", shape="AdminStatus")`) — the status a
/// designation that nothing has asked to remove holds.
pub const ADMIN_STATUS_ENABLED: &str = "ENABLED";

/// The feature a call is about: the one it named, else the documented default.
pub fn requested_feature(feature: Option<&str>) -> String {
    feature.unwrap_or(SECURITY_HUB_FEATURE_CSPM).to_string()
}

/// Whether `feature` is a value of the modelled `SecurityHubFeature`, whose published
/// set is the generated [`SECURITY_HUB_FEATURE_VALUES`] and never a hand-written list.
pub fn is_security_hub_feature(feature: &str) -> bool {
    SECURITY_HUB_FEATURE_VALUES.contains(&feature)
}

/// The message an organization-admin operation refuses a `Feature` outside the
/// modelled enum with: the value the request carried, followed by the values the model
/// declares.
///
/// Reachable because `Feature` arrives as a plain string on this protocol, so the
/// boundary validation does not refuse a value outside the enum.
pub fn unknown_security_hub_feature_message(feature: &str) -> String {
    format!(
        "Feature {feature} is not one of: {}",
        SECURITY_HUB_FEATURE_VALUES.join(", ")
    )
}

/// The key a designation for `feature` in `region` is filed under.
pub fn organization_admin_key(region: &str, feature: &str) -> OrganizationAdminKey {
    OrganizationAdminKey {
        region: region.to_string(),
        feature: feature.to_string(),
    }
}

/// The one projection a read reports a designation with, so every operation that
/// reports it describes the same stored designation identically.
pub fn admin_account_view(record: &OrganizationAdminRecord) -> AdminAccount {
    AdminAccount {
        account_id: Some(record.admin_account_id.clone()),
        status: Some(record.status.clone()),
    }
}

/// The whole message an organization-admin operation refuses a member that must be an
/// Amazon Web Services account id, and carries a value that cannot be one, with.
///
/// Security Hub states the member in its wire spelling and quotes the value back as it
/// arrived — `Invalid accountId: <value>` — so the value the request carried is the
/// only variable part of the message.
pub fn invalid_account_id_message(account_id: &str) -> String {
    format!("Invalid accountId: {account_id}")
}

/// The whole message a designation that names the organization's management account —
/// the account making the call — is refused with.
///
/// Security Hub states the permission and nothing else: no account id, no Region and
/// no reason, so the refusal discloses nothing about the organization behind it and a
/// client can match the sentence as a constant.
pub const ORGANIZATION_DELEGATION_DENIED_MESSAGE: &str =
    "You don't have permissions to perform the requested operation";

/// Whether `admin_account_id` is an account administration can be delegated TO.
///
/// A delegated administrator is an account other than the organization's management
/// account: Amazon Web Services does not let the management account of an organization
/// be registered as the delegated administrator of a service integrated with it, so
/// the account making the designation — the management account, the only caller the
/// operation admits — cannot be the account it designates. Every other account can,
/// and the emulated caller is [`super::request_account_id`].
pub fn may_be_delegated_to(admin_account_id: &str) -> bool {
    admin_account_id != request_account_id()
}

/// The message a designation is refused with when the feature already holds a
/// different administrator account in this Region: the account that holds it, which is
/// the one a caller has to remove before designating another.
pub fn organization_admin_already_designated_message(current_admin_account_id: &str) -> String {
    format!(
        "The request was rejected because account {current_admin_account_id} is already the \
         delegated administrator for this organization. Disable it before designating another \
         account."
    )
}

/// How many distinct Regions this store holds a delegated administrator designation
/// for.
///
/// The Region a designation is filed under comes from the request's own SigV4
/// credential scope, which the caller chooses and nothing authenticates, so the
/// per-Region singleton above bounds this map only together with a cap on how many
/// Regions it holds designations in at all. CHOSEN, not published: Amazon Web Services
/// operates on the order of thirty-odd commercial Regions, so a caller designating an
/// administrator everywhere Security Hub exists stays far below this, while a caller
/// signing for invented Region names cannot grow the store without end.
pub const MAX_ORGANIZATION_ADMIN_REGIONS: usize = 64;

// The cap is a small finite figure and stays one: this fails to COMPILE otherwise, so a
// later edit cannot turn the bound into one a caller can fill the store under.
const _: () = assert!(MAX_ORGANIZATION_ADMIN_REGIONS <= 64);

/// The message a designation is refused with when it would be the first in a Region
/// past [`MAX_ORGANIZATION_ADMIN_REGIONS`]: the bound this emulator holds and the
/// Region that asked for one more.
pub fn organization_admin_region_ceiling_message(region: &str) -> String {
    format!(
        "this emulator holds delegated administrator designations in at most \
         {MAX_ORGANIZATION_ADMIN_REGIONS} Regions at once, and {region} would be one more"
    )
}

/// Whether a designation for `key` would take this store past
/// [`MAX_ORGANIZATION_ADMIN_REGIONS`] — the one comparison the designation makes
/// before it inserts.
///
/// A Region the store already holds a designation in needs no room for a further one,
/// so designating in a Region that already has an administrator for another feature is
/// never refused here.
pub fn organization_admin_region_ceiling_reached(
    store: &ComAmazonawsSecurityhubSecurityhubapiserviceState,
    key: &OrganizationAdminKey,
) -> bool {
    let held: HashSet<&str> = store
        .organization_admins
        .keys()
        .map(|held| held.region.as_str())
        .collect();
    !held.contains(key.region.as_str()) && held.len() >= MAX_ORGANIZATION_ADMIN_REGIONS
}

/// Designate `admin_account_id` as the delegated administrator of `key`'s feature in
/// `key`'s Region, answering the designation the store holds afterwards.
///
/// Designating the account a feature already holds is the SAME designation: it leaves
/// the stored record where it is, so a retried call is not a second designation.
pub fn designate_organization_admin(
    store: &mut ComAmazonawsSecurityhubSecurityhubapiserviceState,
    key: &OrganizationAdminKey,
    admin_account_id: &str,
) -> OrganizationAdminRecord {
    let record = store
        .organization_admins
        .entry(key.clone())
        .or_insert_with(|| OrganizationAdminRecord {
            admin_account_id: admin_account_id.to_string(),
            status: ADMIN_STATUS_ENABLED.to_string(),
        });
    record.clone()
}

/// The delegated administrators of `key`'s feature in `key`'s Region — the list a read
/// of that feature reports, which each key holds at most one of.
pub fn organization_admins_in(
    store: &ComAmazonawsSecurityhubSecurityhubapiserviceState,
    key: &OrganizationAdminKey,
) -> Vec<OrganizationAdminRecord> {
    store
        .organization_admins
        .get(key)
        .cloned()
        .into_iter()
        .collect()
}
