// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the decision
//! taken BEFORE the operation it names is dispatched.
//!
//! AWS decides authorization at the IAM layer, in front of the service, so a caller whose
//! identity-based policies do not grant the action — or that an explicit `Deny` covers — is
//! told it is not authorized rather than told anything about the resource it named, and the
//! call performs nothing: no agent, knowledge base, flow or prompt is minted, no status
//! moves, no ingestion starts and nothing is read back. The refusal is the modelled
//! `AccessDeniedException` (HTTP 403), which this model declares as a SERVICE-COMMON error
//! (`smithy_query(query="service_info")` lists it under `common_errors`), so it is the
//! answer every one of this service's operations gives such a caller; it is rendered here
//! ([`AccessDenied`]) because the decision is taken ahead of the operation that would
//! otherwise return it.
//!
//! The decision is a property of the request's identity, the action it names and the
//! resource it addresses rather than of what a handler goes on to do, so it is taken ONCE —
//! in the layer every operation is served behind ([`Gated`], wired in
//! `crate::service::EmulatedService::override_operation`) — and nowhere else. Every
//! operation this service publishes is judged, implemented or still a scaffold: an
//! operation's modelled errors are the ones the SERVICE raises once it is reached, and a
//! caller IAM refuses never reaches it, so leaving an operation out would serve an
//! unauthorized caller the very resources the policy withholds.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for (`sts_store::resolve_caller`, the same record `sts:GetCallerIdentity`
//!    answers from, so the principal a refusal blames is the one the caller reads back for
//!    the credentials it signed with), else the IAM user that owns the long-term key, else
//!    the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    own accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next request and deleting it lifts it
//!    again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default deny.
//!    The refusal's WORDING comes from that module too, for the same reason the decision
//!    does.
//!
//! Only the store LOOKUPS live here; neither the decision nor its sentence does. They stay
//! in this crate because hoisting them into `service-common` would make every service that
//! depends on that crate link the IAM and STS service crates.
//!
//! ## The action and the resource
//!
//! The action is `bedrock:<OperationName>` ([`ACTION_PREFIX`] is this service's
//! `arnNamespace`, which the shared Bedrock-family reading of this family's ARNs already
//! states once), which is how a policy statement spells one of this service's actions.
//!
//! The resource is the one the REQUEST addresses ([`judged_resource`]), spelled with the
//! resource types this plane's own ARNs carry: this model binds every operation to a URI
//! whose first segment is the KIND of resource it is about and whose second, where it has
//! one, is that resource's own identifier (`smithy_query(query="digest")`), so a call on one
//! resource is judged against that resource's ARN and one that addresses a collection of
//! them — a create, a listing — against [`EVERY_RESOURCE`], which is decided on the weaker
//! question the shared evaluator publishes for exactly that case
//! ([`::service_common::iam_authorization::identity_may_allow_action`]): whether any
//! readable document could allow the action at all, so a statement scoped to one resource
//! still permits the calls it covers. Reading it from the request rather than from a stored
//! record is what keeps a refused call from telling an unauthorized caller whether what it
//! named exists.
//!
//! The two resource-policy operations that WRITE one — `PutResourcePolicy` and
//! `DeleteResourcePolicy` — are still scaffolded (501), so this emulator holds no
//! resource-based policy for any resource of this service and the identity-based decision is
//! the whole decision; a document one of them stores later is read beside the caller's own,
//! as every gate in this tree that holds one does.
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
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no document and no principal
//! spelling, so every action it asks for is refused, and the refusal names that state rather
//! than the key it presented, which this module never quotes back.
//!
//! LOCK ORDER: the STS registry's lock and then the IAM store's, each taken and released in
//! turn BEFORE this service's own store lock is taken — the decision is reached ahead of
//! dispatch, so no handler of this service holds its store while this runs.

use ::std::marker::PhantomData;
use ::std::sync::Arc;
use ::std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_types::body::SdkBody;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};
use ::smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, ServerHttpProtocol, Service,
};

/// This service's IAM action prefix: an action is `bedrock:<OperationName>`, under this
/// service's `arnNamespace` (`smithy_query(query="service_info")`), which the shared reading
/// of this family's ARNs already states once.
pub const ACTION_PREFIX: &str = ::service_common::bedrock::ARN_NAMESPACE;

/// The resource a request that addresses no single resource of its own is decided about, as
/// IAM writes it for such a request: every resource.
pub const EVERY_RESOURCE: &str = "*";

/// The wire error code this gate answers with, and its status: this model's own
/// `AccessDeniedException` (`smithy_query(query="shape", shape="AccessDeniedException")` ->
/// `httpError` 403), which is also the pair AWS answers a request IAM does not permit with
/// across services.
pub const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The media type this restJson1 service renders a body in.
const JSON_CONTENT_TYPE: &str = "application/json";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The ARN prefix a value that is already a whole ARN opens with: several identifier labels
/// of this model admit a resource's NAME or its own ARN, and an ARN is judged as itself
/// rather than spelled into another one.
const ARN_PREFIX: &str = "arn:";

/// The one route of this model whose second path segment is a literal rather than a
/// resource's identifier (`POST /flows/validate-definition`), so the call addresses no flow.
const FLOW_VALIDATION_SEGMENT: &str = "validate-definition";

/// The `message` member of this model's `AccessDeniedException`
/// (`smithy_query(query="shape", shape="AccessDeniedException")`), so a client reads the
/// sentence out of an authorization refusal at the member the model declares for it.
static ACCESS_DENIED_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#AccessDeniedException$message",
        "smithy.framework",
        "message",
    ),
    ShapeType::String,
    "message",
    0,
);

/// Schema for the `AccessDeniedException` this gate renders.
///
/// In the `smithy.framework` namespace deliberately: the shape the gate SERIALIZES is not a
/// shape of an operation's error channel — the decision is taken ahead of the operation —
/// and must not collide with the modelled one. The wire code and the status are
/// [`ACCESS_DENIED_EXCEPTION`] / [`ACCESS_DENIED_STATUS`], the model's own, passed to the
/// serializer rather than read off this schema.
static ACCESS_DENIED_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "smithy.framework#AccessDeniedException",
        "smithy.framework",
        "AccessDeniedException",
    ),
    ShapeType::Structure,
    &[&ACCESS_DENIED_MESSAGE],
);

/// The payload of an `AccessDeniedException`: who was refused, which action, on which
/// resource, and which of IAM's rules refused it.
#[derive(Debug, Clone)]
pub struct AccessDenied {
    pub message: String,
}

impl SerializableStruct for AccessDenied {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&ACCESS_DENIED_MESSAGE, self.message.as_str())?;
        Ok(())
    }
}

/// The resource TYPE a request addressing the URI space `space` is judged under, or `None`
/// for a space whose target is named by an ARN the request itself carries, or that names no
/// resource of this service.
///
/// Every space this model binds is mapped here, from `smithy_query(query="digest")`: the
/// agent spaces (the agent, its versions, aliases, action groups, collaborators and the
/// knowledge bases associated with it) all hang under `/agents/{agentId}`, so a call on any
/// of them is judged against the AGENT it is about; `/knowledgebases/{knowledgeBaseId}` and
/// everything under it (its data sources, documents, ingestion jobs and VPC configurations)
/// against the KNOWLEDGE BASE; `/flows/{flowIdentifier}` and its versions and aliases
/// against the FLOW; `/prompts/{promptIdentifier}` and its versions against the PROMPT; and
/// `/retrievalinstructions/{retrievalInstructionId}` against the RETRIEVAL INSTRUCTION.
/// Each type is spelled as this plane's own ARNs spell it (the `TaggableResourcesArn`
/// expression of this model enumerates `agent`, `agent-alias`, `knowledge-base`, `flow`,
/// `prompt` and `retrieval-instruction`).
fn resource_type_of(space: &str) -> Option<&'static str> {
    match space {
        "agents" => Some("agent"),
        "knowledgebases" => Some("knowledge-base"),
        "flows" => Some("flow"),
        "prompts" => Some("prompt"),
        "retrievalinstructions" => Some("retrieval-instruction"),
        _ => None,
    }
}

/// The URI spaces whose target is named by an ARN the request carries in the path rather
/// than by an identifier: `/tags/{resourceArn}` and `/resourcepolicy/{resourceArn}`
/// (`smithy_query(query="digest")`).
const ARN_IN_PATH_SPACES: &[&str] = &["tags", "resourcepolicy"];

/// The ARN the request ADDRESSES, which is what the action is judged against.
///
/// Read from the REQUEST rather than from a stored record, so the decision does not depend
/// on whether that resource exists — which is also what keeps a refused call from telling an
/// unauthorized caller whether it does.
#[must_use]
pub fn judged_resource(ctx: &OperationContext<'_>, req: &RtRequest) -> String {
    let region = crate::state::request_region(ctx.parts());
    let path = ::service_common::request_path(req.uri());
    let segment = |index: usize| {
        ::service_common::nth_path_segment(path, index)
            .filter(|segment| !segment.is_empty())
            .map(::service_common::percent_decode_path_label)
    };
    let space = segment(0).unwrap_or_default();
    // A space whose target IS an ARN carries nothing to spell: the path's own label is the
    // resource.
    if ARN_IN_PATH_SPACES.contains(&space.as_str()) {
        return segment(1).unwrap_or_else(|| EVERY_RESOURCE.to_string());
    }
    let Some(resource_type) = resource_type_of(&space) else {
        return EVERY_RESOURCE.to_string();
    };
    match segment(1) {
        // The one route whose second segment is a literal addresses no resource of its own.
        Some(literal) if literal == FLOW_VALIDATION_SEGMENT => EVERY_RESOURCE.to_string(),
        // An identifier that is already an ARN is the resource; otherwise it names the
        // resource of this kind in the Region the request signed for.
        Some(identifier) if identifier.starts_with(ARN_PREFIX) => identifier,
        Some(identifier) => {
            ::engine::emulator_arn(ACTION_PREFIX, &region, &format!("{resource_type}/{identifier}"))
        }
        // A create or a listing addresses the collection of that kind rather than one of
        // them.
        None => EVERY_RESOURCE.to_string(),
    }
}

/// Authorize this request against `resource`, where `operation` is the operation's own name
/// ([`Operation::NAME`]).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see
/// the module docs); the refusal otherwise.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service is
/// judged here too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), AccessDenied> {
    let action = format!("{ACTION_PREFIX}:{operation}");
    match caller(engine, ctx).refusal(&action, resource) {
        Some(message) => Err(AccessDenied { message }),
        None => Ok(()),
    }
}

/// The wire answer to a request this gate refused: the refusal at its 403, serialized
/// through the SAME protocol a handler-returned error goes through, so a refusal taken in
/// this layer and one taken in a handler are indistinguishable on the wire.
///
/// The hand-built fallback carries the same three things a client branches on — the status,
/// the code in `x-amzn-errortype` and the `message` body — for the one case the serializer
/// can fail: a value it cannot render.
#[must_use]
pub fn refusal_response(protocol: &dyn ServerHttpProtocol, denial: &AccessDenied) -> RtResponse {
    protocol
        .serialize_error(
            denial,
            &ACCESS_DENIED_SCHEMA,
            ACCESS_DENIED_EXCEPTION,
            ACCESS_DENIED_STATUS,
        )
        .unwrap_or_else(|_| {
            let body = ::serde_json::json!({
                "__type": ACCESS_DENIED_EXCEPTION,
                "message": denial.message.clone(),
            });
            let mut response = RtResponse::new(
                StatusCode::try_from(ACCESS_DENIED_STATUS)
                    .expect("the status is a constant of this module"),
                SdkBody::from(body.to_string()),
            );
            response
                .headers_mut()
                .insert("content-type", JSON_CONTENT_TYPE);
            response
                .headers_mut()
                .insert("x-amzn-errortype", ACCESS_DENIED_EXCEPTION);
            response
        })
}

/// The principal a request speaks for, and everything the decision is made from.
#[derive(Debug, Clone, Default)]
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

    /// `Some(message)` when the documents in force do not let this caller perform `action` on
    /// `resource` — the sentence the refusal carries — and `None` when they do.
    ///
    /// IAM's order: an explicit `Deny` in a document that CAN be read refuses the request
    /// whatever else the principal carries; otherwise a grant is needed, which a caller this
    /// gate does not bound has by definition (see the module docs); and a grant is capped by
    /// the permissions boundary, since "a permissions boundary sets the maximum permissions
    /// that an identity-based policy can grant" (IAM, permissions boundaries), which takes
    /// permissions away and grants none of its own.
    ///
    /// A request that addresses no resource of its own is decided on the shared evaluator's
    /// weaker question ([`iam::identity_may_allow_action`]), which is published for exactly
    /// that case: refusing on `identity_decision` over `*` alone would refuse calls a
    /// resource-scoped statement permits.
    fn refusal(&self, action: &str, resource: &str) -> Option<String> {
        // An identity-based document states no `Principal` element; the spellings travel
        // with the request because that is what such an element would be matched against.
        let request = iam::Request::new(action, resource, &self.principals);
        let said = iam::identity_decision(&self.documents, &request);
        let granted = if resource == EVERY_RESOURCE {
            said != Decision::Denied
                && (iam::identity_may_allow_action(
                    &iam::prepare_documents(&self.documents),
                    action,
                ) || self.identity_is_unbounded())
        } else {
            iam::is_allowed(said, Decision::NotStated)
                || (said != Decision::Denied && self.identity_is_unbounded())
        };
        // The ceiling in force on the principal. A call a document explicitly DENIES is
        // already refused on that deny, so no ceiling is read for it.
        let capped = match (said == Decision::Denied, self.boundary.as_ref()) {
            (false, Some(boundary)) => {
                iam::identity_decision(::std::slice::from_ref(boundary), &request)
            }
            _ => Decision::Allowed,
        };
        if granted && capped == Decision::Allowed {
            return None;
        }
        // WHICH rule withheld the grant is a fact the clause names, so the caller is told
        // what to go and change.
        let reason = if said == Decision::Denied {
            iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
        } else if !granted {
            iam::no_identity_policy_allows(action)
        } else if capped == Decision::Denied {
            iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()
        } else {
            iam::no_permissions_boundary_allows(action)
        };
        Some(iam::not_authorized_by_identity_policy(
            &self.arn, action, resource, &reason,
        ))
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's own
    /// accessors: the documents in force on it, whether it carries an attachment whose
    /// document this emulator cannot read, and the permissions boundary that caps it.
    fn read_policies_of(
        &mut self,
        store: &iam_store::ComAmazonawsIamAwsidentitymanagementv20100508State,
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
fn caller<Eng: Send + Sync + 'static>(engine: &Eng, ctx: &OperationContext<'_>) -> Caller {
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
                ..Caller::default()
            };
        }
    };

    let mut caller = Caller {
        account_credential: identity.arn == sts_store::ambient_identity().arn,
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

/// One modelled operation, served behind IAM's decision about the action it names.
///
/// It carries the operation TYPE — which is what names the action and resolves the delegate
/// — and a handle on the ONE shared engine, which is what the decision reads the caller's
/// identity and its policies through. Every request it does not refuse is delegated to the
/// one operation instance the generated routing table built from that same engine, so there
/// is no second instance of any operation and no second store.
pub struct Gated<Op, Eng> {
    operation: PhantomData<fn() -> Op>,
    engine: Arc<Eng>,
}

impl<Op, Eng> Gated<Op, Eng> {
    /// Judge one operation, against the policies held in `engine`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            operation: PhantomData,
            engine,
        }
    }
}

impl<Op: Operation, Eng: Send + Sync + 'static> ErasedOperation for Gated<Op, Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // IAM's decision, taken before the operation reads or writes anything: the
            // action is this operation's own name under this service's prefix, and the
            // resource is the one the REQUEST addresses. A refused request never reaches the
            // operation, so it mints nothing, moves no status and reports nothing.
            let resource = judged_resource(ctx, req);
            if let Err(denial) = authorize(&*self.engine, ctx, Op::NAME, &resource) {
                // The protocol is read, used and dropped inside this one statement:
                // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the
                // await below would make this future non-`Send`.
                return refusal_response(svc.definition().protocol(), &denial);
            }
            match svc.definition().routing_table().get(Op::NAME) {
                Some(operation) => operation.run(svc, req, ctx).await,
                // Unreachable in practice: this wrapper is only reachable under the key the
                // generated table registered. Answer the framework's own no-such-operation
                // response rather than inventing one.
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The wrapper is not an operation of its own: the request is signed as the MODEL
        // says for the operation it names, so authentication is decided by the modelled
        // operation this wrapper stands in front of.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caller holding exactly `documents`, under a principal a refusal can name.
    fn principal(documents: &[&str]) -> Caller {
        Caller {
            arn: "arn:aws:sts::000000000000:assumed-role/agent-builder/session-1".to_string(),
            principals: vec!["arn:aws:iam::000000000000:role/agent-builder".to_string()],
            documents: documents.iter().map(|text| (*text).to_string()).collect(),
            ..Caller::default()
        }
    }

    /// RULE: the action asked about is `bedrock:<OperationName>`, so a statement scoped to
    /// this service's actions covers every operation of it and nothing outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_namespace() {
        let granted = principal(&[
            r#"{"Statement":[{"Effect":"Allow","Action":"bedrock:*","Resource":"*"}]}"#,
        ]);
        // One operation per resource kind this model binds a URI space for
        // (`smithy_query(query="digest")`).
        for operation in [
            "GetAgent",
            "CreateAgent",
            "ListAgents",
            "GetKnowledgeBase",
            "GetFlow",
            "GetPrompt",
            "ListTagsForResource",
        ] {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let refused = granted.refusal(&action, EVERY_RESOURCE);
            assert!(
                refused.is_none(),
                "case {operation:?}: expected bedrock:* to allow {action}, got {refused:?}"
            );
        }
        let outside = granted.refusal("aoss:ListCollections", EVERY_RESOURCE);
        assert!(
            outside.is_some(),
            "case \"another service\": expected bedrock:* to grant nothing outside this \
             service, got {outside:?}"
        );
    }

    /// RULE: every URI space this model binds is judged under the resource type this plane's
    /// own ARNs carry for it, so a policy written about one resource covers exactly the calls
    /// addressing it.
    #[test]
    fn every_bound_uri_space_is_judged_under_its_own_resource_type() {
        // The first path segment of every route this model binds
        // (`smithy_query(query="digest")`), with the resource type its ARNs spell.
        let rows: &[(&str, Option<&str>)] = &[
            ("agents", Some("agent")),
            ("knowledgebases", Some("knowledge-base")),
            ("flows", Some("flow")),
            ("prompts", Some("prompt")),
            ("retrievalinstructions", Some("retrieval-instruction")),
            // The two spaces whose target the request carries as an ARN of its own, and so
            // has no type to spell here.
            ("tags", None),
            ("resourcepolicy", None),
            // A space this model binds no route for.
            ("sessions", None),
        ];
        for (space, expected) in rows {
            let judged = resource_type_of(space);
            assert_eq!(
                judged, *expected,
                "case {space:?}: expected the resource type {expected:?}, got {judged:?}"
            );
        }
    }

    /// RULE: a caller no readable document grants the action to is refused, and the refusal
    /// names the principal, the action, the resource and which of IAM's rules refused it.
    #[test]
    fn a_caller_without_the_grant_is_refused_in_iams_own_sentence() {
        let agent = "arn:aws:bedrock:us-east-1:000000000000:agent/ABCDEFGHIJ";
        let action = format!("{ACTION_PREFIX}:GetAgent");
        // (case, the documents in force, the resource addressed, whether it is permitted)
        let rows: &[(&str, &[&str], &str, bool)] = &[
            ("no document at all", &[], agent, false),
            (
                "a grant of another action",
                &[r#"{"Statement":[{"Effect":"Allow","Action":"bedrock:InvokeAgent",
                 "Resource":"*"}]}"#],
                agent,
                false,
            ),
            (
                "a grant of this action on every resource",
                &[r#"{"Statement":[{"Effect":"Allow","Action":"bedrock:GetAgent",
                 "Resource":"*"}]}"#],
                agent,
                true,
            ),
            (
                "a grant scoped to the agent addressed",
                &[&format!(
                    r#"{{"Statement":[{{"Effect":"Allow","Action":"bedrock:GetAgent",
                     "Resource":"{agent}"}}]}}"#
                )],
                agent,
                true,
            ),
            (
                "a grant scoped to another agent",
                &[r#"{"Statement":[{"Effect":"Allow","Action":"bedrock:GetAgent",
                 "Resource":"arn:aws:bedrock:us-east-1:000000000000:agent/ZZZZZZZZZZ"}]}"#],
                agent,
                false,
            ),
            (
                "a grant and an explicit deny",
                &[
                    r#"{"Statement":[{"Effect":"Allow","Action":"bedrock:*","Resource":"*"}]}"#,
                    r#"{"Statement":[{"Effect":"Deny","Action":"bedrock:GetAgent",
                     "Resource":"*"}]}"#,
                ],
                agent,
                false,
            ),
        ];
        for (case, documents, resource, permitted) in rows {
            let caller = principal(documents);
            let refused = caller.refusal(&action, resource);
            assert_eq!(
                refused.is_none(),
                *permitted,
                "case {case:?}: expected permitted={permitted}, got {refused:?}"
            );
            if let Some(message) = refused {
                assert_eq!(
                    message,
                    iam::not_authorized_by_identity_policy(
                        &caller.arn,
                        &action,
                        resource,
                        &if documents.len() == 2 {
                            iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
                        } else {
                            iam::no_identity_policy_allows(&action)
                        },
                    ),
                    "case {case:?}: expected IAM's own sentence, got {message:?}"
                );
            }
        }
    }
}
