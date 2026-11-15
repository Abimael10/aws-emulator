// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the
//! decision every operation takes BEFORE it reads or writes anything.
//!
//! `AccessDeniedException` (HTTP 403) is a SERVICE-COMMON error of this model
//! (`smithy_query(query="service_info")` lists it under `common_errors`), so it is the
//! answer every one of the three operations gives a caller whose permissions do not cover
//! the action it named — and the call performs nothing, which is what leaves an
//! unauthorized mutation's prior value intact. AWS takes that decision at the IAM layer,
//! ahead of the service looking at the request's own input, so a refused call reports
//! nothing about the setting and says nothing about the request's own values.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_caller`, the same record
//!    `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the one
//!    the caller reads back for the credentials it signed with), else the IAM user that
//!    owns the long-term key, else the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for
//!    that principal and the permissions boundary that caps them, read through the IAM
//!    service's own accessors (`principal_policies`, `permissions_boundary_document`),
//!    where the collection rule lives — so a document `iam:PutUserPolicy` wrote takes
//!    effect on the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the one shared
//!    evaluation of those documents against `(action, resource)`, so this service and
//!    every other one handed the same documents answer alike: an explicit `Deny` wins,
//!    otherwise an `Allow` about the request grants it, and with neither the request falls
//!    to IAM's default deny. How the refusal is WORDED is that module's too
//!    ([`iam::not_authorized_by_identity_policy`] and the reason clauses beside it), so two
//!    services that reach the same decision cannot word it differently.
//!
//! Only the store LOOKUPS live here; neither the decision nor the reading of a principal
//! ARN does. That is the split every other gate in this tree makes, and the lookups stay
//! in this crate for the reason those keep theirs there: hoisting them into
//! `service-common` would make every service depending on that crate link the IAM and STS
//! service crates. WHICH IAM entity a principal ARN names carries no such dependency, so
//! it is read through the shared [`::service_common::iam_role`].
//!
//! ## The action and the resource
//!
//! The action is `<arn namespace>:<OperationName>` — this service's `arnNamespace`, which
//! is also the name every request to it is signed under ([`action_prefix`]) — so one
//! statement scoped to this service's actions covers every operation of it and nothing
//! outside it.
//!
//! The two operations that address the account's customization settings are judged against
//! that account-level singleton's own ARN ([`account_customizations_resource`]); the
//! catalog listing addresses no resource of its own — it enumerates the identifiers a
//! setting may name, the same for every account — so it is judged against the resource IAM
//! writes for a request that addresses none ([`ANY_RESOURCE`]), and its refusal names no
//! resource.
//!
//! This service publishes no resource-based policy of its own — the model declares no
//! policy operation and no policy member — so the decision here is the identity-based one
//! alone.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to the account's own identity, which is not a
//!   principal the IAM store holds policies for — the position AWS account root
//!   credentials are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed
//!   policy belongs to AWS rather than to this account, so the IAM store holds no document
//!   for it; refusing on a grant this emulator cannot see would deny access AWS permits.
//!   An explicit `Deny` in a document that CAN be read still refuses, and a permissions
//!   boundary the store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no user can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no
//! principal spelling, so every action it asks for is refused. (A session the
//! authenticating layer has already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use crate::ops::types::AccessDeniedException;

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The resource an action that addresses no resource of its own is judged against, as IAM
/// writes it for such a request.
pub const ANY_RESOURCE: &str = "*";

/// The resource field of the account's customization settings: one record per account, so
/// the ARN names the setting collection and carries no identifier of its own. It is the
/// name the model's own HTTP binding addresses it by
/// (`smithy_query(query="digest")`: `/v1/account-customizations` for both the read and the
/// update).
const ACCOUNT_CUSTOMIZATIONS: &str = "account-customizations";

/// The Region segment of that ARN: empty. The settings are per ACCOUNT — the record is keyed
/// by the calling account alone ([`crate::state::calling_account_id`]) and the same values
/// are reported whichever Region a request is signed for — which is how AWS writes the ARN
/// of an account-level resource.
const ACCOUNT_LEVEL: &str = "";

/// This service's IAM action prefix, which is its `arnNamespace` and also its `sigv4Name`
/// (`smithy_query(query="service_info")` gives both as the same string) — so the prefix is
/// read off the request's own resolved identity rather than restated here, and the ARNs this
/// gate builds and the actions it asks about cannot name two different services.
#[must_use]
pub fn action_prefix<'ctx>(ctx: &'ctx ::smithy_server_runtime::OperationContext<'_>) -> &'ctx str {
    ctx.identity().signing_name()
}

/// The ARN of the account-singleton the customization settings live in — the resource the
/// read and the update of those settings are judged against.
///
/// Built from the emulator's own account contract (`engine::emulator_arn`, which carries the
/// canonical account id), never from a value of the request.
#[must_use]
pub fn account_customizations_resource(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    ::engine::emulator_arn(action_prefix(ctx), ACCOUNT_LEVEL, ACCOUNT_CUSTOMIZATIONS)
}

/// Authorize this request, where `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]) and `resource` the ARN of the resource it
/// addresses ([`ANY_RESOURCE`] for one that addresses none).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see
/// the module docs); the modelled refusal otherwise, which each operation lifts into its own
/// error channel. Call it FIRST in a handler, before this service's store is read or
/// written: that ordering is what keeps a refused mutation from changing the stored setting.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service may
/// take this decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), AccessDeniedException> {
    let action = format!("{}:{operation}", action_prefix(ctx));
    let caller = caller(engine, ctx);
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no
    // such element, and this service publishes no resource-based policy (see the module
    // docs), so the decision below rests on the action and the resource.
    let request = iam::Request::new(&action, resource, &caller.principals);
    let identity_said = iam::identity_decision(&caller.documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if identity_said == Decision::Denied {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            iam::IDENTITY_POLICY_EXPLICIT_DENY,
        ));
    }
    let granted =
        iam::is_allowed(identity_said, Decision::NotStated) || caller.identity_is_unbounded();
    if !granted {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            &iam::no_identity_policy_allows(&action),
        ));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy
    // can grant" (IAM, permissions boundaries): a grant the boundary does not also allow is
    // not a permission the principal has. The account's own credential carries none.
    if let Some(boundary) = &caller.boundary {
        let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
            Decision::Allowed => return Ok(()),
            // An explicit deny is a fact about the boundary document, so the refusal names
            // it; a boundary that merely states nothing withheld the grant the
            // identity-based policies gave.
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => iam::no_permissions_boundary_allows(&action),
        };
        return Err(not_authorized(&caller.arn, &action, resource, &reason));
    }
    Ok(())
}

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it. Every value in it comes from the
/// request and from the stored policies — the principal from the credential the request
/// presented, the action from the operation it named, the resource from what that operation
/// addresses.
///
/// The sentence itself is the shared one every gate in this tree words this refusal with
/// ([`iam::not_authorized_by_identity_policy`]). An action that addresses no resource of its
/// own is judged against [`ANY_RESOURCE`], and then there is no resource to name: that form
/// states the principal, the action and the reason alone.
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
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of the
    /// IAM role behind an assumed-role session.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose
    /// document this emulator holds.
    boundary: Option<String>,
    /// Set when this request presents the account's own published credential, which holds
    /// no IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not
    /// hold — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the action: the account's own credential, which holds no
    /// IAM policies, or a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's
    /// own accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State,
        identity_arn: &str,
    ) {
        if let Some(policies) = store.principal_policies(identity_arn) {
            self.documents = policies.documents;
            // An attachment the collection resolved no document for is an entry of AWS's
            // own catalog this emulator does not carry: a grant it cannot read.
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
        // principal spelling, and every action it asks for falls to the default deny. The
        // key id it presented is NOT quoted back — this module echoes no value of the
        // request — and in the assembled emulator the layer that authenticates a request
        // refuses an expired credential before any handler runs.
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
    // minted is the account's ambient identity — but IAM's store maps an access key id to
    // the user that owns it (`access_keys`, the map IAM's own `GetUser` resolves a caller
    // through), so a request signed with one speaks for THAT user and is judged on the
    // user's policies rather than served as the account. A key the IAM store does not hold
    // either really is the account's own published credential.
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
        // An assumed-role session's permissions are the ROLE's, and the role is addressed
        // by the ARN the account holds it under — which carries its PATH, where the session
        // ARN does not.
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

    /// The decision one stored document reaches about an action on a resource, through the
    /// shared evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// RULE: the action this gate asks about carries this service's own prefix, so a
    /// statement scoped to this service's actions covers every operation of it and nothing
    /// outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document = r#"{"Statement":[{"Effect":"Allow","Action":"uxc:*","Resource":"*"}]}"#;
        for operation in [
            "GetAccountCustomizations",
            "ListServices",
            "UpdateAccountCustomizations",
        ] {
            let action = format!("uxc:{operation}");
            let reached = decision(document, &action, ANY_RESOURCE);
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected the service-wide allow to cover {action}, got \
                 {reached:?}"
            );
        }
        let outside = decision(document, "iam:CreateUser", ANY_RESOURCE);
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected the service-wide allow to grant nothing outside \
             this service, got {outside:?}"
        );
    }

    /// RULE: the refusal names the principal, the action, the resource the operation
    /// addresses and which of IAM's rules refused it — and names no resource for an action
    /// that addresses none.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = format!(
            "arn:aws:iam::{}:user/console-reader",
            ::engine::emulator_account_id()
        );
        let singleton = ::engine::emulator_arn("uxc", "", "account-customizations");
        // (case, the operation, the resource it addresses, the clause the sentence carries)
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "the account singleton",
                "UpdateAccountCustomizations",
                &singleton,
                format!(" on resource: {singleton}"),
            ),
            (
                "an action that addresses no resource",
                "ListServices",
                ANY_RESOURCE,
                String::new(),
            ),
        ];
        for (case, operation, resource, clause) in rows {
            let action = format!("uxc:{operation}");
            let reason = iam::no_identity_policy_allows(&action);
            let refusal = not_authorized(&principal, &action, resource, &reason);
            assert_eq!(
                refusal.message,
                format!(
                    "User: {principal} is not authorized to perform: {action}{clause} {reason}"
                ),
                "case {case:?}: expected IAM's wording, got {:?}",
                refusal.message
            );
        }
    }
}
