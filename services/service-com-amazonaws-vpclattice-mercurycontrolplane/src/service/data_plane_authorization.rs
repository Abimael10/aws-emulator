// SPDX-License-Identifier: CC0-1.0

//! Whether the AUTH POLICY in force on a service lets the caller of its data plane invoke it.
//!
//! "Creates or updates the auth policy. The policy string in JSON must not contain newlines
//! or blank lines" (`PutAuthPolicy`), and the policy is in force only while the resource it
//! is attached to authenticates with IAM: "The auth policy is only active when the auth type
//! is set to AWS_IAM. … If the Auth type is NONE, then, any auth policy that you provide
//! remains inactive" (`GetAuthPolicy`'s `state` member). A policy that is ACTIVE is a
//! document about REQUESTS — it names the action a client performs on the service and the
//! principals that may perform it — so the one place it can decide anything is in front of
//! the data plane ([`super::data_plane`]), which is here.
//!
//! THE RULE: a request this plane serves is decided, before the listener's action is
//! performed, against the auth policies ACTIVE on the service it addresses and on the service
//! networks it is reached through, together with the identity-based policies in force on the
//! caller the emulator authenticated — IAM's own evaluation, where an explicit `Deny` in
//! either kind of document refuses whatever any `Allow` says, an `Allow` in either grants,
//! and a request no readable statement allows falls to IAM's default deny. A refused request
//! is answered HTTP 403 `AccessDeniedException` and NOTHING behind the listener runs: no
//! target is chosen for it and no function is invoked.
//!
//! The pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for
//!    ([`sts_store::resolve_caller`], the record `sts:GetCallerIdentity` answers from, so the
//!    principal a refusal blames is the one the caller reads back for the credential it
//!    signed with), else the IAM user that owns the long-term key, else the account's own
//!    published credential. It is read only for a request the EMULATOR AUTHENTICATED: which
//!    mounting of the plane serves a request decides that ([`super::data_plane::Admits`]), and
//!    a request nothing authenticated speaks for nobody rather than for whatever its headers
//!    claimed;
//! 2. **what that principal may do** — the identity-based documents the IAM store holds for
//!    it and the permissions boundary that caps them, read through the IAM service's OWN
//!    accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next request and deleting it lifts it
//!    again;
//! 3. **what the SERVICE allows** — the auth policies this service's own store holds for the
//!    resources the request travels through, which `PutAuthPolicy` wrote
//!    ([`LatticeState::active_auth_policies_reaching_service`]), so a replacement document is
//!    in force for every later request with nothing to migrate;
//! 4. **the decision** — [`::service_common::iam_authorization`], the tree's ONE evaluation
//!    of policy documents against `(action, resource)`, so this plane and every other gate
//!    handed the same documents answer alike.
//!
//! Only the store LOOKUPS live here; neither the decision nor the readings of a principal ARN
//! do. That is the split every other gate in this tree makes, and the lookups stay in this
//! crate because hoisting them into `service-common` would make every service that depends on
//! that crate link the IAM and STS service crates.
//!
//! ## What a request is judged against
//!
//! The ACTION is [`INVOKE_ACTION`]: the data plane's own action namespace and the one
//! operation a client performs on a Lattice service — which is also what a request to that
//! plane is signed for, and what the statements of an auth policy are written over.
//!
//! The RESOURCE is the service's ARN with the request's own path appended, which is what a
//! statement scoped to a part of a service (`<service ARN>/orders/*`) is matched against and
//! what the wildcard a statement about the whole service carries (`<service ARN>/*`) covers.
//! It is built from the request and the stored service alone, so the decision does not depend
//! on anything a target would have done.
//!
//! ## The callers this gate does not bound
//!
//! * **A service no ACTIVE auth policy reaches.** With no document in force there is nothing
//!   this plane was asked to enforce, and a request is served exactly as it was before any
//!   policy existed — which is also what an `authType` of `NONE` leaves, since a policy
//!   attached to such a resource is inactive.
//! * **The account's own published credential**, which is not a principal the IAM store holds
//!   policies for — the position AWS account root credentials are in. An explicit `Deny` that
//!   names the account still refuses it.
//! * **A principal holding an attachment this emulator cannot read** (an AWS-managed policy,
//!   whose document belongs to AWS): refusing on a grant this emulator cannot see would deny
//!   access AWS permits. An explicit `Deny` in a document that CAN be read still refuses.

use ::std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::OperationContext;

use super::data_plane::{Admits, Served};
use crate::state::{ComAmazonawsVpclatticeMercurycontrolplaneState as LatticeState, LatticeService};

/// The action namespace of the Lattice DATA plane: the name a request to a service's own
/// listener is signed for, and the namespace the actions an auth policy's statements name are
/// published under. It is not this control plane's own namespace (`crate::state::ARN_SERVICE`),
/// which is what a request to the modelled API is signed for.
pub(super) const DATA_PLANE_ACTION_NAMESPACE: &str = "vpc-lattice-svcs";

/// The one operation a client performs on a Lattice service's data plane: it INVOKES the
/// service.
const INVOKE_OPERATION: &str = "Invoke";

/// The fully qualified action a data-plane request performs
/// (`<namespace>:<operation>`), built through the tree's one qualification of an IAM action.
#[must_use]
pub(super) fn invoke_action() -> String {
    iam::iam_action(DATA_PLANE_ACTION_NAMESPACE, INVOKE_OPERATION)
}

/// The wire name of the refusal an authorization failure is answered with, and the status it
/// carries: the AWS COMMON `AccessDeniedException`, answered by the layer in front of the
/// listener rather than by any modelled operation of this service.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The media type the refusal's body is: the plane answers a client of a service's own
/// listener, which reads a SENTENCE rather than a modelled error document.
const PLAIN_TEXT: &str = "text/plain";

/// The reason clause of a request an explicit `Deny` in the AUTH POLICY refuses.
///
/// The deny is a fact about THAT document, so the sentence names it rather than the
/// principal's own — the shape every reason clause of this refusal family has
/// ([`iam::IDENTITY_POLICY_EXPLICIT_DENY`] and its siblings). SOURCE: recorded AWS behaviour
/// for a Lattice service whose auth policy denies the caller; the clause carries nothing of
/// the request, so one sentence states every such refusal.
const SERVICE_POLICY_EXPLICIT_DENY: &str = "with an explicit deny in a service-based policy";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// What a refusal names as the principal of a request NOTHING authenticated: it speaks for
/// nobody, which is the principal an anonymous client of an endpoint is.
const ANONYMOUS_PRINCIPAL: &str = "anonymous";

/// The reason clause for a request no ACTIVE auth policy allows, stated for a caller that
/// speaks for nobody: nothing authenticated it, so it holds no identity-based document and the
/// only element that could have named it is a statement's wildcard principal. The counterpart
/// of [`iam::no_identity_policy_allows`] for the document that WAS consulted, worded the way
/// every reason clause of this family is.
fn no_service_policy_allows(action: &str) -> String {
    format!("because no service-based policy allows the {action} action")
}

/// One refused request: the sentence the refusal is stated with, and nothing else — the status
/// and the error name are this module's constants.
#[derive(Debug, Clone)]
pub(super) struct Denial {
    /// What the caller is told: who was refused, which action, on which resource, and which
    /// rule refused it.
    pub(super) message: String,
}

/// What ONE request's authorization decision is made from, read under this service's own store
/// guard and decided with none held.
#[derive(Debug, Clone)]
pub(super) struct InForce {
    /// The resource the decision is about: the addressed service's ARN with the request's
    /// path appended.
    resource: String,
    /// The auth policy documents ACTIVE on the resources this request travels through, in the
    /// order they were read.
    documents: Vec<String>,
}

impl InForce {
    /// What the records say about a request to `service` at `request_path`.
    ///
    /// A pure READ of this service's store: it materialises nothing and writes nothing, so
    /// asking changes nothing a later caller sees.
    pub(super) fn of(store: &LatticeState, service: &LatticeService, request_path: &str) -> Self {
        Self {
            resource: format!("{}{request_path}", service.arn),
            documents: store.active_auth_policies_reaching_service(&service.id),
        }
    }

    /// Whether any document is in force at all — the one question that decides whether this
    /// plane was asked to enforce anything for the request (see the module docs).
    fn enforced(&self) -> bool {
        !self.documents.is_empty()
    }
}

/// Whether this request may invoke the service, deciding `Ok(())` for one the documents in
/// force permit and the refusal otherwise.
///
/// Call it with NO guard of this service's store held: the caller's own stores are read under
/// their own locks. Call it BEFORE the listener's action is performed, so a refused request
/// invokes nothing.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises the
/// session expiries the clock has already made due; that write was owed at the same instant
/// whether or not anybody called.
pub(super) fn decided<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    admits: Admits,
    in_force: &InForce,
) -> Result<(), Denial> {
    if !in_force.enforced() {
        return Ok(());
    }
    let action = invoke_action();
    let resource = in_force.resource.as_str();
    let caller = caller(engine, ctx, admits);
    let refusal = |reason: &str| {
        Err(Denial {
            message: format!(
                "{} {reason}",
                iam::not_authorized_to_perform(&caller.arn, &action, resource)
            ),
        })
    };
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element is matched against; an identity-based document states no such
    // element, so for those the decision rests on the action and the resource. The caller's
    // OWN spellings are what a GRANT is decided under, because a statement naming the account
    // delegates to that account's own policies rather than granting.
    let granting = iam::Request::new(&action, resource, &caller.principals);
    // Every spelling, the account's included: a statement that DENIES the account denies each
    // of its principals.
    let mut named = caller.principals.clone();
    named.extend(iam::account_principals(&caller.arn));
    let denying = iam::Request::new(&action, resource, &named);

    // An explicit `Deny` in an auth policy refuses whatever any grant says — the caller's own
    // identity-based `Allow` for this action among them, and the account's own credential,
    // which the rest of this gate does not bound.
    if in_force
        .documents
        .iter()
        .any(|document| iam::resource_decision(document, &denying) == Decision::Denied)
    {
        return refusal(SERVICE_POLICY_EXPLICIT_DENY);
    }
    let identity_said = iam::identity_decision(&caller.documents, &granting);
    // Taken before any grant is read, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if identity_said == Decision::Denied {
        return refusal(iam::IDENTITY_POLICY_EXPLICIT_DENY);
    }
    // A grant in an auth policy names this principal directly, and "a permissions boundary
    // does not limit the permissions granted to the entity by a resource-based policy that
    // names it as a principal" (IAM, permissions boundaries): such a grant stands on its own,
    // so a principal holding no identity-based policy at all is served.
    if in_force
        .documents
        .iter()
        .any(|document| iam::resource_decision(document, &granting) == Decision::Allowed)
    {
        return Ok(());
    }
    if !(iam::is_allowed(identity_said, Decision::NotStated) || caller.identity_is_unbounded()) {
        // Which document withheld the grant is what the clause names: for a caller nothing
        // authenticated there is no identity-based document to name, and the auth policy is
        // the only one that was consulted.
        let reason = if caller.speaks_for_nobody() {
            no_service_policy_allows(&action)
        } else {
            iam::no_identity_policy_allows(&action)
        };
        return refusal(&reason);
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has. The account's own credential carries none.
    if let Some(boundary) = &caller.boundary {
        let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &granting) {
            Decision::Allowed => return Ok(()),
            // An explicit deny is a fact about the boundary document, so the refusal names
            // it; a boundary that merely states nothing withheld the grant the identity-based
            // policies gave.
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => iam::no_permissions_boundary_allows(&action),
        };
        return refusal(&reason);
    }
    Ok(())
}

/// The wire answer to a request this gate refused: HTTP 403 naming the common
/// `AccessDeniedException` and the sentence that states why, which is the whole body a client
/// of a service's own listener is answered with.
#[must_use]
pub(super) fn refusal(denial: &Denial) -> Served {
    Served {
        status: ACCESS_DENIED_STATUS,
        headers: vec![("content-type".to_string(), PLAIN_TEXT.to_string())],
        body: format!("{ACCESS_DENIED_EXCEPTION}: {}", denial.message).into_bytes(),
    }
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
    /// A caller NOTHING authenticated, and an expired credential, which speak for nobody: no
    /// documents and no principal spelling, so only a statement naming everyone can reach
    /// them.
    fn nobody(arn: &str) -> Self {
        Self {
            arn: arn.to_string(),
            principals: Vec::new(),
            documents: Vec::new(),
            boundary: None,
            account_credential: false,
            unreadable_attachment: false,
        }
    }

    /// Whether this request speaks for NOBODY: nothing authenticated it, or the credential it
    /// presented has expired, so it carries no principal a statement can name.
    fn speaks_for_nobody(&self) -> bool {
        self.principals.is_empty()
    }

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
///
/// A request served by the mounting that declares NO auth scheme was authenticated by nothing
/// ([`Admits::Unsigned`]), so no credential is read off it: it speaks for nobody.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    admits: Admits,
) -> Caller {
    if admits == Admits::Unsigned {
        return Caller::nobody(ANONYMOUS_PRINCIPAL);
    }
    let access_key_id = ::service_common::request_access_key_id(ctx.parts());
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
        // The credential has expired, so it speaks for nobody and every action it asks for
        // falls to the default deny. The key id it presented is NOT quoted back — this module
        // echoes no value of the request — and in the assembled emulator the layer that
        // authenticates a request refuses an expired credential before this plane runs.
        Err(_expired) => return Caller::nobody(EXPIRED_CREDENTIAL_PRINCIPAL),
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
    // user that owns it, so a request signed with one speaks for THAT user and is judged on
    // the user's policies rather than served as the account.
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
            // A statement's `Principal` element names a ROLE by that same ARN, and such a
            // statement covers every session of the role ("the principals in the policy
            // include the role"), so the session is named by both spellings and both travel
            // with the request — which is what a resource-based grant is matched against.
            if !caller.principals.contains(&role_arn) {
                caller.principals.push(role_arn);
            }
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

    /// The service ARN the rows below decide against.
    const SERVICE_ARN: &str = "arn:aws:vpc-lattice:us-west-2:000000000000:service/svc-0123456789abcdef0";

    /// One auth policy document: `effect` for `principal` on `resource`.
    fn policy(effect: &str, principal: &str, resource: &str) -> String {
        format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Principal":{{"AWS":["{principal}"]}},"Action":"{}","Resource":"{resource}"}}]}}"#,
            invoke_action()
        )
    }

    /// RULE: a refused request is answered HTTP 403 whose whole body is the common error's
    /// name, the principal, the action, the resource and the clause naming the rule that
    /// refused it — every value taken from the request and from the stored documents.
    ///
    /// The rows span what the sentence generalises over: the kind of principal a refusal
    /// blames (an assumed-role session, an IAM user, a caller nothing authenticated), the
    /// resource the request addressed (the whole service, a nested path) and the rule that
    /// refused it (an explicit deny in an auth policy, one in an identity-based policy, the
    /// default deny).
    #[test]
    fn a_refusal_states_the_principal_the_action_the_resource_and_the_rule() {
        let action = invoke_action();
        let rows: Vec<(&str, &str, String, String)> = vec![
            (
                "an assumed-role session denied by the auth policy",
                "arn:aws:sts::000000000000:assumed-role/driver/probe",
                format!("{SERVICE_ARN}/probe/42"),
                SERVICE_POLICY_EXPLICIT_DENY.to_string(),
            ),
            (
                "an IAM user denied by its own policy",
                "arn:aws:iam::000000000000:user/operator",
                SERVICE_ARN.to_string(),
                iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string(),
            ),
            (
                "a caller nothing authenticated, allowed by nothing",
                ANONYMOUS_PRINCIPAL,
                format!("{SERVICE_ARN}/"),
                no_service_policy_allows(&action),
            ),
        ];
        for (case, principal, resource, reason) in rows {
            let denial = Denial {
                message: format!(
                    "{} {reason}",
                    iam::not_authorized_to_perform(principal, &action, &resource)
                ),
            };
            let served = refusal(&denial);
            let body = String::from_utf8(served.body.clone()).expect("the sentence is text");
            let expected = format!(
                "AccessDeniedException: User: {principal} is not authorized to perform: \
                 {action} on resource: {resource} {reason}"
            );
            assert_eq!(
                served.status, ACCESS_DENIED_STATUS,
                "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS}, got {}",
                served.status
            );
            assert_eq!(
                body, expected,
                "case {case:?}: expected the body {expected:?}, got {body:?}"
            );
        }
    }

    /// RULE: an explicit `Deny` about the request wins over every `Allow`, whichever document
    /// holds it and whichever order the statements are written in; a document about another
    /// principal, another action or another resource decides nothing.
    ///
    /// The rows span what the rule generalises over: the effect, whether the caller is named
    /// by the session ARN or by its role's, the resource element's wildcard, and the
    /// statements that are not about this request at all.
    #[test]
    fn an_explicit_deny_in_an_auth_policy_refuses_what_an_allow_grants() {
        let session = "arn:aws:sts::000000000000:assumed-role/driver/probe".to_string();
        let role = "arn:aws:iam::000000000000:role/driver".to_string();
        let principals = vec![session.clone(), role.clone()];
        let resource = format!("{SERVICE_ARN}/probe/42");
        let action = invoke_action();
        let request = iam::Request::new(&action, &resource, &principals);
        let rows: &[(&str, String, Decision)] = &[
            (
                "an allow naming the role on the whole service",
                policy("Allow", &role, &format!("{SERVICE_ARN}/*")),
                Decision::Allowed,
            ),
            (
                "a deny naming the role on the whole service",
                policy("Deny", &role, &format!("{SERVICE_ARN}/*")),
                Decision::Denied,
            ),
            (
                "a deny naming the session itself",
                policy("Deny", &session, &format!("{SERVICE_ARN}/*")),
                Decision::Denied,
            ),
            (
                "a deny on the exact path",
                policy("Deny", &role, &resource),
                Decision::Denied,
            ),
            (
                "a deny about another principal",
                policy(
                    "Deny",
                    "arn:aws:iam::000000000000:role/someone-else",
                    &format!("{SERVICE_ARN}/*"),
                ),
                Decision::NotStated,
            ),
            (
                "a deny about another resource",
                policy("Deny", &role, "arn:aws:vpc-lattice:us-west-2:000000000000:service/svc-ffffffffffffffff0/*"),
                Decision::NotStated,
            ),
        ];
        for (case, document, expected) in rows {
            let decided = iam::resource_decision(document, &request);
            assert_eq!(
                decided, *expected,
                "case {case:?}: expected {expected:?}, got {decided:?}"
            );
        }
    }
}
