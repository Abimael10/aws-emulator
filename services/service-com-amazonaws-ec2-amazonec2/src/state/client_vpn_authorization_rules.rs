// SPDX-License-Identifier: CC0-1.0

//! The ingress authorization rules of a Client VPN endpoint: the store they live in, what
//! names ONE rule, the status codes one moves through, the refusals for a rule an endpoint
//! already has and for one it does not have, and the writers an authorize and a revoke call.
//!
//! A submodule of [`crate::state`] beside [`super::client_vpn`], which holds the endpoints
//! themselves, their target networks and their routes: an authorization rule is a record of
//! its own — "ingress authorization rules act as firewall rules that grant access to
//! networks" (`AuthorizeClientVpnIngress`) — so its store, its refusals and its writers live
//! in their own file. Everything here is re-exported by `crate::state`, so a handler reaches
//! it as `crate::state::insert_client_vpn_authorization_rule` exactly as it reaches the
//! helpers of the module root.
//!
//! The store itself is a field of [`ComAmazonawsEc2Amazonec2State`]
//! (`client_vpn_authorization_rules`), because the state struct is one struct; it is only the
//! code that reads and writes it that lives here.
//!
//! ## Why a revoked rule is not kept
//!
//! `ClientVpnAuthorizationRuleStatusCode` declares `authorizing | active | failed | revoking`
//! and no `revoked` member, so there is no state in which a rule that has been taken off an
//! endpoint can be REPORTED: a revoke takes the rule out of the store and answers the
//! `revoking` state it left it in. That is the opposite of a Client VPN ENDPOINT, whose own
//! `ClientVpnEndpointStatusCode` does declare `deleting` and `deleted` — an endpoint stays
//! reportable in them, which is why its delete moves its status instead of dropping it.

use super::*;

/// "authorizing" — the state an authorization rule starts in
/// (`ClientVpnAuthorizationRuleStatusCode`), which `AuthorizeClientVpnIngress` answers with.
pub const CLIENT_VPN_AUTHORIZATION_RULE_STATUS_AUTHORIZING: &str = "authorizing";

/// "active" — the state the clock owes an `authorizing` rule ([`TRANSITION_DELAY`] later,
/// through [`ComAmazonawsEc2Amazonec2State::materialize_due_transitions`]), in which the rule
/// grants the access it describes.
pub const CLIENT_VPN_AUTHORIZATION_RULE_STATUS_ACTIVE: &str = "active";

/// "revoking" — the state `RevokeClientVpnIngress` answers for the rule it has taken off the
/// endpoint (`ClientVpnAuthorizationRuleStatusCode`).
pub const CLIENT_VPN_AUTHORIZATION_RULE_STATUS_REVOKING: &str = "revoking";

/// `InvalidClientVpnDuplicateAuthorizationRule` (HTTP 400): the endpoint already has an
/// authorization rule for that network and that group, so adding this one would make two —
/// the code AWS's EC2 error catalogue documents for a duplicate Client VPN authorization
/// rule, read the same way [`INVALID_CLIENT_VPN_DUPLICATE_ROUTE`] is for a duplicate route.
pub const INVALID_CLIENT_VPN_DUPLICATE_AUTHORIZATION_RULE: &str =
    "InvalidClientVpnDuplicateAuthorizationRule";

/// `InvalidClientVpnEndpointAuthorizationRuleNotFound` (HTTP 400): the endpoint holds no
/// authorization rule for the network and group the request named, which is the catalogue's
/// counterpart code for a rule that cannot be resolved.
pub const INVALID_CLIENT_VPN_ENDPOINT_AUTHORIZATION_RULE_NOT_FOUND: &str =
    "InvalidClientVpnEndpointAuthorizationRuleNotFound";

/// Refuse an authorization rule the endpoint already has: HTTP 400
/// [`INVALID_CLIENT_VPN_DUPLICATE_AUTHORIZATION_RULE`], naming the network and the endpoint
/// in the sentence shape this surface words a duplicate with
/// ([`client_vpn_duplicate_route`]).
pub fn client_vpn_duplicate_authorization_rule<E>(
    destination_cidr: &str,
    endpoint_id: &str,
) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_CLIENT_VPN_DUPLICATE_AUTHORIZATION_RULE,
        400,
        format!(
            "Authorization rule for {destination_cidr} already exists in Client VPN endpoint \
             {endpoint_id}"
        ),
    )
}

/// Refuse a request naming an authorization rule the endpoint does not have: HTTP 400
/// [`INVALID_CLIENT_VPN_ENDPOINT_AUTHORIZATION_RULE_NOT_FOUND`], in the sentence shape this
/// surface words an absent member of an endpoint with
/// ([`client_vpn_active_association_not_found`]).
pub fn client_vpn_authorization_rule_not_found<E>(
    destination_cidr: &str,
    endpoint_id: &str,
) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_CLIENT_VPN_ENDPOINT_AUTHORIZATION_RULE_NOT_FOUND,
        400,
        format!(
            "There is no authorization rule for {destination_cidr} in Client VPN endpoint \
             {endpoint_id}"
        ),
    )
}

/// What names ONE ingress authorization rule of a Client VPN endpoint: the endpoint it is on,
/// the network it grants access to, and the group it grants that access to.
///
/// Those three are exactly what `RevokeClientVpnIngress` names a rule by
/// (`ClientVpnEndpointId` + `TargetNetworkCidr` + either `AccessGroupId` or
/// `RevokeAllGroups`), and `AuthorizationRule` carries no id member of its own, so the store
/// is keyed by them rather than by a minted id — the same reading [`ClientVpnRouteKey`] gives
/// a route.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClientVpnAuthorizationRuleKey {
    /// "The ID of the Client VPN endpoint with which the authorization rule is associated"
    /// (`AuthorizationRule$ClientVpnEndpointId`).
    pub client_vpn_endpoint_id: String,
    /// "The IPv4 address range, in CIDR notation, of the network to which the authorization
    /// rule applies" (`AuthorizationRule$DestinationCidr`), in the canonical form this
    /// service stores every CIDR member in.
    pub destination_cidr: String,
    /// "The ID of the Active Directory group to which the authorization rule grants access"
    /// (`AuthorizationRule$GroupId`), or the EMPTY string for the rule that grants access to
    /// all clients — which is the rule `RevokeAllGroups` revokes and which "does not impact
    /// other authorization rules that allowed ingress to the same `TargetNetworkCidr` with a
    /// specific `AccessGroupId`" (`RevokeClientVpnIngressRequest$RevokeAllGroups`).
    pub group_id: String,
}

impl ClientVpnAuthorizationRuleKey {
    /// This rule's position in the order its endpoint's rules are reported and paged in: the
    /// network, then the group.
    ///
    /// A single string so a page token can name a position with one cursor, separated by the
    /// character [`CLIENT_VPN_ROUTE_CURSOR_SEPARATOR`] is used for the same job on a route's
    /// own cursor — one that orders BELOW every character a CIDR block or a group id carries,
    /// so comparing two cursors as strings orders the pairs they name rather than running the
    /// two halves together.
    pub fn cursor(&self) -> String {
        format!(
            "{destination}{CLIENT_VPN_ROUTE_CURSOR_SEPARATOR}{group}",
            destination = self.destination_cidr,
            group = self.group_id,
        )
    }
}

/// One ingress authorization rule: the modelled [`crate::ops::types::AuthorizationRule`]
/// every operation that reports one answers with, plus the move the clock owes it.
///
/// The modelled shape itself rather than a parallel record, for the reason every other store
/// of this subject holds one: a member a writer sets round-trips through
/// `DescribeClientVpnAuthorizationRules` without a field-by-field copy that can silently drop
/// one.
#[derive(Debug, Clone)]
pub struct ClientVpnAuthorizationRuleRecord {
    /// The rule as the model shapes it, carrying the status reached so far.
    pub view: crate::ops::types::AuthorizationRule,
    /// The `ClientVpnAuthorizationRuleStatusCode` the clock owes this rule while it is still
    /// transient (`authorizing` -> `active`); `None` once it has been applied.
    pub pending: Option<PendingStateMove>,
}

/// What one authorization rule is written with: the endpoint it is added to, the network it
/// grants access to, who it grants that access to, and the description the request gave it.
///
/// Every member the service itself owns — the status the rule starts in — is derived by
/// [`ComAmazonawsEc2Amazonec2State::insert_client_vpn_authorization_rule`] rather than taken
/// from a caller, so two writers cannot report one rule differently.
#[derive(Debug, Clone, Default)]
pub struct ClientVpnAuthorizationRuleSpec {
    /// The endpoint the rule is added to.
    pub client_vpn_endpoint_id: String,
    /// The network the rule grants access to, in the canonical form this service stores a
    /// CIDR member in.
    pub destination_cidr: String,
    /// "The ID of the group to grant access to" (`$AccessGroupId`); `None` for a rule that
    /// grants access to all clients.
    pub group_id: Option<String>,
    /// "Indicates whether the authorization rule grants access to all clients"
    /// (`AuthorizationRule$AccessAll`).
    pub access_all: bool,
    /// "A brief description of the authorization rule" (`$Description`); `None` when the
    /// request gave none.
    pub description: Option<String>,
}

/// The caller-chosen bytes one stored authorization rule holds: the endpoint it is on, the
/// network it covers, the group it names and its description, on top of the fixed
/// [`STORED_RECORD_OVERHEAD_BYTES`] every stored record costs.
///
/// The ONE definition of that charge, as [`client_vpn_route_payload_bytes`] is for a route:
/// the insert charges exactly it, and the revoke gives back the same number.
pub fn client_vpn_authorization_rule_payload_bytes(
    view: &crate::ops::types::AuthorizationRule,
) -> usize {
    STORED_RECORD_OVERHEAD_BYTES
        .saturating_add(view.client_vpn_endpoint_id.as_deref().map_or(0, str::len))
        .saturating_add(view.destination_cidr.as_deref().map_or(0, str::len))
        .saturating_add(view.group_id.as_deref().map_or(0, str::len))
        .saturating_add(view.description.as_deref().map_or(0, str::len))
}

impl ComAmazonawsEc2Amazonec2State {
    /// Whether another authorization rule fits inside the store's ceiling
    /// ([`MAX_STORED_RECORDS_PER_STORE`]), on the contract of [`Self::has_vpc_capacity`]: a
    /// writer asks this BEFORE inserting.
    pub fn has_client_vpn_authorization_rule_capacity(&self) -> bool {
        self.client_vpn_authorization_rules.len() < MAX_STORED_RECORDS_PER_STORE
    }

    /// The authorization rule `key` names, or `None` when the store holds none under it.
    pub fn client_vpn_authorization_rule(
        &self,
        key: &ClientVpnAuthorizationRuleKey,
    ) -> Option<&ClientVpnAuthorizationRuleRecord> {
        self.client_vpn_authorization_rules.get(key)
    }

    /// Add one ingress authorization rule to a Client VPN endpoint and answer it as its
    /// writer reports it, beside the key it was stored under — or `None` when one of the
    /// store's bounds would be crossed and NOTHING was stored.
    ///
    /// The seam `AuthorizeClientVpnIngress` calls.
    ///
    /// The rule starts in `authorizing`, the state that call answers with, and the clock owes
    /// it `active` one [`TRANSITION_DELAY`] later through
    /// [`Self::materialize_due_transitions`]: the span is never zero, so a caller describing
    /// the rule straight after the call that made it still sees `authorizing`.
    pub fn insert_client_vpn_authorization_rule(
        &mut self,
        spec: ClientVpnAuthorizationRuleSpec,
    ) -> Option<(
        ClientVpnAuthorizationRuleKey,
        crate::ops::types::AuthorizationRule,
    )> {
        let key = ClientVpnAuthorizationRuleKey {
            client_vpn_endpoint_id: spec.client_vpn_endpoint_id.clone(),
            destination_cidr: spec.destination_cidr.clone(),
            group_id: spec.group_id.clone().unwrap_or_default(),
        };
        let view = crate::ops::types::AuthorizationRule {
            client_vpn_endpoint_id: Some(spec.client_vpn_endpoint_id),
            description: spec.description,
            group_id: spec.group_id,
            access_all: Some(spec.access_all),
            destination_cidr: Some(spec.destination_cidr),
            status: Some(crate::ops::types::ClientVpnAuthorizationRuleStatus {
                code: Some(CLIENT_VPN_AUTHORIZATION_RULE_STATUS_AUTHORIZING.to_string()),
                message: None,
            }),
        };
        let charge = client_vpn_authorization_rule_payload_bytes(&view);
        if !self.has_client_vpn_authorization_rule_capacity()
            || self.would_exceed_stored_bytes(charge)
        {
            return None;
        }
        self.add_stored_bytes(charge);
        let stored = view.clone();
        self.client_vpn_authorization_rules.insert(
            key.clone(),
            ClientVpnAuthorizationRuleRecord {
                view,
                pending: Some(PendingStateMove::starting_now(
                    CLIENT_VPN_AUTHORIZATION_RULE_STATUS_ACTIVE,
                )),
            },
        );
        Some((key, stored))
    }

    /// Take the authorization rule `key` names off its endpoint and answer it in the
    /// `revoking` state the revoke left it in — or `None` when the store holds no rule under
    /// that key.
    ///
    /// The seam `RevokeClientVpnIngress` calls. The record is DROPPED rather than moved to a
    /// terminal state, for the reason this module's own docs give: the model declares no
    /// state a revoked rule could be reported in. Its bytes go back to the budget, so one
    /// endpoint's rules can be authorized and revoked without the store filling up.
    ///
    /// Every `ClientToken` that names the rule is dropped with it, the way the removal of any
    /// other record of this crate forgets its tokens
    /// ([`Self::forget_route_table_client_tokens_of`]): a token that outlived the record it
    /// names would both grow the index without bound across an authorize-and-revoke loop and
    /// replay into a rule the store no longer holds. One entry per stored rule at most, so the
    /// index is bounded by the rule ceiling and by [`MAX_CLIENT_TOKEN_CHARS`].
    pub fn revoke_client_vpn_authorization_rule(
        &mut self,
        key: &ClientVpnAuthorizationRuleKey,
    ) -> Option<crate::ops::types::AuthorizationRule> {
        let record = self.client_vpn_authorization_rules.remove(key)?;
        self.client_vpn_authorization_rule_client_tokens
            .retain(|_, named| named != key);
        self.release_stored_bytes(client_vpn_authorization_rule_payload_bytes(&record.view));
        Some(crate::ops::types::AuthorizationRule {
            status: Some(crate::ops::types::ClientVpnAuthorizationRuleStatus {
                code: Some(CLIENT_VPN_AUTHORIZATION_RULE_STATUS_REVOKING.to_string()),
                message: None,
            }),
            ..record.view
        })
    }

    /// Apply the `authorizing` -> `active` move every authorization rule that has reached its
    /// due instant by `now` owes.
    ///
    /// Called by [`Self::materialize_due_transitions`], which is what a reporting operation
    /// calls: the move was owed at its instant whether or not anybody asked, so applying it
    /// on a read changes nothing a later caller would not have seen anyway.
    pub(crate) fn materialize_due_client_vpn_authorization_rules(&mut self, now: Instant) {
        for record in self.client_vpn_authorization_rules.values_mut() {
            let due = record
                .pending
                .as_ref()
                .and_then(|pending| pending.due_by(now))
                .map(str::to_string);
            if let Some(code) = due {
                record.view.status = Some(crate::ops::types::ClientVpnAuthorizationRuleStatus {
                    code: Some(code),
                    message: None,
                });
                record.pending = None;
            }
        }
    }
}
