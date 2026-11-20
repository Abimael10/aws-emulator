// SPDX-License-Identifier: CC0-1.0

//! Whether every identity a bucket policy GRANTS TO is one the account can resolve — the
//! question `PutBucketPolicy` answers before it stores the document.
//!
//! A resource-based policy states WHO a permission is granted to, so S3 resolves the
//! identities a document names rather than taking the text on trust: "provide a valid
//! principal" is what it requires of the element, and a document naming one it cannot resolve
//! is refused `MalformedPolicy` with `Invalid principal in policy`, leaving the policy the
//! bucket already carried untouched. A grant to an identity that does not exist would
//! otherwise be stored as a grant to nobody, and the deployment that wrote it would look like
//! it had succeeded.
//!
//! An identity is resolved against the records of the SERVICE THAT OWNS IT, read over the
//! shared engine at the time of the call — never against a copy kept here. Three kinds of
//! value fail, and nothing else does:
//!
//! * an IAM USER or ROLE of this account that the account does not hold — the two IAM
//!   entities a resource policy grants to by name, each read under its own entity type
//!   ([`::service_common::iam_role::user_name`], [`::service_common::iam_role::role_name`]);
//! * an IAM GROUP, whatever the account holds under that name: "you cannot identify a user
//!   group as a `Principal` in a policy (such as a resource-based policy) because groups
//!   relate to permissions, not authentication" (IAM User Guide, *IAM identities*), so a group
//!   is never a principal to resolve — the grant has to name the group's members, or the
//!   permission has to be attached to the group as an identity-based policy instead;
//! * a CLOUDFRONT ORIGIN ACCESS IDENTITY this account holds no identity for. Such a principal
//!   is an IAM user of CloudFront's OWN account rather than of the caller's
//!   (`arn:aws:iam::cloudfront:user/CloudFront Origin Access Identity <id>`, the spelling the
//!   Amazon S3 User Guide's origin-access-identity bucket policy carries), so the value can
//!   name nothing else, and the only record of such an identity is the CloudFront store: the
//!   id is read and resolved there
//!   (`service_com_amazonaws_cloudfront_cloudfront2020_05_31::state`), which is also where the
//!   edge builds the same spelling when it fetches an origin as that identity.
//!
//! Everything else resolves: the `*` wildcard, an account id or an account's root, a service
//! principal, a federated or canonical-user principal, an assumed-role session, and an entity
//! of ANOTHER account, whose IAM this emulator is not the one to ask. A canonical user id in
//! particular names an account owner of any account as readily as an origin access identity of
//! this one, so nothing in such a value says which it was meant to be and it resolves. So does
//! every value of a kind whose owning store this engine does not carry at all: a refusal has
//! to be grounded in what that store holds.
//!
//! WHICH values a document names is the document reading
//! ([`crate::state::policy_principals`]); this module is only where those values meet the
//! records that resolve them.

use ::service_com_amazonaws_cloudfront_cloudfront2020_05_31::state as cloudfront_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

/// The first principal of `policy` the account cannot resolve as a grantee, or `None` when
/// every value it names resolves (see the module docs).
///
/// The values are judged in the order the document names them, so the one reported is the
/// first unresolvable one whatever kind it is. The number of lookups one document can cause is
/// bounded by the published size of a bucket policy ([`crate::state::MAX_BUCKET_POLICY_BYTES`]),
/// which the operation enforces before it asks, and each is a read of another store in this
/// same engine — nothing opens a socket or leaves the host. Each lookup takes and releases
/// that store's own lock, and this service's guard is not held while it does, so no two stores
/// are ever locked at once here.
#[must_use]
pub fn unresolvable_principal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    policy: &str,
) -> Option<String> {
    let named = crate::state::policy_principals(policy);
    named
        .into_iter()
        .find(|principal| unresolvable(engine, principal))
}

/// Whether `principal` is a value the account holds no identity for.
fn unresolvable<Eng: Send + Sync + 'static>(engine: &Eng, principal: &str) -> bool {
    // A CloudFront origin access identity is named as an IAM user of CLOUDFRONT's account, so
    // the value names one or names no identity of any kind: the question is CloudFront's, and
    // a store this engine does not carry answers it not at all.
    if let Some(id) = cloudfront_store::origin_access_identity_principal_id(principal) {
        return cloudfront_store::holds_origin_access_identity(engine, id) == Some(false);
    }
    unresolvable_iam_identity(engine, principal)
}

/// Whether `principal` names an IAM entity of this account that its IAM does not hold, or one
/// that is no principal at all.
///
/// Every answer here rests on IAM's own records, so an engine carrying NO IAM store resolves
/// every value — the group clause included: with nothing to ask, there is nothing to ground a
/// refusal in, which is the order the module docs state.
fn unresolvable_iam_identity<Eng: Send + Sync + 'static>(engine: &Eng, principal: &str) -> bool {
    let names_a_group = ::service_common::iam_role::group_name(principal).is_some();
    let names_a_user = ::service_common::iam_role::user_name(principal).is_some();
    let names_a_role = ::service_common::iam_role::role_name(principal).is_some();
    if !names_a_group && !names_a_user && !names_a_role {
        return false;
    }
    let Some(iam) = sts_store::iam_state(engine) else {
        return false;
    };
    if names_a_group {
        return true;
    }
    let store = iam.lock();
    if names_a_user {
        return !store.users.values().any(|user| user.arn == principal);
    }
    !store.roles.values().any(|role| role.arn == principal)
}
