// SPDX-License-Identifier: CC0-1.0

//! WHOSE registry a request addresses, and the refusal for one that addresses another
//! account's.
//!
//! `registryId` is how an Amazon ECR request names the registry it is about, and a registry
//! belongs to ONE account: "the Amazon Web Services account ID associated with the registry
//! … If you do not specify a registry, the default registry is assumed". A request naming an
//! account other than the one this registry serves therefore reaches into a registry whose
//! contents that account shares only through its OWN resource-based policy, which no
//! identity-based policy written by the caller's account can substitute for — so it is an
//! authorization question, decided here, and never a malformed parameter: the request
//! boundary has already settled the member's `^[0-9]{12}$` shape, so what arrives is a
//! well-formed account id.
//!
//! This emulator serves exactly one account, so no other account's registry is here to carry
//! a policy that could allow such a call, and the decision is always the refusal
//! [`registry_refusal`] words — the one wording of a withheld grant in this gate, naming the
//! caller, the action and the resource in the registry that was ADDRESSED.
//!
//! The decision is a handful of comparisons over what the request carries: it resolves the
//! caller (the same lookups [`super::refusal`] makes), compares two account ids, and on a
//! refusal formats one sentence. It holds no lock over this service's store, loops over
//! nothing, allocates nothing sized by anything the caller sends, and reads the account id
//! as the string it is rather than taking it apart — so a request cannot drive the work it
//! does, and there is nothing in it to index, slice or unwrap.

use super::*;

/// The resource a decision is taken about when the request names no resource at all: the
/// wildcard an IAM policy names every resource by, which is what a registry-scoped call
/// — one addressing the registry itself rather than a repository in it — is decided
/// against.
pub const ANY_RESOURCE: &str = "*";

/// The reason clause of a refusal taken on a registry of ANOTHER account.
///
/// Within one account either document can grant a call ([`withheld_because`]); ACROSS
/// accounts only the target's own resource-based policy can, because the caller's
/// identity-based policies are written by an account that holds no authority over another
/// account's resources. So that is the document AWS names when it refuses a cross-account
/// call, and it is a different sentence from the same-account default deny.
fn withheld_across_accounts(action: &str) -> String {
    format!("because no resource-based policy allows the {action} action")
}

/// The resource a call addressed to the registry `registry_id` is decided against: the ARN
/// the repository the request NAMES carries in THAT registry, and [`ANY_RESOURCE`] for a
/// registry-scoped call that names no repository.
fn registry_resource(region: &str, registry_id: &str, repository_name: Option<&str>) -> String {
    repository_name.map_or_else(
        || ANY_RESOURCE.to_string(),
        |name| crate::state::repository_arn_in(region, registry_id, name),
    )
}

/// `Some(message)` when the request addresses the registry of an account that is not the
/// one this registry serves — the message the refusal carries — and `None` when it names
/// this account's registry or names none at all.
///
/// An Amazon ECR registry belongs to ONE account, and a call reaching into another
/// account's registry is served only when a resource-based policy of that registry allows
/// the caller: a well-formed `registryId` (the boundary validator has already settled its
/// `^[0-9]{12}$` shape) is therefore an authorization question and never a malformed
/// parameter. This emulator serves exactly one account, so no other account's registry is
/// here to carry a policy that could allow the call, and the decision is a refusal.
///
/// A READ: the caller is only resolved, so deciding a call changes nothing a later caller
/// sees.
pub fn registry_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    registry_id: Option<&String>,
    repository_name: Option<&str>,
) -> Option<String> {
    let named = registry_id.map(String::as_str)?;
    if named == crate::state::registry_id() {
        return None;
    }
    let action = action_for(operation);
    let resource = registry_resource(
        &crate::state::request_region(ctx.parts()),
        named,
        repository_name,
    );
    let caller = caller(engine, ctx);
    // The refusal is the security-relevant event of this call, so it is attributable the
    // way every other refusal of this gate is: who asked, for what, and the outcome.
    ::tracing::info!(
        target: "emulator::audit",
        audit = true,
        operation,
        outcome = "refused_access_denied",
        principal = %caller.arn,
        action = %action,
        resource = %resource,
        "the registry this call addresses belongs to another account, which shares none of \
         it with this caller"
    );
    Some(not_authorized(
        &caller.arn,
        &action,
        &resource,
        &withheld_across_accounts(&action),
    ))
}

/// Let a call addressed to the registry `registry_id` names through, or refuse it as
/// unauthorized ([`registry_refusal`]): the one call every handler carrying a `registryId`
/// makes, placed before it reads or writes this service's state.
///
/// Generic over the operation's own modelled error enum for the same reason
/// [`denied_on_repository`] is: the refusal is NOT one of them.
pub fn authorize_registry<Eng: Send + Sync + 'static, E>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    registry_id: Option<&String>,
    repository_name: Option<&str>,
) -> Result<(), crate::scaffolded::Scaffolded<E>> {
    match registry_refusal(engine, ctx, operation, registry_id, repository_name) {
        Some(message) => Err(crate::scaffolded::Scaffolded::access_denied(message)),
        None => Ok(()),
    }
}
