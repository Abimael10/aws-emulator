// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it names ON THE TARGET GROUP
//! the request addresses — the decision taken ahead of every call that writes one, before the
//! handler reads or writes anything.
//!
//! Elastic Load Balancing refuses a caller whose policies do not cover the action it asked for
//! with HTTP 403 and the error code [`ACCESS_DENIED`], naming the principal the credential
//! speaks for, the action and the target group it was refused on. A call addressed to ONE
//! target group is judged by IAM against that group's ARN — the resource type the IAM Service
//! Authorization Reference publishes for the target-group actions of this service — and the
//! request carries that ARN itself, so the decision is a fact about the REQUEST's credential
//! and the policies in force at the instant of the call rather than about what the account
//! holds. That is why it is taken before the store is read: a refused call must leave the
//! account exactly as it was, and a caller holding no grant learns nothing about whether the
//! group it named exists.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for (`sts_store::resolve_caller`, the same place `sts:GetCallerIdentity`
//!    answers from, so the principal a refusal blames is the one the caller reads back for the
//!    credentials it signed with), else the IAM user that holds the long-term key, else the
//!    account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), where the
//!    collection rule itself lives — so a document `iam:PutRolePolicy` wrote takes effect on
//!    the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the tree's ONE evaluation of
//!    those documents against `(action, resource)`, so this service and every other one handed
//!    the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow` about the
//!    request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the refusal's sentence does
//! (both are `services/common`'s, which is where the one copy of them lives).
//!
//! ## Which calls this decides, and why only those
//!
//! The calls this service serves that are addressed to ONE target group by its ARN — the
//! registrations of its targets and the writes of the group itself — because for those the
//! REQUEST determines the single ARN IAM judges the call against. A call addressed to a LIST
//! of resources (the tagging calls, which carry `ResourceArns`), or to a load balancer, a
//! listener or a rule, is not decided here: the gate would have to name a resource the request
//! does not determine on its own, and withholding a grant from such a call would refuse calls
//! AWS permits. Those are left to a later change that can name every resource they address,
//! and are served exactly as before, as are this service's reporting operations.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS nor
//!   IAM holds a record for resolves to the account's own identity, which is not a principal
//!   the IAM store holds policies for — the position AWS account root credentials are in. Such
//!   a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the account's store may hold no document
//!   for it; refusing on a grant this emulator cannot see would deny access AWS permits. An
//!   explicit `Deny` in a document that CAN be read still refuses, and a permissions boundary
//!   the store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no document, so every action it
//! asks for falls to the default deny, and the refusal names that state rather than quoting
//! the key it presented back.
//!
//! Resolving the caller goes through STS's own `resolve_caller`, which first materialises the
//! session expiries the clock has already made due; that write is owed at the same instant
//! whether or not anybody called.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// This service's IAM action prefix: an action is `elasticloadbalancing:<OperationName>`
/// ("Actions defined by Elastic Load Balancing V2", IAM Service Authorization Reference),
/// which is this service's `arnNamespace` and its SigV4 signing name
/// (`smithy_query(query="service_info")`).
pub const ACTION_PREFIX: &str = "elasticloadbalancing";

/// The error code Elastic Load Balancing refuses an unauthorized caller with, which is what a
/// client branches on. A CLIENT fault, so the query envelope reports it with HTTP 403.
pub const ACCESS_DENIED: &str = "AccessDenied";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// This service's [`ACCESS_DENIED`] refusal of `operation` on the target group
/// `target_group_arn`, or `None` when the IAM policies in force permit it.
///
/// The ARN judged — and the one the refusal names — is the one the REQUEST carried, never one
/// read back out of the store: taking it from the request is what keeps a refused call from
/// telling an unauthorized caller whether what it named exists, and what lets the decision be
/// taken before the handler resolves anything.
///
/// Carried as this service's own 403 `AccessDenied` error
/// ([`crate::scaffolded::Scaffolded::access_denied`]), so it is rendered in the same envelope
/// as every other error of this service.
pub fn target_group_action_refusal<Eng: Send + Sync + 'static, E>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    target_group_arn: &str,
) -> Option<crate::scaffolded::Scaffolded<E>> {
    let message = authorize(engine, ctx, operation, target_group_arn).err()?;
    Some(crate::scaffolded::Scaffolded::access_denied(message))
}

/// Whether IAM lets this request perform `operation`'s action on `resource`.
///
/// `Ok(())` for a request IAM permits and for every caller this gate does not bound (see the
/// module docs); `Err` carries the sentence the refusal states, built from the request and the
/// stored policies by the shared [`::service_common::iam_authorization`] — so nothing of IAM's
/// wording is spelled in this crate.
fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), String> {
    let action = format!("{ACTION_PREFIX}:{operation}");
    let caller = caller(engine, ctx);
    // An identity-based document states no `Principal` element, so the decision rests on the
    // action and the resource the request addressed.
    let request = iam::Request::new(&action, resource, &[]);
    let said = iam::identity_decision(&caller.documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if said == Decision::Denied {
        return Err(refused(
            &caller.arn,
            &action,
            resource,
            iam::IDENTITY_POLICY_EXPLICIT_DENY,
        ));
    }
    if !(iam::is_allowed(said, Decision::NotStated) || caller.identity_is_unbounded()) {
        return Err(refused(
            &caller.arn,
            &action,
            resource,
            &iam::no_identity_policy_allows(&action),
        ));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has. The account's own credential carries none.
    let Some(boundary) = caller.boundary.as_ref() else {
        return Ok(());
    };
    let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
        Decision::Allowed => return Ok(()),
        // An explicit deny is a fact about the boundary document, so the refusal names it; a
        // boundary that merely states nothing withheld the grant the identity-based policies
        // gave.
        Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
        Decision::NotStated => iam::no_permissions_boundary_allows(&action),
    };
    Err(refused(&caller.arn, &action, resource, &reason))
}

/// The refusal IAM states an authorization failure with — who was refused, which action, on
/// which resource, and which of IAM's rules refused it.
///
/// The whole sentence is the shared [`iam::not_authorized_by_identity_policy`], so every value
/// in it comes from the request and from the stored policies and no wording of IAM's is
/// restated here.
fn refused(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    iam::not_authorized_by_identity_policy(principal, action, resource, reason)
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose document
    /// this emulator holds.
    boundary: Option<String>,
    /// Set when this request presents the account's own published credential, which holds no
    /// IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not hold
    /// — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no IAM
    /// policies, or a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's OWN
    /// accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &iam_store::ComAmazonawsIamAwsidentitymanagementv20100508State,
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
    let access_key_id = ::service_common::request_access_key_id(ctx.parts());
    let identity = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
        ),
        // No STS store on this engine: the only identity a request can present is the
        // account's own published credential, which holds no identity policy.
        None => Ok(sts_store::ambient_identity()),
    };
    // An expired session credential speaks for nobody: it carries no document, so every action
    // it asks for is refused — and the refusal names the credential's STATE rather than the
    // key id that was presented, which this module never quotes back.
    let identity = match identity {
        Ok(identity) => identity,
        Err(expired) => {
            let _ = expired;
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                ..Caller::default()
            };
        }
    };
    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        arn: identity.arn,
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // A LONG-TERM access key: STS's store holds SESSIONS, so its answer for a key it never
    // minted is the account's ambient identity — but IAM's store maps an access key id to the
    // user that owns it (the map `iam:GetUser` resolves a caller through), so a request signed
    // with one speaks for THAT user and is judged on the user's policies. A key the IAM store
    // does not hold either really is the account's own published credential.
    if caller.account_credential {
        if let Some(user) = store
            .access_keys
            .get(&access_key_id)
            .and_then(|record| store.users.get(&record.user_key))
        {
            caller.account_credential = false;
            caller.arn = user.arn.clone();
        }
    }
    // Which IAM identity the principal's policies are held under is read through the tree's
    // shared readings of a principal ARN.
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by
        // the ARN the account holds it under — which carries its PATH, where the session ARN
        // does not.
        if let Some(role_arn) = store
            .roles
            .get(&iam_store::name_key(role_name))
            .map(|held| held.arn.clone())
        {
            caller.read_policies_of(&store, &role_arn);
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

    /// The decision one stored document reaches about an action on a resource, through the
    /// shared evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// A target group ARN of this service, as `CreateTargetGroup` mints one.
    fn target_group_arn(name: &str) -> String {
        format!("arn:aws:elasticloadbalancing:us-east-1:000000000000:targetgroup/{name}/0123456789abcdef")
    }

    /// RULE: the action this gate asks about carries this service's own prefix, so a statement
    /// scoped to this service's actions covers the operations of it and nothing outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document = concat!(
            r#"{"Statement":[{"Effect":"Allow","#,
            r#""Action":"elasticloadbalancing:*","Resource":"*"}]}"#
        );
        for operation in [
            "RegisterTargets",
            "DeregisterTargets",
            "ModifyTargetGroup",
            "ModifyTargetGroupAttributes",
            "DeleteTargetGroup",
        ] {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let reached = decision(document, &action, &target_group_arn("a-group"));
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected elasticloadbalancing:* to allow {action}, got \
                 {reached:?}"
            );
        }
        let outside = decision(document, "ecs:CreateService", &target_group_arn("a-group"));
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected elasticloadbalancing:* to state nothing outside \
             this service, got {outside:?}"
        );
    }

    /// RULE: the decision is taken against the target group the request addressed, so a grant
    /// scoped to one group does not reach another.
    #[test]
    fn a_grant_scoped_to_one_target_group_does_not_reach_another() {
        let granted = target_group_arn("granted");
        let document = format!(
            r#"{{"Statement":[{{"Effect":"Allow","Action":"elasticloadbalancing:RegisterTargets","Resource":"{granted}"}}]}}"#
        );
        let action = format!("{ACTION_PREFIX}:RegisterTargets");
        // (case, the group the call addresses, the decision)
        let rows: &[(&str, String, Decision)] = &[
            (
                "the group the grant names",
                granted.clone(),
                Decision::Allowed,
            ),
            (
                "another group of the account",
                target_group_arn("elsewhere"),
                Decision::NotStated,
            ),
        ];
        for (case, resource, expected) in rows {
            let reached = decision(&document, &action, resource);
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected {expected:?}, got {reached:?}"
            );
        }
    }
}
