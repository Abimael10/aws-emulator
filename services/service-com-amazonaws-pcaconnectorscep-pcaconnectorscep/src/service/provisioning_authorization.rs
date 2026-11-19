// SPDX-License-Identifier: CC0-1.0

//! Whether the principal that created a connector may have that connector's own certificate
//! ISSUED from the private CA it links.
//!
//! Connector for SCEP issues the connector's internal certificate from the private CA with
//! the permissions of the principal that created the connector, so whether that issuance is
//! allowed is a question about the CALLER rather than about the CA: a connector created by a
//! principal a statement in force on it DENIES `acm-pca:IssueCertificate` on that CA never
//! comes up, and the outcome is reported on the connector
//! (`ConnectorStatusReason::PRIVATECA_ACCESS_DENIED`) because the creation is asynchronous.
//!
//! Three pieces, each read where it is held:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for (`sts_store::resolve_caller`, the same place `sts:GetCallerIdentity`
//!    answers from), else the IAM user that owns the long-term key, else the account's own
//!    published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal, the permissions boundary that caps them, and the session policies the
//!    credential itself was minted with, read through the IAM service's OWN accessors
//!    (`principal_policies`, `permissions_boundary_document`, `session_scoping_documents`), so
//!    a policy `iam:PutUserPolicy` wrote takes effect on the next create and deleting it lifts
//!    it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike.
//!
//! Only the store LOOKUPS live here; neither the decision nor the ARN readings do. The same
//! three-part reading the services that authorize a request take
//! (`service-com-amazonaws-arcregionswitch-arcregionswitch`'s `service::authorization` and
//! `service-com-amazonaws-ssm-amazonssm`'s `state::identity_authorization`), which is why the
//! lookups below are the only part spelled here.
//!
//! ## The callers nothing is withheld from
//!
//! * **A principal no readable statement DENIES the issuance to**, whose credential no mint
//!   scoped away from it: one whose policy set allows it, and one that merely says nothing
//!   about it. The account's own published credential is in the second position by
//!   construction — a request whose access key id neither STS nor IAM holds a record for
//!   resolves to the account's own identity, the position AWS account root credentials are in,
//!   which the IAM store holds no policy for — and so is an identity whose client attached no
//!   policy, since a policy record exists in this emulator's IAM only where a client created
//!   it there. Failing a connector on a permission this emulator holds no record of would fail
//!   one AWS would have brought up.
//! * **A principal holding an attachment this emulator cannot read** (an AWS-managed policy,
//!   whose document belongs to AWS rather than to this account): there is no statement to read
//!   in it either way. The same goes for a managed SESSION policy whose document it does not
//!   hold.
//! * **An engine carrying no STS or IAM store**, where no session and no policy can exist.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The Private CA action a connector performs with its creator's permissions: issuing the
/// certificate the connector itself presents. `IssueCertificate` is that service's own
/// operation name under its ARN namespace (`acm-pca`), which is how an IAM policy names it.
pub const ISSUE_CERTIFICATE_ACTION: &str = "acm-pca:IssueCertificate";

/// Whether the permissions in force on the principal that made this request do not let it
/// have a certificate issued from `certificate_authority_arn`.
///
/// `true` — the connector's creation cannot come up — for the three answers that withhold the
/// issuance:
///
/// * an explicit `Deny` about this very issuance, in the identity's documents, which no grant
///   overturns;
/// * a SESSION policy the credential was minted with, or a permissions BOUNDARY in force on
///   the principal, that does not allow it — each CAPS what the identity's documents grant and
///   grants nothing of its own ("a permissions boundary sets the maximum permissions that an
///   identity-based policy can grant"; "the resulting session's permissions are the
///   intersection of the role's identity-based policy and the session policies"), so each is
///   read on its own rather than beside the documents it caps; and
/// * a principal that STATES permissions none of which could allow `acm-pca:IssueCertificate`
///   at all — on any CA, under any condition — so no statement it carries is about such a
///   request and every one of them falls to IAM's default deny
///   (`::service_common::iam_authorization::identity_may_allow_action`, the weaker question a
///   gate in front of a resource-scoped action asks, so a grant this evaluator cannot decide
///   the resource or the condition of still counts as one).
///
/// `false` — withhold nothing — for everything else: a policy set that could allow the
/// issuance, a principal the IAM store holds NO document for (the routine state of an account
/// whose client attached none, since a policy record exists in this emulator's IAM only where
/// a client created it there), a grant this emulator cannot read, and every caller the module
/// docs name. A connector AWS would have brought up must not be failed because this
/// emulator's IAM holds no record of a permission.
pub fn issuance_is_unauthorized<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    certificate_authority_arn: &str,
) -> bool {
    let caller = caller(engine, ctx);
    // The decision rests on the action, the certificate authority it is taken on, the
    // principal spellings — and the CLASS of the credential presented, which is what a
    // statement conditioned on `aws:PrincipalType` selects on. That key is answered, and
    // declared answered, for every request whose credential this gate resolved, so such a
    // clause decides rather than standing undecided.
    let context = ::service_common::temporary_credential_classes::principal_type_conditions(
        caller.principal_type,
    );
    let published =
        [::service_common::temporary_credential_classes::PRINCIPAL_TYPE_CONDITION_KEY];
    let request = iam::Request::new(
        ISSUE_CERTIFICATE_ACTION,
        certificate_authority_arn,
        &caller.principals,
    )
    .with_context(&context)
    .with_published_keys(&published);
    // What the identity's OWN documents say about this issuance.
    if iam::identity_decision(&caller.documents, &request) == Decision::Denied {
        return true;
    }
    // What the SCOPE the credential itself was minted with says. A credential no mint scoped
    // is capped by nothing, which that reading answers as allowed
    // (`iam_store::ScopingPolicies::decision`, the one reading of a resolved scope).
    if caller.scope.decision(&request) != Decision::Allowed {
        return true;
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a ceiling CAPS what those documents grant and
    // grants nothing of its own, so it is read on its own and an issuance it does not allow is
    // withheld even where the identity's documents could allow it.
    if let Some(boundary) = caller.boundary.as_ref() {
        let capped = iam::PreparedDocument::read(boundary);
        if iam::identity_decision(::std::slice::from_ref(boundary), &request) == Decision::Denied
            || !iam::identity_may_allow_action(
                ::std::slice::from_ref(&capped),
                ISSUE_CERTIFICATE_ACTION,
            )
        {
            return true;
        }
    }
    // A principal the store holds no readable document for states nothing about what it may
    // do, so there is nothing to withhold on.
    if caller.documents.is_empty() || caller.unreadable_attachment {
        return false;
    }
    let prepared: Vec<iam::PreparedDocument> = caller
        .documents
        .iter()
        .map(|document| iam::PreparedDocument::read(document))
        .collect();
    !iam::identity_may_allow_action(&prepared, ISSUE_CERTIFICATE_ACTION)
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of the IAM
    /// role behind an assumed-role session.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose document
    /// this emulator holds.
    boundary: Option<String>,
    /// Set when this request presents the account's own published credential, for which the
    /// IAM store holds no policy record of its own (see the module docs).
    account_credential: bool,
    /// Set when the principal has a policy attached whose document the IAM store does not
    /// hold — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
    /// What the CREDENTIAL itself was minted with: the session policies that CAP what the
    /// identity's documents grant, resolved at the instant of the request. A credential no
    /// mint scoped is capped by nothing.
    scope: iam_store::ScopingPolicies,
    /// The CLASS of the credential the request was signed with, as STS reports it for that
    /// credential (`sts_store::Caller::principal_type`): the value the global condition key
    /// naming the calling principal's type answers with. `None` for a request this gate
    /// resolved no credential for, which carries no value for the key at all.
    principal_type: Option<&'static str>,
}

impl Caller {
    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's OWN
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
///
/// Resolving the caller goes through STS's own `resolve_caller`, which materialises the
/// session expiries the clock has already made due before it answers: that write is owed at
/// the same instant whether or not anybody called.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    let identity = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_request_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
            sts_store::ambient_identity(),
        )
        .map(|caller| (caller.principal_type(), caller.identity)),
        // No STS store on this engine: the only identity a request can present is the
        // account's own published credential.
        None => Ok((
            ::service_common::temporary_credential_classes::PRINCIPAL_TYPE_USER,
            sts_store::ambient_identity(),
        )),
    };
    // An expired session credential speaks for nobody: it carries no document, so nothing it
    // asks for is allowed — and in the assembled emulator the layer that authenticates a
    // request has already refused it before any handler runs.
    let Ok((principal_type, identity)) = identity else {
        return Caller::default();
    };

    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        principals: vec![identity.arn.clone()],
        // WHICH CLASS of credential signed this request, read through STS's one answer for it
        // (`sts_store::Caller::principal_type`).
        principal_type: Some(principal_type),
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // A LONG-TERM access key: STS's store holds SESSIONS, so its answer for a key it never
    // minted is the account's ambient identity — but IAM's store maps an access key id to the
    // user that owns it (`access_keys`, the map IAM's own `GetUser` resolves a caller
    // through), so a request signed with one speaks for THAT user and is judged on the user's
    // policies. A key the IAM store does not hold either really is the account's own
    // published credential.
    let mut principal = identity.arn;
    if caller.account_credential {
        if let Some(user) = store
            .access_keys
            .get(&access_key_id)
            .and_then(|record| store.users.get(&record.user_key))
        {
            caller.account_credential = false;
            principal = user.arn.clone();
            caller.principals = vec![user.arn.clone()];
        }
    }
    // Which IAM identity the principal's policies are held under is read through the tree's
    // shared readings of a principal ARN.
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&principal)
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
    } else {
        // An IAM user's permissions are its own, under the ARN it was resolved by — the
        // account's own published credential included, since a client may attach a policy to
        // the identity that credential speaks for.
        caller.read_policies_of(&store, &principal);
    }
    // What the MINT scoped this credential to, which caps whatever the identity above may do.
    // Resolved at the instant of the request, so a managed session policy edited since the
    // mint caps by what it says now; a credential the account holds no session record for was
    // scoped by nothing.
    if let Some(session) = store.role_session(&access_key_id, iam_store::now()) {
        caller.scope = iam_store::session_scoping_documents(Some(&store), &session.scope);
    }
    caller
}
