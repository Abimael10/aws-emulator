// SPDX-License-Identifier: CC0-1.0

//! Whether IAM lets a request to this data plane perform the action it asks for — the
//! authorization step taken in front of every operation, before it reads or writes anything.
//!
//! AWS decides authorization at the IAM layer, ahead of the service looking at what a handler
//! would do, so a caller whose identity-based policies do not grant the action — or that an
//! explicit `Deny` covers — is told it is not authorized rather than told anything about the
//! topic, the shadow or the connection it named, and the call performs nothing: no message is
//! published, no rule runs and no shadow moves.
//!
//! Three pieces, each held in exactly one place:
//!
//! 1. **who** — the principal the request's access key id speaks for: the session STS minted
//!    that key for (`sts_store::resolve_caller`, the same record `sts:GetCallerIdentity`
//!    answers from, so the principal a refusal blames is the one the caller reads back for the
//!    credentials it signed with), else the IAM user that owns the long-term key, else the
//!    account's own published credential;
//! 2. **what it may do** — the identity-based policy documents the IAM store holds for that
//!    principal and the permissions boundary that caps them, read through the IAM service's
//!    OWN accessors (`principal_policies`, `permissions_boundary_document`), where the
//!    collection rule itself lives — so a document `iam:PutRolePolicy` wrote takes effect on
//!    the next request and deleting it lifts it again;
//! 3. **the decision** — [`::service_common::iam_authorization`], the ONE shared evaluation of
//!    those documents against `(action, resource)`, so this service and every other one handed
//!    the same documents answer alike: an explicit `Deny` wins, otherwise an `Allow` about the
//!    request grants it, and with neither the request falls to IAM's default deny.
//!
//! Only the store LOOKUPS live here; neither the decision nor the sentence does. The lookups
//! stay in this crate — a per-service copy by the tree's convention, the same shape as
//! `service-com-amazonaws-iot-awsiotservice/src/service/authorization.rs` and
//! `…-kinesisvideomedia-…/src/service/authorization.rs` — because they read the IAM and STS
//! service stores, which `services/common` is a dependency of nearly every service crate for.
//!
//! ## The action and the resource
//!
//! The action is `iot:<OperationName>` ([`ACTION_PREFIX`]). This data plane's own signing name
//! and ARN namespace are `iotdata` (`smithy_query(query="service_info")`), but the permissions
//! that govern it are AWS IoT Core's: a role is granted `iot:Publish`, `iot:GetThingShadow`
//! and `iot:UpdateThingShadow`, and the resources those statements name are ARNs of the `iot`
//! namespace, which is also the namespace the registry this plane sits beside mints its
//! resources in. So one statement scoped to `iot:*` covers the control plane and this plane
//! alike, which is what a grant written for AWS IoT means.
//!
//! The resource is the one the REQUEST ADDRESSES, read off the request line: the identifier
//! each operation binds as an `@httpLabel`, under the ARN resource type AWS IoT gives that kind
//! of resource ([`ADDRESSED_RESOURCES`]) — a `topic/<topic>` for the message APIs, a
//! `thing/<thingName>` for the shadow APIs, a `client/<clientId>` for the connection APIs.
//! Taking it from the request rather than from a stored record is what keeps the decision
//! independent of whether that resource exists, which is also what keeps a refused call from
//! telling an unauthorized caller whether it does.
//!
//! A request that addresses no resource of its own — the account-wide listing of retained
//! messages — is judged against [`ANY_RESOURCE`], for which the GRANT question is the weaker
//! one `iam_authorization` states for a gate that does not know the resource ARN: whether any
//! readable `Allow` covers the action at all. Refusing such a call on the resource element
//! instead would deny calls AWS permits. The DENY question is always decided against the
//! resource, so a `Deny` scoped to an ARN a request does carry still refuses it.
//!
//! The IoT POLICY documents attached to a certificate or a Cognito identity are not part of
//! this decision: those grant a DEVICE its MQTT connections, publications and subscriptions,
//! while every request this gate judges is an API call signed by a credential of this account,
//! whose grants are its identity's.
//!
//! ## What a refused call is answered with
//!
//! HTTP 403 `ForbiddenException` — "The caller isn't authorized to make the request", the
//! shape this model declares with that status
//! (`smithy_query(query="shape", shape="ForbiddenException")`). The model binds it to the
//! connection APIs only, and the shadow and message APIs declare `UnauthorizedException`
//! (401) — which is this plane's answer about a DEVICE's own policies, not IAM's about the
//! caller's — so for those operations the pair is the one this plane is RECORDED to answer an
//! identity it holds no grant for with, and it is rendered here in this plane's own envelope
//! rather than returned by a handler.
//!
//! That envelope carries NOTHING of the decision: the recorded 403 of this plane is the two
//! members `message` and `traceId`, with `message` NULL and the trace id the per-request one
//! this plane mints for every envelope it writes ([`crate::state::new_trace_id`]). So the
//! refusal names neither the principal, nor the action, nor the resource — a client reads the
//! error SHAPE and nothing else — and the sentence IAM would state is kept for this service's
//! own log alone ([`Denial::reason`]).
//!
//! ## The callers this gate does not bound
//!
//! * **The account's own published credential.** A request whose access key id neither STS nor
//!   IAM holds a record for resolves to the account's own identity, which is not a principal
//!   the IAM store holds policies for — the position AWS account root credentials are in. Such
//!   a request is allowed.
//! * **A principal holding an attachment this emulator cannot read.** An AWS-managed policy
//!   belongs to AWS rather than to this account, so the IAM store holds no document for it;
//!   refusing on a grant this emulator cannot see would deny access AWS permits. An explicit
//!   `Deny` in a document that CAN be read still refuses, and a permissions boundary the store
//!   DOES hold still caps the grant.
//! * **An engine carrying no STS or IAM store**, where no session and no role can exist.
//!
//! An expired session credential speaks for nobody: it carries no documents and no principal
//! spelling, so every action it asks for is refused. (A session the authenticating layer has
//! already refused never reaches a handler at all.)

use std::time::SystemTime;

use ::aws_smithy_runtime_api::http::{Response as WireResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The IAM service prefix the actions of this plane carry: `iot:Publish`,
/// `iot:GetThingShadow`, `iot:UpdateThingShadow`. See the module docs for why it is AWS IoT
/// Core's namespace rather than this plane's own `iotdata`.
pub const ACTION_PREFIX: &str = "iot";

/// The ARN namespace of every resource a statement about this plane names, which is the
/// namespace the AWS IoT registry mints its resources in.
const ARN_NAMESPACE: &str = "iot";

/// The resource IAM writes for a request that addresses no resource of this plane.
pub const ANY_RESOURCE: &str = "*";

/// The wire name of the refusal this gate answers with, and the status it carries.
const FORBIDDEN_EXCEPTION: &str = "ForbiddenException";
const FORBIDDEN_STATUS: u16 = 403;

/// The media type this restJson1 service renders an error in.
const JSON_CONTENT_TYPE: &str = "application/json";

/// What a refusal names as the principal of a request whose session credential has already
/// expired: that credential speaks for nobody, and the key it presented is not quoted back.
const EXPIRED_CREDENTIAL_PRINCIPAL: &str = "an expired session credential";

/// Where each operation of this plane carries the identifier of the resource it addresses, and
/// the ARN resource type that identifier names: the operation's own name, the `@http` URI
/// segment (0-based) its `@httpLabel` stands at, and the AWS IoT resource type.
///
/// Source: every operation's `@http` URI
/// (`smithy_query(query="digest")` over this service's model) —
/// `POST /topics/{topic}`, `GET /retainedMessage/{topic}`,
/// `GET|POST|DELETE /things/{thingName}/shadow`,
/// `GET /api/things/shadow/ListNamedShadowsForThing/{thingName}`,
/// `GET|DELETE /connections/{clientId}`, `GET /connections/{clientId}/subscriptions` and
/// `POST /connections/{clientId}/messages` — paired with the resource type AWS IoT gives that
/// kind of resource in an ARN: a topic for the message APIs, a thing for the shadow APIs and a
/// client for the connection APIs.
///
/// The table is CLOSED and keyed by the OPERATION rather than by the leading path segment,
/// because one of these URIs carries its label four segments deep under a path whose leading
/// segment names no collection at all. The one operation it does not name,
/// `ListRetainedMessages` (`GET /retainedMessage`), addresses no resource of its own and is
/// judged against [`ANY_RESOURCE`].
const ADDRESSED_RESOURCES: &[(&str, usize, &str)] = &[
    ("Publish", 1, "topic"),
    ("GetRetainedMessage", 1, "topic"),
    ("GetThingShadow", 1, "thing"),
    ("UpdateThingShadow", 1, "thing"),
    ("DeleteThingShadow", 1, "thing"),
    ("ListNamedShadowsForThing", 4, "thing"),
    ("GetConnection", 1, "client"),
    ("DeleteConnection", 1, "client"),
    ("ListSubscriptions", 1, "client"),
    ("SendDirectMessage", 1, "client"),
];

/// One refused request: why IAM refused it, for this service's own log. Nothing of it reaches
/// the caller — the answer this plane gives states no reason (see the module docs) — and the
/// status and the error shape are this module's constants.
#[derive(Debug, Clone)]
pub struct Denial {
    /// Who was refused, which action, on which resource when the request addressed one, and
    /// which of IAM's rules refused it: the sentence IAM states an authorization failure with.
    pub reason: String,
}

/// The resource this request is judged against: the ARN of the resource it ADDRESSES, else
/// [`ANY_RESOURCE`].
///
/// `region` is the Region the request was signed for, which is the Region segment of the ARN,
/// and `path` the request's own path.
#[must_use]
pub fn judged_resource(region: &str, operation: &str, path: &str) -> String {
    path_addressed_arn(region, operation, path).unwrap_or_else(|| ANY_RESOURCE.to_string())
}

/// The ARN of the resource the request line addresses: the AWS IoT ARN of the resource type
/// [`ADDRESSED_RESOURCES`] gives this operation, over the identifier the path segment it names
/// carries.
///
/// `None` for an operation that addresses no resource of its own, and for a request whose path
/// carries no identifier at that segment at all.
fn path_addressed_arn(region: &str, operation: &str, path: &str) -> Option<String> {
    let (_operation, at, resource_type) = ADDRESSED_RESOURCES
        .iter()
        .find(|(named, _at, _resource_type)| *named == operation)?;
    let addressed =
        ::service_common::nth_path_segment(path, *at).filter(|segment| !segment.is_empty())?;
    // The label as the codec that deserializes the request reads it, so an identifier an SDK
    // percent-encoded — a multi-level topic name, whose separators travel escaped — is the
    // identifier the handler will act on.
    let identifier = ::service_common::percent_decode_path_label(addressed);
    Some(::emulator_server::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{resource_type}/{identifier}"),
    ))
}

/// Authorize this request, where `operation` is the operation's own name
/// ([`smithy_server_runtime::Operation::NAME`]) and `resource` the ARN the request ADDRESSES
/// ([`judged_resource`]).
///
/// `Ok(())` for a request IAM permits (and for every caller this gate does not bound, see the
/// module docs); the refusal otherwise. Taken BEFORE the operation runs, so a refused request
/// reads and writes nothing.
///
/// Resolving the caller goes through STS's own `resolve_caller`, which first materialises the
/// session expiries the clock has already made due; that write is owed at the same instant
/// whether or not anybody called, which is why a REPORTING operation of this service takes this
/// decision too.
pub fn authorize<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    resource: &str,
) -> Result<(), Denial> {
    let action = format!("{ACTION_PREFIX}:{operation}");
    let caller = caller(engine, ctx);
    // The principal spellings travel with the request because that is what a statement's
    // `Principal` element would be matched against; an identity-based document states no such
    // element, so the decision below rests on the action and the resource.
    let request = iam::Request::new(&action, resource, &caller.principals);
    let documents = iam::prepare_documents(&caller.documents);
    let identity_said = iam::identity_decision_prepared(&documents, &request);
    // The decision is taken FIRST, so an explicit `Deny` in a document that CAN be read refuses
    // the request whatever else the principal carries — including a grant this emulator cannot
    // read, which only lifts the DEFAULT deny of a request no readable statement allows (see
    // the module docs).
    if identity_said == Decision::Denied {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            iam::IDENTITY_POLICY_EXPLICIT_DENY,
        ));
    }
    let granted = iam::is_allowed(identity_said, Decision::NotStated)
        || may_allow(&documents, &action, resource)
        || caller.identity_is_unbounded();
    if !granted {
        return Err(not_authorized(
            &caller.arn,
            &action,
            resource,
            &iam::no_identity_policy_allows(&action),
        ));
    }
    // "A permissions boundary sets the maximum permissions that an identity-based policy can
    // grant" (IAM, permissions boundaries): a grant the boundary does not also allow is not a
    // permission the principal has. The account's own credential carries none.
    if let Some(boundary) = &caller.boundary {
        let boundary = iam::prepare_documents(::std::slice::from_ref(boundary));
        let reason = match iam::identity_decision_prepared(&boundary, &request) {
            Decision::Allowed => return Ok(()),
            // An explicit deny is a fact about the boundary document, so the refusal names it;
            // a boundary that merely states nothing withheld the grant the identity-based
            // policies gave.
            Decision::Denied => iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string(),
            Decision::NotStated => {
                if may_allow(&boundary, &action, resource) {
                    return Ok(());
                }
                iam::no_permissions_boundary_allows(&action)
            }
        };
        return Err(not_authorized(&caller.arn, &action, resource, &reason));
    }
    Ok(())
}

/// Whether `documents` could allow `action` at all, asked only for a request judged against
/// [`ANY_RESOURCE`] — one that addresses no resource of this plane, and so may still be about a
/// resource a statement is scoped to (see the module docs).
///
/// A request that DOES address a resource is decided against that ARN alone, where the resource
/// element is what AWS matches it on.
fn may_allow(documents: &[iam::PreparedDocument], action: &str, resource: &str) -> bool {
    resource == ANY_RESOURCE && iam::identity_may_allow_action(documents, action)
}

/// The refusal IAM states an authorization failure with: who was refused, which action, on
/// which resource, and which of IAM's rules refused it. Every value in it comes from the request
/// and from the stored policies — the principal from the credential the request presented, the
/// action from the operation it named, the resource from the ARN it addressed.
///
/// It is this service's own record of the decision and reaches no response (see the module
/// docs). A request that addresses no resource of this plane is judged against
/// [`ANY_RESOURCE`], and the sentence states no resource clause for it: there is nothing to
/// name. Every other refusal is the shared sentence, so this service and the others that reach
/// the same decision word it alike.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> Denial {
    let reason = if resource == ANY_RESOURCE {
        format!("User: {principal} is not authorized to perform: {action} {reason}")
    } else {
        iam::not_authorized_by_identity_policy(principal, action, resource, reason)
    };
    Denial { reason }
}

/// The wire answer to a request this gate refused: HTTP 403, naming `ForbiddenException` in the
/// header a restJson1 client resolves an error shape from, in the envelope this plane is
/// recorded to answer a refused call with — the two members `message`, which is NULL, and
/// `traceId`, the per-request correlation id this plane mints for every envelope it writes.
///
/// It carries nothing of the request and nothing of the decision, so a caller it refuses learns
/// only that it was refused.
#[must_use]
pub fn refusal_response() -> WireResponse {
    let body = ::serde_json::json!({
        "message": ::serde_json::Value::Null,
        "traceId": crate::state::new_trace_id(),
    });
    let mut response = WireResponse::new(
        StatusCode::try_from(FORBIDDEN_STATUS).expect("the status is a constant of this module"),
        SdkBody::from(body.to_string()),
    );
    response
        .headers_mut()
        .insert("content-type", JSON_CONTENT_TYPE);
    response
        .headers_mut()
        .insert("x-amzn-errortype", FORBIDDEN_EXCEPTION);
    response
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
    /// Set when the principal has a policy attached whose document the IAM store does not hold
    /// — an AWS-managed policy (see the module docs).
    unreadable_attachment: bool,
}

impl Caller {
    /// Whether nothing the identity-based decision found can refuse this caller once no readable
    /// document DENIES the action: the account's own credential, which holds no IAM policies, or
    /// a principal carrying a grant this emulator cannot read.
    fn identity_is_unbounded(&self) -> bool {
        self.account_credential || self.unreadable_attachment
    }

    /// Read what the IAM identity `identity_arn` names may do, through the IAM service's own
    /// accessors: the documents in force on it, whether it carries an attachment whose document
    /// this emulator cannot read, and the permissions boundary that caps it.
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
        // No STS store on this engine: the only identity a request can present is the account's
        // own published credential.
        None => Ok(sts_store::ambient_identity()),
    };
    let identity = match identity {
        Ok(identity) => identity,
        // The session credential has expired, so it speaks for nobody: no documents, no
        // principal spelling, and every action it asks for falls to the default deny. The key id
        // it presented is NOT quoted back — this module echoes no value of the request — and in
        // the assembled emulator the layer that authenticates a request refuses an expired
        // credential before any handler runs.
        Err(_expired) => {
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
    // minted is the account's ambient identity — but IAM's store maps an access key id to the
    // user that owns it (`access_keys`, the map IAM's own `GetUser` resolves a caller through),
    // so a request signed with one speaks for THAT user and is judged on the user's policies
    // rather than served as the account. A key the IAM store does not hold either really is the
    // account's own published credential.
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
        // An assumed-role session's permissions are the ROLE's, and the role is addressed by the
        // ARN the account holds it under — which carries its PATH, where the session ARN does
        // not.
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

    /// The Region every row below is signed for.
    const REGION: &str = "us-east-1";

    /// RULE: the resource a request is judged against is the AWS IoT ARN of the resource the
    /// request line addresses — a topic for the message APIs, a thing for the shadow APIs, a
    /// client for the connection APIs — and `*` for the one call that addresses no resource of
    /// its own.
    ///
    /// The rows differ along which operation is called and what its path carries: each resource
    /// type, a label carrying percent-escaped separators (a multi-level topic name, the form an
    /// SDK sends one in), a label deeper in the path, the listing that addresses nothing, and a
    /// path carrying no label at all.
    #[test]
    fn the_resource_judged_is_the_one_the_request_line_addresses() {
        let account = ::engine::emulator_account_id();
        let rows: &[(&str, &str, &str, String)] = &[
            (
                "a publish to a single-level topic",
                "Publish",
                "/topics/telemetry",
                format!("arn:aws:iot:{REGION}:{account}:topic/telemetry"),
            ),
            (
                "a publish to a multi-level topic, whose separators travel escaped",
                "Publish",
                "/topics/fleet%2Fstatus%2Fone",
                format!("arn:aws:iot:{REGION}:{account}:topic/fleet/status/one"),
            ),
            (
                "a retained-message read",
                "GetRetainedMessage",
                "/retainedMessage/fleet%2Fstatus",
                format!("arn:aws:iot:{REGION}:{account}:topic/fleet/status"),
            ),
            (
                "a shadow read",
                "GetThingShadow",
                "/things/sensor-1/shadow",
                format!("arn:aws:iot:{REGION}:{account}:thing/sensor-1"),
            ),
            (
                "a shadow write",
                "UpdateThingShadow",
                "/things/sensor-1/shadow?name=calibration",
                format!("arn:aws:iot:{REGION}:{account}:thing/sensor-1"),
            ),
            (
                "a shadow delete",
                "DeleteThingShadow",
                "/things/sensor-1/shadow",
                format!("arn:aws:iot:{REGION}:{account}:thing/sensor-1"),
            ),
            (
                "the named-shadow listing, whose label stands four segments deep",
                "ListNamedShadowsForThing",
                "/api/things/shadow/ListNamedShadowsForThing/sensor-1",
                format!("arn:aws:iot:{REGION}:{account}:thing/sensor-1"),
            ),
            (
                "a connection read",
                "GetConnection",
                "/connections/client-9",
                format!("arn:aws:iot:{REGION}:{account}:client/client-9"),
            ),
            (
                "a subscription listing of one client",
                "ListSubscriptions",
                "/connections/client-9/subscriptions",
                format!("arn:aws:iot:{REGION}:{account}:client/client-9"),
            ),
            (
                "a direct message to one client",
                "SendDirectMessage",
                "/connections/client-9/messages",
                format!("arn:aws:iot:{REGION}:{account}:client/client-9"),
            ),
            (
                "the account-wide retained-message listing",
                "ListRetainedMessages",
                "/retainedMessage",
                ANY_RESOURCE.to_string(),
            ),
            (
                "a path carrying no label where one is bound",
                "GetThingShadow",
                "/things/",
                ANY_RESOURCE.to_string(),
            ),
        ];
        for (case, operation, path, expected) in rows {
            let actual = judged_resource(
                REGION,
                operation,
                ::service_common::request_path(path),
            );
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected {operation} on {path:?} to be judged against \
                 {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the action asked about is `iot:<OperationName>`, so one statement scoped to AWS
    /// IoT Core's actions covers this plane's calls as well as the registry's, and nothing
    /// outside that namespace.
    #[test]
    fn the_action_asked_about_carries_iot_cores_prefix() {
        let topic = format!(
            "arn:aws:iot:{REGION}:{}:topic/telemetry",
            ::engine::emulator_account_id()
        );
        let document = r#"{"Statement":[{"Effect":"Allow","Action":"iot:*","Resource":"*"}]}"#;
        for operation in ["Publish", "GetThingShadow", "UpdateThingShadow"] {
            let action = format!("{ACTION_PREFIX}:{operation}");
            let reached = iam::identity_decision(
                &[document.to_string()],
                &iam::Request::new(&action, &topic, &[]),
            );
            assert_eq!(
                reached,
                Decision::Allowed,
                "case {operation:?}: expected iot:* to allow {action}, got {reached:?}"
            );
        }
        let outside = iam::identity_decision(
            &[document.to_string()],
            &iam::Request::new("iotdata:Publish", &topic, &[]),
        );
        assert_eq!(
            outside,
            Decision::NotStated,
            "case \"another namespace\": expected iot:* to grant nothing outside AWS IoT \
             Core's actions, got {outside:?}"
        );
    }

    /// RULE: a refused call is answered HTTP 403, naming the error shape in the header a
    /// restJson1 client resolves one from, in this plane's own envelope — a NULL `message` and
    /// a per-request `traceId`, and nothing of the request or of the decision.
    #[test]
    fn a_refused_call_is_answered_403_forbidden() {
        let response = refusal_response();
        assert_eq!(
            response.status().as_u16(),
            FORBIDDEN_STATUS,
            "case \"a refused publish\": expected HTTP {FORBIDDEN_STATUS}, got {}",
            response.status().as_u16()
        );
        let named = response.headers().get("x-amzn-errortype");
        assert_eq!(
            named,
            Some(FORBIDDEN_EXCEPTION),
            "case \"a refused publish\": expected the error shape in the header, got {named:?}"
        );
        let body: ::serde_json::Value =
            ::serde_json::from_slice(response.body().bytes().unwrap_or_default())
                .expect("the refusal body is a JSON document");
        let members: Vec<&String> = body
            .as_object()
            .expect("the refusal body is a JSON object")
            .keys()
            .collect();
        assert_eq!(
            members,
            vec!["message", "traceId"],
            "case \"a refused publish\": expected exactly the two members this plane answers \
             with, got {body}"
        );
        assert!(
            body["message"].is_null(),
            "case \"a refused publish\": expected a null message, got {body}"
        );
        let trace = body["traceId"].as_str().unwrap_or_default();
        assert!(
            !trace.is_empty(),
            "case \"a refused publish\": expected a trace id, got {body}"
        );
        // Minted per CALL: a second refusal carries a trace id of its own.
        let again: ::serde_json::Value =
            ::serde_json::from_slice(refusal_response().body().bytes().unwrap_or_default())
                .expect("the refusal body is a JSON document");
        assert_ne!(
            again["traceId"].as_str().unwrap_or_default(),
            trace,
            "case \"two refused publishes\": expected a trace id per call, got one twice"
        );
    }

    /// RULE: the sentence IAM states the refusal with is kept for this service's own log, and
    /// names the principal, the action, the resource the request addressed and which of IAM's
    /// rules refused.
    #[test]
    fn the_logged_reason_names_the_principal_the_action_and_the_resource() {
        let denial = not_authorized(
            "arn:aws:sts::000000000000:assumed-role/publisher/session-1",
            "iot:Publish",
            "arn:aws:iot:us-east-1:000000000000:topic/telemetry",
            &iam::no_identity_policy_allows("iot:Publish"),
        );
        assert!(
            denial.reason.contains("is not authorized to perform: iot:Publish")
                && denial.reason.contains("topic/telemetry")
                && denial
                    .reason
                    .contains("arn:aws:sts::000000000000:assumed-role/publisher/session-1"),
            "case \"a refused publish\": expected the sentence to name the principal, the \
             action and the resource, got {:?}",
            denial.reason
        );
    }
}
