// SPDX-License-Identifier: CC0-1.0

//! Who may call the OpenSearch Serverless and Bedrock Agent CONTROL PLANES, and what a
//! running function's own call to either of them is decided against.
//!
//! RULE: an AWS service decides a request at the IAM layer, in front of the operation, so a
//! call whose signing principal is allowed `<service action namespace>:<OperationName>` on
//! the resource the request addresses by the identity-based policies in force on it is
//! served, and one no such policy allows is refused `AccessDeniedException` (HTTP 403)
//! naming the principal, the action and that resource — with NO effect: a refused create
//! mints nothing and a refused tag write tags nothing. The principal is the identity the
//! access key the request signed with names (the STS session registry, the same source
//! `sts:GetCallerIdentity` answers from), and for a call an AWS SDK makes from inside a
//! running function that identity is a session of the function's own execution role.
//!
//! Four crates have to agree for that to hold — IAM holds the roles and their documents, STS
//! mints the session credential and names its principal, Lambda runs the handler and supplies
//! it the SDK, and the two control planes take the decision — so the suite lives here; a
//! crate-local suite can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: WHICH service the call is made to
//! (two, one binding its requests with an AWS JSON protocol and one with `restJson1`), WHICH
//! operation (a listing, a create, a tag write, a read of one resource), what the role's own
//! document allows (nothing of the service, another service's action, another action of the
//! same service, the action itself, every action of the service, an explicit deny over an
//! allow), and WHICH resource that statement names (the resource addressed, another resource
//! of the same kind, every resource). Each row asserts both halves: what the service answered
//! and what it HOLDS afterwards.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN and every session's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The two action namespaces the rows are decided under: each service's own `arnNamespace`
/// (`smithy_query(query="service_info")` on either model).
const AOSS: &str = "aoss";
const BEDROCK: &str = "bedrock";

/// The code and status AWS refuses a request IAM does not permit with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The encryption policy the collections below are created under: OpenSearch Serverless
/// refuses a collection create whose name no `encryption` security policy covers, so the
/// policy is written first, by the account's own credential.
const ENCRYPTION_POLICY: &str = "gate-encryption";

/// The collection the tag-write rows address, created by the account's own credential.
const BASE_COLLECTION: &str = "gate-base-collection";

/// A collection of this account the scoped rows name INSTEAD of the one they address.
const OTHER_COLLECTION_ID: &str = "zzzzzzzzzzzzzzzzzzzz";

/// A well-formed agent identifier nobody created: ten characters of the `AgentId` alphabet
/// (`smithy_query(query="operation", operation="GetAgent")` -> pattern `^[0-9a-zA-Z]{10}$`).
const MISSING_AGENT_ID: &str = "ZZZZZZZZZ9";

/// Another well-formed agent identifier, which the scoped rows name instead.
const OTHER_AGENT_ID: &str = "AAAAAAAAA1";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack reaching
/// `CREATE_COMPLETE`, a created function reaching `Active` — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The `StackStatus` the functions' stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched
/// back into this same router, so the services it reaches are the ones this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request and the access key id is what names the
    // caller.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential is `access_key_id` and whose scope names
/// `signing_name`, which is what routes the request.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// The long-term key every emulator publishes, which is the account's own credential.
const ACCOUNT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// One awsQuery call of `service`, answered with the HTTP status and the XML body, made as
/// the account's own credential.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(ACCOUNT_KEY, service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One answer from either control plane: the status, the wire error code (the `x-amzn-errortype`
/// header a restJson1 answer carries, else the body's `__type`) and the parsed body.
#[derive(Debug)]
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

/// One OpenSearch Serverless call over its `awsJson1_0` wire, signed as `access_key_id`.
async fn aoss(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("aoss.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization(access_key_id, AOSS))
        .header("x-amz-target", format!("OpenSearchServerless.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    answered(router.dispatch(request).await)
}

/// One Bedrock Agent control-plane call over its `restJson1` wire, signed as `access_key_id`.
async fn bedrock_agent(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    path: &str,
    body: &Value,
) -> Answer {
    let payload = if body.is_null() {
        Bytes::new()
    } else {
        Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("bedrock-agent.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(access_key_id, BEDROCK))
        .body(payload)
        .expect("a well-formed request");
    answered(router.dispatch(request).await)
}

/// One HTTP answer, read the way a client of either protocol reads a failure.
fn answered(response: http::Response<Bytes>) -> Answer {
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let raw = response.into_body();
    let body: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    let code = header_code
        .or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .unwrap_or_default();
    Answer { status, code, body }
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(ACCOUNT_KEY, "lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The ARN of a collection of this account in this Region, as OpenSearch Serverless mints it.
fn collection_arn(id: &str) -> String {
    format!("arn:aws:{AOSS}:{REGION}:{ACCOUNT}:collection/{id}")
}

/// The ARN of an agent of this account in this Region, as the Bedrock Agent plane mints it.
fn agent_arn(id: &str) -> String {
    format!("arn:aws:{BEDROCK}:{REGION}:{ACCOUNT}:agent/{id}")
}

/// The trust policy a role assumed by this suite — and by Lambda, for the function rows —
/// needs.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
                "Action": "sts:AssumeRole",
            },
            {
                "Effect": "Allow",
                "Principal": {"Service": "lambda.amazonaws.com"},
                "Action": "sts:AssumeRole",
            },
        ],
    })
    .to_string()
}

/// The inline document a role carries: the log actions every execution role needs — which
/// name no action of either control plane, so a role carrying nothing else is the position a
/// policy that was never extended is in — plus the statement the row itself puts in force.
///
/// A `Deny` row carries the WILDCARD allow of the same action namespaces beneath its deny, so
/// what the row asks is what an explicit deny does to a grant that would otherwise cover the
/// call.
fn role_document(effect: &str, actions: &[&str], resource: &str) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if !actions.is_empty() {
        if effect == "Deny" {
            let namespaces: Vec<String> = actions
                .iter()
                .map(|action| {
                    let (namespace, _) = action.split_once(':').unwrap_or((action, "*"));
                    format!("{namespace}:*")
                })
                .collect();
            statements.push(json!({
                "Effect": "Allow",
                "Action": namespaces,
                "Resource": "*",
            }));
        }
        statements.push(json!({
            "Effect": effect,
            "Action": actions,
            "Resource": resource,
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// Create one IAM role carrying `document` and answer the ARN IAM minted for it.
async fn create_role(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let arn = opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    });
    let (status, embedded) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "probe-grants"),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {embedded}"
    );
    arn
}

/// Assume `role_arn` and answer the access key id of the session STS minted.
async fn assume_role(router: &MultiServiceRouter, role_arn: &str) -> String {
    let (status, body) = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[("RoleArn", role_arn), ("RoleSessionName", "probe")],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole({role_arn:?}) must succeed, got {status} with {body}"
    );
    opt_element(&body, "AccessKeyId").unwrap_or_else(|| {
        panic!("setup: AssumeRole({role_arn:?}) must answer an access key id, got {body}")
    })
}

/// The sentence a refusal carries, built here from the request rather than from either
/// service's own helper, so the wording has something to be wrong about: IAM names the
/// principal, the action, the resource the request addressed and which of its rules withheld
/// the grant.
fn refusal_message(principal: &str, action: &str, resource: &str, denied: bool) -> String {
    let reason = if denied {
        "with an explicit deny in an identity-based policy".to_string()
    } else {
        format!("because no identity-based policy allows the {action} action")
    };
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// WHICH call a row makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// `aoss:ListCollections`, which addresses no resource of its own.
    ListCollections,
    /// `aoss:CreateCollection`, which addresses no resource of its own: the collection it is
    /// about does not exist yet and the request carries its NAME.
    CreateCollection,
    /// `aoss:TagResource`, which names its target by ARN.
    TagCollection,
    /// `bedrock:ListAgents`, which addresses no resource of its own.
    ListAgents,
    /// `bedrock:GetAgent`, which addresses one agent by its identifier.
    GetAgent,
}

impl Probe {
    /// The IAM action the call performs: `<namespace>:<OperationName>`.
    fn action(self) -> &'static str {
        match self {
            Self::ListCollections => "aoss:ListCollections",
            Self::CreateCollection => "aoss:CreateCollection",
            Self::TagCollection => "aoss:TagResource",
            Self::ListAgents => "bedrock:ListAgents",
            Self::GetAgent => "bedrock:GetAgent",
        }
    }

    /// The resource the call is judged against: the ARN it names, else every resource, which
    /// is what IAM writes for a request that addresses none.
    fn resource(self, base_collection_arn: &str) -> String {
        match self {
            Self::TagCollection => base_collection_arn.to_string(),
            Self::GetAgent => agent_arn(MISSING_AGENT_ID),
            _ => "*".to_string(),
        }
    }

    /// The status the SERVICE answers this call with once IAM permits it: a listing and a
    /// create are served, and a read of an agent nobody created is the plane's own
    /// not-found.
    fn served_status(self) -> u16 {
        match self {
            Self::GetAgent => 404,
            _ => 200,
        }
    }

    /// The wire error code that answer carries: none for a served call, and the plane's own
    /// modelled not-found for the read of an agent nobody created — which is the answer only
    /// a call that REACHED the operation can get.
    fn served_code(self) -> &'static str {
        match self {
            Self::GetAgent => "ResourceNotFoundException",
            _ => "",
        }
    }
}

/// WHICH resource a row's statement names.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// Every resource, which the bare `*` wildcard stands for.
    Every,
    /// The collection the tag write addresses.
    BaseCollection,
    /// Another collection of the same account.
    OtherCollection,
    /// The agent the read addresses.
    ReadAgent,
    /// Another agent of the same account.
    OtherAgent,
}

impl Scope {
    fn resource(self, base_collection_arn: &str) -> String {
        match self {
            Self::Every => "*".to_string(),
            Self::BaseCollection => base_collection_arn.to_string(),
            Self::OtherCollection => collection_arn(OTHER_COLLECTION_ID),
            Self::ReadAgent => agent_arn(MISSING_AGENT_ID),
            Self::OtherAgent => agent_arn(OTHER_AGENT_ID),
        }
    }
}

/// One row: what the caller's role allows, which call it then makes, and whether IAM lets it
/// through.
struct Row {
    case: &'static str,
    /// `Allow` or `Deny` — a deny row carries a wildcard allow of the same namespace beneath
    /// it, so what it asks is what the deny does to a grant that would otherwise cover the
    /// call.
    effect: &'static str,
    /// The actions the statement names, empty for a role granted nothing of either service.
    actions: &'static [&'static str],
    scope: Scope,
    probe: Probe,
    allowed: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a role granting only the log actions lists no collection",
        effect: "Allow",
        actions: &[],
        scope: Scope::Every,
        probe: Probe::ListCollections,
        allowed: false,
    },
    Row {
        case: "another service's action grants no listing",
        effect: "Allow",
        actions: &["es:ListDomainNames"],
        scope: Scope::Every,
        probe: Probe::ListCollections,
        allowed: false,
    },
    Row {
        case: "another action of the same service grants no listing",
        effect: "Allow",
        actions: &["aoss:CreateCollection"],
        scope: Scope::Every,
        probe: Probe::ListCollections,
        allowed: false,
    },
    Row {
        case: "the action itself grants the listing",
        effect: "Allow",
        actions: &["aoss:ListCollections"],
        scope: Scope::Every,
        probe: Probe::ListCollections,
        allowed: true,
    },
    Row {
        case: "every action of the service grants the listing",
        effect: "Allow",
        actions: &["aoss:*"],
        scope: Scope::Every,
        probe: Probe::ListCollections,
        allowed: true,
    },
    Row {
        case: "an explicit deny of the action withholds it from a wildcard allow",
        effect: "Deny",
        actions: &["aoss:ListCollections"],
        scope: Scope::Every,
        probe: Probe::ListCollections,
        allowed: false,
    },
    Row {
        case: "a role granting only the log actions creates no collection",
        effect: "Allow",
        actions: &[],
        scope: Scope::Every,
        probe: Probe::CreateCollection,
        allowed: false,
    },
    Row {
        case: "the create's own action creates the collection",
        effect: "Allow",
        actions: &["aoss:CreateCollection"],
        scope: Scope::Every,
        probe: Probe::CreateCollection,
        allowed: true,
    },
    Row {
        case: "a role granting only the log actions tags nothing",
        effect: "Allow",
        actions: &[],
        scope: Scope::Every,
        probe: Probe::TagCollection,
        allowed: false,
    },
    Row {
        case: "the tag write's action on ANOTHER collection tags nothing",
        effect: "Allow",
        actions: &["aoss:TagResource"],
        scope: Scope::OtherCollection,
        probe: Probe::TagCollection,
        allowed: false,
    },
    Row {
        case: "the tag write's action on the collection addressed tags it",
        effect: "Allow",
        actions: &["aoss:TagResource"],
        scope: Scope::BaseCollection,
        probe: Probe::TagCollection,
        allowed: true,
    },
    Row {
        case: "a role granting only the log actions lists no agent",
        effect: "Allow",
        actions: &[],
        scope: Scope::Every,
        probe: Probe::ListAgents,
        allowed: false,
    },
    Row {
        case: "another action of the agent plane grants no listing",
        effect: "Allow",
        actions: &["bedrock:GetAgent"],
        scope: Scope::Every,
        probe: Probe::ListAgents,
        allowed: false,
    },
    Row {
        case: "the listing's own action lists the agents",
        effect: "Allow",
        actions: &["bedrock:ListAgents"],
        scope: Scope::Every,
        probe: Probe::ListAgents,
        allowed: true,
    },
    Row {
        case: "a role granting only the log actions reads no agent",
        effect: "Allow",
        actions: &[],
        scope: Scope::Every,
        probe: Probe::GetAgent,
        allowed: false,
    },
    Row {
        case: "the read's action on ANOTHER agent reads nothing",
        effect: "Allow",
        actions: &["bedrock:GetAgent"],
        scope: Scope::OtherAgent,
        probe: Probe::GetAgent,
        allowed: false,
    },
    Row {
        case: "the read's action on the agent addressed reaches the plane",
        effect: "Allow",
        actions: &["bedrock:GetAgent"],
        scope: Scope::ReadAgent,
        probe: Probe::GetAgent,
        allowed: true,
    },
    Row {
        case: "every action of the agent plane reaches it too",
        effect: "Allow",
        actions: &["bedrock:*"],
        scope: Scope::Every,
        probe: Probe::GetAgent,
        allowed: true,
    },
];

/// Write the encryption policy the collections are created under, and the collection the tag
/// rows address; answer that collection's ARN.
async fn prepare_collections(router: &MultiServiceRouter) -> String {
    let created = aoss(
        router,
        ACCOUNT_KEY,
        "CreateSecurityPolicy",
        &json!({
            "type": "encryption",
            "name": ENCRYPTION_POLICY,
            "policy": json!({
                "Rules": [{"ResourceType": "collection", "Resource": ["collection/*"]}],
                "AWSOwnedKey": true,
            })
            .to_string(),
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateSecurityPolicy must succeed, got {} with {}",
        created.status, created.body
    );
    let created = aoss(
        router,
        ACCOUNT_KEY,
        "CreateCollection",
        &json!({"name": BASE_COLLECTION, "type": "SEARCH"}),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateCollection({BASE_COLLECTION:?}) must succeed, got {} with {}",
        created.status, created.body
    );
    let arn = created.body["createCollectionDetail"]["arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "setup: the created collection must report its ARN, got {}",
                created.body
            )
        })
        .to_string();
    assert!(
        arn.starts_with(&format!("arn:aws:{AOSS}:{REGION}:{ACCOUNT}:collection/")),
        "setup: the collection's ARN is what a statement and a refusal name, got {arn:?}"
    );
    arn
}

/// Whether OpenSearch Serverless HOLDS a collection under `name`, read back through its own
/// `BatchGetCollection` as the account's own credential.
async fn holds_collection(router: &MultiServiceRouter, case: &str, name: &str) -> bool {
    let read = aoss(
        router,
        ACCOUNT_KEY,
        "BatchGetCollection",
        &json!({"names": [name]}),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: BatchGetCollection({name:?}) must succeed, got {} with {}",
        read.status, read.body
    );
    read.body["collectionDetails"]
        .as_array()
        .is_some_and(|details| !details.is_empty())
}

/// The tag keys OpenSearch Serverless holds for `arn`, read back through its own
/// `ListTagsForResource` as the account's own credential.
async fn held_tag_keys(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<String> {
    let read = aoss(
        router,
        ACCOUNT_KEY,
        "ListTagsForResource",
        &json!({"resourceArn": arn}),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: ListTagsForResource({arn:?}) must succeed, got {} with {}",
        read.status, read.body
    );
    read.body["tags"]
        .as_array()
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| tag["key"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// RULE over a TABLE of identity policies, operations and resources: each call is decided
/// against the policies in force on the principal that signed it, and a refused call has no
/// effect.
#[tokio::test]
async fn every_control_plane_call_is_decided_against_the_callers_identity_policy() {
    let router = build_router();
    let base_collection_arn = prepare_collections(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let role_name = format!("xsvc-gate-role-{index}");
        let document = role_document(row.effect, row.actions, &row.scope.resource(&base_collection_arn));
        let role_arn = create_role(&router, &role_name, &document).await;
        let caller = assume_role(&router, &role_arn).await;
        let principal = format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/probe");
        let made_collection = format!("gate-made-{index}");
        let tag_key = format!("tagged-by-{index}");

        let answer = match row.probe {
            Probe::ListCollections => aoss(&router, &caller, "ListCollections", &json!({})).await,
            Probe::CreateCollection => {
                aoss(
                    &router,
                    &caller,
                    "CreateCollection",
                    &json!({"name": made_collection, "type": "SEARCH"}),
                )
                .await
            }
            Probe::TagCollection => {
                aoss(
                    &router,
                    &caller,
                    "TagResource",
                    &json!({
                        "resourceArn": base_collection_arn,
                        "tags": [{"key": tag_key, "value": "yes"}],
                    }),
                )
                .await
            }
            Probe::ListAgents => {
                bedrock_agent(&router, &caller, "POST", "/agents/", &json!({})).await
            }
            Probe::GetAgent => {
                bedrock_agent(
                    &router,
                    &caller,
                    "GET",
                    &format!("/agents/{MISSING_AGENT_ID}/"),
                    &Value::Null,
                )
                .await
            }
        };

        if row.allowed {
            assert_eq!(
                (answer.status, answer.code.as_str()),
                (row.probe.served_status(), row.probe.served_code()),
                "case {case:?}: expected the service's own answer at HTTP {} ({:?}), got {} {} \
                 with {}",
                row.probe.served_status(),
                row.probe.served_code(),
                answer.status,
                answer.code,
                answer.body
            );
        } else {
            assert_eq!(
                (answer.status, answer.code.as_str()),
                (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {} \
                 with {}",
                answer.status,
                answer.code,
                answer.body
            );
            let expected = refusal_message(
                &principal,
                row.probe.action(),
                &row.probe.resource(&base_collection_arn),
                row.effect == "Deny",
            );
            assert_eq!(
                answer.body["message"].as_str().unwrap_or_default(),
                expected,
                "case {case:?}: expected IAM's own sentence, got {}",
                answer.body
            );
        }

        // And what the SERVICE holds afterwards: the effect of a served call is there, and a
        // refused one left the service exactly as it was.
        match row.probe {
            Probe::CreateCollection => {
                let held = holds_collection(&router, case, &made_collection).await;
                assert_eq!(
                    held, row.allowed,
                    "case {case:?}: expected the collection {made_collection:?} to be held={}, \
                     got held={held}",
                    row.allowed
                );
            }
            Probe::TagCollection => {
                let keys = held_tag_keys(&router, case, &base_collection_arn).await;
                assert_eq!(
                    keys.contains(&tag_key),
                    row.allowed,
                    "case {case:?}: expected the tag {tag_key:?} to be held={}, got {keys:?}",
                    row.allowed
                );
            }
            _ => {}
        }
    }
}

/// The handler the function rows run: it builds a client for the service the event names with
/// the SDK the runtime supplies and reports either the service's own answer or the refusal it
/// came back with. A client-side failure of the SDK itself — `UnknownServiceError`, raised
/// before anything is signed — is reported under its own key, because it tells the handler
/// nothing about the service and must not be mistaken for a refusal.
const PROBE_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    try:
        client = boto3.client(event["service"])
        if event["call"] == "list_agents":
            answered = client.list_agents()
            return {"ok": True, "count": len(answered["agentSummaries"])}
        answered = client.list_collections()
        return {"ok": True, "count": len(answered["collectionSummaries"])}
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except Exception as raised:
        return {"ok": False, "raised": type(raised).__name__, "detail": str(raised)}
"##;

/// One function row: what its execution role allows, which service its handler calls, and
/// whether that call is permitted.
struct FunctionRow {
    case: &'static str,
    actions: &'static [&'static str],
    service: &'static str,
    call: &'static str,
    action: &'static str,
    allowed: bool,
}

const FUNCTION_ROWS: &[FunctionRow] = &[
    FunctionRow {
        case: "a role granting only the log actions lists no agent from inside a function",
        actions: &[],
        service: "bedrock-agent",
        call: "list_agents",
        action: "bedrock:ListAgents",
        allowed: false,
    },
    FunctionRow {
        case: "a role granted the listing lists the agents from inside a function",
        actions: &["bedrock:ListAgents"],
        service: "bedrock-agent",
        call: "list_agents",
        action: "bedrock:ListAgents",
        allowed: true,
    },
    FunctionRow {
        case: "a role granting only the log actions lists no collection from inside a function",
        actions: &[],
        service: "opensearchserverless",
        call: "list_collections",
        action: "aoss:ListCollections",
        allowed: false,
    },
    FunctionRow {
        case: "a role granted the listing lists the collections from inside a function",
        actions: &["aoss:ListCollections"],
        service: "opensearchserverless",
        call: "list_collections",
        action: "aoss:ListCollections",
        allowed: true,
    },
];

/// The stack declaring one function per row, each running as that row's role: the inline
/// source is archived into the deployment package by CloudFormation, which is how a function
/// whose handler is written here comes to exist at all.
fn function_stack(role_arns: &[String]) -> String {
    let mut resources = serde_json::Map::new();
    for (index, role_arn) in role_arns.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("xsvc-gate-probe-{index}"),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the functions' stack, wait for `CREATE_COMPLETE` and answer the physical id of each
/// declared function.
async fn deploy_functions(
    router: &MultiServiceRouter,
    stack: &str,
    role_arns: &[String],
) -> BTreeMap<String, String> {
    let template = function_stack(role_arns);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({stack:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports its functions created,
    // which no fixed sleep can name: this looks until it reports it, with a deadline generous
    // enough that a loaded host does not decide the outcome.
    let mut reported = stack_status(router, stack).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, stack).await;
    }
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", stack)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", stack)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({stack:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// RULE applied to the identity a FUNCTION's own call speaks for: the SDK the managed runtime
/// supplies builds a client for either control plane and dispatches the call, so what the
/// handler observes is that service's own answer — the refusal when the execution role is
/// granted nothing, the served answer when it is granted the action — and never a
/// client-side failure of the SDK.
#[tokio::test]
async fn a_functions_own_control_plane_call_is_decided_against_its_execution_role() {
    let router = build_router();

    let mut role_arns = Vec::new();
    for (index, row) in FUNCTION_ROWS.iter().enumerate() {
        let name = format!("xsvc-gate-fn-role-{index}");
        let document = role_document("Allow", row.actions, "*");
        role_arns.push(create_role(&router, &name, &document).await);
    }
    let physical = deploy_functions(&router, "xsvc-gate-fn-probes", &role_arns).await;

    for (index, row) in FUNCTION_ROWS.iter().enumerate() {
        let case = row.case;
        let logical = format!("Probe{index}");
        let function = physical
            .get(&logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must report a physical id for {logical}"));
        wait_until_active(&router, &function).await;

        let event = json!({"service": row.service, "call": row.call});
        let (status, headers, payload) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            event.to_string().as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation must be served, got {status} with {payload}"
        );
        let faulted = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            faulted, None,
            "case {case:?}: the handler must run to completion, got {faulted:?} with {payload}"
        );
        // Whatever the outcome, the call REACHED a service: the SDK raised no client-side
        // failure of its own, which is what a service the runtime's SDK cannot build a client
        // for looks like from inside a handler.
        assert_eq!(
            payload["raised"], Value::Null,
            "case {case:?}: expected the client to be built and the call dispatched, got \
             {payload}"
        );

        if row.allowed {
            assert_eq!(
                (payload["ok"].as_bool(), payload["count"].as_i64()),
                (Some(true), Some(0)),
                "case {case:?}: expected the service's own answer, got {payload}"
            );
        } else {
            let principal =
                format!("arn:aws:sts::{ACCOUNT}:assumed-role/xsvc-gate-fn-role-{index}/{function}");
            assert_eq!(
                payload,
                json!({
                    "ok": false,
                    "code": ACCESS_DENIED,
                    "message": refusal_message(&principal, row.action, "*", false),
                    "status": ACCESS_DENIED_STATUS,
                }),
                "case {case:?}: expected the service's own refusal, got {payload}"
            );
        }
    }
}
