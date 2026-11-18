// SPDX-License-Identifier: CC0-1.0

//! The ONE authorization pre-condition this service applies to a resource ARN it was handed,
//! ahead of every input check and every lookup.
//!
//! Every input member of this service that carries an ARN of a resource of ITS OWN is
//! caller-chosen: the `resourceArn` of the tag APIs (`/tags/{resourceArn}`) and the
//! `SubscriptionProviderArn` of `GetRegisteredSubscriptionProvider` and
//! `DeregisterSubscriptionProvider` carry the identical modelled pattern, which constrains no
//! field of the ARN, and all of them address the one resource kind this service holds — a
//! registered subscription provider, reached through the one `providers` map
//! ([`crate::state`]). So a request on ANY of them can name a resource of another account.
//! The account field of that ARN settles authorization on its own: no document of the account
//! a request is served for can grant anything over a resource in a different account, so
//! there is nothing to look up and nothing a lookup could change about the verdict. That is
//! why the gate is stated here once and applied by EVERY handler that is handed such an ARN,
//! before it touches the store: a denial must not depend on whether this account happens to
//! hold a resource under the same id, a refusal of absence must never stand in for one of
//! authorization — a client distinguishes the two by status, treating 403 as permanent and
//! 404 as a resource it may create or ignore — and one resource kind must not answer one ARN
//! two different ways depending on which operation was addressed with it.
//!
//! An ARN of ANOTHER service's resource is not this gate's business:
//! `RegisterSubscriptionProvider`'s `SecretArn` names a Secrets Manager secret, and that
//! handler reads it by CALLING Secrets Manager, so whether the caller may reach it is
//! answered by the service that owns it rather than restated here.
//!
//! Nothing here reads the store or the clock: the decision is a function of the request's own
//! ARN and of the account contract this emulator serves, so it is the same on every call.

/// The account every request to this emulator is served for, and therefore the account a
/// resource has to belong to for the caller to hold anything over it.
///
/// Read from the account contract (the same source every ARN this emulator mints carries in
/// its account field), never from the request.
#[must_use]
pub fn caller_account() -> &'static str {
    ::engine::emulator_account_id()
}

/// The IAM principal every request to this emulator speaks for — the identity the account
/// holds and `sts:GetCallerIdentity` reports — which is the principal an authorization
/// refusal names.
///
/// Read from the shared definition of that identity, so the principal this service refuses in
/// the name of is the one every other service of this emulator names.
#[must_use]
pub fn caller_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The authorization verdict for a request addressed at the resource `resource_arn` names.
///
/// `Ok(())` when nothing about the ARN refuses the request — its account field is this
/// emulator's account, or it names no account at all (an account-less ARN belongs to no other
/// account). Otherwise the denial, ready to be returned from the error channel of any
/// operation this service was handed an ARN by: HTTP 403 with the principal whose
/// authorization was evaluated and nothing about what it asked for, so a caller cannot learn
/// from it what exists in an account that is not its own.
///
/// An ARN this service cannot parse is NOT denied here: it names no account to decide on, and
/// the surface already refuses it as a malformed value
/// ([`crate::scaffolded::UnmodeledRefusal::invalid_resource_arn`] for the tag APIs' label,
/// `crate::service::validation`'s constraint envelope elsewhere, both stated at the request
/// boundary by the modelled `@pattern` on the member).
pub fn authorize_resource_arn<E>(
    resource_arn: &str,
) -> Result<(), crate::scaffolded::Scaffolded<E>> {
    let Some(parsed) = ::service_common::arn::parse(resource_arn) else {
        return Ok(());
    };
    if parsed.account.is_empty() || parsed.account == caller_account() {
        return Ok(());
    }
    Err(crate::scaffolded::Scaffolded::access_denied(
        &caller_principal(),
    ))
}
