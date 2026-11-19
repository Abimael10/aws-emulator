// SPDX-License-Identifier: CC0-1.0

//! The IDENTITIES an EMR Studio session mapping maps, which are not this service's.
//!
//! A mapping names one user or group of IAM Identity Center, addressed EITHER by "the
//! globally unique identifier (GUID) of the user or group"
//! (`SessionMappingDetail$IdentityId` doc) or by "the name of the user or group"
//! (`SessionMappingDetail$IdentityName` doc, which points at the Identity Store's own
//! `UserName`). Two members, ONE identity: the create names whichever the caller has, and
//! a `SessionMappingDetail` reports both, so a client reconciling mappings by the
//! human-meaningful name finds it on a mapping created from a GUID and the other way
//! round.
//!
//! The identity itself belongs to IAM Identity Center's identity store, not to Amazon EMR,
//! so the member the request did not carry is resolved THERE — in the identity store
//! service's own state, reached through the SAME shared engine this service's store lives
//! in, so what a mapping reports is the user or group `CreateUser`/`CreateGroup` really
//! wrote rather than a name derived from an id.
//!
//! Nothing is INVENTED when that store has nothing to say: an identity it holds no record of
//! leaves the request's own members exactly as they arrived — both are modelled optional.
//! WHETHER such an identity is refused is not this module's question but the Studio's
//! ([`super::studio_session_mappings`]): a Studio bound to an IAM Identity Center instance
//! holds its mappings to that directory's own users and groups, and one bound to none has no
//! directory to hold them to.

/// The identity a session mapping maps: the pair of members a `SessionMappingDetail`
/// reports for it, each present when it is KNOWN.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MappedIdentity {
    /// `IdentityId`: the GUID the identity store knows the user or group by.
    pub identity_id: Option<String>,
    /// `IdentityName`: the user's `UserName` or the group's `DisplayName`.
    pub identity_name: Option<String>,
}

/// Whether an identity a session mapping request names is the one a stored mapping holds:
/// either member matching it, compared the way a session mapping is keyed
/// ([`super::session_mapping_key`] folds the identity's case, because an Identity Center
/// user or group is not distinguished by case).
///
/// Taken as the two members a stored detail holds them in, so a scan of one Studio's
/// mappings asks the question without building an identity per candidate.
pub fn identity_addresses(
    identity_id: Option<&str>,
    identity_name: Option<&str>,
    identity: &str,
) -> bool {
    [identity_id, identity_name]
        .into_iter()
        .flatten()
        .any(|held| held.eq_ignore_ascii_case(identity))
}

/// `com.amazonaws.emr#IdentityType` (`crate::ops::types::IDENTITY_TYPE_VALUES`), for the
/// two kinds of identity a mapping maps.
pub mod identity_type {
    /// The mapped identity is an Identity Center user.
    pub const USER: &str = "USER";
    /// The mapped identity is an Identity Center group.
    pub const GROUP: &str = "GROUP";
}

/// The OTHER kind of identity a mapping may map: a mapping is keyed by its identity type as
/// well as by the identity, so one GUID is a user or a group and never both
/// ([`IdentityLookup::WrongType`]). `None` for a kind this service does not model.
#[must_use]
pub fn other_identity_type(identity_type: &str) -> Option<&'static str> {
    match identity_type {
        identity_type::USER => Some(identity_type::GROUP),
        identity_type::GROUP => Some(identity_type::USER),
        _ => None,
    }
}

/// How an identity a session-mapping request names stands in the identity store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityLookup {
    /// The identity, with every member the store knows filled in: the one the request named
    /// and the one it did not.
    Held(MappedIdentity),
    /// The store holds that value, but as the OTHER kind of identity
    /// ([`other_identity_type`]) — so the member that named it names no identity of the kind
    /// the request asked for.
    WrongType,
    /// No identity store this emulator holds has a record of it, under either kind.
    Unknown,
}

/// The identity a session mapping request names, with the member it did NOT carry filled
/// in from the identity store when that store knows the identity.
///
/// The store read is the identity store service's OWN state, reached through the SAME
/// shared engine this service's store lives in, so a Studio session mapping resolves the
/// very users and groups `CreateUser`/`CreateGroup` wrote and
/// `DescribeUser`/`DescribeGroup` report rather than a second copy of them.
///
/// `identity_type` says which kind of identity to resolve — a user's name is its
/// `UserName` and a group's is its `DisplayName` — and the lookup covers every identity
/// store this emulator holds, because an EMR Studio session mapping names no identity
/// store of its own: the identity is addressed by a GUID (or a name) unique across the
/// account's store. The stores are walked in identifier order and the identities inside
/// each in the order the OWNING crate lists them
/// ([`::service_com_amazonaws_identitystore_awsidentitystore::state::IdentityStoreData::users_in_order`]
/// / `groups_in_order`), so an identity two stores happen to hold the same name for
/// resolves to the same one on every call, by that service's own ordering rather than a
/// second one kept here.
///
/// A READ of the other service: nothing there is written, so creating a mapping cannot
/// change what a later identity-store caller sees.
pub fn resolve_identity<Eng: Send + Sync + 'static>(
    engine: &Eng,
    identity_type: &str,
    requested: &MappedIdentity,
) -> MappedIdentity {
    match look_up_identity(engine, identity_type, requested) {
        IdentityLookup::Held(held) => held,
        // Nothing the store could not resolve is invented: the request's own members stand.
        IdentityLookup::WrongType | IdentityLookup::Unknown => requested.clone(),
    }
}

/// How the identity a session-mapping request names stands in the identity store: the identity
/// itself when that store holds one of the kind the request asked for, that it holds one of the
/// OTHER kind, or that it holds no record of it at all.
///
/// The store read is the identity store service's OWN state, reached through the SAME shared
/// engine this service's store lives in, so a Studio session mapping is held against the very
/// users and groups `CreateUser`/`CreateGroup` wrote and `DescribeUser`/`DescribeGroup` report
/// rather than a second copy of them.
///
/// `identity_type` says which kind of identity to look for — a user's name is its `UserName`
/// and a group's is its `DisplayName` — and the lookup covers every identity store this
/// emulator holds, because an EMR Studio session mapping names no identity store of its own:
/// the identity is addressed by a GUID (or a name) unique across the account's store. The
/// stores are walked in identifier order and the identities inside each in the order the
/// OWNING crate lists them
/// ([`::service_com_amazonaws_identitystore_awsidentitystore::state::IdentityStoreData::users_in_order`]
/// / `groups_in_order`), so an identity two stores happen to hold the same name for resolves to
/// the same one on every call, by that service's own ordering rather than a second one kept
/// here. A kind the request asked for is looked for in EVERY store before the other kind is
/// answered for, so a value one store holds as a group and another as a user of the asked-for
/// kind resolves to the user.
pub fn look_up_identity<Eng: Send + Sync + 'static>(
    engine: &Eng,
    identity_type: &str,
    requested: &MappedIdentity,
) -> IdentityLookup {
    use ::service_com_amazonaws_identitystore_awsidentitystore::state as identity_store;

    let Some(state) = identity_store::service_state(engine) else {
        return IdentityLookup::Unknown;
    };
    let state = state.lock();
    let mut store_ids: Vec<&String> = state.stores.keys().collect();
    store_ids.sort();
    let stores: Vec<_> = store_ids
        .iter()
        .filter_map(|id| state.stores.get(*id))
        .collect();
    for store in &stores {
        if let Some(found) = held_identity(store, identity_type, requested) {
            // What the identity store holds wins over nothing, and never over a value the
            // request carried: the caller's own member is reported back as it was sent.
            return IdentityLookup::Held(MappedIdentity {
                identity_id: requested.identity_id.clone().or(found.identity_id),
                identity_name: requested.identity_name.clone().or(found.identity_name),
            });
        }
    }
    let Some(other) = other_identity_type(identity_type) else {
        // An identity kind this service does not model: nothing to look it up among.
        return IdentityLookup::Unknown;
    };
    if stores
        .iter()
        .any(|store| held_identity(store, other, requested).is_some())
    {
        return IdentityLookup::WrongType;
    }
    IdentityLookup::Unknown
}

/// The identity of kind `identity_type` that `store` holds for what the request named, if it
/// holds one.
fn held_identity(
    store: &::service_com_amazonaws_identitystore_awsidentitystore::state::IdentityStoreData,
    identity_type: &str,
    requested: &MappedIdentity,
) -> Option<MappedIdentity> {
    match identity_type {
        identity_type::GROUP => store
            .groups_in_order()
            .into_iter()
            .find(|group| {
                matches(requested.identity_id.as_deref(), &group.group_id)
                    || matches(
                        requested.identity_name.as_deref(),
                        group.display_name.as_deref().unwrap_or_default(),
                    )
            })
            .map(|group| MappedIdentity {
                identity_id: Some(group.group_id.clone()),
                identity_name: group.display_name.clone(),
            }),
        identity_type::USER => store
            .users_in_order()
            .into_iter()
            .find(|user| {
                matches(requested.identity_id.as_deref(), &user.user_id)
                    || matches(
                        requested.identity_name.as_deref(),
                        user.user_name.as_deref().unwrap_or_default(),
                    )
            })
            .map(|user| MappedIdentity {
                identity_id: Some(user.user_id.clone()),
                identity_name: user.user_name.clone(),
            }),
        // An identity kind this service does not model: nothing to look it up among.
        _ => None,
    }
}

/// Whether `requested` is the value `held` carries, when the request carried one at all.
///
/// Case-insensitive for the reason [`super::session_mapping_key`] gives: neither an
/// Identity Center name nor the hexadecimal GUID beside it is distinguished by case.
fn matches(requested: Option<&str>, held: &str) -> bool {
    requested.is_some_and(|requested| !held.is_empty() && held.eq_ignore_ascii_case(requested))
}
