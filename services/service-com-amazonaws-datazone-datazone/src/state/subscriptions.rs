// SPDX-License-Identifier: CC0-1.0

//! Subscriptions: the access a decision on a subscription request materialises, and the
//! one view every operation that reports a subscription renders it through.
//!
//! A subscription is the ACCESS itself, not the ask for it: "Gets a subscription in
//! Amazon DataZone" (smithy_query operation="GetSubscription") reports a principal's
//! granted access to a published listing, and it is created by accepting the request that
//! asked for it — so a stored subscription always names the request it came from
//! ([`Subscription::subscription_request_id`]).
//!
//! What is STORED is only the identity of the access: its status, the principal and the
//! listing revision it is on, and who created or last changed it. Everything a response
//! reports ABOUT those references — a project's name, a listing's name, the published
//! entity, the asset scope the decision narrowed the access to — is resolved from the
//! domain on every read through [`subscription_view`], which renders them with the same
//! helpers [`super::subscription_request_view`] uses, so the request and the subscription
//! cannot describe one catalog record differently.

use super::{date_time, Domain, SubscribedListingRef, SubscribedPrincipalRef, SubscriptionRequest};
use crate::ops::types::{SubscribedListing, SubscribedPrincipal};

/// The `SubscriptionStatus` members (model enum
/// `com.amazonaws.datazone#SubscriptionStatus`: APPROVED | CANCELLED | REVOKED).
///
/// A subscription exists because a decision granted the access, so it is created
/// [`SUBSCRIPTION_STATUS_APPROVED`]; `RevokeSubscription` and `CancelSubscription` are
/// what move it to one of the other two. None of the three is in-progress, so a stored
/// subscription owes the clock no transition.
pub const SUBSCRIPTION_STATUS_APPROVED: &str = "APPROVED";
pub const SUBSCRIPTION_STATUS_CANCELLED: &str = "CANCELLED";
pub const SUBSCRIPTION_STATUS_REVOKED: &str = "REVOKED";

/// The `ConflictException` message `RevokeSubscription` refuses a subscription whose
/// access has already been given up by a CANCEL with: the ask conflicts with the status
/// the subscription currently carries.
///
/// Fixed text around two variable parts read from the request and the store — the
/// subscription addressed and the status it carries. The fixed text is this service's own
/// convention, shared with the `ConflictException` `CreateSubscriptionRequest` refuses a
/// domain whose status cannot take a request with, rather than text recorded from the
/// service: no recording exercises this refusal, so a later one can correct the wording.
/// A repeated REVOKE is not refused with it: `RevokeSubscription` reports an
/// already-REVOKED subscription as it stands, because the operation is `@idempotent`.
pub fn cannot_revoke_message(identifier: &str, status: &str) -> String {
    format!("Subscription {identifier} is {status}, so it cannot be revoked")
}

/// One stored subscription.
#[derive(Debug, Clone)]
pub struct Subscription {
    /// The subscription's own `SubscriptionId`, minted by
    /// [`super::ComAmazonawsDatazoneDatazoneState::mint_id`].
    pub id: String,
    pub domain_id: String,
    /// One of [`SUBSCRIPTION_STATUS_APPROVED`] / [`SUBSCRIPTION_STATUS_CANCELLED`] /
    /// [`SUBSCRIPTION_STATUS_REVOKED`].
    pub status: String,
    /// The subscription request whose acceptance created this subscription — the
    /// `subscriptionRequestId` a reported subscription carries, and what
    /// `ListSubscriptions` filters by.
    pub subscription_request_id: String,
    /// The principal the access is held by, as the request named it: the identity only,
    /// resolved for a response by [`super::subscribed_principal`].
    pub principal: SubscribedPrincipalRef,
    /// The listing revision the access is ON — the pair the accepted request pinned, so
    /// the subscription reports the catalog record the ask was made against.
    pub listing: SubscribedListingRef,
    pub created_by: String,
    pub created_at_ms: i64,
    /// The caller and instant of the last change to the subscription. `updated_at_ms`
    /// equals [`Self::created_at_ms`] and `updated_by` is absent until something updates
    /// it, because `updatedAt` is a REQUIRED output member while `updatedBy` is not
    /// (smithy_query operation="GetSubscription").
    pub updated_by: Option<String>,
    pub updated_at_ms: i64,
    /// Whether the permissions were retained when the access was given up — the
    /// `retainPermissions` a revoke carries. Absent while nothing has given it up.
    pub retain_permissions: Option<bool>,
}

impl Subscription {
    /// Whether the access this subscription records still stands, i.e. whether there is
    /// anything left for a give-up operation to take away.
    ///
    /// A subscription exists because a decision granted the access, so only
    /// [`SUBSCRIPTION_STATUS_APPROVED`] carries it; the other two members both mean the
    /// access has already been given up. The one rule a give-up operation asks, so
    /// `RevokeSubscription` and `CancelSubscription` cannot decide it differently.
    pub fn grants_access(&self) -> bool {
        self.status == SUBSCRIPTION_STATUS_APPROVED
    }

    /// Give the access up by REVOKING it: the terminal status, whether the permissions
    /// were retained, and the caller and instant that took it away.
    ///
    /// The one writer of a revocation, so a response can never report a status this
    /// store did not persist.
    pub fn record_revocation(
        &mut self,
        revoked_by: String,
        retain_permissions: Option<bool>,
        now: i64,
    ) {
        self.status = SUBSCRIPTION_STATUS_REVOKED.to_string();
        self.retain_permissions = retain_permissions;
        self.updated_by = Some(revoked_by);
        self.updated_at_ms = now;
    }

    /// This subscription's position in the one total order a listing of subscriptions
    /// pages over: the instant it was last changed, then its own id.
    ///
    /// `updatedAt` is the ordering the model states — `ListSubscriptions`'s `sortBy` is
    /// `@deprecated` with "Results are always sorted by updatedAt" (smithy_query
    /// operation="ListSubscriptions") — so the field is not the caller's to pick; only
    /// `sortOrder` is. Two subscriptions changed in the same millisecond are ordered by
    /// id after it, which is what makes the key TOTAL, so a page's token (which carries
    /// this key) resumes after exactly one of them. The separator is a NUL, which no
    /// `SubscriptionId` admits.
    ///
    /// The instant is biased into the unsigned range and zero-padded, so the TEXT order
    /// of the key is the NUMERIC order of the instant over the whole of `i64` rather
    /// than over the non-negative instants a clock happens to produce.
    pub fn order_key(&self) -> String {
        let position = i128::from(self.updated_at_ms) - i128::from(i64::MIN);
        format!("{position:020}\u{0}{}", self.id)
    }
}

/// The subscription a decision that ACCEPTS `request` materialises, with the id
/// `id` and `created_by` as the principal whose call took the decision.
///
/// `None` when the stored request records no principal or no listing: a reported
/// subscription requires both (smithy_query operation="GetSubscription"), and the
/// modelled `subscribedPrincipals`/`subscribedListings` are 1..1, so a request that
/// somehow recorded neither materialises nothing rather than a subscription missing a
/// member the model requires.
///
/// The one place a subscription is created, so `AcceptSubscriptionRequest` cannot leave a
/// differently-shaped record behind than a later decision path would.
pub fn approved_subscription(
    id: String,
    request: &SubscriptionRequest,
    created_by: String,
    now: i64,
) -> Option<Subscription> {
    Some(Subscription {
        id,
        domain_id: request.domain_id.clone(),
        status: SUBSCRIPTION_STATUS_APPROVED.to_string(),
        subscription_request_id: request.id.clone(),
        principal: request.principals.first()?.clone(),
        listing: request.listings.first()?.clone(),
        created_by,
        created_at_ms: now,
        updated_by: None,
        updated_at_ms: now,
        retain_permissions: None,
    })
}

/// A stored subscription as every operation that reports one answers it.
///
/// `GetSubscriptionOutput`, `RevokeSubscriptionOutput` and `CancelSubscriptionOutput`
/// declare the same members (smithy_query
/// operation="GetSubscription,RevokeSubscription,CancelSubscription"), so the view is
/// built once here and each handler maps it onto its own generated output struct: a read
/// and a status change cannot then report one stored subscription differently.
#[derive(Debug, Clone)]
pub struct SubscriptionView {
    pub id: String,
    pub created_by: String,
    pub updated_by: Option<String>,
    pub domain_id: String,
    pub status: String,
    pub created_at: ::aws_smithy_types::DateTime,
    pub updated_at: ::aws_smithy_types::DateTime,
    pub subscribed_principal: SubscribedPrincipal,
    pub subscribed_listing: SubscribedListing,
    pub subscription_request_id: Option<String>,
    pub retain_permissions: Option<bool>,
}

/// The stored subscription `subscription` of `domain`, as a response reports it.
///
/// `None` when the listing the subscription is on is no longer in the domain's catalog:
/// `subscribedListing` is required, so such a subscription cannot be reported at all and
/// the handler answers a server fault rather than a response missing a required member —
/// the rule [`super::subscription_request_view`] applies to the same reference.
///
/// The asset scope and permissions reported for the listing are the ones the ACCEPTED
/// request carries, since a decision records on that request what it narrowed the access
/// to; a request the store no longer holds leaves them absent rather than inventing a
/// narrowing this store cannot stand behind.
pub fn subscription_view(
    domain: &Domain,
    subscription: &Subscription,
) -> Option<SubscriptionView> {
    let request = domain
        .subscription_requests
        .get(&subscription.subscription_request_id);
    let asset_scopes = request.map(|request| request.asset_scopes.as_slice());
    let asset_permissions = request.map(|request| request.asset_permissions.as_slice());
    Some(SubscriptionView {
        id: subscription.id.clone(),
        created_by: subscription.created_by.clone(),
        updated_by: subscription.updated_by.clone(),
        domain_id: subscription.domain_id.clone(),
        status: subscription.status.clone(),
        created_at: date_time(subscription.created_at_ms),
        updated_at: date_time(subscription.updated_at_ms),
        subscribed_principal: super::subscribed_principal(domain, &subscription.principal),
        subscribed_listing: super::subscription_requests::subscribed_listing(
            domain,
            asset_scopes.unwrap_or_default(),
            asset_permissions.unwrap_or_default(),
            &subscription.listing,
        )?,
        subscription_request_id: Some(subscription.subscription_request_id.clone()),
        retain_permissions: subscription.retain_permissions,
    })
}
