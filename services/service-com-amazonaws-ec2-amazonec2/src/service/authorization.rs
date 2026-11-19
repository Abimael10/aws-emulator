// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the EC2 action it names — the
//! decision taken ahead of EVERY request, before any handler reaches the store.
//!
//! EC2 refuses a caller that may not perform the action it asked for with its own
//! `UnauthorizedOperation` code (HTTP 403), which the `DryRun` member of every request
//! documents as the other side of the permission probe: "Checks whether you have the
//! required permissions for the action … If you have the required permissions, the error
//! response is `DryRunOperation`. Otherwise, it is `UnauthorizedOperation`". So the
//! decision is a fact about the REQUEST's credential and the policies in force at the
//! instant of the call, not about the resources the request names — which is why it is
//! taken once here rather than per handler.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_request_caller`, the same place
//!    `sts:GetCallerIdentity` answers from, so the principal a refusal blames is the one
//!    the caller reads back for the credentials it signed with), else the IAM user that
//!    holds the long-term key, else the account's own published credential;
//! 2. **what it may do** — every policy document in force on that credential: the
//!    identity-based policy documents the IAM store holds for the principal and the
//!    permissions boundary that caps them, read through the IAM service's OWN accessors
//!    (`principal_policies`, `permissions_boundary_document`), where the collection rule
//!    itself lives — so a policy `iam:PutRolePolicy` wrote takes effect on the next request
//!    and deleting it lifts it again — and the SESSION POLICIES the mint of this very
//!    credential scoped it with (`sts_store::session_scoping_documents`), which cap those
//!    documents the same way the boundary does: "the resulting session's permissions are
//!    the intersection of the role's identity-based policy and the session policies"
//!    (`AssumeRoleRequest$Policy`), so a session policy takes permissions away, grants none
//!    of its own, and its explicit `Deny` refuses whatever the role's own documents allow;
//! 3. **the decision** — [`::service_common::iam_authorization`], the tree's ONE
//!    evaluation of those documents against `(action, resource)`, so this service and
//!    every other one handed the same documents answer alike: an explicit `Deny` wins,
//!    otherwise an `Allow` about the request grants it, and with neither the request falls
//!    to IAM's default deny.
//!
//! Each of those documents decides the request as it reads AT THE INSTANT OF THE CALL —
//! nothing about the decision is remembered from the mint of the credential — which is what
//! lets a client that polls for a permission change observe it: a `Deny` stored now refuses
//! the next call, on the session already minted and on one minted afterwards alike, and
//! deleting it serves the next call again.
//!
//! Only the store LOOKUPS live here; neither the decision nor the ARN readings do. WHICH
//! IAM entity a principal ARN names is read through the shared
//! [`::service_common::iam_role`] rather than parsed again.
//!
//! ## How each action is decided, and the resources it names
//!
//! Every EC2 action is decided here. What differs per action is the RESOURCE the decision
//! is taken against, which is what a statement has to cover to grant it.
//!
//! An EC2 `Describe` action supports no resource-level permissions — "the Amazon EC2 API
//! actions that begin with `Describe` do not support resource-level permissions", Amazon
//! EC2 User Guide, "Resource-level permissions for Amazon EC2" — so IAM judges it against
//! every resource ([`EVERY_RESOURCE`]) and a statement permits it only by covering every
//! resource. [`judged_over_every_resource`] is the test, and a refusal of such an action
//! names no resource because there is none to name.
//!
//! Every OTHER EC2 action is judged against the resources the request ADDRESSES, read from
//! the request's own ids ([`super::addressed_resources`]): one decision per resource, so a
//! statement scoped to resource ARNs grants the action exactly on the resources it names,
//! and the first resource no statement allows is the one the request is refused ON — which
//! the refusal then states. An action judged on a resource it CREATES is decided on that
//! resource FIRST ([`created_resource`]), the request carrying no id for a resource that
//! does not exist until the call succeeds.
//!
//! Each decision is taken in the condition context the REQUEST carries
//! ([`super::request_conditions`]), so a statement conditioned on a key the request answers
//! — the instance type a launch asks for — is decided rather than left undecided.
//!
//! ## What a refusal hands the caller to decode
//!
//! "Only certain Amazon Web Services operations return an encoded message"
//! (`sts:DecodeAuthorizationMessage`), so the refusals of the operations that do
//! ([`ENCODED_AUTHORIZATION_MESSAGE_OPERATIONS`]) close with an encoded authorization
//! failure message ([`refusal_message`]) and every other refusal states its decision in its
//! own sentence alone. The blob names the decision taken HERE — whether a `Deny` said so
//! explicitly, the principal, the requested action, the resource it was judged on, the
//! condition keys it was taken over and the statements it rested on — recorded in the
//! registry `sts:DecodeAuthorizationMessage` decodes one from
//! ([`super::authorization_diagnostics`]), so what the caller decodes is this decision
//! rather than a document written to look like one.
//!
//! A request of such an action that addresses no resource this gate could read an ARN for
//! (a create that names nothing existing, a value of a kind the ARN reading does not carry)
//! is decided by the two readings no resource can change: a `Deny` covering every resource
//! refuses it, and documents holding no `Allow` of the action at all permit it nowhere.
//! Between those it is served, because a resource-scoped statement may well cover whatever
//! the request addresses and withholding the grant would refuse a call AWS permits. The
//! session policies in force are read by those same two readings, for the same reason.
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS
//!   nor IAM holds a record for resolves to the account's own identity, which is not a
//!   principal the IAM store holds policies for — the position AWS account root
//!   credentials are in. Such a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed
//!   policy belongs to AWS rather than to this account, so the account's store may hold no
//!   document for it; refusing on a grant this emulator cannot see would deny access AWS
//!   permits. An explicit `Deny` in a document that CAN be read still refuses, and a
//!   permissions boundary the store DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//! * **A credential scoped by a managed session policy whose document this emulator does
//!   not hold** ([`sts_store::ScopingPolicies::unreadable`]), for the reason an unreadable
//!   attachment is not bounded either: the scope may allow more than the documents that CAN
//!   be read show, and a `Deny` in one that can be read still refuses.
//!
//! An expired session credential speaks for nobody: it carries no documents and no
//! principal spelling, so every action it asks for is refused. (A session the
//! authenticating layer has already refused never reaches a handler at all.)
//!
//! Resolving the caller goes through STS's own `resolve_request_caller`, which first
//! materialises the session expiries the clock has already made due; that write is owed at
//! the same instant whether or not anybody called, which is why a REPORTING operation may
//! take this decision too.

use std::time::SystemTime;

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

use super::authorization_diagnostics as diagnostics;

/// This service's IAM action prefix: an action is `ec2:<OperationName>` ("Actions defined
/// by Amazon EC2", IAM Service Authorization Reference), which is this service's
/// `arnNamespace` (`smithy_query(query="service_info")`).
pub const ACTION_PREFIX: &str = "ec2";

/// The resource an action that supports no resource-level permissions is judged against,
/// as IAM writes it for such a request: every resource.
pub const EVERY_RESOURCE: &str = "*";

/// The sentence EC2 opens an `UnauthorizedOperation` refusal with, ahead of the IAM
/// clause that says who was refused what and why.
pub const NOT_AUTHORIZED_SENTENCE: &str = "You are not authorized to perform this operation.";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The name prefix of the EC2 operations whose action IAM judges over every resource. See
/// the module docs for the documented rule this is the one test of.
const ACCOUNT_WIDE_READ_PREFIX: &str = "Describe";

/// Whether IAM judges `operation`'s action over every resource, so a statement permits it
/// only by covering every resource (see the module docs).
#[must_use]
pub fn judged_over_every_resource(operation: &str) -> bool {
    operation.starts_with(ACCOUNT_WIDE_READ_PREFIX)
}

/// The id IAM names a resource an action is judged on but the request carries no id for:
/// the resource does not exist until the call succeeds, so every resource of that type is
/// what a statement has to cover.
const CREATED_RESOURCE_ID: &str = "*";

/// The resource TYPE an action that is judged on a resource it CREATES names, keyed by the
/// operation's own name.
///
/// Most actions are judged on the resources the request names by id
/// ([`super::addressed_resources`]). An action in this catalog is judged on the resource it
/// would bring into being instead — `RunInstances` on
/// `arn:${Partition}:ec2:${Region}:${Account}:instance/${InstanceId}`, the `instance`
/// resource type the IAM Service Authorization Reference lists as the one it requires — and
/// the request carries no id for it, there being no instance until the launch succeeds. So
/// the decision is taken on that type with every id ([`CREATED_RESOURCE_ID`]), ahead of the
/// resources the request references, and that is the resource a refusal names.
///
/// An action is in this catalog only where the recorded contract shows the refusal naming
/// the created resource: `RunInstances` on the `instance` type and `CreateVolume` on the
/// `volume` type are each recorded that way, while `CreateNetworkInterface`, whose recorded
/// refusal names the SUBNET the interface would go in rather than an interface, is judged on
/// the resources the request names and is deliberately absent.
const CREATED_RESOURCE_TYPE_BY_OPERATION: &[(&str, &str)] =
    &[("RunInstances", "instance"), ("CreateVolume", "volume")];

/// The ARN of the resource `operation` is judged on and the request carries no id for, in
/// `region` — `None` for an action judged on the resources the request names.
#[must_use]
pub fn created_resource(operation: &str, region: &str) -> Option<String> {
    CREATED_RESOURCE_TYPE_BY_OPERATION
        .iter()
        .find(|(named, _)| *named == operation)
        .map(|(_, resource_type)| {
            ::engine::emulator_arn(
                ACTION_PREFIX,
                region,
                &format!("{resource_type}/{CREATED_RESOURCE_ID}"),
            )
        })
}

/// The operations whose refusal hands the caller an ENCODED AUTHORIZATION FAILURE MESSAGE.
///
/// "Only certain Amazon Web Services operations return an encoded message. The documentation
/// for an individual operation indicates whether that operation returns an encoded message
/// in addition to returning an HTTP code" (`sts:DecodeAuthorizationMessage`, the STS model):
/// which operations those are is a fact about each operation rather than about the decision,
/// so it is a catalog, and membership comes from the operation's own recorded contract.
///
/// `RunInstances` and `CreateVolume` are each recorded refusing with the trailer — the first
/// from an explicit `Deny` in a session policy, the second from the absence of any allow in
/// the principal's identity-based documents, so WHICH document decided is not what decides
/// whether the caller is handed a blob. `CreateNetworkInterface` is recorded refusing with
/// the same deny classification as the second and NO trailer, which is why this is a catalog
/// of operations and not a reading of the decision.
const ENCODED_AUTHORIZATION_MESSAGE_OPERATIONS: &[&str] = &["RunInstances", "CreateVolume"];

/// Whether `operation`'s refusal hands the caller an encoded authorization failure message
/// ([`ENCODED_AUTHORIZATION_MESSAGE_OPERATIONS`]).
#[must_use]
pub fn hands_over_an_encoded_message(operation: &str) -> bool {
    ENCODED_AUTHORIZATION_MESSAGE_OPERATIONS.contains(&operation)
}

/// WHICH document in force on the credential the decision rested on, for a request it
/// refused.
///
/// Read beside the refusal's own wording because it decides one thing more: whether the
/// decision the refusal's encoded message carries was reached by an explicit `Deny`
/// ([`Denied::encoded_message_decision`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecidedBy {
    /// An explicit `Deny` in an identity-based document of the principal.
    IdentityDeny,
    /// An explicit `Deny` in a SESSION POLICY the mint of this credential passed.
    SessionPolicyDeny,
    /// An explicit `Deny` in the permissions boundary capping the principal.
    BoundaryDeny,
    /// The absence of a grant: no document in force allows the action.
    NoAllow,
}

/// An authorization refusal: who was refused, which action, on which resource when the
/// decision was taken on one, and which of IAM's rules withheld the grant.
#[derive(Debug, Clone)]
pub struct Denied {
    /// The principal the request speaks for, as `sts:GetCallerIdentity` reports it.
    principal: String,
    /// The IAM action the request asked for.
    action: String,
    /// The ARN of the resource the request was refused ON, when the decision was taken
    /// against a resource the request addressed; `None` when there was none to name.
    resource: Option<String>,
    /// Why the grant was withheld, as the refusal's closing clause words it.
    reason: String,
    /// Everything the refusal's DIAGNOSTIC is built from, held behind one pointer: a
    /// refusal travels as the error of a decision taken per resource, and what only the
    /// encoded message reads does not belong in every one of those.
    diagnosed: Box<Diagnosis>,
}

/// What the refusal's encoded authorization failure message is built from, beside the
/// sentence the refusal states.
#[derive(Debug, Clone)]
struct Diagnosis {
    /// The unique id of the principal — the `AssumedRoleId` of a role session — which is
    /// what identifies it in the decision the encoded message carries.
    principal_id: String,
    /// The resource the decision was TAKEN on, which is every resource
    /// ([`EVERY_RESOURCE`]) where there was none to name.
    judged: String,
    /// Which document in force reached that decision.
    decided_by: DecidedBy,
    /// The condition keys the decision was taken over, one entry per key.
    conditions: Vec<(String, Vec<String>)>,
    /// The statements the decision rested on, as the decoded message reports each.
    matched: Vec<sts_store::MatchedStatement>,
}

impl Denied {
    /// The refusal in EC2's own wording: its fixed opening sentence, then the IAM clause
    /// naming the principal the credential speaks for, the action the request asked for,
    /// the resource it was refused on and which of IAM's rules withheld the grant.
    ///
    /// A refusal that NAMES a resource closes the sentence with a full stop; one that names
    /// none does not, there being no resource clause to close. A resource is named exactly
    /// when the decision was taken on one: an action IAM judges over every resource has no
    /// resource to name, and neither has a request that addresses none (see the module
    /// docs).
    #[must_use]
    pub fn message(&self) -> String {
        match &self.resource {
            Some(resource) => format!(
                "{NOT_AUTHORIZED_SENTENCE} {}.",
                iam::not_authorized_by_identity_policy(
                    &self.principal,
                    &self.action,
                    resource,
                    &self.reason,
                )
            ),
            None => format!(
                "{NOT_AUTHORIZED_SENTENCE} User: {} is not authorized to perform: {} {}",
                self.principal, self.action, self.reason
            ),
        }
    }

    /// The decision this refusal was reached by, for the encoded authorization failure
    /// message it hands the caller — `None` for a refusal that carries none.
    ///
    /// A refusal carries one exactly when the OPERATION is one whose refusals do
    /// ([`ENCODED_AUTHORIZATION_MESSAGE_OPERATIONS`]), whichever document in force reached
    /// the decision: the blob is the detail of a decision that "can contain privileged
    /// information that the user who requested the operation should not see"
    /// (`sts:DecodeAuthorizationMessage`), so what it carries is the decision itself —
    /// whether a `Deny` said so explicitly, the principal, the requested action, the
    /// resource it was judged on, the condition keys it was taken over and the statements it
    /// rested on, of which a decision reached by the absence of a grant has none.
    ///
    /// `operation` is the operation's own name, which is what the decoded message reports as
    /// "the requested action" — NOT IAM's service-qualified spelling, which is what the
    /// refusal's own SENTENCE names and what a denying statement carries among its actions.
    /// The recorded decode of exactly this refusal reports the bare operation name, so that is
    /// the requirement; the two spellings sit side by side in one document, the qualified one
    /// under the matched statement and the operation under the request's context.
    #[must_use]
    pub fn encoded_message_decision(
        &self,
        operation: &str,
    ) -> Option<sts_store::AuthorizationDecision> {
        if !hands_over_an_encoded_message(operation) {
            return None;
        }
        Some(sts_store::AuthorizationDecision {
            allowed: false,
            // "Whether the request was denied due to an explicit deny or due to the absence
            // of an explicit allow" (`sts:DecodeAuthorizationMessage`): the decision the
            // gate reached, read off the document that reached it.
            explicit_deny: self.diagnosed.decided_by != DecidedBy::NoAllow,
            principal_id: self.diagnosed.principal_id.clone(),
            principal_arn: self.principal.clone(),
            action: operation.to_string(),
            resource: self.diagnosed.judged.clone(),
            conditions: self.diagnosed.conditions.clone(),
            matched_statements: self.diagnosed.matched.clone(),
        })
    }
}

/// The message EC2 answers `refusal` with: its sentence, closed by the encoded
/// authorization failure message when the refusal carries one
/// ([`Denied::encoded_message_decision`]).
///
/// The blob is minted into the registry `sts:DecodeAuthorizationMessage` decodes one from,
/// so the caller that captures it off this sentence decodes the very decision this gate
/// took. An engine carrying no STS store holds no such registry, and the sentence then
/// stands alone rather than naming a blob nothing could decode.
#[must_use]
pub fn refusal_message<Eng: Send + Sync + 'static>(
    engine: &Eng,
    operation: &str,
    refusal: &Denied,
) -> String {
    let message = refusal.message();
    let Some(decision) = refusal.encoded_message_decision(operation) else {
        return message;
    };
    let Some(sts) = sts_store::service_state(engine) else {
        return message;
    };
    let encoded = sts_store::encode_authorization_message(
        &mut sts.lock(),
        SystemTime::now(),
        &decision,
    );
    sts_store::message_with_encoded_authorization_message(&message, &encoded)
}

/// Whether IAM lets this request perform the action of `operation` — the operation's own
/// name, which is the routing key the request named — given `resources`, the ARNs of the
/// resources the request addresses ([`super::addressed_resources`]), in the condition
/// context `conditions` the request itself carries
/// ([`super::request_conditions::request_conditions`]).
///
/// `Ok(())` for a request IAM permits and for every caller this gate does not bound (see
/// the module docs); `Err` carries the refusal EC2 answers as `UnauthorizedOperation`.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resources: &[String],
    conditions: &[(String, String)],
) -> Result<(), Denied> {
    let action = format!("{ACTION_PREFIX}:{operation}");
    let caller = caller(engine, ctx);
    let documents = Documents::of(&caller);
    // The keys the REQUEST carries, plus the CLASS of the credential it was signed with,
    // which is what a statement conditioned on `aws:PrincipalType` selects on and which this
    // gate holds for every request whose credential it resolved.
    let mut context = conditions.to_vec();
    context.extend(
        ::service_common::temporary_credential_classes::principal_type_conditions(
            caller.principal_type,
        ),
    );
    let asked = Asked {
        action: &action,
        conditions: &context,
    };
    // An action that supports no resource-level permissions is judged over EVERY resource,
    // and a refusal of it names no resource because there is none to name.
    if judged_over_every_resource(operation) {
        return decide(&caller, &documents, asked, EVERY_RESOURCE, None);
    }
    // An action judged on a resource it CREATES is decided on that resource first, the
    // request carrying no id for one that does not exist yet.
    let created = created_resource(operation, &crate::state::request_region(ctx.parts()));
    if let Some(created) = &created {
        decide(&caller, &documents, asked, created, Some(created))?;
    }
    // Every other action is judged against the resources the request addresses, one
    // decision per resource: a grant has to cover the resource, so the first one no
    // statement allows is the one the request is refused on.
    if resources.is_empty() && created.is_none() {
        return decide_over_any_resource(&caller, &documents, asked);
    }
    for resource in resources {
        decide(&caller, &documents, asked, resource, Some(resource))?;
    }
    Ok(())
}

/// What ONE request asks for, as every decision about it is taken over: the IAM action, and
/// the condition keys the request itself carries.
///
/// Held together because a request addressing several resources takes one decision per
/// resource and every one of them is about the same action in the same context.
#[derive(Debug, Clone, Copy)]
struct Asked<'a> {
    /// The IAM action, this service's prefix included.
    action: &'a str,
    /// The condition keys the request carries a value for, as `(key, value)` pairs.
    conditions: &'a [(String, String)],
}

/// The condition keys a decision of this gate DECLARES it answers: the class of the
/// credential the request was signed with, which this gate holds for every request whose
/// credential it resolved — so a clause over it is decided rather than left undecided. Every
/// other key keeps the undecided reading, which refuses nothing.
const PUBLISHED_CONDITION_KEYS: &[&str] =
    &[::service_common::temporary_credential_classes::PRINCIPAL_TYPE_CONDITION_KEY];

/// Every document in force on the caller, PARSED ONCE for the whole request.
///
/// A request that addresses several resources is decided once per resource, and the
/// resources are read from the request's own parameters, so the caller chooses how many
/// decisions one call takes: parsing a document per decision would let one small request
/// multiply the work the gate does. The shared evaluator's prepared form exists for exactly
/// this ([`::service_common::iam_authorization::prepare_documents`],
/// [`sts_store::ScopingPolicies::prepare`]), so every document is parsed here, before the
/// loop, and each decision only walks the statements.
struct Documents {
    /// The identity-based documents in force on the principal.
    identity: Vec<iam::PreparedDocument>,
    /// The permissions boundary that caps them, when the principal carries one whose
    /// document this emulator holds.
    boundary: Option<Vec<iam::PreparedDocument>>,
    /// The session policies the mint of this credential scoped it with.
    scope: sts_store::PreparedScopingPolicies,
}

impl Documents {
    /// Parse everything in force on `caller`.
    fn of(caller: &Caller) -> Self {
        Self {
            identity: iam::prepare_documents(&caller.documents),
            boundary: caller
                .boundary
                .as_ref()
                .map(|boundary| iam::prepare_documents(::std::slice::from_ref(boundary))),
            scope: caller.scope.prepare(),
        }
    }

    /// The permissions boundary in force, or `None` where the principal carries none.
    fn boundary(&self) -> Option<&[iam::PreparedDocument]> {
        self.boundary.as_deref()
    }
}

/// Whether the identity-based documents in force GRANT the request: `said` is what they
/// said about it, and `allows` whether any readable `Allow` among them covers it.
///
/// ONE reading of IAM's order for both decisions below. An explicit `Deny` in a document
/// that CAN be read refuses the request whatever any `Allow` says, so a grant this emulator
/// cannot read ([`Caller::identity_is_unbounded`]) lifts the DEFAULT deny of a request no
/// readable statement allows and nothing more: it never overturns a deny (see the module
/// docs). Taken out of the grant here rather than weighed against it per decision, so the
/// two decisions cannot read one stored deny differently.
fn identity_grants(caller: &Caller, said: Decision, allows: bool) -> bool {
    said != Decision::Denied && (allows || caller.identity_is_unbounded())
}

/// IAM's decision about `action` on `judged`, refused in the name of `named`.
///
/// `named` is what a refusal states the request was refused ON — the resource the request
/// addressed, or `None` where the decision was taken over every resource and there is no
/// resource to name.
fn decide(
    caller: &Caller,
    documents: &Documents,
    asked: Asked<'_>,
    judged: &str,
    named: Option<&str>,
) -> Result<(), Denied> {
    let action = asked.action;
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no
    // such element, so the decision below rests on the action, the resource and the
    // condition keys the request carries.
    let request = iam::Request::new(action, judged, &caller.principals)
        .with_context(asked.conditions)
        .with_published_keys(PUBLISHED_CONDITION_KEYS);
    let identity_said = iam::identity_decision_prepared(&documents.identity, &request);
    let granted = identity_grants(
        caller,
        identity_said,
        iam::is_allowed(identity_said, Decision::NotStated),
    );
    // "The resulting session's permissions are the intersection of the role's
    // identity-based policy and the session policies" (`AssumeRoleRequest$Policy`): the
    // SCOPE the mint of this credential passed caps the grant exactly as the boundary does,
    // and grants nothing of its own. A credential no mint scoped is capped by nothing,
    // which is what the shared reading of a resolved scope answers for it.
    let scoped = documents.scope.decision(&request);
    // "A permissions boundary sets the maximum permissions that an identity-based policy
    // can grant" (IAM, permissions boundaries): a grant the boundary does not also allow
    // is not a permission the principal has. The account's own credential carries none.
    let capped = documents.boundary().map_or(Decision::Allowed, |boundary| {
        iam::identity_decision_prepared(boundary, &request)
    });
    if granted && scoped == Decision::Allowed && capped == Decision::Allowed {
        return Ok(());
    }
    // WHICH document the decision rested on, in the order IAM reads them: an explicit
    // `Deny` in a document that CAN be read refuses the request whatever else the principal
    // carries — including a grant this emulator cannot read, which only lifts the DEFAULT
    // deny of a request no readable statement allows (see the module docs) — and an explicit
    // deny is a different fact, needing a different fix, from a document that merely
    // withheld the grant, so each is named for what it did.
    let reason = if identity_said == Decision::Denied {
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
    let decided_by = decided_by(identity_said, scoped, capped);
    Err(caller.refused(asked, judged, named, &reason, decided_by, &request))
}

/// WHICH document reached the decision, from what each of the three said about the request
/// — in the order [`decide`] words the reason in, so the clause a refusal states and the
/// document its diagnostic names are one reading.
fn decided_by(identity_said: Decision, scoped: Decision, capped: Decision) -> DecidedBy {
    if identity_said == Decision::Denied {
        DecidedBy::IdentityDeny
    } else if scoped == Decision::Denied {
        DecidedBy::SessionPolicyDeny
    } else if capped == Decision::Denied {
        DecidedBy::BoundaryDeny
    } else {
        DecidedBy::NoAllow
    }
}

/// IAM's decision about `action` for a request that addresses no resource this gate could
/// read an ARN for, where the resource a statement must cover is not known.
///
/// Two readings the resource cannot change: a `Deny` covering EVERY resource refuses the
/// request whichever resource it turns out to address, and documents holding no `Allow` of
/// the action at all permit it on no resource
/// ([`::service_common::iam_authorization::identity_may_allow_action`]). Between the two,
/// the request is served — a statement scoped to resource ARNs grants the action on those
/// resources, and withholding the grant here would refuse a call AWS permits. A refusal
/// names no resource, there being none the gate read.
fn decide_over_any_resource(
    caller: &Caller,
    documents: &Documents,
    asked: Asked<'_>,
) -> Result<(), Denied> {
    let action = asked.action;
    let request = iam::Request::new(action, EVERY_RESOURCE, &caller.principals)
        .with_context(asked.conditions)
        .with_published_keys(PUBLISHED_CONDITION_KEYS);
    let identity_said = iam::identity_decision_prepared(&documents.identity, &request);
    let granted = identity_grants(
        caller,
        identity_said,
        iam::identity_may_allow_action(&documents.identity, action),
    );
    // The session policies are read by those same two readings (see the module docs): a
    // `Deny` covering every resource refuses the request, and a scope that allows the
    // action on no resource at all permits it nowhere — the weaker question the shared
    // reading of a resolved scope answers for a resource the gate could not read.
    let scoped = match documents.scope.decision(&request) {
        Decision::Denied => Decision::Denied,
        _ if documents.scope.may_allow_action(action) => Decision::Allowed,
        _ => Decision::NotStated,
    };
    let capped = documents.boundary().map_or(Decision::Allowed, |boundary| {
        if iam::identity_may_allow_action(boundary, action) {
            return Decision::Allowed;
        }
        match iam::identity_decision_prepared(boundary, &request) {
            Decision::Denied => Decision::Denied,
            _ => Decision::NotStated,
        }
    });
    if granted && scoped == Decision::Allowed && capped == Decision::Allowed {
        return Ok(());
    }
    // Which document the decision rested on, in the order [`decide`] names them.
    let reason = if identity_said == Decision::Denied {
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
    let decided_by = decided_by(identity_said, scoped, capped);
    Err(caller.refused(
        asked,
        EVERY_RESOURCE,
        None,
        &reason,
        decided_by,
        &request,
    ))
}

/// Every spelling of the principal this request speaks for: the session ARN
/// `sts:GetCallerIdentity` reports, and the ARN of the IAM role behind an assumed-role
/// session.
///
/// The SAME resolution the authorization gate decides on ([`caller`]), so the principal a
/// refusal blames and the principal a fault injected against a role affects
/// ([`super::injected_faults`]) are one answer and not two: a request signed with a session
/// of a role is the role's request for both questions. Empty for a credential that speaks
/// for nobody.
pub fn request_principals<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Vec<String> {
    caller(engine, ctx).principals
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
struct Caller {
    /// The principal's ARN, as `sts:GetCallerIdentity` reports it — what a refusal names.
    arn: String,
    /// The unique id of that principal, as `sts:GetCallerIdentity` answers it too — the
    /// `AssumedRoleId` of a role session, the `UserId` of an IAM user — which is what
    /// identifies the principal in the decision a refusal's diagnostic carries.
    user_id: String,
    /// Every spelling of the principal: the session ARN STS reports, and the ARN of the
    /// IAM role behind an assumed-role session.
    principals: Vec<String>,
    /// The identity-based policy documents in force on the principal.
    documents: Vec<String>,
    /// The permissions boundary in force on the principal, when it carries one whose
    /// document this emulator holds.
    boundary: Option<String>,
    /// The session policy documents the mint of this request's credential scoped it with,
    /// as they read at the instant of this request. Empty for a credential no mint scoped —
    /// every long-term access key, and every session minted without session policies —
    /// which is a credential capped by nothing.
    scope: sts_store::ScopingPolicies,
    /// The CLASS of the credential the request was signed with, as STS reports it for that
    /// credential (`sts_store::Caller::principal_type`): the value the global condition key
    /// naming the calling principal's type answers with. `None` for a request this gate
    /// resolved no credential for, which carries no value for the key at all.
    principal_type: Option<&'static str>,
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

    /// This caller's refusal of the action `asked` names on `resource`, closed by `reason`.
    ///
    /// `judged` is the resource the decision was TAKEN on and `resource` the one it is stated
    /// ON, which differ for a decision taken over every resource. `decided_by` is the
    /// document that reached it, and `request` the request it was taken about — read once
    /// more here, where a refusal needs the STATEMENTS the decision rested on beside the
    /// decision itself ([`super::authorization_diagnostics`]), which the decision path does
    /// not collect.
    fn refused(
        &self,
        asked: Asked<'_>,
        judged: &str,
        resource: Option<&str>,
        reason: &str,
        decided_by: DecidedBy,
        request: &iam::Request<'_>,
    ) -> Denied {
        Denied {
            principal: self.arn.clone(),
            action: asked.action.to_string(),
            resource: resource.map(str::to_string),
            reason: reason.to_string(),
            diagnosed: Box::new(Diagnosis {
                principal_id: self.user_id.clone(),
                judged: judged.to_string(),
                decided_by,
                conditions: diagnostics::folded_conditions(asked.conditions),
                matched: self.statements_behind(decided_by, request),
            }),
        }
    }

    /// The statements the decision rested on, read off the documents it was taken over:
    /// the session policies for a decision a SESSION POLICY reached, the principal's own
    /// documents for one an identity-based document reached, and none where the decision
    /// came from the absence of a grant, there being no statement to name.
    fn statements_behind(
        &self,
        decided_by: DecidedBy,
        request: &iam::Request<'_>,
    ) -> Vec<sts_store::MatchedStatement> {
        let documents: &[String] = match decided_by {
            DecidedBy::SessionPolicyDeny => &self.scope.documents,
            DecidedBy::IdentityDeny => &self.documents,
            DecidedBy::BoundaryDeny => self.boundary.as_slice(),
            DecidedBy::NoAllow => return Vec::new(),
        };
        let outcome = iam::identity_outcome_prepared(&iam::prepare_documents(documents), request);
        diagnostics::matched_statements(documents, &outcome.matched, &self.user_id)
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's
    /// own accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State,
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

/// The principal this request speaks for, with every policy document in force on the
/// credential it presented.
fn caller<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Caller {
    let access_key_id = ::engine::extract_akid(&ctx.parts().headers);
    // Who the credential speaks for AND what its mint scoped it to, read together from the
    // session registry: the scope travels with the credential, so the only thing a later
    // request carries is where it has to be read from.
    let resolved = match sts_store::service_state(engine) {
        Some(sts) => sts_store::resolve_request_caller(
            &mut sts.lock(),
            &access_key_id,
            SystemTime::now(),
            ctx.credential_store(),
            sts_store::ambient_identity(),
        )
        .map(|caller| {
            (
                caller.principal_type(),
                caller.identity,
                caller.session_policies,
            )
        }),
        // No STS store on this engine: the only identity a request can present is the
        // account's own published credential, which no mint scoped.
        None => Ok((
            ::service_common::temporary_credential_classes::PRINCIPAL_TYPE_USER,
            sts_store::ambient_identity(),
            sts_store::SessionPolicies::default(),
        )),
    };
    let (principal_type, identity, session_policies) = match resolved {
        Ok(resolved) => resolved,
        // The session credential has expired, so it speaks for nobody: no documents, no
        // principal spelling, and every action it asks for falls to the default deny. The
        // key id it presented is NOT quoted back — this module echoes no value of the
        // request — and in the assembled emulator the layer that authenticates a request
        // refuses an expired credential before any handler runs.
        Err(expired) => {
            let _ = expired;
            return Caller {
                arn: EXPIRED_CREDENTIAL_PRINCIPAL.to_string(),
                ..Caller::default()
            };
        }
    };

    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
        principals: vec![identity.arn.clone()],
        arn: identity.arn,
        user_id: identity.user_id,
        // WHICH CLASS of credential signed this request, read through STS's one answer for
        // it: the value `aws:PrincipalType` is decided against.
        principal_type: Some(principal_type),
        // The inline document the mint passed scopes the credential whether or not an IAM
        // store is there; where there is one, the scope is re-read below so a managed
        // session policy counts at the version in force NOW.
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
            caller.user_id = user.user_id.clone();
            caller.principals = vec![user.arn.clone()];
        }
    }
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(&caller.arn)
    {
        // An assumed-role session's permissions are the ROLE's, and the role is addressed
        // by the ARN the account holds it under — which carries its PATH, where the
        // session ARN does not.
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

    /// The decision one stored document reaches about an action, through the shared
    /// evaluator this gate consults.
    fn decision(document: &str, action: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, EVERY_RESOURCE, &[]),
        )
    }

    /// A refusal of `action` for `principal`, stated on `resource` and closed by `reason`,
    /// reached by `decided_by`: the shape the gate builds when it withholds a grant, with
    /// nothing read from a store.
    fn refused_by(
        principal: &str,
        action: &str,
        resource: Option<&str>,
        reason: &str,
        decided_by: DecidedBy,
    ) -> Denied {
        Denied {
            principal: principal.to_string(),
            action: action.to_string(),
            resource: resource.map(str::to_string),
            reason: reason.to_string(),
            diagnosed: Box::new(Diagnosis {
                principal_id: "AROAEXAMPLEROLEID0001:session-1".to_string(),
                judged: resource.unwrap_or(EVERY_RESOURCE).to_string(),
                decided_by,
                conditions: Vec::new(),
                matched: Vec::new(),
            }),
        }
    }

    /// RULE: the action this gate asks about is `ec2:<OperationName>`, so a policy scoped
    /// to this service's actions covers the operations of it and nothing outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_prefix() {
        let document = r#"{"Statement":[{"Effect":"Allow","Action":"ec2:*","Resource":"*"}]}"#;
        for operation in [
            "DescribeVpcs",
            "DescribeSubnets",
            "DescribeRouteTables",
            "DescribeAvailabilityZones",
        ] {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let reached = decision(document, &action);
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected ec2:* to allow {action}, got {reached:?}"
            );
        }
        let outside = decision(document, "ssm:GetParameter");
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected ec2:* to grant nothing outside this \
             service, got {outside:?}"
        );
    }

    /// RULE: a grant scoped to particular resource ARNs does not cover an action IAM judges
    /// over EVERY resource, which is the reason this gate decides those actions alone.
    #[test]
    fn a_resource_scoped_grant_does_not_cover_an_account_wide_action() {
        let scoped = r#"{"Statement":[{"Effect":"Allow","Action":"ec2:DescribeVpcs",
             "Resource":"arn:aws:ec2:us-west-2:000000000000:vpc/vpc-0123456789abcdef0"}]}"#;
        let reached = decision(scoped, "ec2:DescribeVpcs");
        assert_eq!(
            reached,
            Decision::NotStated,
            "case \"a resource-scoped grant\": expected no grant of an account-wide \
             action, got {reached:?}"
        );
    }

    /// RULE: the refusal opens with EC2's fixed sentence and then names the principal, the
    /// action, the resource the decision was taken on and which of IAM's rules withheld the
    /// grant — every part taken from the request and the stored policies. A decision taken
    /// over every resource names none.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_rule() {
        let principal = "arn:aws:sts::000000000000:assumed-role/cdk-lookup/session-1";
        let action = format!("{ACTION_PREFIX}:DescribeVpcs");
        // (case, the clause the decision closed with)
        let rows: &[(&str, String)] = &[
            (
                "an explicit deny",
                "with an explicit deny in an identity-based policy".to_string(),
            ),
            (
                "no statement at all",
                format!("because no identity-based policy allows the {action} action"),
            ),
        ];
        for (case, reason) in rows {
            let refusal = refused_by(principal, &action, None, reason, DecidedBy::IdentityDeny);
            let message = refusal.message();
            assert_eq!(
                message,
                format!(
                    "{NOT_AUTHORIZED_SENTENCE} User: {principal} is not authorized to \
                     perform: {action} {reason}"
                ),
                "case {case:?}: expected EC2's wording, got {message:?}"
            );
            assert!(
                !message.contains("on resource"),
                "case {case:?}: an action judged over every resource names none, got \
                 {message:?}"
            );
        }

        // A decision taken on a resource the request addressed names it, and closes the
        // sentence.
        let action = format!("{ACTION_PREFIX}:CreateNetworkInterface");
        let resource = ::engine::emulator_arn("ec2", "us-west-2", "subnet/subnet-1a2b3c4d");
        let reason = iam::no_identity_policy_allows(&action);
        let refusal = refused_by(
            principal,
            &action,
            Some(&resource),
            &reason,
            DecidedBy::NoAllow,
        );
        let message = refusal.message();
        assert_eq!(
            message,
            format!(
                "{NOT_AUTHORIZED_SENTENCE} User: {principal} is not authorized to perform: \
                 {action} on resource: {resource} {reason}."
            ),
            "case \"a resource-level action\": expected the resource to be named, got \
             {message:?}"
        );
    }

    /// RULE: the actions IAM judges over every resource — the ones that support no
    /// resource-level permissions, the `Describe` family — are the ones whose grant must
    /// cover every resource; every other action is judged against the resources the request
    /// addresses.
    #[test]
    fn the_describe_family_is_what_iam_judges_over_every_resource() {
        // (the operation, whether IAM judges its action over every resource)
        let rows: &[(&str, bool)] = &[
            ("DescribeVpcs", true),
            ("DescribeSubnets", true),
            ("DescribeRouteTables", true),
            ("DescribeAvailabilityZones", true),
            ("DescribeInstances", true),
            ("CreateVpc", false),
            ("RunInstances", false),
            ("CreateTags", false),
            ("GetConsoleOutput", false),
            ("ModifyVpcAttribute", false),
        ];
        for (operation, expected) in rows {
            let decided = judged_over_every_resource(operation);
            assert_eq!(
                decided, *expected,
                "case {operation:?}: expected judged_over_every_resource {expected}, got \
                 {decided}"
            );
        }
    }

    /// RULE: an action judged on a resource it CREATES is decided on that resource type with
    /// every id, in the Region the request was signed for — the request carries no id for a
    /// resource that does not exist yet — and every other action is decided on the resources
    /// the request names, so it asks for none of its own.
    ///
    /// The rows span the Region the request addressed and the kinds of action either side of
    /// the catalog: the launch that creates its resource, a create whose recorded refusal
    /// names a resource the request DOES name, and a read.
    #[test]
    fn an_action_judged_on_what_it_creates_is_decided_on_that_resource_type() {
        // (the operation, the Region, the resource the decision is taken on)
        let rows: &[(&str, &str, Option<String>)] = &[
            (
                "RunInstances",
                "us-west-2",
                Some(::engine::emulator_arn(
                    ACTION_PREFIX,
                    "us-west-2",
                    "instance/*",
                )),
            ),
            (
                "RunInstances",
                "eu-central-1",
                Some(::engine::emulator_arn(
                    ACTION_PREFIX,
                    "eu-central-1",
                    "instance/*",
                )),
            ),
            (
                "CreateVolume",
                "us-west-2",
                Some(::engine::emulator_arn(
                    ACTION_PREFIX,
                    "us-west-2",
                    "volume/*",
                )),
            ),
            ("CreateNetworkInterface", "us-west-2", None),
            ("CreateVpc", "us-west-2", None),
            ("DescribeInstances", "us-west-2", None),
        ];
        for (operation, region, expected) in rows {
            let created = created_resource(operation, region);
            assert_eq!(
                &created, expected,
                "case {operation:?} in {region}: expected {expected:?}, got {created:?}"
            );
        }
    }

    /// RULE: the refusals of the operations whose recorded contract hands the caller an
    /// encoded authorization failure message carry one — whichever document in force reached
    /// the decision — and the refusals of every other operation carry none. The decision the
    /// blob names is the one the gate took: not allowed, explicitly denied exactly when a
    /// `Deny` said so, about the principal, the BARE operation name, the resource the
    /// decision was taken on and the keys it was taken over.
    #[test]
    fn the_operations_recorded_with_a_trailer_carry_the_decision_to_decode() {
        let principal = "arn:aws:sts::000000000000:assumed-role/launcher/session-1";
        let resource = ::engine::emulator_arn(ACTION_PREFIX, "us-west-2", "instance/*");
        // (case, the operation refused, the document the decision rested on, whether a blob
        // is handed over, whether the decision it names is an explicit deny)
        let rows: &[(&str, &str, DecidedBy, bool, bool)] = &[
            (
                "a launch denied by a session policy",
                "RunInstances",
                DecidedBy::SessionPolicyDeny,
                true,
                true,
            ),
            (
                "a launch denied by an identity-based policy",
                "RunInstances",
                DecidedBy::IdentityDeny,
                true,
                true,
            ),
            (
                "a launch denied by the permissions boundary",
                "RunInstances",
                DecidedBy::BoundaryDeny,
                true,
                true,
            ),
            (
                "a launch no document allows at all",
                "RunInstances",
                DecidedBy::NoAllow,
                true,
                false,
            ),
            (
                "a volume create no document allows at all",
                "CreateVolume",
                DecidedBy::NoAllow,
                true,
                false,
            ),
            (
                "an interface create, whose recorded refusal carries no blob",
                "CreateNetworkInterface",
                DecidedBy::NoAllow,
                false,
                false,
            ),
            (
                "the same interface create under an explicit deny",
                "CreateNetworkInterface",
                DecidedBy::SessionPolicyDeny,
                false,
                false,
            ),
            (
                "an account-wide read",
                "DescribeInstances",
                DecidedBy::IdentityDeny,
                false,
                false,
            ),
        ];
        for (case, operation, decided_by, carries, explicit) in rows {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let refusal = refused_by(
                principal,
                &action,
                Some(&resource),
                iam::SESSION_POLICY_EXPLICIT_DENY,
                *decided_by,
            );
            let decision = refusal.encoded_message_decision(operation);
            assert_eq!(
                decision.is_some(),
                *carries,
                "case {case:?}: expected a decision to encode: {carries}, got {decision:?}"
            );
            let Some(decision) = decision else {
                continue;
            };
            assert_eq!(
                (
                    decision.allowed,
                    decision.explicit_deny,
                    decision.principal_arn.as_str(),
                    decision.action.as_str(),
                    decision.resource.as_str(),
                ),
                (
                    false,
                    *explicit,
                    principal,
                    *operation,
                    resource.as_str()
                ),
                "case {case:?}: expected the decision the gate took, got {decision:?}"
            );
        }
    }
}
