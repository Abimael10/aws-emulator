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
/// WHICH stores the account has is the owning service's own rule, asked here through its
/// read-only reading of it ([`IdentityCenterState::account_identity_store_ids`]), so
/// nothing about that rule is restated in this crate. It is a READ: an identity-store
/// request decides none of it.
///
/// `false` — do not refuse — when nothing is KNOWN about the account's identity stores:
/// an emulator with no Identity Center to ask, or an account whose enablement nothing has
/// decided yet, in which case the store a caller names is the store its own environment
/// gave it and this emulator holds no instance that contradicts it. A refusal has to be
/// grounded in what the owning service answered, never assumed.
pub fn identity_store_is_absent<Eng: Send + Sync + 'static>(engine: &Eng, store_key: &str) -> bool {
    use ::service_com_amazonaws_ssoadmin_swbexternalservice::state as identity_center;

    let Some(state) = identity_center::service_state(engine) else {
        return false;
    };
    let Some(store_ids) = state.lock().account_identity_store_ids() else {
        return false;
    };
    !store_ids.iter().any(|held| held == store_key)
}
