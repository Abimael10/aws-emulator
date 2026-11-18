// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the decision
//! an operation takes before it resolves anything the request names.
//!
//! AWS decides a signed call at the IAM layer before the service evaluates the resource, so a
//! caller holding no grant is told it is not authorized rather than told whether the resource
//! it named exists, and a refused write changes nothing. The operations whose model declares
//! `AccessDeniedException` ("The requester doesn't have permissions to perform the requested
//! operation") are the ones that can carry that refusal, and each of them asks [`refusal`] —
//! or, for a call that addresses one parameter, [`parameter_refusal`] — first.
//!
//! A call another AWS service makes on a workload's behalf is decided by the same three
//! pieces, with the ROLE that service acts as in place of the signing principal
//! ([`parameter_refusal_as_role`]).
//!
//! The three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the SAME
//!    place `sts:GetCallerIdentity` answers from ([`sts_store::resolve_caller`]) for a
//!    session credential, and from IAM's own `access_keys` map (the map `iam:GetUser`
//!    resolves a caller through) for a long-term key, so the principal a refusal blames is
//!    the one the caller reads back for the credentials it signed with;
//! 2. **what it may do** — the identity-based policy documents in force on that principal
//!    and the permissions boundary that caps them, read through the IAM service's OWN
//!    accessors (`principal_policies`, `permissions_boundary_document`), where the
//!    collection rule itself lives, so this service sees exactly what IAM's own authorizer
//!    sees — and, for a call that addresses one parameter, the RESOURCE-BASED policy that
//!    parameter itself carries, read out of this service's own store
//!    ([`super::resource_policies`], what `PutResourcePolicy` wrote);
//! 3. **the decision** — [`::service_common::iam_authorization`], the one shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike — and the one place the sentence a refusal
//!    carries is worded.
//!
//! ## The two kinds of document, evaluated together
//!
//! The decision is taken over the union AWS evaluates together: the caller's identity-based
//! policies and the addressed resource's own resource-based policy ("the resource-based
//! policy and the identity-based policy are evaluated together, and access is granted if
//! either allows it"). So, in IAM's own order: an explicit `Deny` in EITHER refuses the
//! request whatever any grant says — including a caller this gate otherwise does not bound —
//! an `Allow` in either is a grant, and with neither the request falls to IAM's default deny.
//! A grant in the resource's own policy names the principal directly, so no permissions
//! boundary caps it ("a permissions boundary does not limit the permissions granted to the
//! entity by a resource-based policy that names it as a principal"), while a grant the
//! identity-based documents gave is capped as ever. A statement that names the ACCOUNT rather
//! than the caller delegates to that account's own identity-based policies rather than
//! granting, which is why the account spellings
//! ([`::service_common::iam_authorization::account_principals`]) decide a `Deny` and never
//! make a grant on their own.
//!
//! Only the store LOOKUPS live here; neither the collection nor the decision does. The
//! lookups stay in this service crate because the IAM service crate depends on
//! `service-common`, so the shared crate cannot read IAM's store without a dependency
//! cycle. WHICH IAM entity a principal ARN names carries no such dependency, so it is read
//! through the shared [`::service_common::iam_role`] rather than parsed again here.
//!
//! ## The two callers the IDENTITY-BASED decision cannot refuse
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to [`sts_store::ambient_identity`], the account's
//!   own identity, which is not a principal the IAM store holds policies for — the same
//!   position AWS account root credentials are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   is owned by AWS, not by the account, so the IAM store holds no document for it. A
//!   principal with such a policy attached is allowed: this service cannot see what that
//!   document grants, and refusing on evidence it does not have would deny access AWS
//!   permits.
//!
//! Neither is beyond an explicit `Deny`: a deny in a document this gate CAN read — the
//! principal's own, or the addressed resource's — refuses both, because such a deny is
//! evidence the gate holds rather than evidence it lacks.
//!
//! An expired session credential speaks for nobody: it carries no document, so every action it
//! asks for is refused, and the refusal names the credential that was presented rather than a
//! blank principal.
//!
//! ## What one decision costs, and what it reads
//!
//! The work is bounded by what IAM holds for ONE principal and does not grow with the
//! request: that principal's inline policies, the managed policies attached to it and —
//! for a user — the groups it belongs to, each collected once by IAM's own accessor and
//! read once by the shared evaluator, beside the policies attached to the ONE resource the
//! request addresses. The decision itself is taken with no guard held — the addressed
//! resource's policies are copied out under a guard this module takes and releases before it
//! decides — and it writes nothing to any store it reads. Every value it reads comes from the
//! request (the access key id in the credential scope, the name it addresses) and from those
//! in-process stores; every lookup answers an `Option`, so a value no store holds leaves the
//! caller unauthorized instead of panicking, and no refusal quotes a value back except the
//! principal's own ARN, the action and the resource the request addressed.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// Every resource of a request that names NO single one, spelled with IAM's own wildcard:
/// such a call addresses the account's resources rather than one record, so a statement
/// scoped to one resource cannot allow it while one covering the account's resources can.
pub const ANY_RESOURCE: &str = "*";

/// The ARN namespace Amazon EC2 mints an instance under, and the resource type an instance
/// carries in it: an EC2 instance is `arn:aws:ec2:<region>:<account>:instance/<id>`, which is
/// the resource "Resource types defined by AWS Systems Manager" names for the actions of this
/// service that address a managed node (the `instance` resource type).
const EC2_ARN_SERVICE: &str = "ec2";
const EC2_INSTANCE_RESOURCE_TYPE: &str = "instance";

/// The ARN of the EC2 instance `instance_id` names in `region` — the resource an
/// authorization decision about a call addressed to that node is made against.
///
/// Built from the id the REQUEST carried, before anything is looked up, so the decision does
/// not depend on whether that instance exists or is managed: a caller holding no grant must
/// not learn from the refusal what the account holds.
#[must_use]
pub fn instance_resource(region: &str, instance_id: &str) -> String {
    ::engine::emulator_arn(
        EC2_ARN_SERVICE,
        region,
        &format!("{EC2_INSTANCE_RESOURCE_TYPE}/{instance_id}"),
    )
}

/// The IAM action `operation` is performed under: `ssm:<OperationName>` ("Actions defined
/// by AWS Systems Manager", IAM Service Authorization Reference), whose prefix is this
/// service's own ARN namespace ([`super::ARN_SERVICE`]).
pub fn action_for(operation: &str) -> String {
    format!("{}:{operation}", super::ARN_SERVICE)
}

/// `Some(message)` when IAM does not allow `ssm:<operation>` on `resource` for the calling
/// principal — the sentence the refusal carries — and `None` when it does.
///
/// `resource` is the ARN the request addresses, computed before anything is looked up so
/// the decision does not depend on whether that resource exists: a caller holding no grant
/// must not learn from the refusal what the account holds. [`ANY_RESOURCE`] stands for a
/// call that names no single resource.
///
/// The question here is the IDENTITY-BASED half alone, which is the whole decision for a
/// resource that carries no resource-based policy of its own; a call addressing a resource
/// that CAN carry one asks [`parameter_refusal`], which hands that document to the same
/// decision.
///
/// Asked with NO store guard held, and before the operation reads or writes anything.
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Option<String> {
    decide(&caller(engine, ctx), operation, resource, &[])
}

/// `Some(message)` when IAM does not allow `ssm:<operation>` on the PARAMETER `name` for the
/// calling principal, and `None` when it does — the decision taken about the one resource a
/// Parameter Store call addresses, over BOTH kinds of document in force on it.
///
/// The resource is that parameter's own ARN ([`super::parameter_arn`]), computed from the name
/// the request carries and the Region it was signed for and before anything is looked up, so a
/// caller holding no grant is refused whether or not the store holds the parameter: a refusal
/// that depended on the record existing would tell a caller with no permissions what the
/// account holds. The resource-based policies that parameter carries take part in the decision
/// ([`attached_parameter_policies`]), so a document attached to the parameter restricts a read
/// of it the way AWS's combined evaluation does.
pub fn parameter_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    region: &str,
    name: &str,
) -> Option<String> {
    let attached = attached_parameter_policies(engine, region, name);
    decide(
        &caller(engine, ctx),
        operation,
        &super::parameter_arn(region, name),
        &attached,
    )
}

/// [`parameter_refusal`] for a call another AWS service performs as a ROLE of the account on
/// a workload's behalf, rather than for a caller that signed a request of its own
/// ([`super::parameter_retrieval`]).
///
/// The documents in force are the ROLE's, which is the whole difference: a grant that sits on
/// some other role decides nothing, and the refusal names the session that role was assumed
/// in ([`role_caller`]). Both kinds of document take part exactly as they do for a signed
/// request, so one statement covers a caller's own read of a parameter and a retrieval made
/// for it by another service.
///
/// Asked with NO store guard held: reading the role's documents takes IAM's lock.
pub fn parameter_refusal_as_role<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    session_arn: Option<&str>,
    operation: &str,
    region: &str,
    name: &str,
) -> Option<String> {
    let attached = attached_parameter_policies(engine, region, name);
    decide(
        &role_caller(engine, role_arn, session_arn),
        operation,
        &super::parameter_arn(region, name),
        &attached,
    )
}

/// The resource-based policies attached to the parameter `name` names at `region`'s
/// endpoint: the documents `PutResourcePolicy` stored for it, in attachment order, and none
/// for a parameter carrying none.
///
/// Keyed by the RESOURCE the policy is attached to ([`super::resource_policies`]), so a
/// policy attached by the parameter's ARN is found by a read that names the parameter in any
/// of its spellings. Copied out under a guard taken and released here, so the decision
/// itself is still taken with no store of this service held.
fn attached_parameter_policies<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    name: &str,
) -> Vec<String> {
    let Some(state) = super::service_state(engine) else {
        return Vec::new();
    };
    let held_under = super::resource_policies::PolicyResource {
        kind: super::resource_policies::PolicyResourceKind::Parameter,
        name: super::canonical_name(name),
        account: "",
        region: "",
    }
    .key(region);
    let store = state.lock();
    store
        .resource_policies
        .policies(&held_under)
        .iter()
        .map(|held| held.policy.clone())
        .collect()
}

/// Whether the documents in force let this request perform `ssm:<operation>` on `resource`,
/// where `resource_policies` are the resource-based policies the addressed resource itself
/// carries (empty for a resource that carries none).
///
/// IAM's order, over the union of the two kinds of document (see the module docs): an
/// explicit `Deny` in a document that CAN be read refuses the request whatever else is in
/// force; otherwise a grant is needed, which a grant in the resource's own policy supplies on
/// its own and which a caller this gate does not bound has by definition; and a grant the
/// identity-based documents gave is capped by the permissions boundary, since "a permissions
/// boundary sets the maximum permissions that an identity-based policy can grant" (IAM,
/// permissions boundaries).
fn decide(
    caller: &Caller,
    operation: &str,
    resource: &str,
    resource_policies: &[String],
) -> Option<String> {
    let action = action_for(operation);
    let refused = |reason: &str| Some(not_authorized(&caller.arn, &action, resource, reason));
    // A GRANT is read under the caller's own spellings — the session ARN and the role behind
    // it — because a statement naming the ACCOUNT delegates rather than grants. An
    // identity-based document states no `Principal` element at all, so for those documents
    // the decision rests on the action and the resource alone.
    let granting = iam::Request::new(&action, resource, &caller.principals);
    // A DENY is read under those spellings AND the account's: a statement that denies the
    // account denies each of its principals.
    let mut named = caller.principals.clone();
    named.extend(iam::account_principals(&caller.arn));
    let denying = iam::Request::new(&action, resource, &named);

    // An explicit `Deny` in the addressed resource's OWN policy refuses whatever any grant
    // says — including the callers the identity-based half of this gate does not bound.
    if resource_policies
        .iter()
        .any(|document| iam::resource_decision(document, &denying) == Decision::Denied)
    {
        return refused(iam::RESOURCE_POLICY_EXPLICIT_DENY);
    }
    let said = iam::identity_decision(&caller.documents, &granting);
    if said == Decision::Denied {
        return refused(iam::IDENTITY_POLICY_EXPLICIT_DENY);
    }
    // A grant in the resource's own policy names this principal directly: "a permissions
    // boundary does not limit the permissions granted to the entity by a resource-based policy
    // that names it as a principal" (IAM, permissions boundaries), so such a grant stands on
    // its own and a principal holding no identity-based policy at all is served.
    if resource_policies
        .iter()
        .any(|document| iam::resource_decision(document, &granting) == Decision::Allowed)
    {
        return None;
    }
    if !(iam::is_allowed(said, Decision::NotStated) || caller.identity_is_unbounded()) {
        return refused(&iam::no_identity_policy_allows(&action));
    }
    let boundary = caller.boundary.as_ref()?;
    let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &granting) {
        Decision::Allowed => return None,
        // An explicit deny is a fact about the boundary document, so the refusal names it;
        // a boundary that merely states nothing withheld the grant the policies gave.
        Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
        Decision::NotStated => iam::no_permissions_boundary_allows(&action),
    };
    refused(&reason)
}

/// The sentence IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which rule withheld the grant. Every value in it comes from the
/// request and the stored policies.
///
/// Worded by the tree's one statement of that sentence
/// ([`::service_common::iam_authorization::not_authorized_to_perform`]) with the clause of
/// the rule that withheld the grant after it, so this gate and every other one that reaches
/// the same decision word it alike.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "{} {reason}",
        iam::not_authorized_to_perform(principal, action, resource)
    )
}

/// The ARN of the principal this request speaks for, as `sts:GetCallerIdentity` reports it —
/// what a write of this service records as the caller that made it, and the same reading a
/// refusal blames.
///
/// A credential neither store holds a record for speaks for the account itself, which is the
/// identity this emulator publishes its own credential as
/// ([`::service_common::caller_principal::ambient_user_arn`]), and so is the fallback for a
/// credential nothing resolves a spelling for at all. A credential that speaks for nobody —
/// an expired session — carries a spelling of its own but never reaches a write: [`refusal`]
/// has already refused it, because it carries no document that allows anything.
pub fn caller_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    let arn = caller(engine, ctx).arn;
    if arn.is_empty() {
        ::service_common::caller_principal::ambient_user_arn()
    } else {
        arn
    }
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// Every spelling of the principal a resource-based policy may name: the session ARN STS
    /// reports, and the ARN of the IAM role behind an assumed-role session. Empty for a
    /// credential that speaks for nobody, which no statement names.
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
            // An attachment the collection resolved no document for is an entry of AWS's
            // own catalog this emulator does not carry: a grant it cannot read.
            self.unreadable_attachment = !policies.uncarried.is_empty();
        }
        self.boundary = store.permissions_boundary_document(identity_arn);
    }
}

/// The principal this request speaks for, with the identity-based policies in force on it.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which materialises the
/// session expiries the clock has already made due before it answers: that write is owed at
/// the same instant whether or not anybody called, which is why a REPORTING operation of
/// this service may take this decision too.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::emulator_server::extract_akid(&ctx.parts().headers);
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
    // An expired session credential speaks for nobody: it carries no document, so every
    // action it asks for is refused — and the refusal still names what was presented rather
    // than a blank principal.
    let identity = match identity {
        Ok(identity) => identity,
        Err(expired) => {
            return Caller {
                arn: format!("an expired session credential ({})", expired.access_key_id),
                ..Caller::default()
            }
        }
    };
    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        // The spelling STS reports for the credential presented is the first spelling a
        // statement can name the caller by; the role behind a session is added below.
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        ..Caller::default()
    };
    let Some(iam) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam.lock();
    // A LONG-TERM access key: STS's store holds SESSIONS, so its answer for a key it never
    // minted is the account's ambient identity — but IAM's store maps an access key id to
    // the user that owns it (the map `iam:GetUser` resolves a caller through), so a request
    // signed with one speaks for THAT user and is judged on the user's policies. A key the
    // IAM store does not hold either really is the account's own published credential.
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
            // The ARN the ACCOUNT holds the role under carries its path, which is the
            // spelling a statement names the role by.
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

/// The principal a call another AWS service makes AS a role of this account speaks for, with
/// the identity-based policies in force on that role.
///
/// Such a caller holds exactly what a session of the role holds: the role's ARN is the
/// spelling a resource-based policy names it by, the role's own documents are what the
/// decision is taken over, and the refusal names the SESSION the retrieving service assumed
/// it in when it has one. It presents no credential of the account's own.
///
/// A role this account's IAM holds no record for may belong to another account, so its
/// documents are not this store's to read: nothing can be shown about what it may do, so such
/// a caller is left unbounded rather than refused on evidence this emulator does not have —
/// the same rule a principal carrying a document the store cannot read is read by (see the
/// module docs). An explicit `Deny` in a document that CAN be read still refuses it.
fn role_caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    session_arn: Option<&str>,
) -> Caller {
    let mut caller = Caller {
        arn: session_arn.unwrap_or(role_arn).to_string(),
        principals: vec![role_arn.to_string()],
        ..Caller::default()
    };
    if let Some(session_arn) = session_arn {
        caller.principals.push(session_arn.to_string());
    }
    let Some(iam) = iam_store::service_state(engine) else {
        caller.unreadable_attachment = true;
        return caller;
    };
    let store = iam.lock();
    if store.roles.values().any(|role| role.arn == role_arn) {
        caller.read_policies_of(&store, role_arn);
    } else {
        caller.unreadable_attachment = true;
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

    /// The ARN of the access request `id` names in `region`, the resource the just-in-time
    /// node access decisions below are made about.
    fn access_request(region: &str, id: &str) -> String {
        super::super::just_in_time_node_access::access_request_arn(region, id)
    }

    /// RULE: the action this gate asks about is `ssm:<OperationName>`, so a document scoped
    /// to this service's actions decides every operation of it and nothing outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document = r#"{"Statement":[{"Effect":"Deny","Action":"ssm:*","Resource":"*"}]}"#;
        for operation in ["GetAccessToken", "StartAccessRequest", "GetParameter"] {
            let action = action_for(operation);
            let reached = decision(document, &action, ANY_RESOURCE);
            assert_eq!(
                reached,
                Decision::Denied,
                "case {operation:?}: expected ssm:* to deny {action}, got {reached:?}"
            );
        }
        let outside = decision(document, "ec2:DescribeInstances", ANY_RESOURCE);
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected ssm:* to state nothing outside this service, \
             got {outside:?}"
        );
    }

    /// RULE: a grant is read for the action AND the resource it names, so a statement that
    /// names neither this action nor this resource allows nothing.
    #[test]
    fn a_grant_is_read_for_the_action_and_the_resource_it_names() {
        let granted = access_request("us-west-2", "oi-0123456789ab");
        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
            "Action":"ssm:GetAccessToken","Resource":"{granted}"}}]}}"#
        );
        // (case, operation, resource, allowed)
        let rows: &[(&str, &str, String, bool)] = &[
            (
                "the granted action on the granted access request",
                "GetAccessToken",
                granted.clone(),
                true,
            ),
            (
                "another action on the same access request",
                "StartAccessRequest",
                granted.clone(),
                false,
            ),
            (
                "the granted action on another access request",
                "GetAccessToken",
                access_request("us-west-2", "oi-ba9876543210"),
                false,
            ),
            (
                "the granted action on the same id in another Region",
                "GetAccessToken",
                access_request("eu-central-1", "oi-0123456789ab"),
                false,
            ),
            (
                "the granted action naming no single resource",
                "GetAccessToken",
                ANY_RESOURCE.to_string(),
                false,
            ),
        ];
        for (case, operation, resource, expected) in rows {
            let said = decision(&document, &action_for(operation), resource);
            let actual = iam::is_allowed(said, Decision::NotStated);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected allowed == {expected}, got {actual} ({said:?})"
            );
        }
    }

    /// RULE: an explicit `Deny` is final, whatever else the principal carries.
    #[test]
    fn an_explicit_deny_wins_over_a_wildcard_grant() {
        let document = r#"{"Version":"2012-10-17","Statement":[
            {"Effect":"Allow","Action":"ssm:*","Resource":"*"},
            {"Effect":"Deny","Action":"ssm:GetAccessToken","Resource":"*"}]}"#;
        let resource = access_request("us-west-2", "oi-0123456789ab");
        let denied = decision(document, &action_for("GetAccessToken"), &resource);
        assert_eq!(
            denied,
            Decision::Denied,
            "case \"the denied action\": expected Denied, got {denied:?}"
        );
        let allowed = decision(document, &action_for("StartAccessRequest"), &resource);
        assert_eq!(
            allowed,
            Decision::Allowed,
            "case \"an action the deny does not name\": expected Allowed, got {allowed:?}"
        );
    }

    /// RULE: the refusal names the principal the credential speaks for, the action asked
    /// for, the resource the request addressed, and the rule that withheld the grant.
    #[test]
    fn the_refusal_names_the_principal_the_action_the_resource_and_the_reason() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/ssm-noperm/session-1",
            ::engine::emulator_account_id()
        );
        let resource = access_request("us-west-2", "oi-0123456789ab");
        let action = action_for("GetAccessToken");
        // (case, the clause the message must carry)
        let rows: Vec<(&str, String)> = vec![
            (
                "no policy allows it",
                format!("because no identity-based policy allows the {action} action"),
            ),
            (
                "a policy denies it",
                "with an explicit deny in an identity-based policy".to_string(),
            ),
            (
                "the permissions boundary withholds it",
                format!("because no permissions boundary allows the {action} action"),
            ),
            (
                "the permissions boundary denies it",
                "with an explicit deny in a permissions boundary".to_string(),
            ),
        ];
        for (case, reason) in rows {
            let message = not_authorized(&principal, &action, &resource, &reason);
            let expected = format!(
                "User: {principal} is not authorized to perform: {action} on resource: \
                 {resource} {reason}"
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}
