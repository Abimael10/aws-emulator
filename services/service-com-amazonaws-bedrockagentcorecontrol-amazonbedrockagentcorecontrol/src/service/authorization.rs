// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLER's own identity-based IAM policies let a request to this control plane
//! perform the action it asks for — the decision taken BEFORE the request's members are
//! validated, which is why it lives in the service layer and not in a handler.
//!
//! A signed call is decided by IAM ahead of the service, so a principal whose policies do
//! not allow the action is told it is not authorized rather than told anything about the
//! request it sent: whether the resource it addressed exists, and whether the identifier it
//! named is even inside the member's modelled format, are both answers this refusal
//! withholds. That ORDER is the point of taking the decision here: the framework's
//! centralized walk validates the model's constraints before any handler runs, so a handler
//! cannot take a decision that precedes it, and a caller without the action's grant would
//! otherwise be answered `ValidationException` for a malformed identifier — a caller-bug
//! verdict — where AWS answers `AccessDeniedException`.
//!
//! The three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for, read from the SAME
//!    place `sts:GetCallerIdentity` answers from ([`sts_store::resolve_request_caller`]) for
//!    a session credential, and from IAM's own `access_keys` map (the map `iam:GetUser`
//!    resolves a caller through) for a long-term key, so the principal a refusal blames is
//!    the one the caller reads back for the credentials it signed with;
//! 2. **what it may do** — the identity-based policy documents in force on that principal
//!    and the permissions boundary that caps them, read through the IAM service's OWN
//!    accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutUserPolicy` wrote takes effect on the next call and deleting it lifts it
//!    again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the one shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike.
//!
//! ## The action and the resource
//!
//! The action is `bedrock-agentcore:<OperationName>` ([`action_for`]): this endpoint's
//! actions are published under the namespace it signs and is ARN-addressed by (`sigv4Name`
//! and `awsService.arnNamespace`, both `bedrock-agentcore` —
//! `smithy_query(query="service_info")`), while the host it answers on is the longer
//! `bedrock-agentcore-control`.
//!
//! The resource a statement names for one of these calls is the ARN of the resource the
//! request addresses, so the decision can only be taken where this layer can name that ARN
//! from the request line — before any input is deserialized. That is the AGENT RUNTIME
//! family: the nine routes whose modelled `@http` URI carries `{agentRuntimeId}`
//! (`OPERATIONS.md`), where the id is the first path segment under `/runtimes/` and the
//! runtime's ARN is `runtime/<agentRuntimeId>` of this account in the signed region
//! ([`crate::state::runtime_arn`], `AgentRuntimeArn`'s own pattern). The id is decided AS
//! CARRIED: no statement scoped to a runtime ARN covers an identifier no runtime of the
//! account answers to, while a statement over every resource still does — and the caller
//! learns nothing from the verdict about whether that runtime exists, since the decision is
//! taken before any handler resolves it. Every other request of this endpoint falls through
//! this gate untouched: a resource family whose ARN this layer cannot name from the request
//! line is not one it may guess at, since judging an action against the wrong resource
//! would refuse a call a scoped statement allows.
//!
//! ## The callers the decision cannot refuse
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to [`sts_store::ambient_identity`], which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed, which is also why the credential this emulator
//!   publishes at start keeps reaching every operation.
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
//! The decision is taken with no guard of this service's own store held, and it writes
//! nothing to either store it reads.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use smithy_server_runtime::{OperationContext, Refused};

/// The namespace a policy statement spells this endpoint's actions under, which is also its
/// SigV4 signing name and its ARN namespace: `bedrock-agentcore` for all three
/// (`smithy_query(query="service_info")` — `sigv4Name`, `awsService.arnNamespace`).
pub const ACTION_PREFIX: &str = "bedrock-agentcore";

/// The first path segment of every modelled route that addresses an agent runtime by
/// `{agentRuntimeId}` (`OPERATIONS.md`: `/runtimes/{agentRuntimeId}/`, the five
/// runtime-endpoint routes under it, and the version listing).
const RUNTIMES_PREFIX: &str = "runtimes";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The IAM action `operation` is performed under: `bedrock-agentcore:<OperationName>`.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The refusal a request to `operation` gets when the calling principal's IAM policies do
/// not allow it, and `None` when they do — and for every request whose resource this layer
/// cannot name from the request line (see the module docs).
///
/// IAM's order: an explicit `Deny` in a document that CAN be read refuses the request
/// whatever else the principal carries; otherwise a grant is needed, which a caller this
/// gate does not bound has by definition (see the module docs); and a grant is capped by the
/// permissions boundary, since "a permissions boundary sets the maximum permissions that an
/// identity-based policy can grant" (IAM, permissions boundaries).
///
/// Answered as a [`Refused`] — the code a client branches on and the sentence it shows — so
/// the status and the envelope stay with the protocol the request is served in. Asked with
/// no store guard of this service held, and before the request's members are validated.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
) -> Option<Refused> {
    let resource = addressed_resource(ctx)?;
    let caller = caller(engine, ctx);
    // Nothing this gate can read bears on the request: there is no document that could deny
    // it, no boundary that could cap it, and the credential is one the gate does not bound.
    if caller.documents.is_empty() && caller.boundary.is_none() && caller.identity_is_unbounded()
    {
        return None;
    }
    let action = action_for(operation);
    // An identity-based document states no `Principal` element, so the decision rests on
    // the action and the resource the request addressed.
    let request = iam::Request::new(&action, &resource, &[]);
    if allows(&caller, &request) {
        return None;
    }
    ::tracing::info!(
        operation = %operation,
        action = %action,
        principal = %caller.arn,
        "iam does not allow this caller to perform this action"
    );
    Some(Refused {
        code: crate::state::ACCESS_DENIED_SHAPE.to_string(),
        message: not_authorized(&caller.arn, &action),
    })
}

/// Whether IAM allows `request` for `caller`, in IAM's own evaluation order (see
/// [`refusal`]).
fn allows(caller: &Caller, request: &iam::Request<'_>) -> bool {
    let said = iam::identity_decision(&caller.documents, request);
    if said == Decision::Denied {
        return false;
    }
    if !(iam::is_allowed(said, Decision::NotStated) || caller.identity_is_unbounded()) {
        return false;
    }
    caller.boundary.as_ref().is_none_or(|boundary| {
        iam::identity_decision(::std::slice::from_ref(boundary), request) == Decision::Allowed
    })
}

/// The sentence this endpoint refuses an unauthorized call with: the principal that signed
/// and the action it asked for, as the request and the signing identity carry them.
///
/// It closes at the action. The tree's shared sentences
/// ([`iam::not_authorized_to_perform`] and the family beside it) all go on to name the
/// resource, and [`iam::not_authorized_by_identity_policy`] names WHICH of IAM's rules
/// withheld the grant as well; this endpoint's recorded refusal of a runtime read carries
/// neither clause, so the two fixed fragments here are all of its fixed text and the only
/// values in it come from the request and the signing identity.
fn not_authorized(principal: &str, action: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action}")
}

/// The ARN of the resource this request addresses, when this layer can name it from the
/// request line, and `None` for every other request (see the module docs).
///
/// The runtime family's routes are `/runtimes/{agentRuntimeId}/…`, so the id is the first
/// path segment under `/runtimes/`, percent-decoded with PATH semantics through the shared
/// reader that states that rule once for the whole tree — the same reading the codec gives
/// the label, so the gate and the operation see one id. `PUT /runtimes/` and
/// `POST /runtimes/` carry no such segment and address no single runtime, so they resolve to
/// `None` here.
fn addressed_resource(ctx: &OperationContext<'_>) -> Option<String> {
    let parts = ctx.parts();
    let uri = parts.uri.to_string();
    let mut segments = ::service_common::uri_path(&uri)
        .split('/')
        .filter(|segment| !segment.is_empty());
    if segments.next()? != RUNTIMES_PREFIX {
        return None;
    }
    let id = ::service_common::percent_decode_path_label(segments.next()?);
    (!id.is_empty()).then(|| crate::state::runtime_arn(&crate::state::request_region(parts), &id))
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
/// Resolving the caller goes through the token service's own resolution, which materialises
/// the session expiries the clock has already made due before it answers: that write is owed
/// at the same instant whether or not anybody called, which is why a reporting operation of
/// this service may take this decision too. Nothing here opens a socket: both stores live in
/// the one engine this service was constructed from.
fn caller<Eng: Send + Sync + 'static>(engine: &Eng, ctx: &OperationContext<'_>) -> Caller {
    let access_key_id = ::emulator_server::extract_akid(&ctx.parts().headers);
    let identity = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
        ),
        // No STS store on this engine: the only identity a request can present there is the
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

    /// RULE: the action this gate asks about carries this endpoint's own namespace, so a
    /// document written over that namespace covers these calls and one written over another
    /// service's covers none of them.
    #[test]
    fn the_action_asked_about_carries_the_agentcore_namespace() {
        let resource = crate::state::runtime_arn("us-east-1", "probeRuntime-abcdefghij");
        let action = action_for("GetAgentRuntime");
        assert_eq!(
            action, "bedrock-agentcore:GetAgentRuntime",
            "case \"the action spelling\": expected this endpoint's namespace, got {action:?}"
        );
        // (case, the document in force, what it says about this action)
        let rows: &[(&str, &str, Decision)] = &[
            (
                "every action of this namespace",
                r#"{"Statement":[{"Effect":"Allow","Action":"bedrock-agentcore:*",
                 "Resource":"*"}]}"#,
                Decision::Allowed,
            ),
            (
                "exactly this action",
                r#"{"Statement":[{"Effect":"Allow","Action":"bedrock-agentcore:GetAgentRuntime",
                 "Resource":"*"}]}"#,
                Decision::Allowed,
            ),
            (
                "another operation of this namespace",
                r#"{"Statement":[{"Effect":"Allow","Action":"bedrock-agentcore:ListAgentRuntimes",
                 "Resource":"*"}]}"#,
                Decision::NotStated,
            ),
            (
                "another service entirely",
                r#"{"Statement":[{"Effect":"Allow","Action":"bedrock:*","Resource":"*"}]}"#,
                Decision::NotStated,
            ),
            (
                "an explicit deny of this action beside an allow of the namespace",
                r#"{"Statement":[
                 {"Effect":"Allow","Action":"bedrock-agentcore:*","Resource":"*"},
                 {"Effect":"Deny","Action":"bedrock-agentcore:GetAgentRuntime",
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

    /// RULE: the decision is taken against the RUNTIME's ARN, so a statement scoped to one
    /// runtime's ARN covers that runtime and no other — not another runtime of the same
    /// account and Region, and not the same id in another Region.
    #[test]
    fn a_statement_is_read_for_the_runtime_arn_the_request_addresses() {
        let addressed = crate::state::runtime_arn("us-east-1", "probeRuntime-abcdefghij");
        let document = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Deny",
             "Action":"bedrock-agentcore:GetAgentRuntime","Resource":"{addressed}"}}]}}"#
        );
        // (case, the resource the decision is taken against, whether the deny covers it)
        let rows: &[(&str, String, bool)] = &[
            ("the runtime the request names", addressed.clone(), true),
            (
                "another runtime of the same account and Region",
                crate::state::runtime_arn("us-east-1", "otherRuntime-abcdefghij"),
                false,
            ),
            (
                "the same id in another Region",
                crate::state::runtime_arn("eu-west-3", "probeRuntime-abcdefghij"),
                false,
            ),
            (
                "the bare id, which names no ARN",
                "probeRuntime-abcdefghij".to_string(),
                false,
            ),
        ];
        for (case, resource, covered) in rows {
            let said = decision(&document, &action_for("GetAgentRuntime"), resource);
            let denied = said == Decision::Denied;
            assert_eq!(
                denied, *covered,
                "case {case:?}: expected the deny to {}cover {resource}, got {said:?}",
                if *covered { "" } else { "not " }
            );
        }
    }

    /// RULE: the sentence states the principal and the action, and closes there.
    #[test]
    fn the_refusal_states_the_signing_principal_and_the_action() {
        let principal = "arn:aws:iam::000000000000:user/probe";
        let sentence = not_authorized(principal, &action_for("GetAgentRuntime"));
        assert_eq!(
            sentence,
            "User: arn:aws:iam::000000000000:user/probe is not authorized to perform: \
             bedrock-agentcore:GetAgentRuntime",
            "case \"the sentence\": got {sentence:?}"
        );
    }
}
