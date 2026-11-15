// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLER's own identity-based IAM policies let a request perform the action
//! it asks for — the decision every operation of this service takes before it reads or
//! writes anything, and before the account-level just-in-time node access gate.
//!
//! Every operation of this service is a signed call (`sigv4`, with no `@optionalAuth`
//! operation anywhere in the model — `smithy_query(query="service_info")`), and AWS
//! decides such a call against the caller's identity-based policies ahead of the service:
//! a principal holding no grant is told it is not authorized rather than told anything
//! about the account's own state — whether the recording preferences singleton exists,
//! and what it holds, are both answers this refusal withholds. All three operations
//! declare the modelled `AccessDeniedException` (HTTP 403, `message` required), so each of
//! them carries this refusal.
//!
//! It also keeps the two denial CAUSES apart, which is the only reason the wording
//! matters: a missing IAM grant (fix the policy) and the account-level just-in-time node
//! access gate ([`super::JUST_IN_TIME_NODE_ACCESS_DISABLED`], enroll the account) are
//! different remediations, so a caller branching on the message of one 403 can tell which
//! it got.
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
//!    accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutUserPolicy` wrote takes effect on the next call and deleting it lifts it
//!    again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the one shared
//!    evaluation of those documents against `(action, resource)`, so this service and
//!    every other one handed the same documents answer alike.
//!
//! Only the store LOOKUPS live here; neither the collection nor the decision does. The
//! lookups stay in this service crate because the IAM service crate depends on
//! `service-common`, so the shared crate cannot read IAM's store without a dependency
//! cycle. WHICH IAM entity a principal ARN names carries no such dependency, so it is read
//! through the shared [`::service_common::iam_role`] rather than parsed again here.
//!
//! ## The action and the resource
//!
//! The action is `ssm-guiconnect:<OperationName>` ([`action_for`]): this service's actions
//! are published under the namespace it signs and is addressed by (`aws.api#service`
//! `arnNamespace` and `aws.auth#sigv4` `name`, both `ssm-guiconnect` —
//! `smithy_query(query="service_info")`).
//!
//! The resource an IAM statement names for one of these calls is the EC2 instance space
//! the recorded connections happen in ([`recorded_instances_resource`]): this API
//! configures the recording of RDP connections to the account's instances and addresses no
//! resource of its own — the model binds no resource shape and no operation of it takes a
//! resource identifier — so the decision is taken against every instance of the account
//! and Region the request was signed for, which is the resource a policy for this API
//! scopes a grant with.
//!
//! ## The two callers the decision cannot refuse
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to [`sts_store::ambient_identity`], which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to the account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An
//!   explicit `Deny` in a document that CAN be read still refuses, and a permissions
//!   boundary the store DOES hold still caps the grant.
//!
//! An expired session credential speaks for nobody: it carries no document, so every action
//! it asks for is refused, and the refusal names that state rather than the key it
//! presented, which this module never quotes back.
//!
//! ## What one decision costs, and what it reads
//!
//! The work is bounded by what IAM holds for ONE principal and does not grow with the
//! request. Every value read comes from the REQUEST (the access key id in its credential
//! scope, and the Region it was signed for) and from the two in-process stores: nothing
//! here reads the host's environment or filesystem, opens a socket, or spawns anything. The
//! decision is taken with NO store guard of this service held and writes nothing to either
//! store it reads, and every lookup answers an `Option`, so a value no store holds leaves
//! the caller unauthorized instead of panicking.
//!
//! Resolving the caller goes through STS's own `resolve_caller`, which materialises the
//! session expiries the clock has already made due before it answers: that write is owed at
//! the same instant whether or not anybody called, which is why a REPORTING operation of
//! this service may take this decision too.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// This service's IAM action prefix, which is also its SigV4 signing name and its ARN
/// namespace: a policy statement spells one of its actions
/// `ssm-guiconnect:<OperationName>`.
pub const ACTION_PREFIX: &str = "ssm-guiconnect";

/// The service namespace of the resource these actions are performed on: the recorded
/// connections are made to Amazon EC2 instances, which is the resource space an IAM
/// statement for this API names.
const INSTANCE_SERVICE: &str = "ec2";

/// The resource portion of [`recorded_instances_resource`]: every instance of the account,
/// spelled with IAM's own wildcard, because no operation of this service addresses one
/// instance.
const ANY_INSTANCE: &str = "instance/*";

/// The region every ARN this gate decides against is scoped to when the request carries no
/// SigV4 credential scope to read one from. It is the region the emulator's own shared AWS
/// config names, so an unsigned call and a default-configured client agree.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The IAM action `operation` is performed under: `ssm-guiconnect:<OperationName>`.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The region this request was signed for, defaulting to [`DEFAULT_REGION`].
///
/// The only region a request carries is the one in its SigV4 credential scope, so that is
/// what the region-scoped resource ARN below is built from.
#[must_use]
pub fn request_region(parts: &::http::request::Parts) -> String {
    ::emulator_server::sigv4_credential_scope(&parts.headers)
        .map_or_else(|| DEFAULT_REGION.to_string(), |scope| scope.region)
}

/// The resource an authorization decision for this service is taken against: every EC2
/// instance of this account in `region`, through the account contract's own ARN builder so
/// the partition and account segments are the ones every other ARN this emulator emits
/// carry.
#[must_use]
pub fn recorded_instances_resource(region: &str) -> String {
    ::emulator_server::emulator_arn(INSTANCE_SERVICE, region, ANY_INSTANCE)
}

/// The refusal a request to `operation` gets when the calling principal's IAM policies do
/// not allow it, and `None` when they do.
///
/// IAM's order: an explicit `Deny` in a document that CAN be read refuses the request
/// whatever else the principal carries; otherwise a grant is needed, which a caller this
/// gate does not bound has by definition (see the module docs); and a grant is capped by
/// the permissions boundary, since "a permissions boundary sets the maximum permissions
/// that an identity-based policy can grant" (IAM, permissions boundaries).
///
/// Asked with NO store guard of this service held, and before anything of this service's
/// own state is read.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
) -> Option<String> {
    let action = action_for(operation);
    let resource = recorded_instances_resource(&request_region(ctx.parts()));
    let caller = caller(engine, ctx);
    // An identity-based document states no `Principal` element, so the decision rests on
    // the action and the resource the request is for.
    let request = iam::Request::new(&action, &resource, &[]);
    let said = iam::identity_decision(&caller.documents, &request);
    if said == Decision::Denied {
        return Some(not_authorized(
            &caller.arn,
            &action,
            &resource,
            "with an explicit deny in an identity-based policy",
        ));
    }
    if !(iam::is_allowed(said, Decision::NotStated) || caller.identity_is_unbounded()) {
        return Some(not_authorized(
            &caller.arn,
            &action,
            &resource,
            &format!("because no identity-based policy allows the {action} action"),
        ));
    }
    let boundary = caller.boundary.as_ref()?;
    let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
        Decision::Allowed => return None,
        // An explicit deny is a fact about the boundary document, so the refusal names it;
        // a boundary that merely states nothing withheld the grant the policies gave.
        Decision::Denied => "with an explicit deny in a permissions boundary".to_string(),
        Decision::NotStated => {
            format!("because no permissions boundary allows the {action} action")
        }
    };
    Some(not_authorized(&caller.arn, &action, &resource, &reason))
}

/// The sentence AWS refuses an unauthorized call with: who was refused, which action, on
/// which resource, and which rule withheld the grant.
///
/// Every value in it comes from the request (the action its operation names, and the
/// Region its credential scope carries) and from the stored policies (the principal, and
/// the rule that decided) — nothing is keyed to a particular caller or call.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
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
    /// OWN accessors: the documents in force on it — its inline policies, the default
    /// version of each managed policy attached to it and, for a user, the policies in force
    /// on the groups it belongs to — whether it carries an attachment whose document this
    /// emulator cannot read, and the permissions boundary that caps it.
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
    // action it asks for is refused — and the refusal names the credential's STATE rather
    // than the key id that was presented, which this module never quotes back.
    let identity = match identity {
        Ok(identity) => identity,
        Err(_) => {
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                ..Caller::default()
            }
        }
    };
    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
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
        }
    }
    // Which IAM identity the principal's policies are held under is read through the tree's
    // shared readings of a principal ARN.
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed
        // by the ARN the account holds it under — which carries its PATH, where the session
        // ARN does not.
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
    use super::{action_for, iam, recorded_instances_resource, Decision, ACTION_PREFIX};

    /// What one stored document says about `action` on `resource`, through the shared
    /// evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// RULE: the action asked about carries THIS service's namespace, so a document written
    /// over that namespace covers its operations and one written over another service's
    /// covers none of them.
    #[test]
    fn the_action_asked_about_carries_this_services_namespace() {
        let resource = recorded_instances_resource("us-east-1");
        let action = action_for("UpdateConnectionRecordingPreferences");
        assert_eq!(
            action, "ssm-guiconnect:UpdateConnectionRecordingPreferences",
            "case \"the action spelling\": expected the {ACTION_PREFIX} namespace, got {action:?}"
        );
        // (case, the document in force, what it says about this action)
        let rows: &[(&str, &str, Decision)] = &[
            (
                "every action of this namespace",
                r#"{"Statement":[{"Effect":"Allow","Action":"ssm-guiconnect:*","Resource":"*"}]}"#,
                Decision::Allowed,
            ),
            (
                "exactly this action",
                r#"{"Statement":[{"Effect":"Allow",
                 "Action":"ssm-guiconnect:UpdateConnectionRecordingPreferences",
                 "Resource":"*"}]}"#,
                Decision::Allowed,
            ),
            (
                "another operation of this namespace",
                r#"{"Statement":[{"Effect":"Allow",
                 "Action":"ssm-guiconnect:GetConnectionRecordingPreferences","Resource":"*"}]}"#,
                Decision::NotStated,
            ),
            (
                "another service entirely",
                r#"{"Statement":[{"Effect":"Allow","Action":"ssm:*","Resource":"*"}]}"#,
                Decision::NotStated,
            ),
            (
                "an explicit deny of this action beside an allow of the namespace",
                r#"{"Statement":[
                 {"Effect":"Allow","Action":"ssm-guiconnect:*","Resource":"*"},
                 {"Effect":"Deny","Action":"ssm-guiconnect:UpdateConnectionRecordingPreferences",
                  "Resource":"*"}]}"#,
                Decision::Denied,
            ),
        ];
        for (case, document, expected) in rows {
            let reached = decision(document, &action, &resource);
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected {expected:?} for {action}, got {reached:?}"
            );
        }
    }

    /// RULE: the decision is taken against the account's EC2 instance space in the REGION
    /// the request was signed for, so a statement scoped to one Region's instances covers
    /// that Region and no other.
    #[test]
    fn a_statement_is_read_for_the_instance_space_of_the_requests_region() {
        let addressed = recorded_instances_resource("us-east-1");
        assert!(
            addressed.ends_with(":instance/*") && addressed.contains(":ec2:us-east-1:"),
            "case \"the resource spelling\": expected the Region's EC2 instance space, got \
             {addressed:?}"
        );
        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Deny",
             "Action":"ssm-guiconnect:*","Resource":"{addressed}"}}]}}"#
        );
        // (case, the resource the decision is taken against, whether the deny covers it)
        let rows: &[(&str, String, bool)] = &[
            ("the Region the request named", addressed.clone(), true),
            (
                "another Region's instances",
                recorded_instances_resource("eu-west-3"),
                false,
            ),
            (
                "one instance of another Region",
                recorded_instances_resource("ap-south-1").replace("instance/*", "instance/i-1"),
                false,
            ),
        ];
        for (case, resource, covered) in rows {
            let said = decision(&document, &action_for("DeleteConnectionRecordingPreferences"), resource);
            let denied = said == Decision::Denied;
            assert_eq!(
                denied, *covered,
                "case {case:?}: expected the deny to {}cover {resource}, got {said:?}",
                if *covered { "" } else { "not " }
            );
        }
    }
}
