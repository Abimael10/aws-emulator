// SPDX-License-Identifier: CC0-1.0

//! The ONE resolution of an identity a resource-based policy GRANTS TO against this
//! account's IAM, for every service that resolves a policy document's principals before it
//! stores the document.
//!
//! A resource-based policy states WHO a permission is granted to, and a grant to an
//! identity that does not exist is a grant to nobody — so the services whose API takes such
//! a document resolve each identity it names rather than taking the text on trust. Which
//! IAM entity a value names is [`crate::iam_role`], the question is put on the wire by
//! [`crate::iam_client`], and whether the document is a policy at all is
//! [`crate::iam_policy`]: this module is only the one place those three are joined, so two
//! services asked about the same principal cannot answer differently.
//!
//! IAM holds an entity under a NAME and a PATH and reports both, so the answer distinguishes
//! an ARN naming an identity IAM holds from one naming the same name under a path it holds
//! nothing under ([`IdentityAnswer`]).
//!
//! What a service then DOES with the answer — which error it words, and which of its
//! operations resolve a principal at all — stays the service's, because each one models its
//! own refusal.
//!
//! NOTHING here opens a socket or leaves the host: the question travels over the operation
//! callback's in-process transport, which is the same path every other service's lookup of a
//! caller-supplied IAM entity takes, and the number of lookups a document can cause is
//! bounded by the modelled length of the member that carries it.

/// What this account's IAM ANSWERED about the identity a principal names, as the services
/// that resolve a grantee word their own refusals from.
///
/// An IAM entity is held under a NAME and a PATH and IAM reports both, so an ARN can name an
/// identity IAM holds, one it holds nothing under at all, or the right name under a path IAM
/// holds nothing under — three answers a service may state differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityAnswer {
    /// Nothing IAM answered refuses the principal: IAM holds the identity the ARN names, or
    /// nothing is known about the value (see [`resolve_identity`]).
    Resolved,
    /// IAM answered that this account holds no identity of that type under that name.
    Absent,
    /// IAM holds that name, under a DIFFERENT path than the one the ARN names.
    AtAnotherPath,
}

/// Whether IAM ANSWERED that `principal` names an identity this account does not hold.
///
/// The [`IdentityAnswer::Absent`] half of [`resolve_identity`], for a caller that states one
/// refusal for every value IAM does not resolve.
pub async fn identity_is_absent(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    principal: &str,
) -> bool {
    resolve_identity(ctx, region, principal).await == IdentityAnswer::Absent
}

/// What IAM answers about `principal`, resolved against this account's IAM.
///
/// `principal` is one value of a statement's `Principal` element — the element a
/// resource-based policy states its GRANTEES in — in whatever form the policy language
/// writes it: the IAM entity ARNs are the two such a policy grants to by name, a role and a
/// user, and each is resolved under its own entity type
/// ([`crate::iam_role::role_name`], [`crate::iam_role::user_name`]). The PATH each ARN names
/// ([`crate::iam_role::role_path`], [`crate::iam_role::user_path`]) is compared against the
/// path IAM reports holding the entity under, so an ARN that names the entity's name under
/// another path is [`IdentityAnswer::AtAnotherPath`] rather than resolved.
///
/// [`IdentityAnswer::Resolved`] — refuse nothing — for everything that is NOT such an
/// answer, because a refusal has to be grounded in what IAM said:
///
/// * a value that names no IAM role or user of this account: the `*` wildcard, an account id
///   or an account's root, a service principal, a federated or canonical-user principal, and
///   an entity of ANOTHER account, whose IAM this emulator is not the one to ask;
/// * an answer this emulator could not read, and any failure other than IAM reporting that
///   the entity is absent — including an emulator with no IAM mounted at all.
pub async fn resolve_identity(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    principal: &str,
) -> IdentityAnswer {
    if let Some(role_name) = crate::iam_role::role_name(principal) {
        let answer = crate::iam_client::iam_client(ctx, region)
            .get_role()
            .role_name(role_name)
            .send()
            .await;
        return match answer {
            Ok(output) => held_at(
                output.role().map(::aws_sdk_iam::types::Role::path),
                crate::iam_role::role_path(principal),
            ),
            Err(error) => absent_or_unknown(error.as_service_error().is_some_and(
                ::aws_sdk_iam::operation::get_role::GetRoleError::is_no_such_entity_exception,
            )),
        };
    }
    if let Some(user_name) = crate::iam_role::user_name(principal) {
        let answer = crate::iam_client::iam_client(ctx, region)
            .get_user()
            .user_name(user_name)
            .send()
            .await;
        return match answer {
            Ok(output) => held_at(
                output.user().map(::aws_sdk_iam::types::User::path),
                crate::iam_role::user_path(principal),
            ),
            Err(error) => absent_or_unknown(error.as_service_error().is_some_and(
                ::aws_sdk_iam::operation::get_user::GetUserError::is_no_such_entity_exception,
            )),
        };
    }
    IdentityAnswer::Resolved
}

/// What an entity IAM DOES hold says about the ARN that named it: the same identity when the
/// two paths agree, and one held somewhere else when they do not.
///
/// A path either side could not be read is nothing to refuse on: an answer this emulator
/// could not read is not IAM reporting a bad principal.
fn held_at(held: Option<&str>, named: Option<&str>) -> IdentityAnswer {
    match (held, named) {
        (Some(held), Some(named)) if held != named => IdentityAnswer::AtAnotherPath,
        _ => IdentityAnswer::Resolved,
    }
}

/// A failed lookup as an answer: absent only when IAM REPORTED the entity absent.
fn absent_or_unknown(reported_absent: bool) -> IdentityAnswer {
    if reported_absent {
        IdentityAnswer::Absent
    } else {
        IdentityAnswer::Resolved
    }
}
