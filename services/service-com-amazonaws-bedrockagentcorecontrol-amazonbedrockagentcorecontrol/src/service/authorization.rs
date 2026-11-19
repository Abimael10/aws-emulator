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
//! from the request line — before any input is deserialized. There are two such kinds of
//! request, and a request of neither kind falls through this gate untouched: a resource
//! family whose ARN this layer cannot name from the request line is not one it may guess
//! at, since judging an action against the wrong resource would refuse a call a scoped
//! statement allows.
//!
//! **One MEMBER of a resource family**: a route whose modelled `@http` URI carries an
//! identifier label, where the member's ARN is that family's ARN type carrying the
//! identifier the request's own segment at the label's index holds
//! ([`member_arn`], each family's own `*Arn` pattern as `crate::state` reads it). The
//! identifier is decided AS CARRIED: no statement scoped to one resource's ARN covers an
//! identifier no resource of the account answers to, while a statement over every resource
//! still does — and the caller learns nothing from the verdict about whether that resource
//! exists, since the decision is taken before any handler resolves it. A route carrying
//! several labels addresses a CHILD, which is addressed only through its parent, so the
//! parent the first label names is what the decision is taken against.
//!
//! **A family's WHOLE COLLECTION**: a route whose modelled URI carries no label at all
//! addresses no single resource — a create mints one the request cannot name yet, a listing
//! addresses the family — so the resource it is judged against is the family's collection:
//! the family's own ARN with [`ANY_RESOURCE_OF_THE_FAMILY`] in place of the identifier,
//! named by the ROUTE rather than by anything the caller carried ([`collection_arn`]), which
//! is the resource AWS's own recorded refusal of a create of this endpoint names. A route of
//! a family this layer cannot name one ARN for — one addressed under two ARN types (a
//! built-in browser or code interpreter beside a customer-created one), or one nested under
//! a parent the route does not name — resolves to no resource here, like every other unnamed
//! family.
//!
//! ## How the refusal is worded
//!
//! A refusal states the principal and the action it asked for; whether it goes on to name
//! the resource and which of IAM's rules withheld the grant follows the resource the decision
//! was taken against and the BACKEND that serves it ([`Addressed`]). A collection's ARN is
//! the route's own, so the sentence names it and the rule, which is the recorded refusal of a
//! listing and of a create of this endpoint; a member's is named the same way, except for the
//! families whose own recorded refusal closes at the action
//! ([`ACTION_ONLY_REFUSAL_FAMILIES`], [`not_authorized`]).
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

use ::aws_smithy_schema::Schema;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use smithy_server_runtime::{OperationContext, Refused};

/// The namespace a policy statement spells this endpoint's actions under, which is also its
/// SigV4 signing name and its ARN namespace: `bedrock-agentcore` for all three
/// (`smithy_query(query="service_info")` — `sigv4Name`, `awsService.arnNamespace`).
pub const ACTION_PREFIX: &str = "bedrock-agentcore";

/// What opens an `@http` URI's LABEL segment, the one thing that tells a template's
/// literal segments from the identifier a request carries in its place.
const LABEL_OPEN: char = '{';

/// What closes it.
const LABEL_CLOSE: char = '}';

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The identifier a family's COLLECTION is named by: the any-resource wildcard, which is what
/// a listing addresses rather than one resource of the family (see the module docs).
const ANY_RESOURCE_OF_THE_FAMILY: &str = "*";

/// The resource families whose refusal of a call naming ONE MEMBER closes at the ACTION,
/// stating neither the resource nor the rule that withheld the grant.
///
/// This endpoint fronts several AgentCore backends, one per resource family, and they do not
/// word a refusal alike — the same split this service's validation wording is keyed by
/// ([`super::validation`]). The recorded refusal of a runtime read carries neither clause,
/// while the recorded refusal of a gateway's delete — and of every family-wide call — states
/// the resource and the rule, which is the tree's shared sentence
/// ([`iam::not_authorized_by_identity_policy`]). A family not listed here keeps that shared
/// sentence.
const ACTION_ONLY_REFUSAL_FAMILIES: &[&str] = &["runtimes"];

/// The IAM action `operation` is performed under: `bedrock-agentcore:<OperationName>`.
#[must_use]
pub fn action_for(operation: &str) -> String {
    format!("{ACTION_PREFIX}:{operation}")
}

/// The modelled `AccessDeniedException` for a call this endpoint denies on the resource it
/// addressed, blaming the principal that signed and the action it asked for.
///
/// For the decisions a HANDLER takes rather than this gate: ones that rest on the resource
/// the request named, which this layer cannot resolve from the request line. The sentence is
/// the same one ([`not_authorized`]), so a client reads one wording for every denial of this
/// endpoint whichever layer reached it, and the principal is read from the same place
/// `sts:GetCallerIdentity` answers from ([`request_principal`]).
///
/// Call it with no guard of this service's own store held — it reads the identity stores.
#[must_use]
pub fn access_denied<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
) -> crate::ops::types::AccessDeniedException {
    crate::ops::types::AccessDeniedException {
        message: Some(not_authorized(
            &request_principal(engine, ctx),
            &action_for(operation),
        )),
    }
}

/// The principal this request speaks for, as `sts:GetCallerIdentity` reports it — the value
/// a refusal of this endpoint blames.
///
/// The one resolution this crate has of "who is calling" ([`caller`]), so a handler that
/// must name the principal names the same one this gate would.
#[must_use]
pub fn request_principal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
) -> String {
    caller(engine, ctx).arn
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
    input_schema: &Schema,
) -> Option<Refused> {
    let addressed = addressed(ctx, input_schema)?;
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
    let request = iam::Request::new(&action, addressed.arn(), &[]);
    let withheld = withheld(&caller, &request, &action)?;
    ::tracing::info!(
        operation = %operation,
        action = %action,
        principal = %caller.arn,
        "iam does not allow this caller to perform this action"
    );
    Some(Refused {
        code: crate::state::ACCESS_DENIED_SHAPE.to_string(),
        message: addressed.refused(&caller.arn, &action, &withheld),
    })
}

/// Which of IAM's rules withheld the grant for `request`, and `None` when IAM allows it — in
/// IAM's own evaluation order (see [`refusal`]).
///
/// The clauses are the shared ones the tree states once, so the rule a refusal reports is
/// worded the same way wherever the same rule withheld the grant.
fn withheld(caller: &Caller, request: &iam::Request<'_>, action: &str) -> Option<String> {
    let said = iam::identity_decision(&caller.documents, request);
    if said == Decision::Denied {
        return Some(iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string());
    }
    if !(iam::is_allowed(said, Decision::NotStated) || caller.identity_is_unbounded()) {
        return Some(iam::no_identity_policy_allows(action));
    }
    match caller.boundary.as_ref() {
        None => None,
        Some(boundary) => {
            match iam::identity_decision(::std::slice::from_ref(boundary), request) {
                Decision::Allowed => None,
                Decision::Denied => Some(iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()),
                _ => Some(iam::no_permissions_boundary_allows(action)),
            }
        }
    }
}

/// The sentence this endpoint refuses an unauthorized call addressing ONE MEMBER of a
/// family whose backend closes at the ACTION with: the principal that signed and the action
/// it asked for, as the request and the signing identity carry them.
///
/// It states neither the resource nor the rule. The tree's shared sentences
/// ([`iam::not_authorized_to_perform`] and the family beside it) all go on to name the
/// resource, and [`iam::not_authorized_by_identity_policy`] names WHICH of IAM's rules
/// withheld the grant as well — which is the sentence every other resource of this endpoint
/// is refused in ([`Addressed::refused`]); the recorded refusal of a runtime read carries
/// neither clause, so the two fixed fragments here are all of its fixed text and the only
/// values in it come from the request and the signing identity.
#[must_use]
pub fn not_authorized(principal: &str, action: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action}")
}

/// The ARN of the resource this request addresses, when this layer can name it from the
/// request line, and `None` for every other request (see the module docs).
///
/// Read off the operation's OWN modelled `@http` URI rather than off a list of operation
/// names, so every route of a family this layer names an ARN type for is judged, and one
/// added to the model later is judged the same way:
///
/// * a template carrying an identifier LABEL addresses one member of the family that label
///   names ([`member_arn`]); the identifier is the request's own segment at the label's
///   index, percent-decoded with PATH semantics through the shared reader that states that
///   rule once for the whole tree — the same reading the codec gives the label, so the gate
///   and the operation see one id. A route carrying SEVERAL labels addresses a child
///   (`/runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/`), and a child is
///   addressed only THROUGH the parent the first label names, so the parent is what the
///   decision is taken against;
/// * a template carrying NO label addresses the family as a whole — a create, or the
///   family's listing — and is judged against that family's collection
///   ([`collection_arn`]).
fn addressed(ctx: &OperationContext<'_>, input_schema: &Schema) -> Option<Addressed> {
    let parts = ctx.parts();
    let region = crate::state::request_region(parts);
    let template = input_schema.http().map(|http| http.uri())?;
    let family = path_segment(template, 0)?;
    let Some((index, label)) = first_label(template) else {
        return collection_arn(family, &region).map(Addressed::Collection);
    };
    let uri = parts.uri.to_string();
    let carried = ::service_common::percent_decode_path_label(path_segment(
        ::service_common::uri_path(&uri),
        index,
    )?);
    (!carried.is_empty())
        .then(|| member_arn(label, &carried, &region))
        .flatten()
        .map(|arn| Addressed::Member {
            arn,
            states_resource: !ACTION_ONLY_REFUSAL_FAMILIES.contains(&family),
        })
}

/// The `index`-th non-empty segment of a path, which for an `@http` URI template is the
/// segment at the same index of the request path it binds.
fn path_segment(path: &str, index: usize) -> Option<&str> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .nth(index)
}

/// The FIRST `@httpLabel` of a modelled URI template: the index of its segment and the
/// member name it binds, or `None` for a template that carries no label at all.
fn first_label(template: &str) -> Option<(usize, &str)> {
    template
        .split('/')
        .filter(|segment| !segment.is_empty())
        .enumerate()
        .find_map(|(index, segment)| {
            let label = segment
                .strip_prefix(LABEL_OPEN)?
                .strip_suffix(LABEL_CLOSE)?
                // A greedy label (`{name+}`) binds the rest of the path; the member it
                // names is the same one.
                .trim_end_matches('+');
            Some((index, label))
        })
}

/// The ARN of the one resource an identifier LABEL names, from the identifier the request
/// carried for it, and `None` for a label naming a family this layer cannot name one ARN
/// for.
///
/// One row per label whose family has a single ARN type with the identifier as its only
/// variable part, built by the SAME function that names that resource everywhere else
/// (`crate::state`'s `*_arn` readings of each `*Arn` pattern), so the resource a statement
/// is read for is spelled exactly as the resource's own responses spell it. `resourceArn`
/// is the resource ITSELF — the tagging and resource-policy routes address a resource by
/// its ARN — so there is nothing to build.
///
/// The labels left out name families this layer must not guess an ARN for: a browser and a
/// code interpreter are each addressed under two ARN types (built-in and customer-created),
/// and an evaluator has no ARN type in this crate at all. A request of one of those falls
/// through the gate untouched, as every unnamed resource does — judging an action against
/// the wrong resource would refuse a call a scoped statement allows.
fn member_arn(label: &str, id: &str, region: &str) -> Option<String> {
    Some(match label {
        "agentRuntimeId" => crate::state::runtime_arn(region, id),
        "gatewayIdentifier" => crate::state::gateway_arn(region, id),
        "memoryId" => crate::state::memory_arn(region, id),
        "harnessId" => crate::state::harness_arn(region, id),
        "registryId" => crate::state::registry_arn(region, id),
        "datasetId" => crate::state::dataset_arn(region, id),
        "bundleId" => crate::state::configuration_bundle_arn(region, id),
        "profileId" => crate::state::browser_profile_arn(region, id),
        "policyEngineId" => crate::state::policy_engine_arn(region, id),
        "capacityProviderId" => crate::state::capacity_provider_arn(region, id),
        "paymentManagerId" => crate::state::payment_manager_arn(region, id),
        "onlineEvaluationConfigId" => crate::state::online_evaluation_config_arn(region, id),
        "resourceArn" => id.to_string(),
        _ => return None,
    })
}

/// The collection ARN a request is judged against when it addresses a resource family's
/// WHOLE collection rather than one member of it — a create, or the family's listing — and
/// `None` for a family this layer cannot name one ARN for.
///
/// Keyed by the FIRST segment of the operation's modelled `@http` URI, the segment that
/// names the family (`OPERATIONS.md`), and built by the same `crate::state` readings
/// [`member_arn`] uses, with [`ANY_RESOURCE_OF_THE_FAMILY`] in place of the identifier: a
/// create mints a resource the request cannot name yet, and a listing addresses no single
/// resource, so both are judged against the family's own pattern — which is the resource
/// AWS's own recorded refusal of a create of this endpoint names.
///
/// The families left out are the ones with no single ARN type (`browsers`,
/// `code-interpreters`), the ones whose resources are nested under a parent the route does
/// not name (`identities`, whose providers hang off a token vault) and the ones this crate
/// builds no ARN for (`evaluators`).
fn collection_arn(family: &str, region: &str) -> Option<String> {
    let any = ANY_RESOURCE_OF_THE_FAMILY;
    Some(match family {
        "runtimes" => crate::state::runtime_arn(region, any),
        "gateways" => crate::state::gateway_arn(region, any),
        "memories" => crate::state::memory_arn(region, any),
        "registries" => crate::state::registry_arn(region, any),
        "datasets" => crate::state::dataset_arn(region, any),
        "harnesses" => crate::state::harness_arn(region, any),
        "configuration-bundles" => crate::state::configuration_bundle_arn(region, any),
        "browser-profiles" => crate::state::browser_profile_arn(region, any),
        "policy-engines" | "policy-engine-summaries" => {
            crate::state::policy_engine_arn(region, any)
        }
        "capacity-providers" => crate::state::capacity_provider_arn(region, any),
        "online-evaluation-configs" => crate::state::online_evaluation_config_arn(region, any),
        "payments" => crate::state::payment_manager_arn(region, any),
        _ => return None,
    })
}

/// The resource this request's authorization decision is taken against, and with it how a
/// refusal naming that resource is worded (see the module docs).
#[derive(Debug, Clone)]
enum Addressed {
    /// ONE member of a resource family, addressed by an identifier the request carried.
    Member {
        arn: String,
        /// Whether the family's own refusal goes on to state that resource and the rule
        /// that withheld the grant ([`ACTION_ONLY_REFUSAL_FAMILIES`]).
        states_resource: bool,
    },
    /// A resource family's WHOLE collection, named by the route the request came in on.
    Collection(String),
}

impl Addressed {
    /// The ARN the decision is taken against.
    fn arn(&self) -> &str {
        match self {
            Self::Member { arn, .. } | Self::Collection(arn) => arn,
        }
    }

    /// The sentence a refusal of this request states: the principal that signed and the
    /// action it asked for, and — for every resource but a member of a family whose own
    /// backend closes at the action — the resource and `withheld`, the rule that withheld
    /// the grant.
    fn refused(&self, principal: &str, action: &str, withheld: &str) -> String {
        match self {
            Self::Member {
                states_resource: false,
                ..
            } => not_authorized(principal, action),
            Self::Member { arn, .. } | Self::Collection(arn) => {
                iam::not_authorized_by_identity_policy(principal, action, arn, withheld)
            }
        }
    }
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

    /// RULE: a request that addresses a family as a whole — a create, which mints a resource
    /// it cannot name yet, or a listing, which names none — is judged against the family's
    /// own ARN carrying the any-resource wildcard in place of the identifier, in the signed
    /// Region; a request that names one member is judged against that member's ARN.
    #[test]
    fn a_family_wide_request_is_judged_against_its_family_collection_arn() {
        let region = "eu-west-3";
        let account = ::engine::emulator_account_id();
        // (the family segment of the route's modelled `@http` URI, the collection ARN that
        // family is addressed under). Source: each family's `*Arn` pattern as
        // `crate::state` states it, and `OPERATIONS.md` for the routes.
        let rows: &[(&str, String)] = &[
            (
                "runtimes",
                format!("arn:aws:bedrock-agentcore:{region}:{account}:runtime/*"),
            ),
            (
                "gateways",
                format!("arn:aws:bedrock-agentcore:{region}:{account}:gateway/*"),
            ),
            (
                "memories",
                format!("arn:aws:bedrock-agentcore:{region}:{account}:memory/*"),
            ),
            (
                "browser-profiles",
                format!("arn:aws:bedrock-agentcore:{region}:{account}:browser-profile/*"),
            ),
            (
                "harnesses",
                format!("arn:aws:bedrock-agentcore:{region}:{account}:harness/*"),
            ),
        ];
        for (family, expected) in rows {
            let reached = collection_arn(family, region);
            assert_eq!(
                reached.as_deref(),
                Some(expected.as_str()),
                "case {family:?}: expected {expected:?}, got {reached:?}"
            );
        }
        // A family this layer cannot name one ARN for is judged against no collection: a
        // browser and a code interpreter each carry two ARN types, and a credential provider
        // hangs off a token vault the route does not name.
        for family in ["browsers", "code-interpreters", "identities", "evaluators"] {
            let reached = collection_arn(family, region);
            assert_eq!(
                reached, None,
                "case {family:?}: expected no collection resource, got {reached:?}"
            );
        }
        // And one member of a family is judged against that member's own ARN, by the label
        // the route carries it in.
        let member = member_arn("gatewayIdentifier", "probe-abcdefghij", region);
        assert_eq!(
            member.as_deref(),
            Some(crate::state::gateway_arn(region, "probe-abcdefghij").as_str()),
            "case \"one gateway\": expected the gateway's own ARN, got {member:?}"
        );
        let unnamed = member_arn("browserId", "probe-abcdefghij", region);
        assert_eq!(
            unnamed, None,
            "case \"a family with two ARN types\": expected no resource, got {unnamed:?}"
        );
    }

    /// RULE: the resource is read off the route's own modelled URI template, so the label a
    /// route carries decides which resource the decision is taken against and a template
    /// with no label addresses the family.
    #[test]
    fn the_resource_is_read_off_the_routes_own_uri_template() {
        // (the modelled `@http` URI, the label the gate reads and the segment index it is
        // carried at). Source: `OPERATIONS.md`.
        let rows: &[(&str, Option<(usize, &str)>)] = &[
            ("/runtimes/{agentRuntimeId}/", Some((1, "agentRuntimeId"))),
            (
                "/runtimes/{agentRuntimeId}/runtime-endpoints/{endpointName}/",
                Some((1, "agentRuntimeId")),
            ),
            ("/gateways/{gatewayIdentifier}/", Some((1, "gatewayIdentifier"))),
            ("/memories/{memoryId}/details", Some((1, "memoryId"))),
            (
                "/payments/managers/{paymentManagerId}",
                Some((2, "paymentManagerId")),
            ),
            ("/tags/{resourceArn}", Some((1, "resourceArn"))),
            ("/runtimes/", None),
            ("/memories/create", None),
            ("/harnesses", None),
        ];
        for (template, expected) in rows {
            let reached = first_label(template);
            assert_eq!(
                reached, *expected,
                "case {template:?}: expected {expected:?}, got {reached:?}"
            );
        }
    }

    /// RULE: a refusal of a family-wide request names the collection the decision was taken
    /// against and the rule that withheld the grant, where a refusal of one member of a
    /// family closes at the action.
    #[test]
    fn the_refusal_of_a_listing_names_the_collection_and_the_rule() {
        let principal = "arn:aws:sts::000000000000:assumed-role/reader/session-1";
        let action = action_for("ListAgentRuntimes");
        let collection = crate::state::runtime_arn("us-east-1", ANY_RESOURCE_OF_THE_FAMILY);
        let withheld = iam::no_identity_policy_allows(&action);
        let sentence =
            Addressed::Collection(collection.clone()).refused(principal, &action, &withheld);
        let expected = format!(
            "User: {principal} is not authorized to perform: {action} on resource: \
             {collection} because no identity-based policy allows the {action} action"
        );
        assert_eq!(
            sentence, expected,
            "case \"a listing's refusal\": expected {expected:?}, got {sentence:?}"
        );
        let member = Addressed::Member {
            arn: crate::state::runtime_arn("us-east-1", "probe-abcdefghij"),
            states_resource: false,
        }
        .refused(principal, &action, &withheld);
        assert_eq!(
            member,
            not_authorized(principal, &action),
            "case \"a runtime member's refusal\": got {member:?}"
        );
        // Every other family's backend states the resource and the rule for a member too,
        // which is the recorded refusal of a gateway's delete.
        let gateway = crate::state::gateway_arn("us-east-1", "probe-abcdefghij");
        let named = Addressed::Member {
            arn: gateway.clone(),
            states_resource: true,
        }
        .refused(principal, &action, &withheld);
        let expected = format!(
            "User: {principal} is not authorized to perform: {action} on resource: \
             {gateway} because no identity-based policy allows the {action} action"
        );
        assert_eq!(
            named, expected,
            "case \"a gateway member's refusal\": expected {expected:?}, got {named:?}"
        );
    }
}
