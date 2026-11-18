// SPDX-License-Identifier: CC0-1.0

//! WHO a request speaks for, what it may do, and how a refusal of it is worded — the one
//! reading of a request's caller this crate has.
//!
//! Every authorization question this service answers rests on the same three pieces, so they
//! live here once rather than per gate:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the IAM store
//!    of the same shared engine, which holds both halves of that question: the registry
//!    `iam:CreateAccessKey` wrote for a long-term key, and the role-session record the
//!    minting operation left for a temporary one. So the principal a refusal blames is the one
//!    the account really holds for the credential the request signed with;
//! 2. **what it may do** — the identity-based documents the IAM store holds for that
//!    principal, and the permissions boundary that caps them, read through IAM's own accessors
//!    (`principal_policies`, `permissions_boundary_document`) so a document `iam:PutRolePolicy`
//!    wrote takes effect on the next request and deleting it lifts the refusal again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the tree's ONE evaluation of
//!    policy documents against `(action, resource)`, so this service and every other one
//!    handed the same documents decide alike: an explicit `Deny` wins over every `Allow`, an
//!    `Allow` about the request grants it, and with neither the request falls to IAM's default
//!    deny — capped by the permissions boundary, which "sets the maximum permissions that an
//!    identity-based policy can grant".
//!
//! The sentence a refusal is worded with is the shared one
//! ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]), built from the
//! principal the credential speaks for, the action, and the resource the REQUEST addressed —
//! so nothing has to be looked up to state it, which is why a gate can refuse before it
//! resolves anything.
//!
//! The two gates standing on this are the request's OWN action
//! ([`super::caller_authorization`]) and the pass of an execution role to this service
//! ([`super::pass_role`]).
//!
//! Only the store lookups live here; they stay in this crate because hoisting them into
//! `service-common` would make every service depending on that crate link the IAM service
//! crate.
//!
//! ## What a decision taken here does not bound
//!
//! Nothing is refused on evidence this emulator does not have:
//!
//! * **the account's own published credential**, which no IAM record holds policies for — the
//!   position AWS account root credentials are in, and what every in-process call another
//!   emulated service makes is signed with;
//! * **a principal carrying an attachment whose document this account does not hold** (an
//!   AWS-managed policy belongs to AWS, so the store holds no document for it) is not refused
//!   on a grant that cannot be read, though an explicit `Deny` in a document that CAN be read
//!   still refuses it;
//! * **an engine carrying no IAM store**, where no policy can exist at all.
//!
//! LOCK ORDER: the IAM store's lock is taken by the reading below and released before it
//! answers, which is why every handler takes its decision BEFORE this service's own store is
//! locked. IAM never reads this service's state.

use ::service_common::iam_authorization::{self as iam, Decision};

/// The principal a request speaks for, and everything a decision about it is made from.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    pub arn: String,
    /// Every spelling of the principal a policy may name it by: the ARN STS reports, and the
    /// ARN of the IAM role behind an assumed-role session.
    pub principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    pub documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose document
    /// this emulator holds.
    pub boundary: Option<String>,
    /// Set when this request presents the account's own published credential, which holds no
    /// IAM policies (see the module docs).
    pub account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not
    /// hold — an AWS-managed policy (see the module docs).
    pub unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no
    /// readable document DENIES the request: the account's own credential, which holds no IAM
    /// policies, or a principal carrying a grant this emulator cannot read.
    pub fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }
}

/// Who this request speaks for, resolved from the access key id it signed with.
///
/// Which principal an access key id speaks for is a question about the account's IDENTITIES,
/// and the IAM store of the same shared engine answers both halves of it: a long-term key
/// through the registry `iam:CreateAccessKey` wrote, and a TEMPORARY credential of a role
/// session through the record the minting operation left there — so no other service's store is
/// consulted here. A key the account holds no live record for is the account's OWN published
/// credential; whether a credential verifies at all is the emulator's authentication layer's
/// decision, taken before any handler runs.
pub fn request_caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    let Some(iam) = super::iam_state(engine) else {
        // No IAM store on this engine: the account holds no identity for the key to speak
        // for, so the request is the account's own.
        return account_caller();
    };
    let store = iam.lock();
    let now = ::aws_smithy_types::DateTime::from(::std::time::SystemTime::now());
    // A temporary credential of a role session: the principal is the SESSION, and the policies
    // in force are the ROLE's ("the temporary security credentials … have the permissions of
    // the role", `AssumeRole`). The role's own ARN is also a spelling a policy names the caller
    // by, so a statement about the role decides the session.
    if let Some(session) = store.role_session(&access_key_id, now) {
        let role_arn = session.role_arn.clone();
        let mut caller = named_caller(&session.session_arn);
        if !caller.principals.contains(&role_arn) {
            caller.principals.push(role_arn.clone());
        }
        read_policies_of(&store, &role_arn, &mut caller);
        return caller;
    }
    // A long-term access key speaks for the IAM user that holds it, which is the identity
    // `sts:GetCallerIdentity` reports for it.
    let user_arn = store
        .access_keys
        .get(&access_key_id)
        .and_then(|record| store.users.get(&record.user_key))
        .map(|user| user.arn.clone());
    match user_arn {
        Some(user_arn) => {
            let mut caller = named_caller(&user_arn);
            read_policies_of(&store, &user_arn, &mut caller);
            caller
        }
        None => account_caller(),
    }
}

/// The caller a request presenting the account's own published credential speaks for: the
/// principal `sts:GetCallerIdentity` reports for it, read from the shared definition so this
/// service refuses in the name of the same principal every other service names.
fn account_caller() -> Caller {
    Caller {
        account_credential: true,
        ..named_caller(&::service_common::caller_principal::ambient_user_arn())
    }
}

/// The caller `arn` names, with no documents read yet.
fn named_caller(arn: &str) -> Caller {
    Caller {
        arn: arn.to_string(),
        principals: vec![arn.to_string()],
        ..Caller::default()
    }
}

/// Read what the IAM identity `identity_arn` names may do onto `caller`, through IAM's own
/// accessors: the documents in force on it, whether it carries an attachment whose document
/// this emulator cannot read, and the permissions boundary that caps it.
fn read_policies_of(store: &super::IamState, identity_arn: &str, caller: &mut Caller) {
    if let Some(policies) = store.principal_policies(identity_arn) {
        caller.documents = policies.documents;
        // An attachment the collection resolved no document for is an entry of AWS's own
        // catalog this emulator does not carry: a grant it cannot read.
        caller.unreadable_attachment = !policies.uncarried.is_empty();
    }
    caller.boundary = store.permissions_boundary_document(identity_arn);
}

/// Why IAM does not let `caller` perform `action` on `resource`, or `None` when it does —
/// including every caller a decision does not bound (see the module docs).
///
/// `request` is the access request the documents are read against: the same action and resource
/// the sentence names, together with whatever condition context the call itself fixes
/// ([`super::pass_role`] carries one; a request that publishes no condition key of its own
/// passes [`iam::Request::new`]). A pure function of the documents, the action and the
/// resource, so nothing it answers depends on when it is called.
pub fn identity_denial(
    caller: &Caller,
    action: &str,
    resource: &str,
    request: &iam::Request<'_>,
) -> Option<String> {
    let refusal = |reason: &str| {
        Some(iam::not_authorized_by_identity_policy(
            &caller.arn,
            action,
            resource,
            reason,
        ))
    };
    let stated = iam::identity_decision(&caller.documents, request);
    // Taken FIRST: an explicit `Deny` in a document that CAN be read refuses the request
    // whatever else the principal carries — including a grant this emulator cannot read, which
    // only lifts the DEFAULT deny of a request no readable statement allows.
    if stated == Decision::Denied {
        return refusal(iam::IDENTITY_POLICY_EXPLICIT_DENY);
    }
    if !(iam::is_allowed(stated, Decision::NotStated) || caller.identity_is_unbounded()) {
        return refusal(&iam::no_identity_policy_allows(action));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has. The account's own credential carries none.
    let boundary = caller.boundary.as_ref()?;
    let reason = match iam::identity_decision(::std::slice::from_ref(boundary), request) {
        Decision::Allowed => return None,
        // An explicit deny is a fact about the boundary document, so the refusal names it; a
        // boundary that merely states nothing withheld the grant the identity-based policies
        // gave.
        Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
        Decision::NotStated => iam::no_permissions_boundary_allows(action),
    };
    refusal(&reason)
}
