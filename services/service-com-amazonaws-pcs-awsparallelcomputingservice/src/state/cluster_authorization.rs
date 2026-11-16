// SPDX-License-Identifier: CC0-1.0

//! Why a request about a resource INSIDE a cluster is refused when the cluster the request
//! names cannot be resolved.
//!
//! The cluster is the authorization boundary of every resource nested in one: PCS decides
//! whether the caller may perform `pcs:<Operation>` on the cluster the request names before
//! it looks for the queue or the compute node group inside it, so an identifier that
//! resolves to no cluster of this account cannot be authorized at all and the call is
//! refused as a PERMISSIONS failure rather than as a not-found. "The cluster ID doesn't
//! exist" is one of the examples `AccessDeniedException`'s own documentation gives
//! (`smithy_query(query="shape",shape="AccessDeniedException")`), and every cluster-scoped
//! child operation models that error.
//!
//! Two consequences a caller sees, and both are the point:
//!
//! * the error CODE is `AccessDeniedException`, a terminal permissions failure, not the
//!   `ResourceNotFoundException` that reads as "create the cluster and retry". Only an
//!   operation whose OWN resource is the cluster (`GetCluster`, `DeleteCluster`,
//!   `UpdateCluster`, and the child lookups once their cluster HAS resolved) reports a
//!   not-found ([`super::not_found`]);
//! * the refusal does not disclose whether the cluster exists: the sentence is the same
//!   whether the identifier names nothing or names a cluster of another account.
//!
//! The sentence itself is IAM's ([`not_authorized`]), built from the request and the caller
//! identity: the principal that signed the call, the action it asked for and the identifier
//! it named. The RESOURCE it names is that identifier exactly as the request supplied it —
//! a name or a `pcs_…` id — and not an ARN built around it: the refusal is stated before
//! anything is resolved, so the only resource there is to name is the value the caller sent.
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

/// The modelled `AccessDeniedException` a cluster-scoped operation is refused with when the
/// `clusterIdentifier` it names resolves to no cluster this account holds.
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
    /// resource of THIS request in it. The rows span the identifier forms
    /// `ClusterIdentifier` admits (a generated `pcs_` id and a cluster name) and the
    /// operations that address a cluster's children.
    #[test]
    fn a_cluster_refusal_names_the_caller_the_action_and_the_identifier() {
        let principal = "arn:aws:iam::000000000000:user/emulator";
        for (operation, identifier) in [
            ("GetQueue", "pcs_00000000000000000000"),
            ("CreateQueue", "no-such-cluster"),
            ("GetComputeNodeGroup", "AnotherMissingOne"),
            ("ListQueues", "pcs_11111111111111111111"),
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
}
