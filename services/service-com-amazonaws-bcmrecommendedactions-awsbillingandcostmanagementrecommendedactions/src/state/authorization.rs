// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the
//! decision this service's operation takes BEFORE it reads the recommended-actions
//! store.
//!
//! `AccessDeniedException` (HTTP 403) is a service-common error of this model
//! (`smithy_query(query="service_info")` lists it under `common_errors`, and
//! `smithy_query(query="operation", operation="ListRecommendedActions")` declares it on
//! the operation), and its doc is "You do not have sufficient access to perform this
//! action." AWS takes that decision at the IAM layer, ahead of the service looking at
//! what a handler would report, so a caller whose identity-based policies do not allow
//! the action — or that an explicit `Deny` covers — is told it is not authorized rather
//! than told anything about the account's recommendations, and the call reports nothing.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_request_caller`, the same record
//!    `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the one
//!    the caller reads back for the credentials it signed with), else the IAM user that
//!    owns the long-term key, else the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for
//!    that principal, the permissions boundary that caps them, read through the IAM
//!    service's OWN accessors (`principal_policies`, `permissions_boundary_document`), so
//!    a document `iam:PutRolePolicy` wrote takes effect on the next request and deleting
//!    it lifts it again — and the SESSION POLICIES the mint of this very credential scoped
//!    it with (`sts_store::session_scoping_documents`), which cap those documents the same
//!    way: "the resulting session's permissions are the intersection of the role's
//!    identity-based policy and the session policies" (`AssumeRoleRequest$PolicyArns`);
//! 3. **the decision** — [`::service_common::iam_authorization`], the one shared
//!    evaluation of those documents against `(action, resource)`, and the reason clauses a
//!    refusal closes with, so this service and every other one handed the same documents
//!    answer alike and word it alike: an explicit `Deny` wins, otherwise an `Allow` about
//!    the request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the reading of a principal
//! ARN does. That is the split every other gate in this tree makes, and the lookups stay
//! in this crate for the reason those keep theirs there: hoisting them into
//! `service-common` would make every service that depends on that crate link the IAM and
//! STS service crates.
//!
//! ## The action and the resource
//!
//! The action is `<signing name>:<OperationName>` — this service's `arnNamespace`, which
//! is also its `sigv4Name` (`smithy_query(query="service_info")`) — read off the request's
//! own resolved identity rather than restated here, so one statement scoped to this
//! service's actions covers every operation of it and nothing outside it.
//!
//! This service's operations address no resource of their own: the model declares no ARN
//! member on any input and mints no ARN, so the resource element of a statement that
//! covers one of its actions is the all-resources wildcard ([`ANY_RESOURCE`]), and a
//! statement scoped to some other ARN covers no request to it. That is also why a refusal
//! states no resource clause — there is nothing to name in it — which is the one part of
//! the sentence this module spells instead of taking from the shared wording.
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
//!   boundary or a session policy the store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no document and no
//! principal spelling, so every action it asks for is refused, and the refusal names that
//! state rather than the key it presented, which this module never quotes back. (In the
//! assembled emulator the layer that authenticates a request refuses an expired credential
//! before any handler runs.)

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use crate::ops::types::AccessDeniedException;

/// The resource IAM writes for a request that addresses no resource of its own, which is
/// every request to this service (see the module docs).
pub const ANY_RESOURCE: &str = "*";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// This service's IAM action prefix: the signing name the request's own resolved identity
/// carries, which is this service's `arnNamespace` and `sigv4Name` alike — so the action a
/// refusal names cannot drift from the name the request was signed under.
#[must_use]
pub fn action_prefix<'ctx>(ctx: &'ctx ::smithy_server_runtime::OperationContext<'_>) -> &'ctx str {
    ctx.identity().signing_name()
}

/// Authorize this request, where `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see
/// the module docs); the modelled refusal otherwise, which the operation lifts into its own
/// error channel. Call it FIRST in a handler, before the store is read, so a refused request
/// reports nothing of what the store holds.
///
/// IAM's order: an explicit `Deny` in a document that CAN be read refuses the request
/// whatever else the principal carries; otherwise a grant is needed; and a grant is capped
/// BOTH by the session policies the credential was minted with, since "the resulting
/// session's permissions are the intersection of the role's identity-based policy and the
/// session policies" (`AssumeRoleRequest$PolicyArns`), AND by the permissions boundary,
/// since "a permissions boundary sets the maximum permissions that an identity-based policy
/// can grant" (IAM, permissions boundaries). Each cap takes permissions away and grants none
/// of its own, so a call only a cap allows is refused as if that cap had allowed nothing.
///
/// Resolving the caller goes through STS's own `resolve_request_caller`, which first
/// materialises the session expiries the clock has already made due; that write is owed at
/// the same instant whether or not anybody called, which is why a REPORTING operation of
/// this service takes this decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
) -> Result<(), AccessDeniedException> {
    let action = iam::iam_action(action_prefix(ctx), operation);
    let caller = caller(engine, ctx);
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no
    // such element, and this service publishes no resource-based policy (see the module
    // docs), so the decision below rests on the action and the resource.
    let request = iam::Request::new(&action, ANY_RESOURCE, &caller.principals);
    let said = iam::identity_decision(&caller.documents, &request);
    // Whether anything the principal carries grants the call at all. A caller this gate
    // does not bound is granted by definition (see the module docs), but an explicit `Deny`
    // in a document that CAN be read still wins over every grant.
    let granted = iam::is_allowed(said, Decision::NotStated)
        || (said != Decision::Denied && caller.identity_is_unbounded());
    // What the SCOPE this credential was minted with says, through the tree's one reading of
    // a resolved scope: a credential no mint scoped is capped by nothing, which that reading
    // answers as allowed.
    let scoped = caller.scope.decision(&request);
    // The ceiling in force on the principal. A call an identity-based policy explicitly
    // DENIES is already refused on that deny, so no ceiling is read for it.
    let capped = match (said == Decision::Denied, caller.boundary.as_ref()) {
        (false, Some(boundary)) => {
            iam::identity_decision(::std::slice::from_ref(boundary), &request)
        }
        _ => Decision::Allowed,
    };
    if granted && scoped == Decision::Allowed && capped == Decision::Allowed {
        return Ok(());
    }
    // An explicit deny is a fact about the document that carries it, so the refusal names
    // that document; otherwise the refusal names the document that WITHHELD the grant — the
    // principal's own policies where nothing granted the call, the scope where they granted
    // it and the mint's session policies did not, and the boundary where both did.
    let reason = if said == Decision::Denied {
        iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
    } else if scoped == Decision::Denied {
        iam::SESSION_POLICY_EXPLICIT_DENY.to_string()
    } else if capped == Decision::Denied {
        iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()
    } else if !granted {
        iam::no_identity_policy_allows(&action)
    } else if scoped != Decision::Allowed {
        iam::no_session_policy_allows(&action)
    } else {
        iam::no_permissions_boundary_allows(&action)
    };
    Err(not_authorized(&caller.arn, &action, &reason))
}

/// The modelled refusal carrying the sentence IAM states an authorization failure with: who
/// was refused, which action, and which of IAM's rules refused it.
///
/// Every value in it comes from the request and from the stored policies — the principal
/// from the credential the request presented, the action from the operation it named — and
/// the reason clauses are the shared ones
/// ([`::service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY`] and the
/// functions beside it), so this service and every other one that reaches the same decision
/// word it alike. The sentence states NO resource clause: a request to this service
/// addresses no resource of its own, so there is nothing to name in it — which is the case
/// the shared
/// [`::service_common::iam_authorization::not_authorized_by_identity_policy`] documents as
/// outside its own sentence.
fn not_authorized(principal: &str, action: &str, reason: &str) -> AccessDeniedException {
    AccessDeniedException {
        message: format!("User: {principal} is not authorized to perform: {action} {reason}"),
    }
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of the IAM
    /// role behind an assumed-role session.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose
    /// document this emulator holds.
    boundary: Option<String>,
    /// The session policy documents the mint of this request's credential scoped it with, as
    /// they read at the instant of this request. Empty for a credential no mint scoped —
    /// every long-term access key, and every session minted without session policies —
    /// which is a credential capped by nothing.
    scope: sts_store::ScopingPolicies,
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

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's own
    /// accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &iam_store::ComAmazonawsIamAwsidentitymanagementv20100508State,
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

/// The principal this request speaks for, with the policies in force on it.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    // Who the credential speaks for AND what its mint scoped it to, read together from the
    // session registry: the scope travels with the credential, so the only thing a later
    // request carries is where it has to be read from. No STS store on this engine means no
    // session can exist, so the only identity a request can present is the account's own
    // published credential.
    let resolved = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_request_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
            sts_store::ambient_identity(),
        ),
        None => {
            return Caller {
                arn: sts_store::ambient_identity().arn,
                account_credential: true,
                ..Caller::default()
            };
        }
    };
    let resolved = match resolved {
        Ok(resolved) => resolved,
        // The session credential has expired, so it speaks for nobody: no documents, no
        // principal spelling, and every action it asks for falls to the default deny.
        Err(_expired) => {
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                ..Caller::default()
            };
        }
    };

    // WHOSE identity-based documents are in force for this credential, read through STS's
    // one answer for it: the role behind an assumed-role session (resolved below to the ARN
    // the account holds the role under, which carries its path), the IAM user behind a
    // federated-user session, and the principal itself otherwise. Taken before the resolved
    // caller is broken up below.
    let identity_policies_arn = resolved.identity_policies_arn().to_string();
    let federated_session = resolved.federating_user_arn.is_some();
    let mut caller = Caller {
        account_credential: resolved.identity.arn == sts_store::ambient_identity().arn,
        principals: vec![resolved.identity.arn.clone()],
        arn: resolved.identity.arn,
        // The inline document the mint passed scopes the credential whether or not an IAM
        // store is there; where there is one, the scope is re-read below so a managed
        // session policy counts at the version in force NOW.
        scope: sts_store::session_scoping_documents(None, &resolved.session_policies),
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // What the mint scoped this credential to, as the documents read at the instant of this
    // request: a managed session policy stands for its DEFAULT version, so one edited since
    // the mint scopes by what it says now.
    caller.scope = sts_store::session_scoping_documents(Some(&store), &resolved.session_policies);
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
    // Which IAM identity the principal's policies are held under is read through the tree's
    // shared readings of a principal ARN.
    if federated_session {
        // A FEDERATED-user session is judged on the documents of the IAM user whose
        // long-term credentials minted it — "the session permissions are the intersection of
        // the IAM user policies and the session policies that you pass"
        // (`GetFederationToken`) — because the federated user is no IAM identity and carries
        // no document of its own.
        caller.read_policies_of(&store, &identity_policies_arn);
    } else if let Some(role_name) =
        ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by
        // the ARN the account holds it under — which carries its PATH, where the session ARN
        // does not.
        if let Some(role_arn) = store
            .roles
            .get(&iam_store::name_key(role_name))
            .map(|held| held.arn.clone())
        {
            caller.principals.push(role_arn.clone());
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

    /// The operation this service binds, from `smithy_query(query="list_operations")`.
    const OPERATION: &str = "ListRecommendedActions";

    /// This service's action namespace, which is its `arnNamespace` and its `sigv4Name`
    /// (`smithy_query(query="service_info")`). Spelled here only to state what the request's
    /// resolved identity carries; the gate itself reads it off that identity.
    const ACTION_PREFIX: &str = "bcm-recommended-actions";

    /// What one stored document says about `action` on the resource every request to this
    /// service is judged against, through the shared evaluator this gate consults.
    fn decision(document: &str, action: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, ANY_RESOURCE, &[]),
        )
    }

    /// RULE: the action this gate asks about carries this service's OWN namespace, so a
    /// statement scoped to this service's actions covers its operation and a statement about
    /// another service's actions covers nothing of it.
    #[test]
    fn the_action_asked_about_carries_this_services_namespace() {
        let action = iam::iam_action(ACTION_PREFIX, OPERATION);
        assert_eq!(
            action, "bcm-recommended-actions:ListRecommendedActions",
            "case \"the action spelling\": expected this service's namespace, got {action:?}"
        );
        // (case, the document in force, what it says about this action)
        let rows: &[(&str, &str, Decision)] = &[
            (
                "every action of this service",
                r#"{"Statement":[{"Effect":"Allow","Action":"bcm-recommended-actions:*",
                 "Resource":"*"}]}"#,
                Decision::Allowed,
            ),
            (
                "exactly this action",
                r#"{"Statement":[{"Effect":"Allow",
                 "Action":"bcm-recommended-actions:ListRecommendedActions","Resource":"*"}]}"#,
                Decision::Allowed,
            ),
            (
                "every action of another billing service",
                r#"{"Statement":[{"Effect":"Allow","Action":"bcm-data-exports:*","Resource":"*"}]}"#,
                Decision::NotStated,
            ),
            (
                "this operation's name under another namespace",
                r#"{"Statement":[{"Effect":"Allow",
                 "Action":"ce:ListRecommendedActions","Resource":"*"}]}"#,
                Decision::NotStated,
            ),
            (
                "an explicit deny of this action beside an allow of this whole service",
                r#"{"Statement":[
                 {"Effect":"Allow","Action":"bcm-recommended-actions:*","Resource":"*"},
                 {"Effect":"Deny",
                  "Action":"bcm-recommended-actions:ListRecommendedActions","Resource":"*"}]}"#,
                Decision::Denied,
            ),
        ];
        for (case, document, expected) in rows {
            let reached = decision(document, &action);
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected {expected:?} for {action}, got {reached:?}"
            );
        }
    }

    /// RULE: a request to this service addresses no resource of its own, so a grant covers
    /// it only through a resource element that covers the all-resources wildcard; one scoped
    /// to some ARN covers no request to this service.
    #[test]
    fn a_grant_covers_this_service_only_through_the_all_resources_element() {
        let action = iam::iam_action(ACTION_PREFIX, OPERATION);
        // (case, the `Resource` element the allow is scoped to, whether it covers the call)
        let rows: &[(&str, &str, bool)] = &[
            ("every resource", "*", true),
            ("every resource of this account", "arn:aws:*:*:*:*", false),
            (
                "an ARN under this service's own namespace",
                "arn:aws:bcm-recommended-actions:us-east-1:000000000000:recommended-action/*",
                false,
            ),
            (
                "another service's ARN",
                "arn:aws:ce:::anomaly-monitor/abc",
                false,
            ),
        ];
        for (case, resource, covered) in rows {
            let document = format!(
                r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"{action}",
                 "Resource":"{resource}"}}]}}"#
            );
            let said = decision(&document, &action);
            let allowed = iam::is_allowed(said, Decision::NotStated);
            assert_eq!(
                allowed, *covered,
                "case {case:?}: expected the grant to {}cover {action}, got {said:?}",
                if *covered { "" } else { "not " }
            );
        }
    }

    /// RULE: the refusal reaching this service's error shape carries IAM's sentence — the
    /// principal, the action and which of IAM's rules refused it — whichever of the reasons
    /// the decision rested on, and names no resource, there being none to name.
    ///
    /// The expectation is SPELLED OUT here rather than built from the helpers the
    /// implementation calls, so a change to either half of the wording fails this test.
    #[test]
    fn the_refusal_states_iams_sentence_without_a_resource() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/billing-reader/session",
            ::engine::emulator_account_id()
        );
        let action = iam::iam_action(ACTION_PREFIX, OPERATION);
        // (case, the reason the decision rested on, the clause IAM closes with)
        let rows = [
            (
                "a default deny",
                iam::no_identity_policy_allows(&action),
                format!("because no identity-based policy allows the {action} action"),
            ),
            (
                "an explicit deny in an identity-based policy",
                iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
                "with an explicit deny in an identity-based policy".to_string(),
            ),
            (
                "an explicit deny in a session policy",
                iam::SESSION_POLICY_EXPLICIT_DENY.to_string(),
                "with an explicit deny in a session policy".to_string(),
            ),
            (
                "a grant no session policy allows",
                iam::no_session_policy_allows(&action),
                format!("because no session policy allows the {action} action"),
            ),
            (
                "an explicit deny in a permissions boundary",
                iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
                "with an explicit deny in a permissions boundary".to_string(),
            ),
            (
                "a grant no permissions boundary allows",
                iam::no_permissions_boundary_allows(&action),
                format!("because no permissions boundary allows the {action} action"),
            ),
        ];
        for (case, reason, clause) in rows {
            let refusal = not_authorized(&principal, &action, &reason);
            assert_eq!(
                refusal.message,
                format!("User: {principal} is not authorized to perform: {action} {clause}"),
                "case {case:?}: expected IAM's wording, got {:?}",
                refusal.message
            );
        }
    }
}
