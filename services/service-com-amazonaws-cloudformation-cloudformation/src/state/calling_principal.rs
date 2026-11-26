// SPDX-License-Identifier: CC0-1.0

//! The CALLING PRINCIPAL of one request, and what the account's IAM records say that
//! principal may do.
//!
//! ## The rule this module implements
//!
//! A stack operation with no `RoleARN` is carried out with the CALLER's permissions —
//! "CloudFormation uses a temporary session that's generated from your user credentials" —
//! so a call such an operation makes is decided against the identity-based policies in force
//! on the principal that signed the request, exactly as the owning service would decide it if
//! the client had made the call itself. This module is the one place that principal and its
//! permissions are read; [`super::service_role`] is the other half of the same reading, for a
//! stack that names a role to assume instead.
//!
//! The three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the SAME
//!    place `sts:GetCallerIdentity` answers from (`sts_store::resolve_request_caller`, over
//!    the long-term identity `sts_store::long_term_identity` resolves an IAM user's own key
//!    to), so the principal judged is the one the caller reads back for the credentials it
//!    signed with;
//! 2. **what it may do** — the identity-based policy documents in force on that principal and
//!    the permissions boundary that caps them, read through the IAM service's OWN accessors
//!    (`principal_policies`, `permissions_boundary_in_force`), where the collection rule
//!    lives — so a policy `iam:PutUserPolicy` wrote takes effect on the next request and
//!    deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the tree's ONE evaluation of
//!    those documents against `(action, resource)`, so this service and the service that owns
//!    the resource cannot answer differently about the same documents.
//!
//! ## The callers this decision cannot refuse
//!
//! * **The account's own published credential.** A request whose access key id neither STS nor
//!   IAM holds a record for speaks for the account itself — the position AWS account root
//!   credentials are in, which no identity-based policy bounds. This is also what every
//!   in-process call another emulated service makes presents.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy is
//!   owned by AWS rather than by the account, so the account's store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny a call AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses.
//! * **An engine carrying no IAM store**, where no identity can hold a policy at all.
//!
//! An expired session credential speaks for nobody: it carries no document, so the call it
//! asks for is refused.
//!
//! ## What one decision costs
//!
//! The work is bounded by what IAM holds for ONE principal and does not grow with the request:
//! that principal's inline policies, the managed policies attached to it and — for a user —
//! the groups it belongs to, each collected once by IAM's own accessor and read once by the
//! shared evaluator. It is taken with no store of this service locked and writes nothing to
//! either store it reads.

use std::time::SystemTime;

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// WHY one call is refused a principal, which is the clause AWS closes an authorization
/// refusal with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    /// A document in force states `Deny` for the call: "an explicit deny in any policy
    /// overrides any allows".
    ExplicitDeny,
    /// Nothing in force grants the call, and this principal is one whose grants this emulator
    /// can read in full.
    NoIdentityGrant,
    /// A grant is in force, but an explicit `Deny` in the permissions boundary capping it
    /// refuses the call — the one refusal of this family whose sentence NAMES the document the
    /// deciding statement came from.
    BoundaryExplicitDeny,
    /// A grant is in force, but the permissions boundary capping it does not allow the call.
    OutsideBoundary,
}

/// The principal a request speaks for, and everything a decision about its calls is made
/// from.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// The ARN of the principal the request speaks for, which is what a refusal names it by.
    /// `None` for a credential that speaks for nobody.
    pub arn: Option<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on it, when it carries one whose document this
    /// emulator holds: the ARN of the managed policy that is the boundary, and that document.
    /// The ARN is kept because a refusal an explicit `Deny` in the boundary explains names the
    /// boundary that carried it.
    boundary: Option<(String, String)>,
    /// Set when this request presents the account's own published credential, which holds no
    /// IAM policies (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not hold
    /// — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
    /// Set when this engine carries NO IAM store, where no identity can hold a policy at all,
    /// so nothing could be read about what this caller may do (see the module docs).
    no_identity_records: bool,
}

impl Caller {
    /// Whether nothing this decision found can refuse the caller once no readable document
    /// DENIES the call: the account's own credential, which holds no IAM policies, a principal
    /// carrying a grant this emulator cannot read, or an engine holding no identity records at
    /// all.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment || self.no_identity_records
    }

    /// Whether the account's IAM records show this principal may NOT perform `action` on
    /// `resource`.
    ///
    /// `true` is the one answer that refuses a call: the documents in force deny it, or none of
    /// them grants it and the caller is not one this decision cannot bound (see the module
    /// docs). `false` is every other case, the ones where nothing establishes that the call
    /// would be refused.
    pub fn unauthorized(&self, action: &str, resource: &str) -> bool {
        self.refused(action, resource).is_some()
    }

    /// The sentence AWS refuses an unauthorized call with — the principal, the action, the
    /// resource and WHY — or `None` where nothing establishes that the call would be refused.
    ///
    /// The wording is the workspace's one authorization refusal
    /// ([`iam::not_authorized_by_identity_policy`] with the clause that names the rule which
    /// withheld the call), so this service and the service that owns the resource cannot word
    /// the same refusal differently. `None` also for a credential this service could resolve no
    /// principal for: a sentence about a caller it cannot name is one it cannot word, so such a
    /// call is decided by nothing here.
    pub fn unauthorized_message(&self, action: &str, resource: &str) -> Option<String> {
        let principal = self.arn.as_deref()?;
        let reason = match self.refused(action, resource)? {
            Refused::ExplicitDeny => iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
            Refused::NoIdentityGrant => iam::no_identity_policy_allows(action),
            Refused::BoundaryExplicitDeny => match &self.boundary {
                Some((boundary_arn, _)) => iam::permissions_boundary_explicit_deny(boundary_arn),
                None => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            },
            Refused::OutsideBoundary => iam::no_permissions_boundary_allows(action),
        };
        Some(iam::not_authorized_by_identity_policy(
            principal, action, resource, &reason,
        ))
    }

    /// WHY `action` on `resource` is refused this principal, or `None` where nothing refuses
    /// it.
    fn refused(&self, action: &str, resource: &str) -> Option<Refused> {
        // An identity-based document states no `Principal` element, so the decision rests on
        // the action and the resource the call is about.
        let request = iam::Request::new(action, resource, &[]);
        let said = iam::identity_decision(&self.documents, &request);
        // IAM's order: an explicit `Deny` in a document that CAN be read refuses the call
        // whatever else the principal carries; otherwise a grant is needed, which a caller this
        // decision does not bound has by definition; and a grant is capped by the permissions
        // boundary, since "a permissions boundary sets the maximum permissions that an
        // identity-based policy can grant" (IAM, permissions boundaries).
        if said == Decision::Denied {
            return Some(Refused::ExplicitDeny);
        }
        if !(iam::is_allowed(said, Decision::NotStated) || self.identity_is_unbounded()) {
            return Some(Refused::NoIdentityGrant);
        }
        let (_, document) = self.boundary.as_ref()?;
        match iam::identity_decision(::std::slice::from_ref(document), &request) {
            Decision::Allowed => None,
            Decision::Denied => Some(Refused::BoundaryExplicitDeny),
            _ => Some(Refused::OutsideBoundary),
        }
    }

    /// The principal this request speaks for, with the identity-based policies in force on it.
    ///
    /// Resolving the caller goes through STS's own resolution, which materialises the session
    /// expiries the clock has already made due before it answers: that write is owed at the
    /// same instant whether or not anybody called.
    ///
    /// Call it with the handler's `&*self.engine` and NO store of this service locked: it takes
    /// the stores it reads, clones what it needs and releases them.
    pub fn of<Eng: Send + Sync + 'static>(
        engine: &Eng,
        ctx: &::smithy_server_runtime::OperationContext<'_>,
    ) -> Self {
        let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
        let iam = sts_store::iam_state(engine);
        // A LONG-TERM access key speaks for the IAM user that holds it — the map `iam:GetUser`
        // resolves a caller through — and a key no store holds is the account's own published
        // credential.
        let long_term = match &iam {
            Some(iam) => sts_store::long_term_identity(Some(&iam.lock()), &access_key_id),
            None => sts_store::ambient_identity(),
        };
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
                    caller.identity.arn.clone(),
                    // WHOSE identity-based documents are in force for this credential is not
                    // always the principal it speaks for: the role behind an assumed-role
                    // session, the IAM user behind a federated-user session, read through STS's
                    // one answer for it.
                    caller.identity_policies_arn().to_string(),
                )
            }),
            // No STS store on this engine: the credential names no session, so it speaks for
            // the long-term identity it was resolved to and is judged on that identity's
            // documents.
            None => Ok((long_term.arn.clone(), long_term.arn.clone())),
        };
        // An expired session credential speaks for nobody: it carries no document, so the call
        // it asks for is refused.
        let Ok((arn, policies_arn)) = resolved else {
            return Self::default();
        };
        let mut caller = Self {
            account_credential: arn == sts_store::ambient_identity().arn,
            arn: Some(arn),
            ..Self::default()
        };
        let Some(iam) = iam else {
            // No IAM store on this engine: no identity can hold a policy at all, so there is
            // nothing to read about what this caller may do and nothing withholds the call.
            caller.no_identity_records = true;
            return caller;
        };
        let held = iam.lock();
        // The documents are IAM's OWN collection of what an identity carries — its inline
        // policies, the documents of the policies attached to it and, for a user, its groups' —
        // called rather than re-derived, so this decision reads exactly what IAM itself judges
        // that principal on.
        if let Some(policies) = held.principal_policies(&policies_arn) {
            caller.documents = policies.documents;
            // An attachment the collection resolved no document for is an entry of AWS's own
            // catalog this emulator does not carry: a grant it cannot read.
            caller.unreadable_attachment = !policies.uncarried.is_empty();
        }
        caller.boundary = held.permissions_boundary_in_force(&policies_arn);
        caller
    }
}
