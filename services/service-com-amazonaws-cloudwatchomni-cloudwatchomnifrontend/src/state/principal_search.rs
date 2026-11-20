// SPDX-License-Identifier: CC0-1.0

//! The PRINCIPALS a domain's principal search reports, which are not this service's.
//!
//! `SearchPrincipals` "searches Identity Center for users and groups in a domain", and
//! those users and groups belong to IAM Identity Center's identity store rather than to
//! CloudWatchOmni: nothing here keeps a second copy of them. They are read out of the
//! identity store service's OWN state, reached through the SAME shared engine this
//! service's store lives in, so a search reports exactly the identities
//! `CreateUser`/`CreateGroup` wrote there and `DescribeUser`/`DescribeGroup` report — the
//! same seam [`::service_com_amazonaws_identitystore_awsidentitystore`] is read through
//! wherever one service's resource names an Identity Center identity.
//!
//! A CloudWatchOmni domain names an Identity Center INSTANCE (its
//! `identityCenterConfiguration`), never an identity store, and an identity store's users
//! and groups are addressed by identifiers unique across the account — so the search
//! covers every identity store this emulator holds, the stores walked in identifier order
//! and the identities inside each in the order the OWNING crate lists them
//! (`users_in_order` / `groups_in_order`). That order is total, so a paginated walk is
//! stable across calls.
//!
//! A READ of the other service: nothing here is written, so searching cannot change what a
//! later identity-store caller sees.
//!
//! The work one search does is BOUNDED by the owning service's own quotas — it holds at
//! most `MAX_IDENTITY_STORES` stores of `MAX_USERS_PER_STORE` users and
//! `MAX_GROUPS_PER_STORE` groups — so a request walks and allocates in proportion to state
//! that cannot grow without end, and the page the caller gets is bounded by `maxResults`.

use crate::ops::types::{Domain, PrincipalSearchResult};

/// `PrincipalType`'s value for an Identity Center user — its model doc for the search
/// result's `userName` reads "Present for users only".
pub const PRINCIPAL_TYPE_USER: &str = "USER";

/// `PrincipalType`'s value for an Identity Center group.
pub const PRINCIPAL_TYPE_GROUP: &str = "GROUP";

/// The search term that asks for every principal.
///
/// `SearchPrincipalsInput$searchQuery`'s model doc: "A search term to match against user
/// names, display names, and IDs. Pass * to list all principals."
pub const SEARCH_ALL_PRINCIPALS: &str = "*";

/// Whether `domain` is configured with Identity Center, which is the condition
/// `SearchPrincipals`'s own doc states ("The domain must be configured with Identity
/// Center").
///
/// A domain's `identityProviders` is the set of providers it was configured with, and
/// `IdentityProvider` enumerates exactly `IAM` and `IDC` — so a domain is an Identity
/// Center domain when that set names [`super::IDENTITY_PROVIDER_IDC`], whether or not it
/// also names `IAM`.
pub fn is_identity_center_domain(domain: &Domain) -> bool {
    domain
        .identity_providers
        .iter()
        .any(|provider| provider == super::IDENTITY_PROVIDER_IDC)
}

/// How a search of a domain that is NOT configured with Identity Center is worded.
///
/// One wording, read by every operation that refuses on this condition: it states what
/// the domain is missing — the Identity Center application a provisioned domain carries
/// ([`super::identity_center_application_arn`]) — and then the step that clears it. It is
/// WHOLLY FIXED TEXT: the condition is a property of the domain rather than of the
/// request, so the sentence names neither the domain the request addressed nor the
/// operation that refused, and every domain in this state earns exactly this message.
pub const NOT_IDENTITY_CENTER_MESSAGE: &str =
    "Domain does not have an IdC application ARN configured. The domain must be provisioned \
     with Identity Center before this operation can proceed.";

/// Every Identity Center principal this emulator holds, as the search results they are
/// reported as, in the deterministic order this module's docs state.
///
/// Each member is the identity store's own record rather than a value derived here:
///
/// * `principalId` is the user's or group's identifier, which is what
///   `SearchPrincipals`'s doc says to pass to `CreateAccessGrant` as the principal;
/// * `displayName` is `@required`, so a record that carries no display name reports the
///   next thing that names it — a user's user name, and failing that the identifier the
///   identity store knows it by;
/// * `userName` is a user's `UserName` and is absent for a group ("Present for users
///   only");
/// * `description` is a group's `Description`. The identity store holds no description
///   for a USER, so a user reports none rather than an invented one.
///
/// An emulator with no identity store mounted holds no Identity Center identities at all,
/// which is a search that matches nothing.
pub fn identity_center_principals<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Vec<PrincipalSearchResult> {
    use ::service_com_amazonaws_identitystore_awsidentitystore::state as identity_store;

    let Some(state) = identity_store::service_state(engine) else {
        return Vec::new();
    };
    let state = state.lock();
    let mut store_ids: Vec<&String> = state.stores.keys().collect();
    store_ids.sort();
    let mut principals = Vec::new();
    for store in store_ids.iter().filter_map(|id| state.stores.get(*id)) {
        for user in store.users_in_order() {
            principals.push(PrincipalSearchResult {
                principal_id: user.user_id.clone(),
                principal_type: PRINCIPAL_TYPE_USER.to_string(),
                display_name: user
                    .display_name
                    .clone()
                    .or_else(|| user.user_name.clone())
                    .unwrap_or_else(|| user.user_id.clone()),
                user_name: user.user_name.clone(),
                description: None,
            });
        }
        for group in store.groups_in_order() {
            principals.push(PrincipalSearchResult {
                principal_id: group.group_id.clone(),
                principal_type: PRINCIPAL_TYPE_GROUP.to_string(),
                display_name: group
                    .display_name
                    .clone()
                    .unwrap_or_else(|| group.group_id.clone()),
                user_name: None,
                description: group.description.clone(),
            });
        }
    }
    principals
}

/// Whether `search_query` matches `principal`.
///
/// The term is matched "against user names, display names, and IDs" — the three members a
/// result reports an identity by — and [`SEARCH_ALL_PRINCIPALS`] matches every principal.
/// A term matches when one of those members CONTAINS it, compared without regard to case:
/// an Identity Center user or group is not distinguished by case, so neither is a search
/// for one.
pub fn principal_matches(search_query: &str, principal: &PrincipalSearchResult) -> bool {
    if search_query == SEARCH_ALL_PRINCIPALS {
        return true;
    }
    let term = search_query.to_lowercase();
    [
        Some(principal.principal_id.as_str()),
        Some(principal.display_name.as_str()),
        principal.user_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|member| member.to_lowercase().contains(&term))
}
