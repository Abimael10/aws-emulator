// SPDX-License-Identifier: CC0-1.0

//! Why a request about a resource INSIDE a cluster is refused when the cluster the request
//! names cannot be resolved, and WHICH of the two modelled refusals it earns.
//!
//! `ClusterIdentifier` admits two forms (`@pattern
//! ^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,40})$`): the generated id PCS mints for a cluster
//! ([`super::GENERATED_ID_PREFIX`]) and a cluster NAME. Every cluster-scoped child operation
//! models `ResourceNotFoundException` and `AccessDeniedException` side by side, and the FORM
//! of the identifier is what selects between them ([`unresolved_cluster`]):
//!
//! * a GENERATED ID that resolves to nothing names a cluster of this account's own namespace
//!   and nothing else, so there is nothing to decide a permission about: the call is the
//!   modelled not-found, carrying the fixed sentence for a cluster, the identifier as supplied
//!   in `resourceId` and the identifier kind in `resourceType` ([`super::not_found`]) — which
//!   is what lets provisioning code that creates a cluster and then a child by the id it was
//!   handed treat "not there yet" as retryable;
//! * a NAME that resolves to nothing may be any account's, so PCS decides whether the caller
//!   may perform `pcs:<Operation>` on the cluster the request names before it looks for the
//!   queue or the compute node group inside it, and an unresolvable name cannot be authorized
//!   at all: the call is refused as a PERMISSIONS failure ([`cluster_denied`]). "The cluster
//!   ID doesn't exist" is one of the examples `AccessDeniedException`'s own documentation
//!   gives (`smithy_query(query="shape",shape="AccessDeniedException")`), and the refusal does
//!   not disclose whether such a cluster exists: the sentence is the same whether the name
//!   names nothing or names a cluster of another account.
//!
//! Only an operation whose OWN resource is the cluster (`GetCluster`, `DeleteCluster`,
//! `UpdateCluster`, and the child lookups once their cluster HAS resolved) reports a not-found
//! for the resource it addresses ([`super::not_found`]).
//!
//! The denial sentence itself is IAM's ([`not_authorized`]), built from the request and the
//! caller identity: the principal that signed the call, the action it asked for and the
//! identifier it named. The RESOURCE it names is that identifier exactly as the request
//! supplied it — a name, since the id form is answered by the not-found above — and not an ARN
//! built around it: the refusal is stated before anything is resolved, so the only resource
//! there is to name is the value the caller sent.
//!
//! WHO the caller is is not invented here. It is read from the same place
//! `sts:GetCallerIdentity` answers from — the session registry, keyed by the access key id
//! the request signed with — so the principal a refusal blames is the one the caller reads
//! back from STS for its own credentials. A key that names no session there belongs to the
//! account's own published credential, whose principal is the IAM user the account holds for
//! it, and an emulator with no STS mounted can only have been called by that credential.

use ::std::time::SystemTime;

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

use crate::ops::types::AccessDeniedException;

/// The IAM service prefix this service's actions carry: `pcs:GetQueue`,
/// `pcs:CreateComputeNodeGroup`. It is this service's ARN namespace
/// (`smithy_query(query="service_info")` -> `awsService.arnNamespace`).
pub const ACTION_PREFIX: &str = "pcs";

/// Why IAM refused the call, as IAM states it for a request about a resource it cannot
/// decide the question on: the resource may not exist, a policy may deny the action, or no
/// policy may grant it — and the refusal does not say which.
const UNDECIDABLE_REASON: &str = "as either the resource does not exist, some policy \
                                  explicitly denies access, or no policy grants access";

/// The sentence IAM states an authorization failure with: who was refused, which action, on
/// which resource, and the clause above.
///
/// The sentence itself is the tree's one statement of it
/// ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]), so a
/// wording change lands in one place for every service; only the CLAUSE is this service's,
/// because the question is one no document of this account decided.
///
/// Every value in it comes from the request and the credential it presented — the
/// principal, the action the operation names and the resource the request addresses — so
/// nothing has to be looked up to state it, which is why it can be stated before the
/// resource is resolved.
fn not_authorized(principal: &str, action: &str, resource: &str) -> String {
    ::service_common::iam_authorization::not_authorized_by_identity_policy(
        principal,
        action,
        resource,
        UNDECIDABLE_REASON,
    )
}

/// What a cluster-scoped child operation answers when its `clusterIdentifier` resolves to no
/// cluster: decided by the FORM of the identifier alone (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedCluster {
    /// The identifier is a generated cluster id (`pcs_…`) nothing holds: the modelled
    /// `ResourceNotFoundException`, built with [`super::not_found`] for [`super::CLUSTER`].
    NotFound,
    /// The identifier is a cluster NAME nothing resolves: the modelled
    /// `AccessDeniedException`, built with [`cluster_denied`].
    Denied,
}

/// Which of the two refusals an unresolvable `cluster_identifier` earns.
///
/// The question is the identifier's FORM, which is what `ClusterIdentifier`'s own pattern
/// draws: a value carrying the generated-id prefix ([`super::GENERATED_ID_PREFIX`]) can only
/// ever have been minted by this service, so one nothing holds is a resource that is not
/// there; a NAME is a value the caller chose and says nothing about whose cluster it is.
/// Asked by every cluster-scoped child operation, so none of them can word this differently.
///
/// TOTAL for every value a caller can send, which is why a handler may ask it while the
/// store's guard is held: the whole body is one `str::starts_with` over the identifier as
/// received. It reads no index and no byte range, parses nothing, allocates nothing, reaches
/// no store and returns one of two values — so no caller-supplied string, however long,
/// empty or non-ASCII, can make it fail, panic or poison a lock.
#[must_use]
pub fn unresolved_cluster(cluster_identifier: &str) -> UnresolvedCluster {
    match cluster_identifier.starts_with(crate::state::GENERATED_ID_PREFIX) {
        true => UnresolvedCluster::NotFound,
        false => UnresolvedCluster::Denied,
    }
}

/// The modelled `AccessDeniedException` a cluster-scoped operation is refused with when the
/// `clusterIdentifier` it names is a NAME that resolves to no cluster this account holds.
///
/// `principal` is the caller ([`caller_principal_arn`]), `operation` the operation's own
/// name (`Self::NAME` in a handler), which names the IAM action `pcs:<Operation>`, and
/// `cluster_identifier` the identifier as the request supplied it.
#[must_use]
pub fn cluster_denied(
    principal: &str,
    operation: &str,
    cluster_identifier: &str,
) -> AccessDeniedException {
    AccessDeniedException {
        message: not_authorized(
            principal,
            &format!("{ACTION_PREFIX}:{operation}"),
            cluster_identifier,
        ),
    }
}

/// The principal THIS request speaks for, as `sts:GetCallerIdentity` reports it — what a
/// refusal of the caller names.
///
/// Read with NO store of this service locked: the session registry is never reached while
/// this service's own guard is held. `resolve_caller` materialises the session expiries the
/// clock has already made due before it answers, a write owed at the same instant whether or
/// not anybody called, which is why a REPORTING operation may ask the question.
///
/// An expired session credential speaks for nobody, and the key it presented is not quoted
/// back: the refusal names it as the expired credential it is.
pub fn caller_principal_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    let access_key_id = ::service_common::request_access_key_id(ctx.parts());
    let identity = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
        ),
        // No STS store on this engine: the only credential a request can have presented is
        // the account's own published one.
        None => Ok(sts_store::ambient_identity()),
    };
    match identity {
        Ok(identity) => identity.arn,
        Err(_expired) => EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
    }
}

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the sentence is the fixed IAM text with the principal, the action and the
    /// resource of THIS request in it. The rows span the operations that address a cluster's
    /// children, each with a cluster NAME — the form this refusal answers for.
    #[test]
    fn a_cluster_refusal_names_the_caller_the_action_and_the_identifier() {
        let principal = "arn:aws:iam::000000000000:user/emulator";
        for (operation, identifier) in [
            ("GetQueue", "another-cluster"),
            ("CreateQueue", "no-such-cluster"),
            ("GetComputeNodeGroup", "AnotherMissingOne"),
            ("ListQueues", "cluster-nobody-holds"),
        ] {
            let refused = cluster_denied(principal, operation, identifier);
            // Spelled out rather than built from the constants, so this pins the WORDING
            // and not just the composition.
            let expected = format!(
                "User: {principal} is not authorized to perform: pcs:{operation} on resource: \
                 {identifier} as either the resource does not exist, some policy explicitly \
                 denies access, or no policy grants access"
            );
            assert_eq!(
                refused.message, expected,
                "case {operation} on {identifier}: expected {expected:?}, got {:?}",
                refused.message
            );
        }
    }

    /// RULE: the FORM of an unresolvable `clusterIdentifier` decides which refusal it earns —
    /// a generated id (`pcs_…`) is the not-found, a cluster NAME the denial.
    #[test]
    fn the_identifier_form_decides_which_refusal_an_unresolved_cluster_earns() {
        // Rows span both forms `ClusterIdentifier`'s pattern admits, at each end of the
        // lengths it allows: generated ids (the prefix alone, one this store would mint, and
        // one of another width) against names (the shortest the pattern allows, a hyphenated
        // one, and one that merely CONTAINS the prefix rather than carrying it).
        let rows: [(&str, &str, UnresolvedCluster); 7] = [
            ("the minted id form", "pcs_0123456789abcdef", UnresolvedCluster::NotFound),
            ("an id of another width", "pcs_1", UnresolvedCluster::NotFound),
            ("the prefix alone", "pcs_", UnresolvedCluster::NotFound),
            ("a short name", "abc", UnresolvedCluster::Denied),
            ("a hyphenated name", "no-such-cluster", UnresolvedCluster::Denied),
            ("a mixed-case name", "AnotherMissingOne", UnresolvedCluster::Denied),
            ("a name carrying the prefix inside it", "my-pcs_cluster", UnresolvedCluster::Denied),
        ];

        for (case, identifier, expected) in rows {
            let answered = unresolved_cluster(identifier);
            assert_eq!(
                answered, expected,
                "case {case:?} ({identifier:?}): expected {expected:?}, got {answered:?}"
            );
        }
    }
}
