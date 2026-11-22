// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the step
//! every operation whose model declares `AccessDeniedException` takes BEFORE it reads or
//! writes a dashboard, a scheduled report or the policy stored on one.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at the
//! request's own input, so a caller whose identity-based policies do not allow the action
//! is told it is not authorized rather than told anything about the resource it named, and
//! the call changes nothing. `AccessDeniedException` (HTTP 403) is documented for exactly
//! that — "You do not have sufficient permissions to perform this action. Verify your IAM
//! permissions and any resource policies"
//! (`smithy_query(query="shape",shape="AccessDeniedException")`) — and the model declares
//! it on twelve of this service's fifteen operations, which are the twelve this gate
//! answers for. The other three are the resource-tagging operations
//! (`ListTagsForResource`, `TagResource`, `UntagResource`): their model declares no
//! `AccessDeniedException`, so there is no modelled refusal for them to answer a denied
//! caller with, and a gate that could only refuse them with another error code would state
//! something the model does not.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_caller`, the same place
//!    `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the one
//!    the caller reads back for the credentials it signed with), else the IAM user that
//!    owns the long-term key, else the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for
//!    that principal and the permissions boundary that caps them, read through the IAM
//!    service's OWN accessors (`principal_policies`, `permissions_boundary_document`),
//!    where the collection rule itself lives — so a policy `iam:PutUserPolicy` wrote takes
//!    effect on the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default
//!    deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the refusal's sentence does.
//! The lookups stay in this crate because hoisting them into `service-common` would make
//! every service that depends on that crate link the IAM and STS service crates. WHICH IAM
//! entity a principal ARN names carries no such dependency, so it is read through the
//! shared [`::service_common::iam_role`] rather than parsed again here.
//!
//! # The resource a request is judged against
//!
//! The one the request's OWN input addresses: the dashboard ARN for a call that names a
//! dashboard, the scheduled-report ARN for one that names a report, and [`ANY_RESOURCE`]
//! for a call that addresses no resource of its own — the two creates, whose resource does
//! not exist until the store mints its ARN, and the two listings, which name nothing. The
//! ARN comes from what the REQUEST carried rather than from a stored record, so the
//! decision does not depend on whether that resource exists — which is also what keeps a
//! refused call from telling an unauthorized caller whether it does.
//!
//! The decision here is the identity-based one alone. This service publishes a
//! resource-based policy per dashboard (`GetResourcePolicy` reads it back), but its model
//! declares no operation that writes one, so no stored policy can grant or deny anything
//! the account's identity-based documents do not already decide.
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
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no
//! principal spelling, so every action it asks for is refused. (A session the
//! authenticating layer has already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use crate::ops::types::AccessDeniedException;

/// The resource an action that is not resource-scoped is judged against, as IAM writes it
/// for such a request.
pub const ANY_RESOURCE: &str = "*";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// This service's IAM action prefix: an action is `bcm-dashboards:<OperationName>`, which
/// is this service's `arnNamespace` and sigv4 signing name alike
/// (`smithy_query(query="service_info")`) — the same string every ARN this service mints
/// spells.
pub const ACTION_PREFIX: &str = "bcm-dashboards";

/// The outcome a STATE-CHANGING operation records in the audit stream
/// ([`crate::state::record_state_change`]) when this gate refuses it: the modelled error
/// code, as every other refusal of a state change records its own. A reporting operation
/// records nothing, here as everywhere — it changed nothing to account for.
pub const ACCESS_DENIED_CODE: &str = "AccessDeniedException";

/// The sentinel an audit record names in place of a resource for a call that addresses
/// none: the same one the create paths of this service record.
pub const NO_RESOURCE_AUDITED: &str = "-";

/// Authorize this request, where `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]) and `resource` the ARN the request
/// ADDRESSES ([`ANY_RESOURCE`] for one that addresses none).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see
/// the module docs); the modelled refusal otherwise, which each operation returns as its
/// own `AccessDeniedException`. Call it FIRST in a handler, before the store is reached and
/// before any rule about the request's other members is applied, because that is the order
/// AWS decides in.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises
/// the session expiries the clock has already made due; that write is owed at the same
/// instant whether or not anybody called, which is why a REPORTING operation of this
/// service may take this decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), AccessDeniedException> {
    let action = iam::iam_action(ACTION_PREFIX, operation);
    let caller = caller(engine, ctx);
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no
    // such element, and no stored resource-based policy of this service can be written (see
    // the module docs), so the decision below rests on the action and the resource.
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
    // can grant" (IAM, permissions boundaries): a grant the boundary does not also allow
    // is not a permission the principal has. The account's own credential carries none.
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

/// The modelled refusal carrying the sentence IAM states an authorization failure with: who
/// was refused, which action, on which resource, and which of IAM's rules refused it.
///
/// The sentence itself is
/// [`::service_common::iam_authorization::not_authorized_by_identity_policy`], the one
/// wording every gate in this tree states a denial with; this function only lifts it into
/// the error shape this service's model declares. Every value in it comes from the request
/// and from the stored policies — the principal from the credential the request presented,
/// the action from the operation it named, the resource from the ARN it addressed.
///
/// An action that addresses no resource of its own is judged against [`ANY_RESOURCE`], and
/// the sentence states no resource clause for it: there is nothing to name.
///
/// The text is held to the bound every error message of this service carries
/// ([`crate::state::within_message_bound`]): the principal and the resource are ARNs, which
/// this surface admits twice as long as the `message` member that reports them.
fn not_authorized(
    principal: &str,
    action: &str,
    resource: &str,
    reason: &str,
) -> AccessDeniedException {
    let sentence = if resource == ANY_RESOURCE {
        format!("User: {principal} is not authorized to perform: {action} {reason}")
    } else {
        iam::not_authorized_by_identity_policy(principal, action, resource, reason)
    };
    AccessDeniedException {
        message: crate::state::within_message_bound(sentence),
    }
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
        store: &crate::state::execution_role::IamState,
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
        Err(expired) => {
            let _ = expired;
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
    // The IAM store in the SAME shared engine, reached through the accessor this crate
    // already holds for it (`state::execution_role`), so a scheduled report's execution
    // role and a request's caller are judged against one store rather than two readings
    // of it.
    let Some(iam) = crate::state::execution_role::iam_state(engine) else {
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
        // by the ARN the account holds it under — which carries its PATH, where the
        // session ARN does not.
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

    /// RULE: the action this gate asks about is `bcm-dashboards:<OperationName>`, so a
    /// policy scoped to this service's actions covers every operation of it and nothing
    /// outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document =
            r#"{"Statement":[{"Effect":"Allow","Action":"bcm-dashboards:*","Resource":"*"}]}"#;
        for operation in [
            "CreateDashboard",
            "CreateScheduledReport",
            "DeleteDashboard",
            "DeleteScheduledReport",
            "ExecuteScheduledReport",
            "GetDashboard",
            "GetResourcePolicy",
            "GetScheduledReport",
            "ListDashboards",
            "ListScheduledReports",
            "UpdateDashboard",
            "UpdateScheduledReport",
        ] {
            let action = iam::iam_action(ACTION_PREFIX, operation);
            let reached = decision(document, &action, ANY_RESOURCE);
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected bcm-dashboards:* to allow {action}, got {reached:?}"
            );
        }
        let outside = decision(document, "ce:GetCostAndUsage", ANY_RESOURCE);
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected bcm-dashboards:* to grant nothing outside \
             this service, got {outside:?}"
        );
    }

    /// RULE: the refusal names the principal, the action, the resource the request
    /// addressed and which of IAM's rules refused — every part taken from the request and
    /// the stored policies — and it names no resource for an action that addresses none.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/dashboard-reader/session-1",
            ::engine::emulator_account_id()
        );
        let dashboard = format!(
            "arn:aws:bcm-dashboards::{}:dashboard/0000000000000000000000000000002a",
            ::engine::emulator_account_id()
        );
        // (case, the operation, the resource it addressed, the clause the sentence carries)
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "a resource-scoped action",
                "GetDashboard",
                &dashboard,
                format!(" on resource: {dashboard}"),
            ),
            (
                "an action that addresses no resource",
                "ListDashboards",
                ANY_RESOURCE,
                String::new(),
            ),
        ];
        for (case, operation, resource, clause) in rows {
            let action = iam::iam_action(ACTION_PREFIX, operation);
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

    /// RULE: a refusal's text is held to the bound every error message of this service
    /// carries, so a principal or resource ARN longer than that member admits cannot put an
    /// over-long value on the wire.
    #[test]
    fn the_refusal_stays_within_the_message_bound() {
        let action = iam::iam_action(ACTION_PREFIX, "GetDashboard");
        // An ARN at `DashboardArn`'s own ceiling (2048 characters), which is twice what
        // `GenericString` behind `message` admits.
        let resource = format!(
            "arn:aws:bcm-dashboards::{}:dashboard/{}",
            ::engine::emulator_account_id(),
            "a".repeat(2_000)
        );
        let refusal = not_authorized(
            "arn:aws:iam::000000000000:user/reader",
            &action,
            &resource,
            &iam::no_identity_policy_allows(&action),
        );
        let length = refusal.message.chars().count();
        assert!(
            length <= crate::state::MAX_MESSAGE_LENGTH,
            "case \"an ARN at its own ceiling\": expected at most {} characters, got {length}",
            crate::state::MAX_MESSAGE_LENGTH
        );
    }
}
