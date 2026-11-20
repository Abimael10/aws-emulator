// SPDX-License-Identifier: CC0-1.0

//! The capacity providers Amazon ECS RESERVES for every account, and what an operation
//! asked to modify one of them answers.
//!
//! `MANAGED_CAPACITY_PROVIDERS` names them: they are published by Amazon ECS rather than
//! created by anybody, carry no configuration of the caller's, and are resolved rather than
//! stored ([`managed_capacity_provider`]) — so a request that asks to change or retire one
//! names a provider that is not the caller's to act on, and is refused before anything is
//! read out of the store.

use super::*;

/// `ClientException$message` for a request that asks to MODIFY one of the capacity
/// providers Amazon ECS reserves for every account.
///
/// Fixed text for this class: the sentence names no provider, no cluster and no operation,
/// so nothing in it comes from the request or from the store. `ClientException` is the
/// class Amazon ECS answers it in — the provider is there and well named, and what the
/// caller may not do is act on it, which is a client action refused rather than a parameter
/// value Amazon ECS cannot read ([`CAPACITY_PROVIDER_NOT_FOUND_MESSAGE`] is the other).
pub const RESERVED_CAPACITY_PROVIDER_MESSAGE: &str =
    "The specified capacity provider is reserved and cannot be modified by this operation. \
     Specify a valid name or ARN and try again.";

/// The refusal a request naming the capacity provider `name` earns when that provider is
/// one Amazon ECS reserves, or `None` for every other name — the ONE place an operation
/// that modifies a provider asks the question, so a delete and an update of a reserved
/// provider are refused alike.
///
/// `name` is the provider the request REFERS to
/// ([`capacity_provider_name_from_reference`]), so a reference spelled as the provider's
/// ARN is weighed the same as its short name.
pub fn reserved_capacity_provider_refusal(name: &str) -> Option<&'static str> {
    MANAGED_CAPACITY_PROVIDERS
        .contains(&name)
        .then_some(RESERVED_CAPACITY_PROVIDER_MESSAGE)
}
