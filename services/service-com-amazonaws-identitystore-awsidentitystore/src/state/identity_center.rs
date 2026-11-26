// SPDX-License-Identifier: CC0-1.0

//! Which identity stores this account HAS, as IAM Identity Center reports them.
//!
//! This API creates no identity store: one comes with an IAM Identity Center instance and
//! is that instance's `InstanceMetadata$IdentityStoreId`, which is where a client reads
//! the id of the store it has. So the question "is this `IdentityStoreId` a store the
//! account has?" is answered by the service that OWNS instances, through its own
//! read-only reading of them, rather than decided from the identifier's form here.

/// The IAM Identity Center store the account's instances live in.
///
/// It is that service's OWN state struct, reached through the SAME shared engine this
/// service's store lives in, so the stores resolved here are the stores its
/// `CreateInstance` wrote and its `ListInstances` reports rather than a second copy of
/// them.
pub type IdentityCenterState =
    ::service_com_amazonaws_ssoadmin_swbexternalservice::state::ComAmazonawsSsoadminSwbexternalserviceState;

/// Whether IAM Identity Center ANSWERED that this account holds no identity store
/// `store_key` names.
///
/// WHICH stores the account has is the owning service's own rule, asked here through
/// [`account_identity_store_keys`], so nothing about that rule is restated in this crate.
/// It is a READ: an identity-store request decides none of it.
///
/// `false` — do not refuse — when nothing is KNOWN about the account's identity stores:
/// an emulator with no Identity Center to ask, or an account whose enablement nothing has
/// decided yet, in which case the store a caller names is the store its own environment
/// gave it and this emulator holds no instance that contradicts it. A refusal has to be
/// grounded in what the owning service answered, never assumed.
pub fn identity_store_is_absent<Eng: Send + Sync + 'static>(engine: &Eng, store_key: &str) -> bool {
    let Some(store_keys) = account_identity_store_keys(engine) else {
        return false;
    };
    !store_keys.iter().any(|held| held == store_key)
}

/// The identity stores IAM Identity Center ANSWERS that this account holds, as the store
/// keys this crate addresses a store by ([`super::identity_store_key`]), or `None` when
/// nothing is KNOWN about them.
///
/// The one place the question is asked, so the stores a request may be ADDRESSED to
/// ([`identity_store_is_absent`]) and the stores a reader REPORTS are one answer rather
/// than two derivations of it. The answer is the owning service's own read of its
/// instances ([`IdentityCenterState::account_identity_store_ids`]) — an identity store is
/// the store OF an Identity Center instance — so nothing about which stores exist is
/// decided here.
///
/// `None` when this emulator has no Identity Center to ask, or when that service has not
/// decided the account's enablement yet: in that window the account's identity stores are
/// not knowable from instances, and a caller's own environment is what names the store it
/// has.
///
/// A READ: it takes the Identity Center state's lock and releases it before returning,
/// writing nothing there and nothing here.
pub fn account_identity_store_keys<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<Vec<String>> {
    use ::service_com_amazonaws_ssoadmin_swbexternalservice::state as identity_center;

    let state = identity_center::service_state(engine)?;
    let store_ids = state.lock().account_identity_store_ids()?;
    Some(
        store_ids
            .iter()
            .filter_map(|store_id| super::identity_store_key(store_id))
            .collect(),
    )
}
