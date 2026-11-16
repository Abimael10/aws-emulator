// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLER's own identity-based policies let a request to this service
//! perform the action it asks for — the decision an operation takes before it
//! resolves anything the request names.
//!
//! AWS authorizes at the IAM layer, ahead of the service evaluating the resource, so
//! a caller IAM does not permit is told it is not authorized rather than told
//! whether the resource exists. This service's model declares no shape for that
//! refusal, so it leaves through the same channel the resource-scoped decision's
//! does ([`crate::state::authorization`]), carrying the code and status the real
//! service answers.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the
//!    SAME place `sts:GetCallerIdentity` answers from (`sts_store::resolve_caller`),
//!    so the principal a refusal blames is the one the caller reads back from STS
//!    for the credentials it signed with;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds
//!    for that principal, read through IAM's OWN accessor (`principal_policies`,
//!    where the collection rule lives), so a document `iam:PutRolePolicy` wrote
//!    takes effect on the next request and deleting it lifts the refusal again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared
//!    evaluation of those documents against `(action, resource)`, so this service
//!    and every other one handed the same documents answer alike.
//!
//! Only the store LOOKUPS live here; neither the decision nor the ARN readings do.
//! That is the split every other gate in this tree makes, and the lookups stay in
//! this crate for the reason those keep theirs here: the IAM service crate depends
//! on `service-common`, so the shared crate cannot read IAM's store without a
//! dependency cycle. WHICH IAM ENTITY a principal ARN names carries no such
//! dependency, so it is read through the shared [`::service_common::iam_role`]
//! rather than parsed again here.
//!
//! ## What is refused, and what is not
//!
//! IAM's own order, over the documents this emulator can read:
//!
//! * an EXPLICIT `Deny` about the action refuses the request whatever else the
//!   account holds, and it is the decision that can be taken before anything about
//!   the resource is known;
//! * otherwise the request needs a GRANT — an `Allow` of the action covering the
//!   resource the request addressed — because a request no policy allows falls to
//!   IAM's default deny. A request addressing no resource of its own
//!   ([`crate::state::authorization::ANY_RESOURCE`]) is granted by any `Allow` of
//!   the action, whichever resources that statement is scoped to
//!   ([`::service_common::iam_authorization::identity_may_allow_action`]): the
//!   resource a statement would have to cover is not known, so withholding the
//!   grant would refuse calls AWS permits.
//!
//! The callers this gate does not bound, because nothing it can read says what they
//! may do — refusing them would deny calls AWS permits:
//!
//! * the account's own published credential, which holds no identity-based policy
//!   at all (`sts_store::identity_is_account_credential`), the position AWS account
//!   root credentials are in;
//! * a principal whose permissions this emulator cannot read in full: one carrying
//!   an attachment the IAM store resolves no document for (an entry of AWS's own
//!   managed-policy catalog), and one whose identity-in-force the IAM store holds no
//!   record of at all — a session of a role the account does not hold, a federated
//!   user, or a request served by an engine carrying no IAM or STS store;
//! * an expired session credential, which carries no documents and speaks for
//!   nobody (the layer that authenticates a request is what rejects it).
//!
//! An explicit `Deny` in a document that CAN be read still refuses every one of
//! them.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The principal the REQUEST's credential speaks for, as `sts:GetCallerIdentity`
/// reports it — the identity an authorization refusal blames.
///
/// Resolved from the same two stores the decision below reads, so a refusal names
/// the session or the user that signed the call rather than the account's own
/// published principal; a credential neither store knows is that published
/// principal, which is the identity such a request speaks for.
pub fn calling_principal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    caller_identity(engine, ctx).principal()
}

/// The principal a request speaks for, and everything the identity-based decision
/// is made from.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal
    /// names. Empty for a credential that speaks for nobody.
    arn: String,
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of
    /// the IAM role behind an assumed-role session — which is what a resource-based
    /// policy's `Principal` element may name the caller by.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// Set when this request presents the account's own published credential, which
    /// holds no IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the IAM store answered for the identity whose documents are in
    /// force, so the documents collected ARE this principal's permission set.
    policies_readable: bool,
    /// Set when the principal carries an attachment the IAM store resolves no
    /// document for — an entry of AWS's own catalog (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// The principal a refusal blames: the identity the credential speaks for, and
    /// the account's own published principal for a credential that speaks for
    /// nobody.
    pub fn principal(&self) -> String {
        if self.arn.is_empty() {
            return crate::state::authorization::caller_principal();
        }
        self.arn.clone()
    }

    /// Every spelling a policy's `Principal` element may name this caller by.
    pub fn principals(&self) -> &[String] {
        &self.principals
    }

    /// Whether nothing this gate can read says what the caller may do, so no
    /// absence of a grant refuses it (see the module docs). An explicit `Deny` in a
    /// readable document still does.
    pub fn unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment || !self.policies_readable
    }

    /// Whether a readable identity-based document explicitly DENIES `action` on
    /// `resource`.
    pub fn denies(&self, action: &str, resource: &str) -> bool {
        !self.documents.is_empty() && self.decision(action, resource) == Decision::Denied
    }

    /// Whether the identity-based half of the decision GRANTS `action` on
    /// `resource`: an `Allow` about the request, or a caller nothing readable bounds.
    pub fn grants(&self, action: &str, resource: &str) -> bool {
        if self.unbounded() {
            return true;
        }
        if self.documents.is_empty() {
            return false;
        }
        // A request that addresses no resource of its own leaves no resource for a
        // statement to cover, so the weaker question is the one IAM's grant can be
        // read from here.
        if resource == crate::state::authorization::ANY_RESOURCE {
            return iam::identity_may_allow_action(&iam::prepare_documents(&self.documents), action);
        }
        self.decision(action, resource) == Decision::Allowed
    }

    /// IAM's whole identity-based decision as a refusal: the explicit deny first,
    /// then the default deny of a request no readable document allows. `None` when
    /// the identity-based half grants the call.
    ///
    /// `action` is the fully qualified IAM action the call asks for —
    /// `kafka:<ApiName>` for a control-plane call
    /// ([`crate::state::authorization::control_plane_action`]), the data-plane
    /// action for a call that reaches a cluster's Apache Kafka plane — and
    /// `resource` is the ARN the request NAMES, as submitted, before anything is
    /// resolved: the decision does not depend on whether that resource exists, so a
    /// refused call tells an unauthorized caller nothing about whether it does.
    ///
    /// Generic over the operation's own modelled error enum because the refusal is
    /// none of them: it leaves through the error channel's unmodelled variant, with
    /// the code and status [`crate::state::authorization`] names.
    pub fn refusal<E>(
        &self,
        action: &str,
        resource: &str,
    ) -> Option<crate::scaffolded::Scaffolded<E>> {
        if let Some(refusal) = self.explicit_deny(action, resource) {
            return Some(refusal);
        }
        (!self.grants(action, resource)).then(|| self.no_grant(action, resource))
    }

    /// Only the EXPLICIT-DENY half of that decision — the half that can be taken
    /// whatever else is in force, which is why it is the one an action a RESOURCE
    /// policy may still grant takes before the resource is resolved
    /// ([`crate::state::authorization::authorize_cluster_policy`] takes the rest).
    pub fn explicit_deny<E>(
        &self,
        action: &str,
        resource: &str,
    ) -> Option<crate::scaffolded::Scaffolded<E>> {
        self.denies(action, resource).then(|| {
            crate::state::authorization::access_denied(not_authorized_by_explicit_deny(
                &self.principal(),
                action,
                resource,
            ))
        })
    }

    /// The refusal of a request no readable document allows — IAM's default deny,
    /// whose sentence names the action nothing granted.
    pub fn no_grant<E>(&self, action: &str, resource: &str) -> crate::scaffolded::Scaffolded<E> {
        crate::state::authorization::access_denied(iam::not_authorized_by_identity_policy(
            &self.principal(),
            action,
            resource,
            &iam::no_identity_policy_allows(action),
        ))
    }

    /// What this caller's documents say about `action` on `resource`, through the
    /// shared evaluation.
    fn decision(&self, action: &str, resource: &str) -> Decision {
        // An identity-based document states no `Principal` element, so the decision
        // rests on the action and the resource the request addressed.
        iam::identity_decision(&self.documents, &iam::Request::new(action, resource, &[]))
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM
    /// service's own accessor — where the collection rule lives.
    fn read_policies_of(
        &mut self,
        store: &::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State,
        identity_arn: &str,
    ) {
        let Some(policies) = store.principal_policies(identity_arn) else {
            return;
        };
        self.policies_readable = true;
        // An attachment the collection resolved no document for is an entry of
        // AWS's own catalog this emulator does not carry: a grant it cannot read.
        self.unreadable_attachment = !policies.uncarried.is_empty();
        self.documents = policies.documents;
    }
}

/// The sentence IAM states an explicit-deny decision with: the principal, the action
/// and the resource — each taken from the request and the credential it presented —
/// closed by the clause AWS names an identity-based document with, so a client
/// reading it learns that fixing the resource's own policy will not help.
fn not_authorized_by_explicit_deny(principal: &str, action: &str, resource: &str) -> String {
    iam::not_authorized_by_identity_policy(
        principal,
        action,
        resource,
        iam::IDENTITY_POLICY_EXPLICIT_DENY,
    )
}

/// The principal this request speaks for, with the identity-based policies in force
/// on it.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which materialises
/// the session expiries the clock has already made due before it answers: that write
/// is owed at the same instant whether or not anybody called, which is why a
/// REPORTING operation of this service may take this decision too.
pub fn caller_identity<Eng: Send + Sync + 'static>(
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
        // No STS store on this engine: the only identity a request can present is
        // the account's own published credential, which holds no identity policy.
        None => Ok(sts_store::ambient_identity()),
    };
    // An expired session credential speaks for nobody: it carries no documents and
    // no principal spelling, so nothing here refuses it (the layer that
    // authenticates a request is what rejects it).
    let Ok(identity) = identity else {
        return Caller::default();
    };
    let mut caller = Caller {
        // The ONE reading of "nothing identity-based bounds this credential"
        // (`sts_store::identity_is_account_credential`), so this gate cannot drift
        // from the other services that decide a request signed with it.
        account_credential: sts_store::identity_is_account_credential(&identity.arn),
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // A long-term access key names no session, so STS answers the account's own
    // identity for it — but IAM's store maps an access key id to the USER that owns
    // it, and a request signed with one speaks for that user and is judged on the
    // user's policies.
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
    // The identity whose policies are in force: for an assumed-role session the
    // ROLE's, addressed by the ARN the account holds the role under (which carries
    // its path, where the session ARN does not); for an IAM user its own.
    match ::service_common::iam_role::assumed_role_session_role_name(&caller.arn) {
        Some(role_name) => {
            if let Some(role_arn) = store
                .roles
                .get(&iam_store::name_key(role_name))
                .map(|held| held.arn.clone())
            {
                // A resource-based policy may name the caller by the ROLE as well
                // as by the session, so both spellings travel with the request.
                caller.principals.push(role_arn.clone());
                caller.read_policies_of(&store, &role_arn);
            }
        }
        None => {
            if ::service_common::iam_role::user_name(&caller.arn).is_some() {
                let user_arn = caller.arn.clone();
                caller.read_policies_of(&store, &user_arn);
            }
        }
    }
    caller
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision one stored document reaches about an action on a resource,
    /// through the shared evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// RULE: the action this gate asks about is `kafka:<ApiName>`, so a document
    /// scoped to this service's actions decides every operation of it and nothing
    /// outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document = r#"{"Statement":[{"Effect":"Deny","Action":"kafka:*","Resource":"*"}]}"#;
        for operation in ["RebootBroker", "DescribeCluster", "UpdateBrokerCount"] {
            let action = crate::state::authorization::control_plane_action(operation);
            let reached = decision(document, &action, "*");
            assert_eq!(
                reached,
                Decision::Denied,
                "case {operation:?}: expected kafka:* to deny {action}, got {reached:?}"
            );
        }
        let outside = decision(document, "ec2:DescribeSubnets", "*");
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected kafka:* to state nothing outside this \
             service, got {outside:?}"
        );
    }

    /// RULE: the refusal names the principal the credential speaks for, the action
    /// asked for, the resource the request addressed, and that an identity-based
    /// document explicitly denied it.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/kafka-denied/session-1",
            ::engine::emulator_account_id()
        );
        let cluster = ::engine::emulator_arn(
            "kafka",
            "us-west-2",
            "cluster/orders/11111111-2222-3333-4444-555555555555-1",
        );
        let action = crate::state::authorization::control_plane_action("RebootBroker");
        let message = not_authorized_by_explicit_deny(&principal, &action, &cluster);
        assert_eq!(
            message,
            format!(
                "User: {principal} is not authorized to perform: {action} on resource: \
                 {cluster} with an explicit deny in an identity-based policy"
            ),
            "case \"an explicit deny\": expected IAM's wording, got {message:?}"
        );
    }

    /// RULE: a request no readable document allows falls to IAM's default deny,
    /// whose sentence closes by naming the action nothing granted.
    #[test]
    fn the_default_deny_names_the_action_nothing_allowed() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/kafka-unpermitted/session-1",
            ::engine::emulator_account_id()
        );
        let cluster = ::engine::emulator_arn(
            "kafka",
            "us-west-2",
            "cluster/orders/11111111-2222-3333-4444-555555555555-1",
        );
        let action = crate::state::authorization::control_plane_action("GetBootstrapBrokers");
        let caller = Caller {
            arn: principal.clone(),
            principals: vec![principal.clone()],
            policies_readable: true,
            ..Caller::default()
        };
        assert!(
            !caller.grants(&action, &cluster),
            "case \"no document at all\": a caller whose readable permission set is empty \
             must reach no grant"
        );
        let refusal: crate::scaffolded::Scaffolded<()> = caller.no_grant(&action, &cluster);
        let crate::scaffolded::Scaffolded::Unmodeled(refused) = &refusal else {
            panic!("case \"no document at all\": expected the unmodelled refusal, got {refusal:?}")
        };
        let message = refused.message.clone();
        assert_eq!(
            message,
            format!(
                "User: {principal} is not authorized to perform: {action} on resource: \
                 {cluster} because no identity-based policy allows the {action} action"
            ),
            "case \"no document at all\": expected IAM's default-deny wording, got {message:?}"
        );
    }

    /// RULE: the callers nothing readable bounds are granted every action no
    /// readable document denies — the account's own credential, a principal
    /// carrying an attachment this emulator cannot read, and one the IAM store
    /// holds no record of.
    #[test]
    fn a_caller_this_emulator_cannot_read_is_not_refused_for_a_missing_grant() {
        let action = crate::state::authorization::control_plane_action("ListScramSecrets");
        let resource = ::engine::emulator_arn(
            "kafka",
            "us-west-2",
            "cluster/orders/11111111-2222-3333-4444-555555555555-1",
        );
        let rows: Vec<(&str, Caller)> = vec![
            (
                "the account's own published credential",
                Caller {
                    account_credential: true,
                    ..Caller::default()
                },
            ),
            (
                "a principal carrying an unreadable attachment",
                Caller {
                    policies_readable: true,
                    unreadable_attachment: true,
                    ..Caller::default()
                },
            ),
            (
                "a principal the IAM store holds no record of",
                Caller::default(),
            ),
        ];
        for (case, caller) in rows {
            assert!(
                caller.grants(&action, &resource),
                "case {case:?}: nothing readable bounds this caller, so the absence of a \
                 grant must not refuse it"
            );
        }
    }
}
