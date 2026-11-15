// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this service perform the action it asks for — the
//! decision taken BEFORE the operation it names is dispatched.
//!
//! AWS decides authorization at the IAM layer, in front of the service, so a caller whose
//! identity-based policies do not grant the action — or that an explicit `Deny` covers — is
//! told it is not authorized rather than told anything about the resource it named, and the
//! call performs nothing: no pipeline, configuration or pool is minted, no status moves and
//! nothing is read back. The refusal is `AccessDeniedException` (HTTP 403), the standard
//! SigV4 authorization answer: this model declares no shape for it
//! (`smithy_query(query="service_info")` lists `BadRequestException`,
//! `ForbiddenException`, `ServiceFailureException`, `ServiceUnavailableException`,
//! `ThrottledClientException` and `UnauthorizedClientException` as its common errors), for
//! the same reason it declares nothing for a bad signature — the decision is not the
//! service's, so the shape is rendered here ([`AccessDenied`]) exactly as the framework
//! renders its own synthetic errors.
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
//! 1. **who** — the principal the request's access key id speaks for: the session STS
//!    minted that key for (`sts_store::resolve_caller`, the same record
//!    `sts:GetCallerIdentity` answers from), else the IAM user that owns the long-term key,
//!    else the account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    own accessors (`principal_policies`, `permissions_boundary_document`), so a document
//!    `iam:PutRolePolicy` wrote takes effect on the next request and deleting it lifts it
//!    again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation
//!    of those documents against `(action, resource)`, so this service and every other one
//!    handed the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow`
//!    about the request grants it, and with neither the request falls to IAM's default
//!    deny. The refusal's WORDING comes from that module too, for the same reason the
//!    decision does.
//!
//! Only the store LOOKUPS live here; neither the decision nor its sentence does. They stay
//! in this crate because hoisting them into `service-common` would make every service that
//! depends on that crate link the IAM and STS service crates.
//!
//! ## The action and the resource
//!
//! The action is `<arn namespace>:<OperationName>` — this service's `arnNamespace`, which
//! is also the signing name every request to it is signed under ([`action_prefix`]), so the
//! prefix is read off the request's own resolved identity rather than restated here and the
//! ARNs this gate builds cannot name a different service from the actions it asks about.
//!
//! The resource is the one the REQUEST addresses ([`judged_resource`]), spelled with the
//! resource types this service's own ARN builders use: a call that addresses one resource is
//! judged against that resource's ARN, and one that addresses a COLLECTION — a create, a
//! listing — against the collection's own kind with [`ANY_RESOURCE`] in place of an
//! identifier, which is how IAM writes the resource of an action whose resource does not
//! exist yet. Reading it from the request rather than from a stored record is what keeps a
//! refused call from telling an unauthorized caller whether what it named exists.
//!
//! This service publishes no resource-based policy of its own — the model declares no policy
//! operation and no policy member — so the decision here is the identity-based one alone.
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
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)
//!
//! LOCK ORDER: the STS registry's lock and then the IAM store's, each taken and released in
//! turn BEFORE this service's own store lock is taken — the decision is reached ahead of
//! dispatch, so no handler of this service holds its store while this runs. Neither of those
//! services reads this one's state.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::SystemTime;

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

/// The resource an action that addresses no single resource of its own is judged against, as
/// IAM writes it: a create names the kind it is about to add to, a listing the kind it reads
/// over.
pub const ANY_RESOURCE: &str = "*";

/// The wire error code this gate answers with, and its status: the pair AWS answers a
/// request IAM does not permit with, across services.
pub const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The media type this restJson1 service renders an error in.
const JSON_CONTENT_TYPE: &str = "application/json";

/// Every action this service publishes, i.e. the whole set this gate is asked about, as
/// [`Operation::NAME`] spells each one — which is also the name of the IAM action
/// (`<prefix>:<OperationName>`) a policy states about it.
///
/// Source: the operations bound to the service shape
/// (`smithy_query(query="list_operations")`). The list is held to the MODEL by this crate's
/// own suite (`tests/all/identity_authorization.rs`) and to the registered table by
/// `crate::service`'s own test, so an operation the model gains is judged rather than
/// quietly served to a caller no policy allows.
pub const GATED_OPERATIONS: &[&str] = &[
    "CreateMediaCapturePipeline",
    "CreateMediaConcatenationPipeline",
    "CreateMediaInsightsPipeline",
    "CreateMediaInsightsPipelineConfiguration",
    "CreateMediaLiveConnectorPipeline",
    "CreateMediaPipelineKinesisVideoStreamPool",
    "CreateMediaStreamPipeline",
    "DeleteMediaCapturePipeline",
    "DeleteMediaInsightsPipelineConfiguration",
    "DeleteMediaPipeline",
    "DeleteMediaPipelineKinesisVideoStreamPool",
    "GetMediaCapturePipeline",
    "GetMediaInsightsPipelineConfiguration",
    "GetMediaPipeline",
    "GetMediaPipelineKinesisVideoStreamPool",
    "GetSpeakerSearchTask",
    "GetVoiceToneAnalysisTask",
    "ListMediaCapturePipelines",
    "ListMediaInsightsPipelineConfigurations",
    "ListMediaPipelineKinesisVideoStreamPools",
    "ListMediaPipelines",
    "ListTagsForResource",
    "StartSpeakerSearchTask",
    "StartVoiceToneAnalysisTask",
    "StopSpeakerSearchTask",
    "StopVoiceToneAnalysisTask",
    "TagResource",
    "UntagResource",
    "UpdateMediaInsightsPipelineConfiguration",
    "UpdateMediaInsightsPipelineStatus",
    "UpdateMediaPipelineKinesisVideoStreamPool",
];

/// The `Message` member of the synthetic `AccessDeniedException` shape.
///
/// Spelled `Message`, as every modelled error shape of this model spells it
/// (`smithy_query(query="shape",shape="NotFoundException")`), so a client reads the sentence
/// out of an authorization refusal at the same member as out of a service error.
static ACCESS_DENIED_MESSAGE: Schema = Schema::new_member(
    ShapeId::from_static(
        "smithy.framework#AccessDeniedException$Message",
        "smithy.framework",
        "Message",
    ),
    ShapeType::String,
    "Message",
    0,
)
.with_xml_name("Message");

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

/// This service's IAM action prefix, which is its `arnNamespace`
/// (`smithy_query(query="service_info")`) and also the name every request to it is signed
/// under — so the prefix is read off the request's own resolved identity rather than
/// restated here, and the ARNs this gate builds and the actions it asks about cannot name
/// two different services.
#[must_use]
pub fn action_prefix<'ctx>(ctx: &'ctx OperationContext<'_>) -> &'ctx str {
    ctx.identity().signing_name()
}

/// The URI space of the TAGGING surface, whose target is named by an ARN the request itself
/// carries rather than by a path label: `GET /tags` (`ResourceARN` as the query parameter
/// [`TAG_TARGET_QUERY`]) and `POST /tags?operation={tag,untag}-resource` (`ResourceARN` in
/// the body). Source: `smithy_query(query="operation",
/// operation="ListTagsForResource,TagResource,UntagResource")`.
const TAGS_SPACE: &str = "tags";

/// The query parameter `ListTagsForResource` binds its `ResourceARN` to (`@httpQuery("arn")`).
const TAG_TARGET_QUERY: &str = "arn";

/// The input member the two tagging WRITES carry their target's ARN in.
const TAG_TARGET_MEMBER: &str = "ResourceARN";

/// The ARN prefix a value that is already a whole ARN opens with: an `Identifier` label of
/// this service resolves a resource by its NAME or by its ARN, and an ARN is judged as
/// itself rather than spelled into another one.
const ARN_PREFIX: &str = "arn:";

/// The resource TYPE a request addressing the URI space `space` is judged under, or `None`
/// for a space that names no resource of this service.
///
/// Every space this model binds is mapped here, from `smithy_query(query="digest")`: the
/// pipeline spaces (capture, concatenation, live-connector, stream and insights pipelines,
/// and the insights pipeline's status) address a MEDIA PIPELINE, which is the resource type
/// every one of those kinds carries an ARN of
/// ([`crate::state::MEDIA_PIPELINE_RESOURCE_TYPE`]); the configuration space addresses a
/// media insights pipeline CONFIGURATION; the pool space addresses a Kinesis Video Stream
/// POOL. A task space hangs UNDER a pipeline (`/media-insights-pipelines/{Identifier}/…`),
/// so a call on a task is judged against the pipeline that runs it, which is the resource
/// such a call names.
fn resource_type_of(space: &str) -> Option<&'static str> {
    match space {
        "sdk-media-capture-pipelines"
        | "sdk-media-pipelines"
        | "sdk-media-concatenation-pipelines"
        | "sdk-media-live-connector-pipelines"
        | "sdk-media-stream-pipelines"
        | "media-insights-pipelines"
        | "media-insights-pipeline-status" => Some(crate::state::MEDIA_PIPELINE_RESOURCE_TYPE),
        "media-insights-pipeline-configurations" => {
            Some(crate::state::MEDIA_INSIGHTS_CONFIGURATION_RESOURCE_TYPE)
        }
        "media-pipeline-kinesis-video-stream-pools" => {
            Some(crate::state::KINESIS_VIDEO_STREAM_POOL_RESOURCE_TYPE)
        }
        _ => None,
    }
}

/// The ARN the request NAMES, which is what the action is judged against.
///
/// Read from the REQUEST rather than from a stored record, so the decision does not depend
/// on whether that resource exists — which is also what keeps a refused call from telling an
/// unauthorized caller whether it does.
#[must_use]
pub fn judged_resource(ctx: &OperationContext<'_>, req: &RtRequest) -> String {
    let namespace = action_prefix(ctx);
    let region = crate::state::request_region(ctx.parts());
    let path = ::service_common::request_path(req.uri());
    let segment = |index: usize| {
        ::service_common::nth_path_segment(path, index)
            .filter(|segment| !segment.is_empty())
            .map(::service_common::percent_decode_path_label)
    };
    let space = segment(0).unwrap_or_default();
    // The tagging surface carries the ARN of its target, so there is nothing to spell.
    if space == TAGS_SPACE {
        return tag_target(req).unwrap_or_else(|| any_resource(namespace, &region));
    }
    let Some(resource_type) = resource_type_of(&space) else {
        return any_resource(namespace, &region);
    };
    // A resource of this service is addressed by its id or NAME, and a configuration or pool
    // also by its own ARN; a request carrying no identifier at all addresses the collection.
    match segment(1) {
        Some(identifier) if identifier.starts_with(ARN_PREFIX) => identifier,
        Some(identifier) => ::engine::emulator_arn(
            namespace,
            &region,
            &format!("{resource_type}/{identifier}"),
        ),
        None => ::engine::emulator_arn(
            namespace,
            &region,
            &format!("{resource_type}/{ANY_RESOURCE}"),
        ),
    }
}

/// Every resource of this service in this Region, which is what a call naming no resource
/// of its own is judged against.
fn any_resource(namespace: &str, region: &str) -> String {
    ::engine::emulator_arn(namespace, region, ANY_RESOURCE)
}

/// The ARN a call on the tagging surface names its target by: the query parameter
/// `ListTagsForResource` binds, else the body member the two writes carry.
fn tag_target(req: &RtRequest) -> Option<String> {
    if let Some(from_query) = query_value(req.uri(), TAG_TARGET_QUERY) {
        return Some(from_query);
    }
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

/// The value of the query parameter `name`, decoded, or `None` when the URI carries none.
///
/// The SPLIT is the shared reader's ([`::service_common::query_parameter`], the one place a
/// raw query string is parsed in this tree); only the decode is here, because that reader
/// answers the value as written and an ARN arrives with its `:` and `/` escaped.
fn query_value(uri: &str, name: &str) -> Option<String> {
    let (_, query) = uri.split_once('?')?;
    ::service_common::query_parameter(query, name)
        .map(::smithy_server_runtime::percent_decode)
        .filter(|value| !value.is_empty())
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
    let action = format!("{}:{operation}", action_prefix(ctx));
    let caller = caller(engine, ctx);
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no
    // such element, and this service publishes no resource-based policy (see the module
    // docs), so the decision below rests on the action and the resource.
    let request = iam::Request::new(&action, resource, &caller.principals);
    let identity_said = iam::identity_decision(&caller.documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read
    // refuses the request whatever else the principal carries — including a grant this
    // emulator cannot read, which only lifts the DEFAULT deny of a request no readable
    // statement allows (see the module docs).
    if identity_said == Decision::Denied {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            iam::IDENTITY_POLICY_EXPLICIT_DENY,
        ));
    }
    let granted =
        iam::is_allowed(identity_said, Decision::NotStated) || caller.identity_is_unbounded();
    if !granted {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            &iam::no_identity_policy_allows(&action),
        ));
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
            Decision::NotStated => iam::no_permissions_boundary_allows(&action),
        };
        return Err(not_authorized(&caller.arn, &action, resource, &reason));
    }
    Ok(())
}

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it — the one sentence every authorizing
/// service in this tree words it as ([`iam::not_authorized_by_identity_policy`]). Every
/// value in it comes from the request and from the stored policies.
fn not_authorized(
    principal: &str,
    action: &str,
    resource: &str,
    reason: &str,
) -> AccessDenied {
    AccessDenied {
        message: iam::not_authorized_by_identity_policy(principal, action, resource, reason),
    }
}

/// The wire answer to a request this gate refused: the refusal at its 403, serialized
/// through the SAME protocol a handler-returned error goes through, so a refusal taken in
/// this layer and one taken in a handler are indistinguishable on the wire.
///
/// The hand-built fallback carries the same three things a client branches on — the status,
/// the code in `x-amzn-errortype` and the `__type` body — for the one case the serializer
/// can fail: a value it cannot render.
#[must_use]
pub fn refusal_response(
    protocol: &dyn ServerHttpProtocol,
    denial: &AccessDenied,
) -> RtResponse {
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
                "Message": denial.message.clone(),
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

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

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
    /// Wrap one operation, judged against the policies held in `engine`.
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
            // resource is the one the REQUEST names. A refused request never reaches the
            // delegate, so it mints nothing, moves no status and reports nothing.
            let resource = judged_resource(ctx, req);
            if let Err(denial) = authorize(&*self.engine, ctx, Op::NAME, &resource) {
                // The protocol is read, used and dropped inside this one statement:
                // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the
                // await below would make this future non-`Send`.
                return refusal_response(svc.definition().protocol(), &denial);
            }
            let delegate = svc.definition().routing_table().get(Op::NAME);
            match delegate {
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

    /// The decision one stored document reaches about an action on a resource, through the
    /// shared evaluator this gate consults.
    fn decision(document: &str, action: &str, resource: &str) -> Decision {
        iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new(action, resource, &[]),
        )
    }

    /// RULE: every URI space this model binds is judged under the resource type its own ARNs
    /// carry, so a policy written about one resource covers exactly the calls addressing it.
    #[test]
    fn every_bound_uri_space_is_judged_under_its_own_resource_type() {
        for (space, expected) in [
            (
                "sdk-media-capture-pipelines",
                crate::state::MEDIA_PIPELINE_RESOURCE_TYPE,
            ),
            ("sdk-media-pipelines", crate::state::MEDIA_PIPELINE_RESOURCE_TYPE),
            (
                "sdk-media-concatenation-pipelines",
                crate::state::MEDIA_PIPELINE_RESOURCE_TYPE,
            ),
            (
                "sdk-media-live-connector-pipelines",
                crate::state::MEDIA_PIPELINE_RESOURCE_TYPE,
            ),
            (
                "sdk-media-stream-pipelines",
                crate::state::MEDIA_PIPELINE_RESOURCE_TYPE,
            ),
            (
                "media-insights-pipelines",
                crate::state::MEDIA_PIPELINE_RESOURCE_TYPE,
            ),
            (
                "media-insights-pipeline-status",
                crate::state::MEDIA_PIPELINE_RESOURCE_TYPE,
            ),
            (
                "media-insights-pipeline-configurations",
                crate::state::MEDIA_INSIGHTS_CONFIGURATION_RESOURCE_TYPE,
            ),
            (
                "media-pipeline-kinesis-video-stream-pools",
                crate::state::KINESIS_VIDEO_STREAM_POOL_RESOURCE_TYPE,
            ),
        ] {
            let reached = resource_type_of(space);
            assert_eq!(
                reached,
                Some(expected),
                "case {space:?}: expected the space to be judged under {expected:?}, got \
                 {reached:?}"
            );
        }
        assert_eq!(
            resource_type_of(TAGS_SPACE),
            None,
            "case \"the tagging surface\": its target is an ARN the request carries, not a \
             space this spells"
        );
    }

    /// RULE: the action asked about carries this service's own prefix, so one statement
    /// scoped to this service's actions covers every operation of it and nothing outside it.
    #[test]
    fn a_statement_scoped_to_this_service_covers_its_actions_only() {
        let document = r#"{"Statement":[{"Effect":"Allow","Action":"chime:*","Resource":"*"}]}"#;
        for operation in [
            "CreateMediaPipelineKinesisVideoStreamPool",
            "CreateMediaInsightsPipelineConfiguration",
            "ListMediaPipelines",
            "GetSpeakerSearchTask",
            "TagResource",
        ] {
            let action = format!("chime:{operation}");
            let reached = decision(document, &action, "*");
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected the service-wide allow to cover {action}, got \
                 {reached:?}"
            );
        }
        let outside = decision(document, "chime-sdk-voice:CreateVoiceConnector", "*");
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another service\": expected the service-wide allow to grant nothing \
             outside this service, got {outside:?}"
        );
    }

    /// RULE: the refusal names the principal, the action, the resource the request addressed
    /// and which of IAM's rules refused it — every part taken from the request and the
    /// stored policies.
    #[test]
    fn the_refusal_names_the_principal_the_action_and_the_resource() {
        let principal = format!(
            "arn:aws:sts::{}:assumed-role/pool-creator/probe",
            ::engine::emulator_account_id()
        );
        let action = "chime:CreateMediaPipelineKinesisVideoStreamPool";
        let resource = format!(
            "arn:aws:chime:us-east-1:{}:{}/{ANY_RESOURCE}",
            ::engine::emulator_account_id(),
            crate::state::KINESIS_VIDEO_STREAM_POOL_RESOURCE_TYPE
        );
        let refusal = not_authorized(
            &principal,
            action,
            &resource,
            &iam::no_identity_policy_allows(action),
        );
        assert_eq!(
            refusal.message,
            format!(
                "User: {principal} is not authorized to perform: {action} on resource: \
                 {resource} because no identity-based policy allows the {action} action"
            ),
            "case \"IAM's default deny\": expected IAM's wording, got {:?}",
            refusal.message
        );
    }

    /// RULE: a query parameter the tagging read binds its target ARN to is read back
    /// decoded, and nothing is read for a URI that carries none.
    #[test]
    fn the_tagging_reads_target_is_read_off_the_query() {
        let arn = "arn:aws:chime:us-east-1:000000000000:media-pipeline/abc";
        let encoded = arn.replace(':', "%3A").replace('/', "%2F");
        for (case, uri, expected) in [
            (
                "the bound parameter",
                format!("https://host/tags?{TAG_TARGET_QUERY}={encoded}"),
                Some(arn.to_string()),
            ),
            (
                "another parameter beside it",
                format!("https://host/tags?x=1&{TAG_TARGET_QUERY}={encoded}"),
                Some(arn.to_string()),
            ),
            ("no query at all", "https://host/tags".to_string(), None),
            (
                "the parameter present but empty",
                format!("https://host/tags?{TAG_TARGET_QUERY}="),
                None,
            ),
        ] {
            let read = query_value(&uri, TAG_TARGET_QUERY);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
