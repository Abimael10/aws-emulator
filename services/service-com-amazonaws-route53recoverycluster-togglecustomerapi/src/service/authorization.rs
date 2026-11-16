// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this cluster (data-plane) API perform the action it asks
//! for — the authorization step every operation takes BEFORE it reads a routing control or
//! moves a switch.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at the resource
//! a request named, so a caller whose identity-based policies do not grant the action — or
//! that an explicit `Deny` covers — is told it is not authorized rather than told anything
//! about the routing control, and the call changes nothing. `AccessDeniedException` (HTTP
//! 403, "You don't have sufficient permissions to perform this action") is the modelled
//! answer: this model declares it for EVERY operation
//! (`smithy_query(query="digest", model_path="services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/smithy-model/com-amazonaws-route53recoverycluster-togglecustomerapi.json")`
//! lists it among the service's common errors), so every operation asks this gate first.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_caller`, the same record
//!    `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the one the
//!    caller reads back for the credentials it signed with), else the IAM user that owns the
//!    long-term key, else the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next request and deleting it lifts it
//!    again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the refusal's sentence does.
//! They stay in this crate because hoisting them into `service-common` would make every
//! service that depends on that crate link the IAM and STS service crates.
//!
//! ## The action and the resource
//!
//! The action is `<signing name>:<OperationName>`, read off the request's own resolved
//! identity rather than restated here — this service's `sigv4Name`, which is also its
//! `arnNamespace` (`smithy_query(query="service_info")`), so one statement scoped to
//! `route53-recovery-cluster:*` covers every operation of this API and nothing outside it.
//!
//! The resource is the one the REQUEST addresses: the routing control ARN a get or an
//! update names, the control panel ARN a listing is filtered by, and [`ANY_RESOURCE`] — what
//! IAM writes for a request that addresses no resource of its own — for a listing with no
//! filter. Taking it from the request rather than from a stored record is what keeps the
//! decision independent of whether that resource exists, which is also what keeps a refused
//! call from telling an unauthorized caller whether it does, and what puts this refusal
//! ahead of every precondition about the control's own state.
//!
//! A request judged against [`ANY_RESOURCE`] addresses every routing control the endpoint
//! serves, so for it the GRANT question is the weaker one `iam_authorization` states for a
//! gate that does not know one resource ARN: whether any readable `Allow` covers the action
//! at all. Refusing it on the resource element instead would deny calls AWS permits. The
//! DENY question is always decided against the resource, so a `Deny` scoped to an ARN the
//! request does carry still refuses it.
//!
//! This API publishes no resource-based policy of its own — the model declares no policy
//! operation and no policy member — so the decision here is the identity-based one alone.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to the account's own identity, which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses, and a permissions boundary the
//!   store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use crate::ops::types::AccessDeniedException;

/// The resource IAM writes for a request that addresses no single resource of this API.
pub const ANY_RESOURCE: &str = "*";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// Authorize this request, where `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]) and `resource` the ARN the request
/// ADDRESSES ([`ANY_RESOURCE`] when it addresses none).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see
/// the module docs); the modelled refusal otherwise, which each operation returns as its own
/// `AccessDeniedException`. Call it BEFORE the stores are reached, so a refused request reads
/// and writes nothing.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service may
/// take this decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), AccessDeniedException> {
    authorize_each(engine, ctx, operation, &[resource])
}

/// [`authorize`] for a request that addresses SEVERAL resources of its own — a batch update,
/// whose entries each name a routing control: the request is permitted only where every one
/// of them is, and the first refusal is the answer.
///
/// The caller is resolved and its documents are READ once for the whole request, so a batch
/// costs one store lookup and one document parse however many entries it carries; only the
/// per-resource statement walk repeats.
pub fn authorize_each<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resources: &[&str],
) -> Result<(), AccessDeniedException> {
    let action = format!("{}:{operation}", ctx.identity().signing_name());
    let caller = caller(engine, ctx);
    let documents = iam::prepare_documents(&caller.documents);
    let boundary = caller
        .boundary
        .as_ref()
        .map(|document| iam::prepare_documents(::std::slice::from_ref(document)));
    for resource in resources {
        decide(&caller, &documents, boundary.as_deref(), &action, resource)?;
    }
    Ok(())
}

/// The decision about ONE `(action, resource)` pair, over documents already read.
fn decide(
    caller: &Caller,
    documents: &[iam::PreparedDocument],
    boundary: Option<&[iam::PreparedDocument]>,
    action: &str,
    resource: &str,
) -> Result<(), AccessDeniedException> {
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no such
    // element, and this API publishes no resource-based policy (see the module docs), so the
    // decision below rests on the action and the resource.
    let request = iam::Request::new(action, resource, &caller.principals);
    let identity_said = iam::identity_decision_prepared(documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if identity_said == Decision::Denied {
        return Err(not_authorized(
            &caller.arn,
            action,
            resource,
            iam::IDENTITY_POLICY_EXPLICIT_DENY,
        ));
    }
    let granted = iam::is_allowed(identity_said, Decision::NotStated)
        || may_allow(documents, action, resource)
        || caller.identity_is_unbounded();
    if !granted {
        return Err(not_authorized(
            &caller.arn,
            action,
            resource,
            &iam::no_identity_policy_allows(action),
        ));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has. The account's own credential carries none.
    if let Some(boundary) = boundary {
        let reason = match iam::identity_decision_prepared(boundary, &request) {
            Decision::Allowed => return Ok(()),
            // An explicit deny is a fact about the boundary document, so the refusal names
            // it; a boundary that merely states nothing withheld the grant the identity-based
            // policies gave.
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => {
                if may_allow(boundary, action, resource) {
                    return Ok(());
                }
                iam::no_permissions_boundary_allows(action)
            }
        };
        return Err(not_authorized(&caller.arn, action, resource, &reason));
    }
    Ok(())
}

/// Whether `documents` could allow `action` at all, asked only for a request judged against
/// [`ANY_RESOURCE`] — one that addresses no single resource, and so may still be about
/// resources a statement is scoped to (see the module docs).
///
/// A request that DOES name an ARN is decided against that ARN alone, where the resource
/// element is what AWS matches it on.
fn may_allow(documents: &[iam::PreparedDocument], action: &str, resource: &str) -> bool {
    resource == ANY_RESOURCE && iam::identity_may_allow_action(documents, action)
}

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it. Every value in it comes from the
/// request and from the stored policies — the principal from the credential the request
/// presented, the action from the operation it named, the resource from the ARN it addressed.
///
/// A request that addresses no single resource is judged against [`ANY_RESOURCE`], and the
/// sentence states no resource clause for it: there is nothing to name. Every other refusal
/// is the shared sentence, so this service and the others that reach the same decision word
/// it alike.
fn not_authorized(
    principal: &str,
    action: &str,
    resource: &str,
    reason: &str,
) -> AccessDeniedException {
    let message = if resource == ANY_RESOURCE {
        format!("User: {principal} is not authorized to perform: {action} {reason}")
    } else {
        iam::not_authorized_by_identity_policy(principal, action, resource, reason)
    };
    AccessDeniedException { message }
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of the IAM
    /// role behind an assumed-role session.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose document
    /// this emulator holds.
    boundary: Option<String>,
    /// Set when this request presents the account's own published credential, which holds no
    /// IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not
    /// hold — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no IAM
    /// policies, or a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's own
    /// accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State,
        identity_arn: &str,
    ) {
        if let Some(policies) = store.principal_policies(identity_arn) {
            self.documents = policies.documents;
            // An attachment the collection resolved no document for is an entry of AWS's own
            // catalog this emulator does not carry: a grant it cannot read.
            self.unreadable_attachment = !policies.uncarried.is_empty();
        }
        self.boundary = store.permissions_boundary_document(identity_arn);
    }
}

/// The principal this request speaks for, with the identity-based policies in force on it.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    let identity = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
        ),
        // No STS store on this engine: the only identity a request can present is the
        // account's own published credential.
        None => Ok(sts_store::ambient_identity()),
    };
    let identity = match identity {
        Ok(identity) => identity,
        // The session credential has expired, so it speaks for nobody: no documents, no
        // principal spelling, and every action it asks for falls to the default deny. The key
        // id it presented is NOT quoted back — this module echoes no value of the request —
        // and in the assembled emulator the layer that authenticates a request refuses an
        // expired credential before any handler runs.
        Err(_expired) => {
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                principals: Vec::new(),
                documents: Vec::new(),
                boundary: None,
                account_credential: false,
                unreadable_attachment: false,
            };
        }
    };

    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        documents: Vec::new(),
        boundary: None,
        unreadable_attachment: false,
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // A LONG-TERM access key: STS's store holds SESSIONS, so its answer for a key it never
    // minted is the account's ambient identity — but IAM's store maps an access key id to the
    // user that owns it (`access_keys`, the map IAM's own `GetUser` resolves a caller
    // through), so a request signed with one speaks for THAT user and is judged on the user's
    // policies rather than served as the account. A key the IAM store does not hold either
    // really is the account's own published credential.
    if caller.account_credential {
        if let Some(user) = store
            .access_keys
            .get(&access_key_id)
            .and_then(|record| store.users.get(&record.user_key))
        {
            caller.account_credential = false;
            caller.arn = user.arn.clone();
            caller.principals = vec![user.arn.clone()];
        }
    }
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by
        // the ARN the account holds it under — which carries its PATH, where the session ARN
        // does not.
        if let Some(held) = store.roles.get(&iam_store::name_key(role_name)) {
            caller.principals.push(held.arn.clone());
            caller.read_policies_of(&store, &held.arn.clone());
        }
    } else if ::service_common::iam_role::user_name(&caller.arn).is_some() {
        // An IAM user's permissions are its own, under the ARN it was resolved by.
        let user_arn = caller.arn.clone();
        caller.read_policies_of(&store, &user_arn);
    }
    caller
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the refusal names the principal, the action, the resource the request addressed
    /// and which of IAM's rules refused — every part taken from the request and the stored
    /// policies — and it names no resource for a request that addresses none.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/operator/session-1",
            ::engine::emulator_account_id()
        );
        let control = format!(
            "arn:aws:route53-recovery-control::{}:controlpanel/abc/routingcontrol/def",
            ::engine::emulator_account_id()
        );
        // (case, the operation, the resource it addressed, the clause the sentence carries)
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "an operation addressing one routing control",
                "GetRoutingControlState",
                &control,
                format!(" on resource: {control}"),
            ),
            (
                "a listing that addresses no single resource",
                "ListRoutingControls",
                ANY_RESOURCE,
                String::new(),
            ),
        ];
        for (case, operation, resource, clause) in rows {
            let action = format!("route53-recovery-cluster:{operation}");
            for reason in [
                iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
                iam::no_identity_policy_allows(&action),
            ] {
                let refused = not_authorized(&principal, &action, resource, &reason);
                assert_eq!(
                    refused.message,
                    format!(
                        "User: {principal} is not authorized to perform: {action}{clause} \
                         {reason}"
                    ),
                    "case {case:?}: expected IAM's wording, got {:?}",
                    refused.message
                );
            }
        }
    }
}
