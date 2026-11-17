// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service's CONTROL PLANE perform the action it asks
//! for — the decision taken BEFORE the operation it names is dispatched.
//!
//! AWS decides authorization at the IAM layer, in front of the service, so a caller whose
//! identity-based policies do not grant the action — or that an explicit `Deny` covers — is
//! told it is not authorized rather than told anything about the resource it named, and the
//! call performs nothing: no collection, policy or endpoint is minted, no status moves and
//! nothing is read back. The refusal is `AccessDeniedException` (HTTP 403), the standard
//! SigV4 authorization answer: this model declares no shape for it
//! (`smithy_query(query="service_info")` lists `InternalServerException` as its only common
//! error, and `smithy_query(query="shape", shape="AccessDeniedException")` finds no such
//! shape), for the same reason it declares nothing for a bad signature — the decision is not
//! the service's, so the shape is rendered here ([`AccessDenied`]) exactly as the framework
//! renders its own synthetic errors.
//!
//! The decision is a property of the request's identity, the action it names and the
//! resource it addresses rather than of what a handler goes on to do, so it is taken ONCE —
//! in the layer every control-plane operation is served behind ([`Gated`], wired in
//! `crate::service::EmulatedService::override_operation`) — and nowhere else. Every
//! operation this service publishes is judged, implemented or still a scaffold: an
//! operation's modelled errors are the ones the SERVICE raises once it is reached, and a
//! caller IAM refuses never reaches it, so leaving an operation out would serve an
//! unauthorized caller the very resources the policy withholds.
//!
//! A COLLECTION's own data plane is not judged here: a request to the endpoint a collection
//! publishes is authorized by that collection's `network` and `data` access policies, which
//! is a decision of its own ([`super::collection_rest::authorization`]) and is reached on a
//! path this gate never sees.
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
//! The action is `aoss:<OperationName>` ([`ACTION_PREFIX`] is this service's `arnNamespace`,
//! which is also its SigV4 signing name), which is how a policy statement spells one of this
//! service's actions.
//!
//! The resource is the one the REQUEST itself names by ARN ([`judged_resource`]): the
//! `resourceArn` the tagging operations carry, which is the only member of this model typed
//! as an ARN of a resource of this service (`smithy_query(query="shape", shape="Arn")`).
//! Every other operation names its target by a NAME or by a server-minted id and carries no
//! ARN at all, so it addresses no resource this gate can spell and is decided on the weaker
//! question the shared evaluator publishes for exactly that case
//! ([`::service_common::iam_authorization::identity_may_allow_action`]): whether any
//! readable document could allow the action at all, so a statement scoped to one collection
//! still permits the calls it covers. An input of ONE of those three that this gate cannot
//! read an ARN out of is not read as naming no resource — that would hand a statement scoped
//! to one collection a tag write whose target it says nothing about — but as every resource
//! of this service in the Region the request signed for, which such a statement does not
//! cover. Reading the ARN from the request rather than from a stored record is what keeps a
//! refused call from telling an unauthorized caller whether what it named exists.
//!
//! This service publishes no resource-based policy of its own — the model declares no policy
//! operation over an ARN and no policy member on a resource — so the decision here is the
//! identity-based one alone. (A `data` access policy is not one: it governs a COLLECTION's
//! data plane, not this control plane, and is read where that plane is served.)
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

/// This service's IAM action prefix: an action is `aoss:<OperationName>`, under this
/// service's `arnNamespace` (`smithy_query(query="service_info")`), which this crate already
/// states once.
pub const ACTION_PREFIX: &str = crate::state::ARN_NAMESPACE;

/// The resource a request that names no resource of its own is decided about, as IAM writes
/// it for such a request: every resource.
pub const EVERY_RESOURCE: &str = "*";

/// The wire error code this gate answers with, and its status: the pair AWS answers a
/// request IAM does not permit with, across services.
pub const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The media type this `awsJson1_0` service renders a body in.
const JSON_CONTENT_TYPE: &str = "application/x-amz-json-1.0";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// The input member the tagging operations carry their target's ARN in
/// (`smithy_query(query="operation", operation="TagResource,UntagResource,
/// ListTagsForResource")`).
const TAG_TARGET_MEMBER: &str = "resourceArn";

/// The operations whose input carries [`TAG_TARGET_MEMBER`], which are the only operations of
/// this model whose input names a resource of this service by ARN at all (the same three, and
/// no other, carry a member typed `Arn`).
const TAG_TARGET_OPERATIONS: &[&str] = &["ListTagsForResource", "TagResource", "UntagResource"];

/// The `message` member of the synthetic `AccessDeniedException` shape.
///
/// Spelled `message`, as every modelled error shape of this model spells it
/// (`smithy_query(query="shape", shape="ValidationException")`), so a client reads the
/// sentence out of an authorization refusal at the same member as out of a service error.
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

/// Schema for the synthetic `AccessDeniedException` shape.
///
/// In the `smithy.framework` namespace deliberately: the shape is in no service's model —
/// authorization is decided ahead of the service, so this model declares no shape for its
/// refusal — and must not collide with a modelled one. The wire code and the status are
/// [`ACCESS_DENIED_EXCEPTION`] / [`ACCESS_DENIED_STATUS`], passed to the serializer rather
/// than read off this schema.
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

/// The ARN the request NAMES, and [`EVERY_RESOURCE`] for a request that names none.
///
/// Read from the REQUEST rather than from a stored record, so the decision does not depend
/// on whether that resource exists — which is also what keeps a refused call from telling an
/// unauthorized caller whether it does.
///
/// `operation` is the operation's own name ([`Operation::NAME`]): the operations whose input
/// carries an ARN of its own ([`TAG_TARGET_OPERATIONS`]) are judged against a resource of
/// this service whatever the request turned out to carry, so an input this gate could not
/// read the ARN out of is NOT decided on the weaker question a request addressing no resource
/// is decided on — which would hand a statement scoped to one collection a tag write whose
/// target it says nothing about. For such an input the resource is every resource OF THIS
/// SERVICE in the Region the request signed for, which a statement scoped to one of them does
/// not cover.
#[must_use]
pub fn judged_resource(ctx: &OperationContext<'_>, req: &RtRequest, operation: &str) -> String {
    if !TAG_TARGET_OPERATIONS.contains(&operation) {
        return EVERY_RESOURCE.to_string();
    }
    tag_target(req).unwrap_or_else(|| {
        ::engine::emulator_arn(
            ACTION_PREFIX,
            &crate::state::request_region(ctx),
            EVERY_RESOURCE,
        )
    })
}

/// The ARN a tagging call names its target by, read off the request's own input document —
/// this service binds every operation to one RPC target, so the input IS the body.
fn tag_target(req: &RtRequest) -> Option<String> {
    let body = req.body().bytes()?;
    let ::serde_json::Value::Object(members) = ::serde_json::from_slice(body).ok()? else {
        return None;
    };
    members
        .get(TAG_TARGET_MEMBER)?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
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
/// the code in `__type` and in `x-amzn-errortype` — for the one case the serializer can
/// fail: a value it cannot render.
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
    /// A request that names no resource of its own is decided on the shared evaluator's
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
/// It carries the operation TYPE — which is what names the action — and a handle on the ONE
/// shared engine, which is what the decision reads the caller's identity and its policies
/// through. Every request it does not refuse is served by the operation this service already
/// serves it with: the wrapper this service wraps that operation in when it has one
/// ([`Gated::serving`]), else the one instance the generated routing table built from the
/// same engine — so there is no second instance of any operation and no second store.
pub struct Gated<Op, Eng> {
    operation: PhantomData<fn() -> Op>,
    engine: Arc<Eng>,
    /// The operation a permitted request is served by, for an operation this service serves
    /// through a wrapper of its own; `None` to serve it from the generated routing table.
    served_by: Option<Box<dyn ErasedOperation>>,
}

impl<Op, Eng> Gated<Op, Eng> {
    /// Judge one operation, served from the generated routing table.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            operation: PhantomData,
            engine,
            served_by: None,
        }
    }

    /// Judge one operation this service serves through `served_by` — the wrapper the request
    /// would have reached without this gate, so gating a call changes nothing but whether it
    /// is made.
    pub fn serving(engine: Arc<Eng>, served_by: Box<dyn ErasedOperation>) -> Self {
        Self {
            operation: PhantomData,
            engine,
            served_by: Some(served_by),
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
            // resource is the one the REQUEST names. A refused request never reaches the
            // operation, so it mints nothing, moves no status and reports nothing.
            let resource = judged_resource(ctx, req, Op::NAME);
            if let Err(denial) = authorize(&*self.engine, ctx, Op::NAME, &resource) {
                // The protocol is read, used and dropped inside this one statement:
                // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the
                // await below would make this future non-`Send`.
                return refusal_response(svc.definition().protocol(), &denial);
            }
            if let Some(served_by) = &self.served_by {
                return served_by.run(svc, req, ctx).await;
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
            arn: "arn:aws:sts::000000000000:assumed-role/search-writer/session-1".to_string(),
            principals: vec!["arn:aws:iam::000000000000:role/search-writer".to_string()],
            documents: documents.iter().map(|text| (*text).to_string()).collect(),
            ..Caller::default()
        }
    }

    /// RULE: the action asked about is `aoss:<OperationName>`, so a statement scoped to this
    /// service's actions covers every operation of it and nothing outside it.
    #[test]
    fn the_action_asked_about_carries_this_services_namespace() {
        let granted = principal(&[r#"{"Statement":[{"Effect":"Allow","Action":"aoss:*",
             "Resource":"*"}]}"#]);
        // Four kinds of operation this model binds: a create, a batch read, a listing and a
        // tag write (`smithy_query(query="list_operations")`).
        for operation in [
            "CreateCollection",
            "BatchGetCollection",
            "ListCollections",
            "TagResource",
        ] {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let refused = granted.refusal(&action, EVERY_RESOURCE);
            assert!(
                refused.is_none(),
                "case {operation:?}: expected aoss:* to allow {action}, got {refused:?}"
            );
        }
        let outside = granted.refusal("es:DescribeDomain", EVERY_RESOURCE);
        assert!(
            outside.is_some(),
            "case \"another service\": expected aoss:* to grant nothing outside this service, \
             got {outside:?}"
        );
    }

    /// RULE: a caller no readable document grants the action to is refused, and the refusal
    /// names the principal, the action, the resource and which of IAM's rules refused it.
    #[test]
    fn a_caller_without_the_grant_is_refused_in_iams_own_sentence() {
        let action = format!("{ACTION_PREFIX}:ListCollections");
        // (case, the documents in force, whether the call is permitted)
        let rows: &[(&str, &[&str], bool)] = &[
            ("no document at all", &[], false),
            (
                "a grant of another action",
                &[r#"{"Statement":[{"Effect":"Allow","Action":"aoss:CreateCollection",
                 "Resource":"*"}]}"#],
                false,
            ),
            (
                "a grant of this action",
                &[r#"{"Statement":[{"Effect":"Allow","Action":"aoss:ListCollections",
                 "Resource":"*"}]}"#],
                true,
            ),
            (
                "a grant scoped to one collection",
                &[r#"{"Statement":[{"Effect":"Allow","Action":"aoss:ListCollections",
                 "Resource":"arn:aws:aoss:us-east-1:000000000000:collection/abc123"}]}"#],
                true,
            ),
            (
                "a grant and an explicit deny",
                &[
                    r#"{"Statement":[{"Effect":"Allow","Action":"aoss:*","Resource":"*"}]}"#,
                    r#"{"Statement":[{"Effect":"Deny","Action":"aoss:ListCollections",
                     "Resource":"*"}]}"#,
                ],
                false,
            ),
        ];
        for (case, documents, permitted) in rows {
            let caller = principal(documents);
            let refused = caller.refusal(&action, EVERY_RESOURCE);
            assert_eq!(
                refused.is_none(),
                *permitted,
                "case {case:?}: expected permitted={permitted}, got {refused:?}"
            );
            if let Some(message) = refused {
                assert!(
                    message.starts_with(&format!(
                        "User: {} is not authorized to perform: {action}",
                        caller.arn
                    )),
                    "case {case:?}: expected IAM's own sentence, got {message:?}"
                );
            }
        }
    }

    /// RULE: the resource a call is judged against is the ARN its own input names, which for
    /// this model is the `resourceArn` a tagging call carries and nothing else — and an input
    /// no ARN can be read out of is NOT read as "no resource": that is what would hand a
    /// statement scoped to one collection a tag write whose target it says nothing about.
    #[test]
    fn the_resource_is_the_arn_the_requests_own_input_names() {
        let arn = "arn:aws:aoss:us-east-1:000000000000:collection/abc123";
        // (case, the request body, the ARN it names)
        let rows: &[(&str, ::serde_json::Value, Option<String>)] = &[
            (
                "a tag write naming its target",
                ::serde_json::json!({ "resourceArn": arn, "tags": [] }),
                Some(arn.to_string()),
            ),
            (
                "a create naming a collection by name",
                ::serde_json::json!({ "name": "demo" }),
                None,
            ),
            ("a listing naming nothing", ::serde_json::json!({}), None),
            (
                "a tag write whose target is empty",
                ::serde_json::json!({ "resourceArn": "" }),
                None,
            ),
            (
                "a body that is not an input document at all",
                ::serde_json::Value::String("not an object".to_string()),
                None,
            ),
        ];
        for (case, body, expected) in rows {
            let req = RtRequest::new(SdkBody::from(body.to_string()));
            let named = tag_target(&req);
            assert_eq!(
                named, *expected,
                "case {case:?}: expected the input to name {expected:?}, got {named:?}"
            );
        }
        // And WHICH operations are judged against that ARN: the three whose input carries it,
        // so a tag write whose target could not be read falls back to a resource of this
        // service rather than to the weaker question.
        for operation in ["ListTagsForResource", "TagResource", "UntagResource"] {
            assert!(
                TAG_TARGET_OPERATIONS.contains(&operation),
                "case {operation:?}: expected the operation carrying {TAG_TARGET_MEMBER} to be \
                 judged against the ARN it names, got {TAG_TARGET_OPERATIONS:?}"
            );
        }
        for operation in ["CreateCollection", "ListCollections", "BatchGetCollection"] {
            assert!(
                !TAG_TARGET_OPERATIONS.contains(&operation),
                "case {operation:?}: expected an operation whose input names no ARN to be \
                 judged against every resource, got {TAG_TARGET_OPERATIONS:?}"
            );
        }
    }
}
