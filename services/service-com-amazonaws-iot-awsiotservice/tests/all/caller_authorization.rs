// SPDX-License-Identifier: CC0-1.0

//! WHO may call this control plane, driven end to end over the real wire path (a restJson1
//! request through the multi-service router into the mounted service), so what is asserted is
//! what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every operation of this service is authorized, BEFORE it reads or
//! writes anything, against the identity-based IAM policies in force on the principal its
//! credentials name — for the action `iot:<OperationName>` on the resource the request
//! addresses, which is the ARN its input names where the model binds one (`resourceArn`, the tag
//! calls), else the ARN this service mints for the resource whose identifier the request line
//! carries, and every resource for a request that addresses none. A request no readable
//! statement allows (IAM's default deny), or one an explicit `Deny` covers, is answered HTTP 403
//! `AccessDeniedException` with IAM's own sentence and changes nothing — a denied `CreateThing`
//! registers no thing; a request a matching `Allow` covers reaches the service's own verdict.
//!
//! The role, the user, their policies and the session live in the IAM and STS stores of the
//! SAME shared engine — the stores those services' own handlers write — so the documents under
//! test are the ones `iam:PutRolePolicy` / `iam:PutUserPolicy` store and the principal a
//! refusal blames is the one `sts:GetCallerIdentity` answers for the key the request signed
//! with.
//!
//! The tables span what the rule generalises over: the OPERATION called (a registry write with
//! its name in the request line, a read of one, an update, a delete, two listings, and the tag
//! calls that carry an ARN — in the body for the write and in the query for the read), the
//! document in force (none at all, every action of this service, exactly this action, another
//! action of this service, another service's whole namespace, an explicit deny beside an
//! allow), the resource an allow is scoped to (the ARN addressed, another resource, every
//! resource) and the kind of principal (an assumed-role session, an IAM user's long-term key,
//! the account's own published credential).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, which is the Region of the ARN a decision is
/// taken against.
const REGION: &str = "us-east-1";

/// An access key id neither store holds a record for: the account's own published credential,
/// which holds no IAM policy.
const AMBIENT_KEY_ID: &str = "AKIDIOTAUTHAMBIENT";

/// The access key id of the assumed-role session the role rows sign with.
const SESSION_KEY_ID: &str = "ASIAEXAMPLEIOTSESSION";

/// The access key id of the IAM user the long-term rows sign with.
const USER_KEY_ID: &str = "AKIAEXAMPLEIOTUSER";

/// The role the session is assumed from, the session's own name, the IAM user, and the name of
/// the inline policy each of them carries.
const ROLE: &str = "iot-control-plane-caller";
const SESSION: &str = "deny-test";
const USER: &str = "iot-control-plane-user";
const POLICY_NAME: &str = "inline";

/// How long the seeded session lasts: an hour, STS's own default.
const SESSION_SECONDS: i32 = 3_600;

/// The status and error code an unauthorized call is answered with: the one the AWS "Common
/// Errors" table gives an authorization refusal, which this model declares for no operation of
/// its own.
const ACCESS_DENIED: (u16, &str) = (403, "AccessDeniedException");

/// The thing every row addresses, which no call below creates — so a refusal is decided without
/// the thing existing.
const THING: &str = "orders-authorized";

/// A second thing, for the rows that read a grant scoped to one resource against another.
const OTHER_THING: &str = "orders-unrelated";

/// The resource IAM writes for a request that addresses no resource of this service.
const ANY_RESOURCE: &str = "*";

/// The ARN this service mints for the thing `name` in `region`. Spelled out here rather than
/// built from the implementation's own helper, so a change to either fails this test.
fn thing_arn(region: &str, name: &str) -> String {
    format!(
        "arn:aws:iot:{region}:{}:thing/{name}",
        engine::emulator_account_id()
    )
}

/// The principal ARN the seeded session speaks for — what a refusal must name.
fn session_arn() -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{ROLE}/{SESSION}",
        engine::emulator_account_id()
    )
}

/// The ARN the account holds the role under, which is the identity whose policies an
/// assumed-role session's request is judged on.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", engine::emulator_account_id())
}

/// The ARN the account holds the IAM user under, which a refusal names as that caller.
fn user_arn() -> String {
    format!("arn:aws:iam::{}:user/{USER}", engine::emulator_account_id())
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: String,
    body: Value,
}

impl Reply {
    /// The sentence the refusal carries, or the empty string for a body carrying none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// This service over a shared engine whose IAM and STS stores this suite seeds, reached through
/// the same `MultiServiceRouter` the production router uses.
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// One request signed with `access_key_id`, whose credential scope names this service's
    /// signing name (which is how the router picks the service) and [`REGION`]. Request
    /// authentication is off, exactly as an unauthenticated emulator runs, so the signature
    /// itself is never verified — the key id is what the gate resolves the principal from.
    async fn send_as(
        &self,
        access_key_id: &str,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> Reply {
        let payload = body
            .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
            .unwrap_or_default();
        let host = format!("iot.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/iot/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(bytes::Bytes::from(payload))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }
}

/// The operations this suite calls, chosen to span how a request of this service is bound: a
/// write and a read whose resource is a label in the request line, an update and a delete of
/// one, a listing that names no resource at all, the endpoint reader that carries no member of
/// its own, and the tag calls that carry an ARN — in the document for the write, in the query
/// string for the read.
///
/// Source: `smithy_query(query="operation", operation="CreateThing,DescribeThing,UpdateThing,
/// DeleteThing,ListThings,DescribeEndpoint,TagResource,ListTagsForResource")` — each row's
/// method, URI and bound members are that model's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    CreateThing,
    DescribeThing,
    UpdateThing,
    DeleteThing,
    ListThings,
    DescribeEndpoint,
    TagResource,
    ListTagsForResource,
}

/// Every operation this suite drives.
const EVERY_OP: &[Op] = &[
    Op::CreateThing,
    Op::DescribeThing,
    Op::UpdateThing,
    Op::DeleteThing,
    Op::ListThings,
    Op::DescribeEndpoint,
    Op::TagResource,
    Op::ListTagsForResource,
];

impl Op {
    /// The operation's own name, which is also the name of the IAM action it is performed
    /// under.
    fn name(self) -> &'static str {
        match self {
            Self::CreateThing => "CreateThing",
            Self::DescribeThing => "DescribeThing",
            Self::UpdateThing => "UpdateThing",
            Self::DeleteThing => "DeleteThing",
            Self::ListThings => "ListThings",
            Self::DescribeEndpoint => "DescribeEndpoint",
            Self::TagResource => "TagResource",
            Self::ListTagsForResource => "ListTagsForResource",
        }
    }

    /// The IAM action this operation is performed under: this service's namespace and the
    /// operation's own name.
    fn action(self) -> String {
        format!("iot:{}", self.name())
    }

    /// The resource a decision about this operation is taken against: the ARN the request's
    /// input NAMES for the tag calls, the ARN of the thing whose name the request LINE carries
    /// for the registry operations, and every resource for the operations that address no
    /// resource of this service at all.
    fn resource(self, name: &str) -> String {
        match self {
            Self::ListThings | Self::DescribeEndpoint => ANY_RESOURCE.to_string(),
            _ => thing_arn(REGION, name),
        }
    }

    /// Call this operation on the thing `name`, signed with `access_key_id`.
    async fn call(self, harness: &Harness, access_key_id: &str, name: &str) -> Reply {
        let arn = thing_arn(REGION, name);
        let (method, uri, body) = match self {
            Self::CreateThing => ("POST", format!("/things/{name}"), Some(json!({}))),
            Self::DescribeThing => ("GET", format!("/things/{name}"), None),
            Self::UpdateThing => (
                "PATCH",
                format!("/things/{name}"),
                Some(json!({ "attributePayload": { "attributes": { "site": "berlin" } } })),
            ),
            Self::DeleteThing => ("DELETE", format!("/things/{name}"), None),
            Self::ListThings => ("GET", "/things?maxResults=10".to_string(), None),
            Self::DescribeEndpoint => ("GET", "/endpoint".to_string(), None),
            Self::TagResource => (
                "POST",
                "/tags".to_string(),
                Some(json!({ "resourceArn": arn, "tags": [{ "Key": "env", "Value": "prod" }] })),
            ),
            Self::ListTagsForResource => (
                "GET",
                format!(
                    "/tags?{}",
                    form_urlencoded::Serializer::new(String::new())
                        .append_pair("resourceArn", &arn)
                        .finish()
                ),
                None,
            ),
        };
        harness.send_as(access_key_id, method, &uri, body).await
    }
}

/// Record the role and the assumed-role session [`SESSION_KEY_ID`] speaks for, as
/// `iam:CreateRole` and `sts:AssumeRole` record them.
fn given_session(engine: &Arc<engine::InMemoryEngine>) {
    let role_arn = role_arn();
    let iam_store = iam::service_state(&**engine).expect("the IAM store");
    iam_store.lock().roles.insert(
        iam::name_key(ROLE),
        service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::Role {
            path: "/".to_string(),
            role_name: ROLE.to_string(),
            role_id: "AROAEXAMPLEIOTCALLER".to_string(),
            arn: role_arn.clone(),
            create_date: aws_smithy_types::DateTime::from_secs(0),
            assume_role_policy_document: None,
            description: None,
            max_session_duration: None,
            permissions_boundary: None,
            tags: None,
            role_last_used: None,
            source_role_template: None,
        },
    );
    let vend = sts::NewSession {
        role_arn: Some(role_arn.clone()),
        ..sts::NewSession::new(
            sts::Identity {
                user_id: sts::role_id(&role_arn),
                account: engine::emulator_account_id().to_string(),
                arn: session_arn(),
            },
            sts::SessionKind::AssumedRole,
            SESSION_SECONDS,
        )
    };
    let store = sts::service_state(&**engine).expect("the STS store");
    store
        .lock()
        .insert(vend.into_session(SESSION_KEY_ID, SystemTime::now()))
        .expect("the session is recorded");
}

/// Record the IAM user [`USER_KEY_ID`] belongs to, as `iam:CreateUser` and
/// `iam:CreateAccessKey` record one: the user, and the key record that maps the access key id
/// to it.
fn given_user(engine: &Arc<engine::InMemoryEngine>) {
    let minted = aws_smithy_types::DateTime::from_secs(0);
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.users.insert(
        iam::name_key(USER),
        service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
            path: "/".to_string(),
            user_name: USER.to_string(),
            user_id: "AIDAEXAMPLEIOTUSER1".to_string(),
            arn: user_arn(),
            create_date: minted,
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    store.access_keys.insert(
        USER_KEY_ID.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(USER),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: minted,
        },
    );
}

/// Which identity an inline policy is attached to.
#[derive(Debug, Clone, Copy)]
enum Target {
    Role,
    User,
}

/// Put `document` in force as the identity's inline policy, replacing whatever it carried —
/// what `iam:PutRolePolicy` / `iam:PutUserPolicy` do. `None` takes every policy off it, as the
/// matching delete does.
fn given_inline_policy(
    engine: &Arc<engine::InMemoryEngine>,
    target: Target,
    document: Option<&Value>,
) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    let (key, attached) = match target {
        Target::Role => (iam::name_key(ROLE), &mut store.role_inline_policies),
        Target::User => (iam::name_key(USER), &mut store.user_inline_policies),
    };
    match document {
        None => {
            attached.remove(&key);
        }
        Some(document) => {
            attached.insert(
                key,
                BTreeMap::from([(
                    iam::name_key(POLICY_NAME),
                    iam::InlinePolicy {
                        policy_name: POLICY_NAME.to_string(),
                        policy_document: document.to_string(),
                    },
                )]),
            );
        }
    }
}

/// A policy allowing `actions` on every resource.
fn allow(actions: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
    })
}

/// A policy allowing `action` on `resource` alone.
fn allow_on(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
}

/// Which of IAM's refusals a row expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The default deny of a request no readable statement allows.
    DefaultDeny,
    /// The explicit deny of a request a `Deny` statement covers.
    ExplicitDeny,
}

impl Refusal {
    /// The clause IAM closes the refusal with.
    fn reason(self, action: &str) -> String {
        match self {
            Self::DefaultDeny => {
                format!("because no identity-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// IAM's whole sentence for a refusal: who was refused, which action, on which resource when
/// the request addressed one, and which of IAM's rules refused it. Spelled out here rather than
/// built from the helpers the implementation calls, so a change to either half of the wording
/// fails this test.
fn denial(principal: &str, action: &str, resource: &str, refusal: Refusal) -> String {
    let on_resource = if resource == ANY_RESOURCE {
        String::new()
    } else {
        format!(" on resource: {resource}")
    };
    format!(
        "User: {principal} is not authorized to perform: {action}{on_resource} {}",
        refusal.reason(action)
    )
}

/// Assert that `reply` is the authorization refusal naming `principal`, `action` and
/// `resource`.
fn assert_refused(
    case: &str,
    reply: &Reply,
    principal: &str,
    action: &str,
    resource: &str,
    refusal: Refusal,
) {
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        ACCESS_DENIED,
        "case {case:?}: expected {ACCESS_DENIED:?}, got {reply:?}"
    );
    assert_eq!(
        reply.message(),
        denial(principal, action, resource, refusal),
        "case {case:?}: expected IAM's own sentence, got {:?}",
        reply.message()
    );
}

/// Assert that `reply` is not an authorization refusal: the call reached the service's own
/// verdict, whatever that verdict was.
fn assert_not_refused(case: &str, reply: &Reply) {
    assert_ne!(
        (reply.status, reply.error_code.as_str()),
        ACCESS_DENIED,
        "case {case:?}: expected the call to reach the service, got {reply:?}"
    );
}

/// One document a row puts in force, and what it says about the operation under test.
struct Document {
    case: &'static str,
    /// The policy attached to the identity, or `None` for one carrying none at all.
    document: Option<Value>,
    /// How IAM refuses the call under it, or `None` for a document that allows it.
    expected: Option<Refusal>,
}

/// The documents a row puts in force, and what each says about the operation under test: `None`
/// for a row the policies allow, `Some(refusal)` for one they do not.
fn documents_in_force(op: Op) -> Vec<Document> {
    vec![
        Document {
            case: "no document at all",
            document: None,
            expected: Some(Refusal::DefaultDeny),
        },
        Document {
            case: "every action of this service",
            document: Some(allow(json!("iot:*"))),
            expected: None,
        },
        Document {
            case: "exactly this action",
            document: Some(allow(json!(op.action()))),
            expected: None,
        },
        Document {
            case: "another action of this service",
            document: Some(allow(json!(other_action(op)))),
            expected: Some(Refusal::DefaultDeny),
        },
        Document {
            case: "every action of the device data plane",
            document: Some(allow(json!("iotdata:*"))),
            expected: Some(Refusal::DefaultDeny),
        },
        Document {
            case: "an explicit deny beside an allow of the namespace",
            document: Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "iot:*", "Resource": "*" },
                    { "Effect": "Deny", "Action": op.action(), "Resource": "*" },
                ],
            })),
            expected: Some(Refusal::ExplicitDeny),
        },
    ]
}

/// An action of this service that is NOT the one under test, so a grant of it allows the row
/// nothing.
fn other_action(op: Op) -> String {
    EVERY_OP
        .iter()
        .find(|other| **other != op)
        .expect("the suite drives more than one operation")
        .action()
}

/// RULE: each operation is authorized under its OWN `iot:` action, so a document that allows
/// another action of this service, or every action of another service, allows none of them —
/// and an explicit deny refuses whatever else the principal carries.
#[tokio::test]
async fn every_operation_is_authorized_against_its_own_iot_action() {
    let harness = Harness::new();
    given_session(&harness.engine);
    let principal = session_arn();
    for op in EVERY_OP {
        for document in documents_in_force(*op) {
            let case = format!("{} under {}", op.name(), document.case);
            given_inline_policy(&harness.engine, Target::Role, document.document.as_ref());
            let reply = op.call(&harness, SESSION_KEY_ID, THING).await;
            match document.expected {
                None => assert_not_refused(&case, &reply),
                Some(refusal) => assert_refused(
                    &case,
                    &reply,
                    &principal,
                    &op.action(),
                    &op.resource(THING),
                    refusal,
                ),
            }
        }
    }
}

/// RULE: the principal a decision is made about is the one the request's access key id speaks
/// for — an assumed-role session is judged on its ROLE's documents, an IAM user's long-term key
/// on the USER's own, and the account's published credential carries no IAM policy at all, so
/// it is not bounded by one.
#[tokio::test]
async fn the_principal_judged_is_the_one_the_key_speaks_for() {
    let harness = Harness::new();
    given_session(&harness.engine);
    given_user(&harness.engine);
    let op = Op::CreateThing;
    // (case, the key the request signs with, the identity the policy is attached to, the
    // principal a refusal must name)
    let callers: &[(&str, &str, Option<Target>, Option<String>)] = &[
        (
            "an assumed-role session",
            SESSION_KEY_ID,
            Some(Target::Role),
            Some(session_arn()),
        ),
        (
            "an IAM user's long-term key",
            USER_KEY_ID,
            Some(Target::User),
            Some(user_arn()),
        ),
        (
            "the account's own published credential",
            AMBIENT_KEY_ID,
            None,
            None,
        ),
    ];
    for (case, key_id, target, principal) in callers {
        // With no document in force, a principal the IAM store holds is refused and the
        // account's own credential is not.
        if let Some(target) = target {
            given_inline_policy(&harness.engine, *target, None);
        }
        let reply = op.call(&harness, key_id, THING).await;
        match principal {
            Some(principal) => assert_refused(
                &format!("{case}: carrying no document"),
                &reply,
                principal,
                &op.action(),
                &op.resource(THING),
                Refusal::DefaultDeny,
            ),
            None => assert_not_refused(&format!("{case}: carrying no document"), &reply),
        }
        // And with the action allowed, the one the store holds reaches the service.
        if let Some(target) = target {
            given_inline_policy(&harness.engine, *target, Some(&allow(json!(op.action()))));
            let reply = op.call(&harness, key_id, THING).await;
            assert_not_refused(&format!("{case}: allowed the action"), &reply);
        }
    }
}

/// RULE: the resource a decision is taken against is the ARN of the resource the request
/// ADDRESSES — the one its input names for a tag call, and the one this service mints for the
/// identifier the request line carries otherwise — so a grant scoped to one resource covers the
/// calls that address it and no others.
#[tokio::test]
async fn a_grant_scoped_to_one_resource_covers_the_call_that_names_it() {
    let harness = Harness::new();
    given_session(&harness.engine);
    let principal = session_arn();
    // The tag calls carry the ARN in their input (in the document and in the query string); the
    // registry calls carry the thing's NAME in the request line.
    for op in [
        Op::TagResource,
        Op::ListTagsForResource,
        Op::DescribeThing,
        Op::CreateThing,
    ] {
        given_inline_policy(
            &harness.engine,
            Target::Role,
            Some(&allow_on(&op.action(), &thing_arn(REGION, THING))),
        );
        let reply = op.call(&harness, SESSION_KEY_ID, THING).await;
        assert_not_refused(
            &format!("{}: the resource the grant names", op.name()),
            &reply,
        );
        let reply = op.call(&harness, SESSION_KEY_ID, OTHER_THING).await;
        assert_refused(
            &format!("{}: another resource", op.name()),
            &reply,
            &principal,
            &op.action(),
            &op.resource(OTHER_THING),
            Refusal::DefaultDeny,
        );
    }
}

/// RULE: a refused request performs NOTHING — the decision is taken in front of the handler, so
/// a denied create registers no thing, while the same create under a grant registers one that
/// reads back.
#[tokio::test]
async fn a_refused_call_writes_nothing_and_an_allowed_one_writes() {
    let harness = Harness::new();
    given_session(&harness.engine);
    // Denied: no document in force at all.
    given_inline_policy(&harness.engine, Target::Role, None);
    let reply = Op::CreateThing.call(&harness, SESSION_KEY_ID, THING).await;
    assert_refused(
        "the create a default deny refused",
        &reply,
        &session_arn(),
        &Op::CreateThing.action(),
        &Op::CreateThing.resource(THING),
        Refusal::DefaultDeny,
    );
    // Read back as the account's own credential, which no document bounds: the registry holds
    // nothing, so the refused create wrote nothing.
    let read = Op::DescribeThing.call(&harness, AMBIENT_KEY_ID, THING).await;
    assert_eq!(
        (read.status, read.error_code.as_str()),
        (404, "ResourceNotFoundException"),
        "case {:?}: expected the refused create to have registered nothing, got {read:?}",
        "the registry after a refused create"
    );
    // Allowed: the same create under a grant of the action reaches the handler and persists.
    given_inline_policy(
        &harness.engine,
        Target::Role,
        Some(&allow(json!(Op::CreateThing.action()))),
    );
    let reply = Op::CreateThing.call(&harness, SESSION_KEY_ID, THING).await;
    assert_eq!(
        reply.status, 200,
        "case {:?}: expected the authorized create to be served, got {reply:?}",
        "the create a grant allowed"
    );
    let read = Op::DescribeThing.call(&harness, AMBIENT_KEY_ID, THING).await;
    assert_eq!(
        (read.status, read.body.get("thingName").and_then(Value::as_str)),
        (200, Some(THING)),
        "case {:?}: expected the authorized create to have registered the thing, got {read:?}",
        "the registry after an authorized create"
    );
}

/// RULE, the other half of the one above: the resource a decision is taken against is the one
/// the request ADDRESSES, so a value standing at the ARN member of a request whose operation
/// does not BIND that member names nothing — the decision stays on the resource the request line
/// carries, and a caller cannot nominate the resource its own request is judged against.
///
/// The gate is asked before the request is deserialized, so such a member is still in the
/// document the codec will discard; the rows are the same create with and without it, under a
/// grant scoped to a DIFFERENT thing than the one the request line names.
#[tokio::test]
async fn a_resource_arn_a_request_does_not_bind_names_no_resource() {
    let harness = Harness::new();
    given_session(&harness.engine);
    let op = Op::CreateThing;
    // The grant covers one thing only; every row below addresses another.
    given_inline_policy(
        &harness.engine,
        Target::Role,
        Some(&allow_on(&op.action(), &thing_arn(REGION, THING))),
    );
    // (case, the body the create carries)
    let rows: Vec<(&str, Value)> = vec![
        ("a create carrying no such member", json!({})),
        (
            "a create carrying the ARN the grant names at the tag calls' member",
            json!({ "resourceArn": thing_arn(REGION, THING) }),
        ),
        (
            "a create carrying that ARN beside a member it does bind",
            json!({
                "resourceArn": thing_arn(REGION, THING),
                "attributePayload": { "attributes": { "site": "berlin" } },
            }),
        ),
    ];
    for (case, body) in rows {
        let reply = harness
            .send_as(
                SESSION_KEY_ID,
                "POST",
                &format!("/things/{OTHER_THING}"),
                Some(body),
            )
            .await;
        assert_refused(
            case,
            &reply,
            &session_arn(),
            &op.action(),
            &op.resource(OTHER_THING),
            Refusal::DefaultDeny,
        );
    }
    // And the thing the request line named was never registered, read as the account's own
    // credential, which no document bounds.
    let read = Op::DescribeThing
        .call(&harness, AMBIENT_KEY_ID, OTHER_THING)
        .await;
    assert_eq!(
        (read.status, read.error_code.as_str()),
        (404, "ResourceNotFoundException"),
        "case {:?}: expected the refused creates to have registered nothing, got {read:?}",
        "the registry after the refused creates"
    );
}
