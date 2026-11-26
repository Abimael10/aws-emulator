// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this data plane perform the action it asks for — the
//! decision an operation takes BEFORE it mints anything or reads what the request named.
//!
//! Every operation of this service models `AccessDeniedException` (HTTP 403, "The exception
//! that occurs when you do not have sufficient permissions to perform an action"), and the
//! identity plane's token mints are the calls where that decision is observable: a workload
//! access token is scoped to ONE workload identity, so a principal whose grant names one
//! identity may not obtain a token for another. AWS takes that decision at the IAM layer,
//! ahead of the service resolving what the request named, so the refusal says nothing about
//! whether the identity it named exists.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_caller`, the same record
//!    `sts:GetCallerIdentity` answers from), else the IAM user that owns the long-term key,
//!    else the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    own accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next request and deleting it lifts it
//!    again;
//! 3. **the decision and its wording** — [`::service_common::iam_authorization`], the one
//!    shared evaluation of those documents against `(action, resource)` and the one
//!    spelling of the sentence a refusal carries, so this service and every other one
//!    handed the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the reading of a principal
//! ARN does. That is the split every other gate in this tree makes
//! (`service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery`,
//! `service-com-amazonaws-kinesisvideosignaling-awsacuitysignalingservice`), and the lookups
//! stay in this crate for the reason those keep theirs there: hoisting them into
//! `service-common` would make every service depending on that crate link the IAM and STS
//! service crates.
//!
//! ## The action and the resource
//!
//! The action is `<arn namespace>:<OperationName>` — this service's `arnNamespace`
//! ([`crate::state::ARN_SERVICE`]), the same namespace every AgentCore ARN carries — so one
//! policy statement scoped to this service's actions covers every operation of it and
//! nothing outside it.
//!
//! The resource is the one the request addresses, built from the request alone: for the
//! identity plane that is the workload identity a `workloadName` names
//! ([`crate::state::workload_identity_arn`]). Taking it from the REQUEST rather than from a
//! stored record is also what keeps a refused call from telling an unauthorized caller
//! whether the identity it named exists.
//!
//! This service publishes no resource-based policy of its own — the model declares no
//! policy operation and no policy member — so the decision here is the identity-based one
//! alone.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to the account's own identity, which is not a
//!   principal the IAM store holds policies for — the position AWS account root credentials
//!   are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses, and a permissions boundary the
//!   store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no user can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)
//!
//! ## The condition keys a request publishes
//!
//! A statement's `Condition` is decided only for the keys the REQUEST carries a value for,
//! so a gate that passes an empty context leaves every conditioned statement undecided —
//! and an undecided `Allow` never grants
//! ([`::service_common::iam_authorization`]'s fail-closed reading). This plane therefore
//! publishes the condition key its own model names: a call of the MEMORY plane carries the
//! namespace its request addresses under [`MEMORY_NAMESPACE_CONDITION_KEY`]
//! ([`authorize_memory`]), which is what the model says that member is for ("This value is
//! used for IAM condition key authorization", `smithy_query(query="doc",
//! shape="GetMemoryRecordInput", member="namespace")`). The key is DECLARED by every call
//! whose request HAS that member, whether or not it carries a value for it, so its absence
//! is decidable rather than unknown — a request addressing a whole namespace hierarchy
//! carries no value for it, and a clause that compares one then does not hold.
//!
//! Nothing here guesses a condition value: a request with no `namespace` member at all
//! ([`AddressedNamespace::NoNamespaceMember`]) publishes none, and a statement conditioned
//! on the key stays undecided for it.
//!
//! ## How a refusal is worded
//!
//! By WHAT withheld the grant, which is what the caller has to act on — and in both sentences
//! every value comes from the request and from the stored policies, never from this module.
//!
//! * No grant of the account's covers the call — no statement about the action and the
//!   resource, or an explicit `Deny` over them, or a boundary that caps them: the refusal is
//!   the sentence IAM states such a failure with, naming the principal that signed, the action
//!   it asked for, the resource it addressed and which of IAM's rules withheld the grant
//!   ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]). That is
//!   what a request for a resource the caller's grant does not name is refused with, and what
//!   every gate whose operation publishes no condition key of its own answers.
//! * A grant DOES cover the action and the resource and only its `Condition` over a key the
//!   request published refused the call ([`condition_withheld_the_grant`]): the refusal names
//!   the principal and nothing further
//!   ([`::service_common::iam_authorization::not_authorized_to_perform_this_operation`]).
//!   Naming the action and the resource there would say the caller holds no grant for them
//!   when it does: what it may not do is address this key's value under that grant, so the
//!   remedy is the clause rather than the action — which is why the two denials are told
//!   apart at all.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use crate::ops::types::AccessDeniedException;

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The condition key a memory-record call of this plane publishes the namespace it
/// addresses under.
///
/// A service's own condition keys are named `<arn namespace>:<key>` — this service's
/// [`super::ARN_SERVICE`] — and the model documents the `namespace` member of a
/// memory-record request as the value that key carries: "The namespace of the memory record
/// to retrieve. This value is used for IAM condition key authorization"
/// (`smithy_query(query="doc", shape="GetMemoryRecordInput", member="namespace")`).
pub const MEMORY_NAMESPACE_CONDITION_KEY: &str = "bedrock-agentcore:namespace";

/// What a MEMORY call publishes for that key: the one namespace its request addresses, or
/// the fact that its request has no such member at all.
///
/// The distinction matters because the shared evaluator reads absence two ways: a key the
/// request MODELS and carries no value for is decidably absent, while a key the request does
/// not model at all leaves a clause over it undecided. Only a request with a `namespace`
/// member can say the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressedNamespace<'a> {
    /// The `namespace` member of a memory-record request — the member the model documents as
    /// the value this key carries — as the request carried it: `Some` for a call addressing
    /// one namespace, `None` for one that left the member out or asked for a whole namespace
    /// hierarchy instead, which addresses no single namespace.
    Named(Option<&'a str>),
    /// The request has NO `namespace` member to publish: a batch write carries one
    /// `namespaces` list per entry rather than one namespace the whole call addresses. Such
    /// a call publishes no value for the key and does not claim to carry none either, so a
    /// statement conditioned on the key stays undecided and grants nothing — nothing here
    /// guesses what AWS would populate it with.
    NoNamespaceMember,
}

/// What one request publishes to the decision: the `(key, value)` pairs its own members
/// populate, and every key it MODELS — the keys whose absence from the context means this
/// request carries no value for them rather than that this emulator does not model them.
#[derive(Debug, Clone, Default)]
struct PublishedConditions {
    carried: Vec<(String, String)>,
    published: Vec<&'static str>,
}

impl PublishedConditions {
    /// What a call that publishes no condition key of its own carries.
    fn none() -> Self {
        Self::default()
    }

    /// What a MEMORY call publishes: this plane's namespace key, declared as modelled by
    /// every call whose request HAS the member, and carrying the namespace that member named
    /// when it named one.
    ///
    /// Declaring the key is what makes its absence decidable: a request that addresses a
    /// whole namespace hierarchy rather than one namespace carries no value for it, so a
    /// clause comparing the key to a value does not hold while a negated one does.
    fn of_memory_call(namespace: AddressedNamespace<'_>) -> Self {
        let AddressedNamespace::Named(named) = namespace else {
            return Self::none();
        };
        Self {
            carried: named
                .filter(|named| !named.is_empty())
                .map(|named| (MEMORY_NAMESPACE_CONDITION_KEY.to_string(), named.to_string()))
                .into_iter()
                .collect(),
            published: vec![MEMORY_NAMESPACE_CONDITION_KEY],
        }
    }
}

/// A refused decision: the principal that was refused, which of IAM's rules refused it, and
/// whether what refused it was a CONDITION on a grant that otherwise covered the request.
/// How the refusal is WORDED is the calling surface's own (see the module docs), so nothing
/// is spelled here.
#[derive(Debug, Clone)]
struct Refused {
    principal: String,
    reason: String,
    /// Set when a document in force on the caller grants this very action but narrows that
    /// grant with a `Condition` over a key the request publishes, and what the request
    /// carried for that key does not satisfy the clause — as against a request no grant
    /// covers at all ([`condition_withheld_the_grant`]).
    condition_withheld: bool,
}

/// The IAM action one operation of this service asks about: this service's `arnNamespace`
/// and the operation's own name.
fn action_of(operation: &str) -> String {
    format!("{}:{operation}", super::ARN_SERVICE)
}

/// Authorize this request on `resource`, where `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see
/// the module docs); the modelled refusal otherwise, which each operation lifts into its own
/// error channel. Call it FIRST in a handler, before anything is minted, read or written.
///
/// The decision carries no condition key of this request's own, which is right for an
/// operation whose model publishes none; a call of the MEMORY plane takes
/// [`authorize_memory`] instead.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), AccessDeniedException> {
    let action = action_of(operation);
    decide(engine, ctx, &action, resource, &PublishedConditions::none())
        .map_err(|refused| not_authorized(&refused, &action, resource))
}

/// Authorize a call of the MEMORY plane on the memory container `memory_arn` its `memoryId`
/// addresses, publishing the namespace it addresses as
/// [`MEMORY_NAMESPACE_CONDITION_KEY`].
///
/// Publishing the key is what makes a grant conditioned on it decidable: a statement
/// allowing the action on the container only for one namespace (or one namespace prefix)
/// grants exactly the calls whose namespace the clause covers, and refuses every other one.
///
/// The refusal is IAM's own full sentence, as [`authorize`]'s is: the principal that signed,
/// the action it asked for, the resource it addressed — the memory container, which is the
/// resource this plane's actions are scoped to — and which of IAM's rules withheld the
/// grant. One wording for every gate of this service, so a caller reads the same account of
/// a denial whichever plane it called.
///
/// The one denial worded otherwise is the one a CONDITION took: where a grant covers this
/// action on this container and only its clause over the published key refused the call, the
/// refusal names the principal alone ([`not_authorized`]), because the caller does hold a
/// grant for the action and what it may not do is address this namespace under it.
pub fn authorize_memory<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    memory_arn: &str,
    namespace: AddressedNamespace<'_>,
) -> Result<(), AccessDeniedException> {
    let action = action_of(operation);
    decide(
        engine,
        ctx,
        &action,
        memory_arn,
        &PublishedConditions::of_memory_call(namespace),
    )
    .map_err(|refused| not_authorized(&refused, &action, memory_arn))
}

/// The decision itself: whether the identity-based policies in force on the caller allow
/// `action` on `resource`, with `conditions` as the request's condition context.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service may
/// take this decision too.
fn decide<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    action: &str,
    resource: &str,
    conditions: &PublishedConditions,
) -> Result<(), Refused> {
    let caller = caller(engine, ctx);
    let refused = |reason: String| Refused {
        principal: caller.arn.clone(),
        reason,
        condition_withheld: false,
    };
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no
    // such element, and this service publishes no resource-based policy (see the module
    // docs), so the decision below rests on the action, the resource and the conditions.
    let request = iam::Request::new(action, resource, &caller.principals)
        .with_context(&conditions.carried)
        .with_published_keys(&conditions.published);
    let identity_said = iam::identity_decision(&caller.documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if identity_said == Decision::Denied {
        return Err(refused(iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()));
    }
    let granted =
        iam::is_allowed(identity_said, Decision::NotStated) || caller.identity_is_unbounded();
    if !granted {
        return Err(Refused {
            condition_withheld: condition_withheld_the_grant(
                &caller.documents,
                &request,
                conditions,
            ),
            ..refused(iam::no_identity_policy_allows(action))
        });
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not
    // a permission the principal has. The account's own credential carries none.
    if let Some(boundary) = &caller.boundary {
        let reason = match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
            Decision::Allowed => return Ok(()),
            // An explicit deny is a fact about the boundary document, so the refusal names
            // it; a boundary that merely states nothing withheld the grant the
            // identity-based policies gave.
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => iam::no_permissions_boundary_allows(action),
        };
        // The same reading of what refused the call: a boundary that caps this action only
        // for some values of the published key refused this request on that clause, exactly
        // as a conditioned grant does.
        return Err(Refused {
            condition_withheld: condition_withheld_the_grant(
                ::std::slice::from_ref(boundary),
                &request,
                conditions,
            ),
            ..refused(reason)
        });
    }
    Ok(())
}

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it — worded by the one shared spelling
/// of that sentence. Every value in it comes from the request and from the stored policies:
/// the principal from the credential the request presented, the action from the operation it
/// named, the resource from what it addressed.
///
/// A denial a CONDITION took is worded differently, and the difference is what the caller
/// has to act on: the principal does hold a grant for this action on this resource, and what
/// refused the call is the clause over the key the request published, so naming the action
/// and the resource would send a client to widen the wrong thing. That refusal names the
/// principal alone ([`iam::not_authorized_to_perform_this_operation`]), which is the
/// sentence this service answers it in.
fn not_authorized(refused: &Refused, action: &str, resource: &str) -> AccessDeniedException {
    let message = if refused.condition_withheld {
        iam::not_authorized_to_perform_this_operation(&refused.principal)
    } else {
        iam::not_authorized_by_identity_policy(
            &refused.principal,
            action,
            resource,
            &refused.reason,
        )
    };
    AccessDeniedException {
        message: Some(message),
    }
}

/// Whether what withheld the grant for this request was a `Condition` over a key it
/// PUBLISHES, on a statement of `documents` that grants the action ON THE RESOURCE the
/// request addressed — rather than there being no grant about the request at all.
///
/// Asked only once a decision has already refused the request, and asked per published key
/// through the shared evaluation
/// ([`iam::identity_grant_withheld_by_condition_key`]), which requires of ONE statement that
/// it be an `Allow` whose action and resource cover this request, that its `Condition` name
/// the key, and that the clause be decidably false for what the request carries. The resource
/// is part of that question: a grant of the same action over ANOTHER resource withheld
/// nothing on a condition — it does not cover this call — so such a refusal keeps IAM's own
/// sentence.
///
/// A set of documents holding no grant about the request, or no clause over any published
/// key, withheld nothing on a condition.
fn condition_withheld_the_grant(
    documents: &[String],
    request: &iam::Request<'_>,
    conditions: &PublishedConditions,
) -> bool {
    conditions.published.iter().any(|key| {
        iam::identity_grant_withheld_by_condition_key(documents, request, key)
    })
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
    /// The permissions boundary in force on the principal, when it carries one whose
    /// document this emulator holds.
    boundary: Option<String>,
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
fn caller<Eng: Send + Sync + 'static>(
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
        // No STS store on this engine: the only identity a request can present is the
        // account's own published credential.
        None => Ok(sts_store::ambient_identity()),
    };
    let identity = match identity {
        Ok(identity) => identity,
        // The session credential has expired, so it speaks for nobody: no documents, no
        // principal spelling, and every action it asks for falls to the default deny. The
        // key id it presented is NOT quoted back — this module echoes no value of the
        // request — and in the assembled emulator the layer that authenticates a request
        // refuses an expired credential before any handler runs.
        Err(expired) => {
            let _ = expired;
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                principals: Vec::new(),
                documents: Vec::new(),
                boundary: None,
                account_credential: false,
                unreadable_attachment: false,
            };
        }
    };

    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        documents: Vec::new(),
        boundary: None,
        unreadable_attachment: false,
    };
    let Some(iam_state) = iam_store::service_state(engine) else {
        return caller;
    };
    let store = iam_state.lock();
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
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by
        // the ARN the account holds it under — which carries its PATH, where the session ARN
        // does not.
        if let Some(held) = store.roles.get(&iam_store::name_key(role_name)) {
            caller.principals.push(held.arn.clone());
            caller.read_policies_of(&store, &held.arn.clone());
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

    /// The decision one stored document reaches about an action on a resource, through the
    /// shared evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// RULE: the action this gate asks about carries this service's own ARN namespace, so a
    /// statement scoped to this service's actions covers every operation of it and nothing
    /// outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_namespace() {
        let document = format!(
            r#"{{"Statement":[{{"Effect":"Allow","Action":"{}:*","Resource":"*"}}]}}"#,
            super::super::ARN_SERVICE
        );
        for operation in [
            "GetWorkloadAccessToken",
            "GetWorkloadAccessTokenForJWT",
            "GetWorkloadAccessTokenForUserId",
        ] {
            let action = format!("{}:{operation}", super::super::ARN_SERVICE);
            let reached = decision(&document, &action, "*");
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected the service-wide allow to cover {action}, got \
                 {reached:?}"
            );
        }
        let outside = decision(&document, "bedrock:InvokeModel", "*");
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected the service-wide allow to grant nothing outside \
             this service, got {outside:?}"
        );
    }

    /// RULE: a refusal no grant covered names the principal, the action, the resource the
    /// request addressed and which of IAM's rules refused it; one a CONDITION on a covering
    /// grant took names the principal alone — every part taken from the request and the
    /// stored policies.
    #[test]
    fn the_refusal_is_worded_by_what_withheld_the_grant() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/agent-role/session-1",
            ::engine::emulator_account_id()
        );
        let action = format!("{}:GetWorkloadAccessToken", super::super::ARN_SERVICE);
        let resource = super::super::workload_identity_arn("us-east-1", "demo.agent");
        let reason = iam::no_identity_policy_allows(&action);
        let long = format!(
            "User: {principal} is not authorized to perform: {action} on resource: {resource} \
             {reason}"
        );
        let short = format!("User: {principal} is not authorized to perform this operation");
        // (case, what withheld the grant, the whole sentence it is refused in)
        for (case, condition_withheld, expected) in [
            ("IAM's default deny", false, &long),
            ("a condition on a covering grant", true, &short),
        ] {
            let refused = Refused {
                principal: principal.clone(),
                reason: reason.clone(),
                condition_withheld,
            };
            let refusal = not_authorized(&refused, &action, &resource);
            assert_eq!(
                refusal.message.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {:?}",
                refusal.message
            );
        }
    }

    /// RULE: a refusal is attributed to a CONDITION only where a document in force on the
    /// caller grants the action ON THE RESOURCE the request addressed and narrows that grant
    /// with a clause over a key the request PUBLISHES which what the request carried does not
    /// satisfy; a grant the call misses for any other reason — the resource it names among
    /// them — withheld nothing on a condition.
    #[test]
    fn only_an_unsatisfied_clause_on_a_covering_grant_is_a_condition_refusal() {
        let action = format!("{}:ListMemoryRecords", super::super::ARN_SERVICE);
        let allowed = "/t/tenant/actor/alpha/facts/";
        let sibling = "/t/tenant/actor/beta/facts/";
        // The container the request addresses, and a second one a grant can be scoped to
        // instead.
        let addressed_memory = super::super::memory_arn("us-east-1", "held-memory-0123456789");
        let other_memory = super::super::memory_arn("us-east-1", "other-memory-9876543210");
        let conditioned_on = |resource: &str, operator: &str, value: &str| {
            format!(
                r#"{{"Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"{resource}",
                 "Condition":{{"{operator}":{{"{MEMORY_NAMESPACE_CONDITION_KEY}":"{value}"}}}}}}]}}"#
            )
        };
        let conditioned = |operator: &str, value: &str| conditioned_on("*", operator, value);
        let unconditioned =
            format!(r#"{{"Statement":[{{"Effect":"Allow","Action":"{action}","Resource":"*"}}]}}"#);
        // (case, the documents in force, the namespace the request addresses, whether a
        // condition is what withheld the grant)
        let rows: Vec<(&str, Vec<String>, AddressedNamespace<'_>, bool)> = vec![
            (
                "an equality clause the addressed namespace does not satisfy",
                vec![conditioned("StringEquals", allowed)],
                AddressedNamespace::Named(Some(sibling)),
                true,
            ),
            (
                "a pattern clause the addressed namespace does not satisfy",
                vec![conditioned("StringLike", "/t/tenant/actor/alpha/*")],
                AddressedNamespace::Named(Some(sibling)),
                true,
            ),
            (
                "an equality clause and a request carrying no value for the key",
                vec![conditioned("StringEquals", allowed)],
                AddressedNamespace::Named(None),
                true,
            ),
            (
                "an equality clause the addressed namespace satisfies",
                vec![conditioned("StringEquals", allowed)],
                AddressedNamespace::Named(Some(allowed)),
                false,
            ),
            (
                "a negated clause the addressed namespace satisfies",
                vec![conditioned("StringNotEquals", allowed)],
                AddressedNamespace::Named(Some(sibling)),
                false,
            ),
            (
                "a grant that conditions on nothing",
                vec![unconditioned.clone()],
                AddressedNamespace::Named(Some(sibling)),
                false,
            ),
            (
                "no document at all",
                Vec::new(),
                AddressedNamespace::Named(Some(sibling)),
                false,
            ),
            (
                "a clause on the key for a call that publishes no key of its own",
                vec![conditioned("StringEquals", allowed)],
                AddressedNamespace::NoNamespaceMember,
                false,
            ),
            (
                // The grant names ANOTHER container, so nothing of it is about this request:
                // the resource withheld the grant, not the clause.
                "a clause the namespace does not satisfy on a grant scoped to another resource",
                vec![conditioned_on(&other_memory, "StringEquals", allowed)],
                AddressedNamespace::Named(Some(sibling)),
                false,
            ),
            (
                // The same grant, scoped to the container the request DOES address.
                "a clause the namespace does not satisfy on a grant scoped to this resource",
                vec![conditioned_on(&addressed_memory, "StringEquals", allowed)],
                AddressedNamespace::Named(Some(sibling)),
                true,
            ),
        ];
        for (case, documents, addressed, expected) in rows {
            let conditions = PublishedConditions::of_memory_call(addressed);
            let request = iam::Request::new(&action, &addressed_memory, &[])
                .with_context(&conditions.carried)
                .with_published_keys(&conditions.published);
            let withheld = condition_withheld_the_grant(&documents, &request, &conditions);
            assert_eq!(
                withheld, expected,
                "case {case:?}: expected a condition to have withheld the grant = {expected}, \
                 got {withheld}"
            );
        }
    }

    /// RULE: the condition key a memory call publishes is named after this service's own ARN
    /// namespace, as every service-specific condition key is.
    #[test]
    fn the_namespace_condition_key_carries_this_services_namespace() {
        let expected = format!("{}:namespace", super::super::ARN_SERVICE);
        assert_eq!(
            MEMORY_NAMESPACE_CONDITION_KEY, expected,
            "case \"the published key\": expected {expected:?}, got \
             {MEMORY_NAMESPACE_CONDITION_KEY:?}"
        );
    }

    /// RULE: a memory call publishes the namespace it addresses under that key and declares
    /// the key whenever its request HAS the member — including a request that names no
    /// namespace, which therefore carries no value for it — while a request with no such
    /// member publishes nothing at all.
    #[test]
    fn a_memory_call_publishes_the_namespace_it_addresses() {
        // (case, what the request addresses, the values the key carries, whether it is
        // declared as modelled)
        let rows: &[(&str, AddressedNamespace<'_>, &[&str], bool)] = &[
            (
                "a request naming one namespace",
                AddressedNamespace::Named(Some("/t/tenant/actor/alpha/facts/")),
                &["/t/tenant/actor/alpha/facts/"],
                true,
            ),
            (
                "a request naming a sibling namespace",
                AddressedNamespace::Named(Some("/t/tenant/actor/beta/facts/")),
                &["/t/tenant/actor/beta/facts/"],
                true,
            ),
            (
                "a request whose member is absent",
                AddressedNamespace::Named(None),
                &[],
                true,
            ),
            (
                "a request naming an empty namespace",
                AddressedNamespace::Named(Some("")),
                &[],
                true,
            ),
            (
                "a request with no namespace member at all",
                AddressedNamespace::NoNamespaceMember,
                &[],
                false,
            ),
        ];
        for (case, addressed, expected, declared) in rows {
            let conditions = PublishedConditions::of_memory_call(*addressed);
            let carried: Vec<&str> = conditions
                .carried
                .iter()
                .filter(|(key, _)| key == MEMORY_NAMESPACE_CONDITION_KEY)
                .map(|(_, value)| value.as_str())
                .collect();
            assert_eq!(
                carried, *expected,
                "case {case:?}: expected the key to carry {expected:?}, got {carried:?}"
            );
            assert_eq!(
                conditions
                    .published
                    .contains(&MEMORY_NAMESPACE_CONDITION_KEY),
                *declared,
                "case {case:?}: expected the key declared as modelled = {declared}, got {:?}",
                conditions.published
            );
        }
        assert!(
            PublishedConditions::none().published.is_empty(),
            "case \"a call publishing no key of its own\": expected no modelled key, got {:?}",
            PublishedConditions::none().published
        );
    }
}
