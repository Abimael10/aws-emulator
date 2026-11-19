// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets the CALLER of a request to this service perform the action it asks for
//! — the decision an operation takes BEFORE it reads or writes a trust anchor, a profile or
//! a certificate revocation list.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at what the
//! request named, so a principal whose policies do not allow the action is told it is not
//! authorized rather than told anything about the resource it addressed, and the call
//! performs nothing: a refused `CreateProfile` stores no profile, a refused
//! `DisableProfile` leaves the flag where it was. `AccessDeniedException` (HTTP 403) is the
//! answer this service's model declares for it — "You do not have sufficient access to
//! perform this action" (`smithy_query(query="shape", shape="AccessDeniedException")`) — so
//! every operation whose model declares that error asks this gate first, and an operation
//! whose model declares none has no refusal of this kind to give and does not ask.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the role session the
//!    mint recorded for that key, else the session the token registry holds for it
//!    (`sts_store::resolve_request_caller`, the same place `sts:GetCallerIdentity` answers
//!    from, so the principal a refusal blames is the one the caller reads back for the
//!    credentials it signed with), else the IAM user that holds the long-term key, else the
//!    account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), where the
//!    collection rule itself lives — so a document `iam:PutUserPolicy` wrote takes effect on
//!    the next request and deleting it lifts it again — and the SESSION POLICIES the mint of
//!    this very credential scoped it with (`sts_store::session_scoping_documents`), which cap
//!    those documents the same way: "the resulting session's permissions are the
//!    intersection of the role's identity-based policy and the session policies"
//!    (`AssumeRoleRequest$Policy`), so a session policy takes permissions away and grants
//!    none of its own;
//! 3. **the decision** — [`::service_common::iam_authorization`], the tree's ONE evaluation
//!    of those documents against `(action, resource)`, and the SENTENCE a refusal carries,
//!    which is that module's `not_authorized_by_identity_policy` and the reason clauses
//!    beside it, so nothing of IAM's wording is spelled here.
//!
//! Only the store LOOKUPS live here; neither the collection nor the decision does. They stay
//! in this service crate because the IAM service crate depends on `service-common`, so the
//! shared crate cannot read IAM's store without a dependency cycle. WHICH IAM entity a
//! principal ARN names carries no such dependency, so it is read through the shared
//! [`::service_common::iam_role`] rather than parsed again here.
//!
//! ## The action and the resource
//!
//! The action is `rolesanywhere:<OperationName>` ([`action_for`]): `rolesanywhere` is this
//! service's `arnNamespace` and its `sigv4Name` alike
//! (`smithy_query(query="service_info")`), which is the namespace its actions are published
//! under and the one its own documentation names ("Required permissions:
//! rolesanywhere:CreateProfile", `CreateProfile`).
//!
//! The resource is what the request ADDRESSES, in the one ARN form this namespace publishes
//! for that kind of resource ([`super::profile_arn`], [`super::trust_anchor_arn`],
//! [`super::crl_arn`], [`super::subject_arn`]): built from the identifier the request
//! carried and the Region it was signed for, never resolved out of the store, so the
//! decision is the same whether or not that resource exists here and a refusal tells an
//! unauthorized caller nothing about what this account holds. The three ARN-addressed
//! tagging operations are decided against the ARN the request itself names. A call that
//! addresses no single resource of its own — one that CREATES a resource whose id does not
//! exist yet, or one that reports the account's — is decided against the COLLECTION it adds
//! to or reads over ([`any_profile`] and its siblings), the wildcard form a statement has to
//! cover to permit such a call.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither the
//!   session registry nor IAM holds a record for resolves to
//!   [`sts_store::ambient_identity`], which is not a principal the IAM store holds policies
//!   for — the position AWS account root credentials are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses, and a permissions boundary the
//!   store DOES hold still caps the grant.
//! * **An engine carrying no session or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no document, so every action
//! it asks for is refused, and the refusal names that state rather than the key it
//! presented, which this module never quotes back.
//!
//! ## What this gate is not
//!
//! The native session plane ([`crate::service::create_session`]) is not decided here: its
//! caller holds an X.509 certificate instead of an AWS credential, so there is no principal
//! to read policies for, and what that request has to satisfy — a chain to an enabled trust
//! anchor, an enabled profile, a role the profile lists — is stated there.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use crate::ops::types::AccessDeniedException;

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The action namespace this service's operations are published under, which is its
/// `arnNamespace` and its SigV4 signing name alike (`smithy_query(query="service_info")`):
/// a policy statement spells one of them `rolesanywhere:<OperationName>`.
pub const ACTION_PREFIX: &str = super::ARN_NAMESPACE;

/// The resource id that stands for EVERY resource of one kind, which is how IAM writes the
/// resource of a call that addresses no single one of them.
const EVERY_ID: &str = "*";

/// The IAM action `operation` is performed under: `rolesanywhere:<OperationName>`.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The profile this request ADDRESSES by id, in the Region it was signed for: the ARN the
/// decision about such a request is taken against.
#[must_use]
pub fn addressed_profile(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    profile_id: &str,
) -> String {
    super::profile_arn(&super::request_region(ctx.parts()), profile_id)
}

/// The trust anchor this request addresses by id. The mirror of [`addressed_profile`].
#[must_use]
pub fn addressed_trust_anchor(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    trust_anchor_id: &str,
) -> String {
    super::trust_anchor_arn(&super::request_region(ctx.parts()), trust_anchor_id)
}

/// The certificate revocation list this request addresses by id. The mirror of
/// [`addressed_profile`].
#[must_use]
pub fn addressed_crl(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    crl_id: &str,
) -> String {
    super::crl_arn(&super::request_region(ctx.parts()), crl_id)
}

/// The subject this request addresses by id. The mirror of [`addressed_profile`].
#[must_use]
pub fn addressed_subject(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    subject_id: &str,
) -> String {
    super::subject_arn(&super::request_region(ctx.parts()), subject_id)
}

/// Every profile of this account in the Region the request was signed for: what a call that
/// addresses no single profile — creating one, listing them — is decided against.
#[must_use]
pub fn any_profile(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    addressed_profile(ctx, EVERY_ID)
}

/// Every trust anchor of this account in that Region. The mirror of [`any_profile`].
#[must_use]
pub fn any_trust_anchor(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    addressed_trust_anchor(ctx, EVERY_ID)
}

/// Every certificate revocation list of this account in that Region. The mirror of
/// [`any_profile`].
#[must_use]
pub fn any_crl(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    addressed_crl(ctx, EVERY_ID)
}

/// Every subject of this account in that Region. The mirror of [`any_profile`].
#[must_use]
pub fn any_subject(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    addressed_subject(ctx, EVERY_ID)
}

/// The refusal a request to `operation` on `resource` gets when the calling principal's IAM
/// policies do not allow it, and `None` when they do.
///
/// `operation` is the operation's own name ([`smithy_server_runtime::Operation::NAME`]) and
/// `resource` the ARN the request ADDRESSES (see the module docs).
///
/// Asked BEFORE this service's store is read or written, and with the identifier the request
/// carried rather than one resolved from a stored record, so the decision does not depend on
/// whether that resource exists — which is what keeps a refused call from telling an
/// unauthorized caller whether it does, and what puts this refusal ahead of every refusal
/// about the resource itself.
///
/// Resolving the caller goes through the token service's own resolution, which first
/// materialises the session expiries the clock has already made due; that write is owed at
/// the same instant whether or not anybody called, which is why a REPORTING operation of
/// this service may take this decision too.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    caller(engine, ctx).refusal(&action_for(operation), resource)
}

/// The authorization refusal of this request as the modelled shape every operation of this
/// service carries it in — `AccessDeniedException`, which the model binds to HTTP 403 — or
/// `None` when the calling principal's policies allow the call.
///
/// The one form every operation asks the gate in, so no handler words the refusal or chooses
/// its status and code for itself; each handler only lifts it into its own error enum's
/// `AccessDeniedException` variant.
#[must_use]
pub fn refused<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<AccessDeniedException> {
    refusal(engine, ctx, operation, resource).map(|message| AccessDeniedException {
        message: Some(message),
    })
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose
    /// document this emulator holds.
    boundary: Option<String>,
    /// The session policy documents the mint of this request's credential scoped it with, as
    /// they read at the instant of this request. Empty for a credential no mint scoped —
    /// every long-term access key, and every session minted without session policies — which
    /// is a credential capped by nothing.
    scope: sts_store::ScopingPolicies,
    /// The CLASS of the credential the request was signed with, as STS reports it for that
    /// credential (`sts_store::Caller::principal_type`): the value the global condition key
    /// naming the calling principal's type answers with. `None` for a request this gate
    /// resolved no credential for, which carries no value for the key at all.
    principal_type: Option<&'static str>,
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

    /// `Some(message)` when IAM does not allow `action` on `resource` for this caller — the
    /// sentence the refusal carries — and `None` when it does.
    ///
    /// IAM's order: an explicit `Deny` in a document that CAN be read refuses the request
    /// whatever else the principal carries; otherwise a grant is needed, which a caller this
    /// gate does not bound has by definition (see the module docs); and a grant is capped
    /// BOTH by the session policies the credential was minted with, since "the resulting
    /// session's permissions are the intersection of the role's identity-based policy and the
    /// session policies" (`AssumeRoleRequest$Policy`), AND by the permissions boundary, since
    /// "a permissions boundary sets the maximum permissions that an identity-based policy can
    /// grant" (IAM, permissions boundaries). Each cap takes permissions away and grants none
    /// of its own, so a call only a cap allows is refused as if that cap had allowed nothing.
    ///
    /// Which document withheld the grant is a different fact, and needs a different fix, from
    /// an explicit deny in it — so the refusal names the one the decision rested on, in the
    /// order IAM reads them.
    fn refusal(&self, action: &str, resource: &str) -> Option<String> {
        // An identity-based document states no `Principal` element, so the decision rests on
        // the action and the resource the request addressed — and on the CLASS of the
        // credential presented, which is what a statement conditioned on `aws:PrincipalType`
        // selects on. That key is answered, and declared answered, for every request whose
        // credential this gate resolved, so such a clause decides rather than standing
        // undecided.
        let context = ::service_common::temporary_credential_classes::principal_type_conditions(
            self.principal_type,
        );
        let published =
            [::service_common::temporary_credential_classes::PRINCIPAL_TYPE_CONDITION_KEY];
        let request = iam::Request::new(action, resource, &[])
            .with_context(&context)
            .with_published_keys(&published);
        let said = iam::identity_decision(&self.documents, &request);
        let granted = iam::is_allowed(said, Decision::NotStated)
            || (said != Decision::Denied && self.identity_is_unbounded());
        // What the SCOPE this credential was minted with says, through the tree's one reading
        // of a resolved scope: a credential no mint scoped is capped by nothing, which that
        // reading answers as allowed.
        let scoped = self.scope.decision(&request);
        // The ceiling in force on the principal. A call an identity-based policy explicitly
        // DENIES is already refused on that deny, so no ceiling is read for it.
        let capped = match (said == Decision::Denied, self.boundary.as_ref()) {
            (false, Some(boundary)) => {
                iam::identity_decision(::std::slice::from_ref(boundary), &request)
            }
            _ => Decision::Allowed,
        };
        if granted && scoped == Decision::Allowed && capped == Decision::Allowed {
            return None;
        }
        let reason = if said == Decision::Denied {
            iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
        } else if scoped == Decision::Denied {
            iam::SESSION_POLICY_EXPLICIT_DENY.to_string()
        } else if capped == Decision::Denied {
            iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()
        } else if !granted {
            iam::no_identity_policy_allows(action)
        } else if scoped != Decision::Allowed {
            iam::no_session_policy_allows(action)
        } else {
            iam::no_permissions_boundary_allows(action)
        };
        Some(iam::not_authorized_by_identity_policy(
            &self.arn, action, resource, &reason,
        ))
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's OWN
    /// accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    ///
    /// `principal_policies` is where the collection rule itself lives — a role's or user's
    /// inline policies, the default version of each managed policy attached to it, and for a
    /// user the policies of the groups it belongs to — so this service reads an identity's
    /// permissions exactly as IAM's own authorizer does.
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

/// The principal this request speaks for, with the policies in force on it.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    // The identity a LONG-TERM key speaks for, through the token service's own resolution of
    // such a key against the account's IAM records — the same answer `sts:GetCallerIdentity`
    // gives the caller for it, and the same one a profile's `createdBy` records
    // ([`super::caller_principal_arn`]).
    let long_term = {
        let iam = sts_store::iam_state(engine);
        let guard = iam.as_ref().map(|iam| iam.lock());
        sts_store::long_term_identity(guard.as_deref(), &access_key_id)
    };
    // Who the credential speaks for AND what its mint scoped it to, read together from the
    // session registry: the scope travels with the credential.
    let resolved = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_request_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
            long_term.clone(),
        )
        .map(|caller| {
            (
                caller.principal_type(),
                caller.identity,
                caller.session_policies,
            )
        }),
        // No session registry on this engine: the only identity a request can present is the
        // one its long-term key resolves to, which no mint scoped.
        None => Ok((
            ::service_common::temporary_credential_classes::PRINCIPAL_TYPE_USER,
            long_term,
            sts_store::SessionPolicies::default(),
        )),
    };
    // An expired session credential speaks for nobody: it carries no document, so every
    // action it asks for is refused — and the refusal names the credential's STATE rather
    // than the key id that was presented, which this module never quotes back.
    let (principal_type, identity, session_policies) = match resolved {
        Ok(resolved) => resolved,
        Err(_) => {
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                ..Caller::default()
            }
        }
    };

    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        // WHICH CLASS of credential signed this request, read through STS's one answer for it
        // (`sts_store::Caller::principal_type`).
        principal_type: Some(principal_type),
        arn: identity.arn,
        // The inline document the mint passed scopes the credential whether or not an IAM
        // store is there; where there is one, the scope is re-read below so a managed session
        // policy counts at the version in force NOW.
        scope: sts_store::session_scoping_documents(None, &session_policies),
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // What the mint scoped this credential to, as the documents read at the instant of this
    // request: a managed session policy stands for its DEFAULT version, so one edited since
    // the mint scopes by what it says now.
    caller.scope = sts_store::session_scoping_documents(Some(&store), &session_policies);
    // A TEMPORARY credential of a ROLE SESSION: the record the minting operation left in
    // IAM's own store is what says which principal that access key id speaks for and which
    // role's policies the call is judged on — the same record IAM's own authorizer resolves
    // such a credential through — so it is read FIRST. It also carries the scope the mint
    // passed, for the same reason.
    let now = ::aws_smithy_types::DateTime::from(SystemTime::now());
    if let Some(session) = store.role_session(&access_key_id, now) {
        let role_arn = session.role_arn.clone();
        caller.account_credential = false;
        caller.arn = session.session_arn.clone();
        caller.scope = sts_store::session_scoping_documents(Some(&store), &session.scope);
        caller.read_policies_of(&store, &role_arn);
        return caller;
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

    /// What one stored document says about `action` on `resource`, through the shared
    /// evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// RULE: the action this gate asks about carries this service's OWN namespace and the
    /// operation's own name, so a policy scoped to this service's actions covers every
    /// operation of it and a policy about another service covers none.
    ///
    /// The operations are the ones this model binds, as `smithy_query(query="digest")` lists
    /// them.
    #[test]
    fn the_action_asked_about_names_this_service_and_the_operation() {
        for operation in [
            "CreateProfile",
            "CreateTrustAnchor",
            "DeleteAttributeMapping",
            "DeleteCrl",
            "DeleteProfile",
            "DeleteTrustAnchor",
            "DisableProfile",
            "DisableTrustAnchor",
            "EnableProfile",
            "EnableTrustAnchor",
            "GetProfile",
            "GetSubject",
            "GetTrustAnchor",
            "ImportCrl",
            "ListCrls",
            "ListProfiles",
            "ListTagsForResource",
            "ListTrustAnchors",
            "PutAttributeMapping",
            "PutNotificationSettings",
            "ResetNotificationSettings",
            "TagResource",
            "UntagResource",
            "UpdateCrl",
            "UpdateProfile",
            "UpdateTrustAnchor",
        ] {
            let action = action_for(operation);
            assert_eq!(
                action,
                format!("rolesanywhere:{operation}"),
                "case {operation:?}: expected this service's namespace, got {action:?}"
            );
            let resource = super::super::profile_arn("us-west-2", "the-profile");
            // (case, the document in force, what it says about this action)
            let rows: &[(&str, String, Decision)] = &[
                (
                    "every action of this service",
                    r#"{"Statement":[{"Effect":"Allow","Action":"rolesanywhere:*","Resource":"*"}]}"#
                        .to_string(),
                    Decision::Allowed,
                ),
                (
                    "exactly this action",
                    format!(
                        r#"{{"Statement":[{{"Effect":"Allow","Action":"{action}",
                         "Resource":"*"}}]}}"#
                    ),
                    Decision::Allowed,
                ),
                (
                    "every action of the token service",
                    r#"{"Statement":[{"Effect":"Allow","Action":"sts:*","Resource":"*"}]}"#
                        .to_string(),
                    Decision::NotStated,
                ),
                (
                    "an explicit deny of this action beside an allow of the namespace",
                    format!(
                        r#"{{"Statement":[
                         {{"Effect":"Allow","Action":"rolesanywhere:*","Resource":"*"}},
                         {{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
                    ),
                    Decision::Denied,
                ),
            ];
            for (case, document, expected) in rows {
                let reached = decision(document, &action, &resource);
                assert_eq!(
                    reached, *expected,
                    "case {case:?} / {operation}: expected {expected:?} for {action}, got \
                     {reached:?}"
                );
            }
        }
    }

    /// RULE: the decision is taken against the ARN the request addressed, so a grant scoped
    /// to one resource covers that resource and no other — not a sibling of the same kind,
    /// not the same id in another Region, and not another kind of resource.
    #[test]
    fn a_grant_is_read_for_the_resource_the_request_addressed() {
        let addressed = super::super::profile_arn("us-west-2", "ab12cd34-ef56-7890-ab12-cd34ef567890");
        let action = action_for("GetProfile");
        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
             "Action":"{action}","Resource":"{addressed}"}}]}}"#
        );
        // (case, the resource the decision is taken against, whether the grant covers it)
        let rows: &[(&str, String, bool)] = &[
            ("the profile the request names", addressed.clone(), true),
            (
                "another profile of the same account and Region",
                super::super::profile_arn("us-west-2", "00000000-0000-0000-0000-000000000000"),
                false,
            ),
            (
                "the same profile id in another Region",
                super::super::profile_arn("eu-west-3", "ab12cd34-ef56-7890-ab12-cd34ef567890"),
                false,
            ),
            (
                "the trust anchor of the same id",
                super::super::trust_anchor_arn("us-west-2", "ab12cd34-ef56-7890-ab12-cd34ef567890"),
                false,
            ),
            (
                "every profile of the account",
                super::super::profile_arn("us-west-2", EVERY_ID),
                false,
            ),
        ];
        for (case, resource, covered) in rows {
            let said = decision(&document, &action, resource);
            let actual = iam::is_allowed(said, Decision::NotStated);
            assert_eq!(
                actual, *covered,
                "case {case:?}: expected the grant to {}cover {resource}, got {said:?}",
                if *covered { "" } else { "not " }
            );
        }
    }

    /// RULE: a call that addresses no single resource is decided against the COLLECTION of
    /// that kind, which is the wildcard form of this namespace's ARN for it — so a statement
    /// naming every resource of the kind, or everything, permits such a call.
    #[test]
    fn a_collection_resource_is_the_wildcard_arn_of_its_kind() {
        let account = ::engine::emulator_account_id();
        // (case, the collection ARN, the resource part it must carry)
        let rows: &[(&str, String, &str)] = &[
            (
                "every profile",
                super::super::profile_arn("us-west-2", EVERY_ID),
                "profile/*",
            ),
            (
                "every trust anchor",
                super::super::trust_anchor_arn("us-west-2", EVERY_ID),
                "trust-anchor/*",
            ),
            (
                "every certificate revocation list",
                super::super::crl_arn("us-west-2", EVERY_ID),
                "crl/*",
            ),
            (
                "every subject",
                super::super::subject_arn("us-west-2", EVERY_ID),
                "subject/*",
            ),
        ];
        let action = action_for("CreateProfile");
        for (case, collection, resource_part) in rows {
            assert_eq!(
                *collection,
                format!("arn:aws:rolesanywhere:us-west-2:{account}:{resource_part}"),
                "case {case:?}: expected this namespace's wildcard ARN, got {collection:?}"
            );
            for document in [
                r#"{"Statement":[{"Effect":"Allow","Action":"rolesanywhere:*","Resource":"*"}]}"#
                    .to_string(),
                format!(
                    r#"{{"Statement":[{{"Effect":"Allow","Action":"rolesanywhere:*",
                     "Resource":"{collection}"}}]}}"#
                ),
            ] {
                let said = decision(&document, &action, collection);
                assert_eq!(
                    said,
                    Decision::Allowed,
                    "case {case:?}: expected {document} to permit a call on {collection}, got \
                     {said:?}"
                );
            }
        }
    }
}
