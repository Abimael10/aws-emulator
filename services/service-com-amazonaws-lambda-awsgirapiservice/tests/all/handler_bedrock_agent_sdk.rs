// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING handler reaches when it builds an SDK client for the Bedrock agent runtime
//! plane or for the serverless search plane, end to end over the wire: AWS Lambda, the Bedrock
//! Agent runtime plane, OpenSearch Serverless, IAM and the token service on ONE engine, and a
//! `python3.x` function whose code does `import boto3` and calls one of them.
//!
//! RULE: a managed runtime ships the AWS SDK with the service data of every service its call
//! seam reaches, so a client a handler builds for one of them CONSTRUCTS and its call is
//! dispatched to that service — and what the handler observes is that SERVICE's own answer,
//! each modelled outcome class included: the authorization refusal a role without the grant
//! gets, the not-found refusal a well-formed identifier that resolves to nothing gets, the
//! validation refusal a malformed one gets, and the served answer, whose effect a client reads
//! back through the service's own API. A client-side failure of the SDK itself
//! (`UnknownServiceError`, raised before anything is signed) is none of those: it tells the
//! handler nothing about the service, which is why a role that is merely under-permissioned
//! must not look like a broken runtime image.
//!
//! The rows differ along what the rule generalises over: WHICH service the client is built for
//! (two, named by the token the AWS SDK for Python builds each with), which protocol that
//! service binds its requests with (`restJson1` and an AWS JSON one), which operation is
//! called, and which outcome class the call lands in (403 denial, 404 not-found, 400
//! validation, and two served calls — one per service — whose resource is read back through the
//! serving service's own API). Every row runs as a role carrying the AWS-managed logging
//! policy a Lambda execution role carries, beside the statement the row itself puts in force,
//! so an attachment is read for what it grants rather than as a grant nothing can refuse.
//! The negative half of the rule — that a token NO service
//! answers to still raises `UnknownServiceError`, carrying botocore's own sentence — is pinned
//! by `handler_sdk_service_inventory.rs` and is not repeated here.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{await_function_active, call, router_on};
// The percent-encoding an awsQuery parameter travels under, exported by the suite that first
// needed it rather than written again here.
use crate::event_source_mapping_role_access::form_encode;
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than written again here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for, and the one the function's own invocation —
/// and therefore the client its handler builds — runs in. `pub(crate)` with the role and
/// refusal helpers below, which the build-time plane's own suite
/// (`crate::handler_bedrock_agent_build_plane_sdk`) drives the same way rather than copying.
pub(crate) const REGION: &str = "us-east-1";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The name of the inline identity policy a row writes on the execution role.
const INLINE_POLICY: &str = "inline";

/// The AWS-managed policy a Lambda execution role carries for its own logging, attached to
/// the role every row runs as: its document — which IAM's own catalog publishes — allows the
/// three CloudWatch Logs actions and nothing else, so what a row's call is decided by is the
/// statement the row itself put in force.
const LOGGING_POLICY: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The access-key id standing for the account's own published credential, which is what the
/// requests this suite makes on its own behalf present.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The two IAM actions the Bedrock runtime calls below are decided against, as a policy
/// statement spells them.
const INVOKE_AGENT_ACTION: &str = "bedrock:InvokeAgent";
const RETRIEVE_ACTION: &str = "bedrock:Retrieve";

/// The action the served `restJson1` row is decided against.
const CREATE_SESSION_ACTION: &str = "bedrock:CreateSession";

/// The status a session this plane opens is reported in
/// (`smithy_query(query="shape",shape="SessionStatus")` -> ACTIVE | EXPIRED | ENDED).
const SESSION_ACTIVE: &str = "ACTIVE";

/// The error code and status AWS states an authorization failure with, and the two refusal
/// classes a caller tells a missing grant from an unresolvable identifier by.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;
const NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: i64 = 404;
const VALIDATION: &str = "ValidationException";
const VALIDATION_STATUS: i64 = 400;

/// A well-formed agent identifier no agent was ever created under: ten characters of the
/// `AgentId` alphabet (len ..10, pattern `^[0-9a-zA-Z]+$`).
const MISSING_AGENT_ID: &str = "ZZZZZZZZZ9";

/// An agent identifier outside that alphabet, which names no agent at all.
const MALFORMED_AGENT_ID: &str = "not-an-agent-id";

/// The draft alias every agent carries, which is the alias an invocation addresses.
const DRAFT_ALIAS_ID: &str = "TSTALIASID";

/// A well-formed knowledge-base identifier nothing holds: ten characters of the
/// `KnowledgeBaseIdentifier` alphabet (pattern `^[0-9a-zA-Z]{10}$`).
const MISSING_KNOWLEDGE_BASE_ID: &str = "ZZZZZZZZZZ";

/// The session the handler's invocation runs under (`SessionId`, len 2..100, pattern
/// `^[0-9a-zA-Z._:-]+$`), and the text it sends.
const SESSION_ID: &str = "handler-session-1";
const INPUT_TEXT: &str = "What is 2 + 2?";

/// The collection the served row creates (`CollectionName`, len 3..64, pattern
/// `^[a-z][a-z0-9-]+$`), and the collection type a knowledge base's vectors live in.
const COLLECTION_NAME: &str = "handler-vectors";
const COLLECTION_TYPE: &str = "VECTORSEARCH";

/// The encryption policy that collection is created under (`PolicyName`, len 3..32, pattern
/// `^[a-z][a-z0-9-]+$`), and its type: OpenSearch Serverless refuses a collection create whose
/// name no `encryption` security policy covers, so the policy is written first — by this suite,
/// as an operator writes it, not by the handler under test.
const ENCRYPTION_POLICY_NAME: &str = "handler-vectors-encryption";
const ENCRYPTION_POLICY_TYPE: &str = "encryption";

/// Lambda, the Bedrock Agent runtime plane, OpenSearch Serverless, IAM and the token service
/// over ONE shared engine, as `core/router/src/lib.rs` mounts them: the handler's SDK call is
/// dispatched back into this same router and signed as the session of its own execution role,
/// so the service that answers it is the service this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_bedrockagentruntime_amazonbedrockagentruntimeservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_opensearchserverless_opensearchserverless::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request and where the Region comes from. The signature is never checked here.
pub(crate) fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One AWS JSON call to OpenSearch Serverless, made as a CLIENT makes it: the operation in the
/// target header, the input document as the body.
async fn opensearch_serverless(
    router: &MultiServiceRouter,
    what: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let host = format!("aoss.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("authorization", authorization("aoss"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("OpenSearchServerless.{operation}"))
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "{what}: expected HTTP 200 from {operation}, got {status} ({parsed})"
    );
    parsed
}

/// One `restJson1` READ of the Bedrock Agent runtime plane, made as a CLIENT makes it, so the
/// resource a handler's call produced is read back where a client reads it.
async fn bedrock_runtime(router: &MultiServiceRouter, what: &str, path: &str) -> (u16, Value) {
    let host = format!("bedrock-agent-runtime.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("bedrock"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "{what}: expected HTTP 200 from GET {path}, got {status} ({parsed})"
    );
    (status, parsed)
}

/// One awsQuery call to IAM that must be served.
pub(crate) async fn iam(
    router: &MultiServiceRouter,
    what: &str,
    action: &str,
    params: &[(&str, String)],
) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "setup {what}: IAM {action} must succeed, got {status} {}",
        String::from_utf8_lossy(response.body())
    );
}

/// Create the execution role `role_name`, trusted by Lambda, carrying `policy` as its inline
/// identity policy — written with IAM's own operations, so the role a session is minted of is
/// the record `iam:CreateRole` wrote.
pub(crate) async fn execution_role(
    router: &MultiServiceRouter,
    role_name: &str,
    policy: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        "the execution role",
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    role_policy(router, "the execution role", role_name, policy).await;
    format!(
        "arn:aws:iam::{}:role/{role_name}",
        engine::emulator_account_id()
    )
}

/// The identity policy in force on `role_name` from now on, so a row is about the grant in
/// force when the handler calls rather than the one in force when the role was made.
pub(crate) async fn role_policy(
    router: &MultiServiceRouter,
    what: &str,
    role_name: &str,
    document: &str,
) {
    iam(
        router,
        what,
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", INLINE_POLICY.to_string()),
            ("PolicyDocument", document.to_string()),
        ],
    )
    .await;
}

/// One identity policy document granting `actions` on every resource.
pub(crate) fn allow(actions: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": actions, "Resource": "*"}],
    })
    .to_string()
}

/// The logging grant every execution role carries, which names no action of either service
/// below: the position a role whose policy was never extended is in.
pub(crate) fn logs_only() -> String {
    allow(&[
        "logs:CreateLogGroup",
        "logs:CreateLogStream",
        "logs:PutLogEvents",
    ])
}

/// The ARN of the agent alias an invocation is decided against, as an identity-based policy for
/// `bedrock:InvokeAgent` names it — built here rather than read off the crate under test.
fn agent_alias_arn(agent_id: &str, agent_alias_id: &str) -> String {
    format!(
        "arn:aws:bedrock:{REGION}:{}:agent-alias/{agent_id}/{agent_alias_id}",
        engine::emulator_account_id()
    )
}

/// The ARN of the knowledge base a retrieval is decided against, the same way.
fn knowledge_base_arn(knowledge_base_id: &str) -> String {
    format!(
        "arn:aws:bedrock:{REGION}:{}:knowledge-base/{knowledge_base_id}",
        engine::emulator_account_id()
    )
}

/// The assumed-role session a function's own calls are made as:
/// `arn:aws:sts::<account>:assumed-role/<role name>/<function name>`, the identity AWS reports
/// for a function's calls.
pub(crate) fn session_principal(role_name: &str, function_name: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{function_name}",
        engine::emulator_account_id()
    )
}

/// The sentence IAM refuses a request with, built here from the principal, the action and the
/// resource rather than read off the crates under test.
pub(crate) fn denial(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// The handler every row runs: it builds a client for the service the event names, makes the
/// one call the event names, and reports what the SDK gave it — the answer, or the error code,
/// message and status of a refusal.
///
/// A client-side failure of the SDK is reported under the exception class that was raised, so a
/// row that never reached a service is distinguishable from one that was refused by it.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    client = boto3.client(event["service"])
    try:
        if event["call"] == "invoke_agent":
            client.invoke_agent(
                agentId=event["agentId"],
                agentAliasId=event["agentAliasId"],
                sessionId=event["sessionId"],
                inputText=event["inputText"],
            )
            answered = {}
        elif event["call"] == "retrieve":
            read = client.retrieve(
                knowledgeBaseId=event["knowledgeBaseId"],
                retrievalQuery={"text": event["inputText"]},
            )
            answered = {"results": len(read.get("retrievalResults") or [])}
        elif event["call"] == "create_session":
            opened = client.create_session()
            answered = {
                "sessionId": opened.get("sessionId"),
                "status": opened.get("sessionStatus"),
            }
        else:
            created = client.create_collection(
                name=event["collection"], type=event["collectionType"]
            )
            detail = created.get("createCollectionDetail") or {}
            answered = {"name": detail.get("name"), "type": detail.get("type")}
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "raised": "ClientError",
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except botocore.exceptions.BotoCoreError as error:
        return {"ok": False, "raised": type(error).__name__, "message": str(error)}
    return {"ok": True, "answered": answered}
"#;

/// What an authorization refusal must name: the IAM action the call is decided against, and
/// the ARN of the resource it is decided on.
type DeniedOn = (&'static str, fn() -> String);

/// What a row's call must come back as.
enum Expected {
    /// The service served the call, and WHICH served answer the handler must report — each
    /// one read back afterwards through the serving service's own API.
    Served(Served),
    /// The service refused it with this error code and HTTP status, and — for an authorization
    /// refusal — with IAM's own sentence about this action and resource.
    Refused {
        code: &'static str,
        status: i64,
        denied: Option<DeniedOn>,
    },
}

/// The served answers a row can carry.
enum Served {
    /// The session the handler opened on the `restJson1` plane.
    Session,
    /// The collection the handler created on the AWS JSON plane.
    Collection,
}

/// One row: the grant in force, the call the handler makes, and what it must observe.
struct Row {
    case: &'static str,
    /// The inline policy the execution role carries when the handler calls.
    policy: fn() -> String,
    /// The event the handler is invoked with.
    event: fn() -> Value,
    expected: Expected,
}

/// The rows, spanning the two services, their two protocols, and every outcome class a handler
/// has to be able to tell apart.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an agent invocation by a role granted no Bedrock action",
            policy: logs_only,
            event: || {
                json!({
                    "service": "bedrock-agent-runtime",
                    "call": "invoke_agent",
                    "agentId": MISSING_AGENT_ID,
                    "agentAliasId": DRAFT_ALIAS_ID,
                    "sessionId": SESSION_ID,
                    "inputText": INPUT_TEXT,
                })
            },
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                status: ACCESS_DENIED_STATUS,
                denied: Some((INVOKE_AGENT_ACTION, || {
                    agent_alias_arn(MISSING_AGENT_ID, DRAFT_ALIAS_ID)
                })),
            },
        },
        Row {
            case: "an agent invocation by a granted role, on an agent nobody created",
            policy: || allow(&[INVOKE_AGENT_ACTION]),
            event: || {
                json!({
                    "service": "bedrock-agent-runtime",
                    "call": "invoke_agent",
                    "agentId": MISSING_AGENT_ID,
                    "agentAliasId": DRAFT_ALIAS_ID,
                    "sessionId": SESSION_ID,
                    "inputText": INPUT_TEXT,
                })
            },
            expected: Expected::Refused {
                code: NOT_FOUND,
                status: NOT_FOUND_STATUS,
                denied: None,
            },
        },
        Row {
            case: "an agent invocation by a granted role, on a malformed agent identifier",
            policy: || allow(&[INVOKE_AGENT_ACTION]),
            event: || {
                json!({
                    "service": "bedrock-agent-runtime",
                    "call": "invoke_agent",
                    "agentId": MALFORMED_AGENT_ID,
                    "agentAliasId": DRAFT_ALIAS_ID,
                    "sessionId": SESSION_ID,
                    "inputText": INPUT_TEXT,
                })
            },
            expected: Expected::Refused {
                code: VALIDATION,
                status: VALIDATION_STATUS,
                denied: None,
            },
        },
        Row {
            case: "a knowledge-base retrieval by a role granted no Bedrock action",
            policy: logs_only,
            event: || {
                json!({
                    "service": "bedrock-agent-runtime",
                    "call": "retrieve",
                    "knowledgeBaseId": MISSING_KNOWLEDGE_BASE_ID,
                    "inputText": INPUT_TEXT,
                })
            },
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                status: ACCESS_DENIED_STATUS,
                denied: Some((RETRIEVE_ACTION, || {
                    knowledge_base_arn(MISSING_KNOWLEDGE_BASE_ID)
                })),
            },
        },
        Row {
            case: "a knowledge-base retrieval by a granted role, on a knowledge base nobody \
                   created",
            policy: || allow(&[RETRIEVE_ACTION]),
            event: || {
                json!({
                    "service": "bedrock-agent-runtime",
                    "call": "retrieve",
                    "knowledgeBaseId": MISSING_KNOWLEDGE_BASE_ID,
                    "inputText": INPUT_TEXT,
                })
            },
            expected: Expected::Refused {
                code: NOT_FOUND,
                status: NOT_FOUND_STATUS,
                denied: None,
            },
        },
        Row {
            case: "a session opened through the restJson1 service by a granted role",
            policy: || allow(&[CREATE_SESSION_ACTION]),
            event: || {
                json!({
                    "service": "bedrock-agent-runtime",
                    "call": "create_session",
                })
            },
            expected: Expected::Served(Served::Session),
        },
        Row {
            case: "a serverless search collection created through an AWS JSON service",
            policy: || allow(&["aoss:CreateCollection"]),
            event: || {
                json!({
                    "service": "opensearchserverless",
                    "call": "create_collection",
                    "collection": COLLECTION_NAME,
                    "collectionType": COLLECTION_TYPE,
                })
            },
            expected: Expected::Served(Served::Collection),
        },
    ]
}

/// RULE: a client a handler builds for a service the runtime's call seam reaches is CONSTRUCTED
/// and its call dispatched, so what the handler observes is that service's own answer — the
/// authorization refusal, the not-found refusal, the validation refusal or the served answer —
/// and never a client-side failure of the SDK.
#[tokio::test]
async fn a_handler_calls_the_bedrock_agent_runtime_and_search_planes_through_the_runtimes_sdk() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    let role_name = "lambda-bedrock-agent-caller";
    let function_name = "bedrock-agent-sdk-caller";
    let role = execution_role(&router, role_name, &logs_only()).await;
    // The attachment a Lambda execution role carries for its own logging, put on with IAM's
    // own operation: it is in force for every row, and grants no action of either service
    // below.
    iam(
        &router,
        "the attached logging policy",
        "AttachRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyArn", LOGGING_POLICY.to_string()),
        ],
    )
    .await;

    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": function_name,
            "Role": role,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 25,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(&router, "setup", function_name).await;
    let principal = session_principal(role_name, function_name);

    // The encryption policy the served row's collection is created under: a collection create
    // whose name no policy of that type covers is refused, so the policy comes first. Its
    // document is the one AWS's encryption policies take — rules over `collection/<name>`
    // patterns beside the key to use — and it travels as the text of that document.
    opensearch_serverless(
        &router,
        "setup: the encryption policy the collection is created under",
        "CreateSecurityPolicy",
        &json!({
            "type": ENCRYPTION_POLICY_TYPE,
            "name": ENCRYPTION_POLICY_NAME,
            "policy": json!({
                "Rules": [{
                    "ResourceType": "collection",
                    "Resource": [format!("collection/{COLLECTION_NAME}")],
                }],
                "AWSOwnedKey": true,
            })
            .to_string(),
        }),
    )
    .await;

    for row in rows() {
        let case = row.case;
        role_policy(&router, case, role_name, &(row.policy)()).await;
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function_name}/invocations"),
            &(row.event)(),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: the handler must run to completion, got {}",
            answered.text()
        );
        let payload = answered.json();
        // Whatever the outcome, the call REACHED a service: the SDK raised no client-side
        // failure of its own, which is what a missing service model in the runtime's SDK looks
        // like from inside a handler.
        assert_ne!(
            payload["raised"].as_str(),
            Some("UnknownServiceError"),
            "case {case:?}: expected the client to be built and the call dispatched, got \
             {payload}"
        );
        match row.expected {
            Expected::Served(served) => {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the call to be served, got {payload}"
                );
                match served {
                    Served::Collection => {
                        assert_eq!(
                            payload["answered"]["name"],
                            json!(COLLECTION_NAME),
                            "case {case:?}: expected the created collection's own name, got \
                             {payload}"
                        );
                        assert_eq!(
                            payload["answered"]["type"],
                            json!(COLLECTION_TYPE),
                            "case {case:?}: expected the created collection's own type, got \
                             {payload}"
                        );
                    }
                    Served::Session => {
                        assert_eq!(
                            payload["answered"]["status"],
                            json!(SESSION_ACTIVE),
                            "case {case:?}: expected the opened session's own status, got \
                             {payload}"
                        );
                        // The session the handler opened is the session the SERVICE holds: it
                        // is read back through that service's own `GetSession`, under the id
                        // the service minted and the handler reported.
                        let session_id = payload["answered"]["sessionId"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        assert!(
                            !session_id.is_empty(),
                            "case {case:?}: expected the service's own minted session id, got \
                             {payload}"
                        );
                        let (_status, read) = bedrock_runtime(
                            &router,
                            case,
                            &format!("/sessions/{session_id}/"),
                        )
                        .await;
                        assert_eq!(
                            read["sessionId"],
                            json!(session_id),
                            "case {case:?}: expected GetSession to report the session the \
                             handler opened, got {read}"
                        );
                        assert_eq!(
                            read["sessionStatus"],
                            json!(SESSION_ACTIVE),
                            "case {case:?}: expected the stored session to be active, got {read}"
                        );
                    }
                }
            }
            Expected::Refused {
                code,
                status,
                denied,
            } => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the service's own refusal, got {payload}"
                );
                assert_eq!(
                    payload["raised"],
                    json!("ClientError"),
                    "case {case:?}: expected the refusal to reach the handler as the SDK's \
                     service error, got {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(code),
                    "case {case:?}: expected the refusal {code:?}, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(status),
                    "case {case:?}: expected HTTP {status} for that refusal, got {payload}"
                );
                match denied {
                    Some((action, resource)) => assert_eq!(
                        payload["message"],
                        json!(denial(&principal, action, &resource())),
                        "case {case:?}: expected IAM's own denial sentence, got {payload}"
                    ),
                    None => assert!(
                        payload["message"]
                            .as_str()
                            .is_some_and(|message| !message.is_empty()),
                        "case {case:?}: expected the service's own message, got {payload}"
                    ),
                }
            }
        }
    }

    // The served row did the work it reported: the collection the handler created is the
    // collection OpenSearch Serverless itself holds, read back through its own API.
    let read = opensearch_serverless(
        &router,
        "the collection the handler created",
        "BatchGetCollection",
        &json!({"names": [COLLECTION_NAME]}),
    )
    .await;
    let names: Vec<&str> = read["collectionDetails"]
        .as_array()
        .map(|details| {
            details
                .iter()
                .filter_map(|detail| detail["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        names,
        vec![COLLECTION_NAME],
        "case {:?}: expected the collection the handler created to be the one the service \
         holds, got {read}",
        "a serverless search collection created through an AWS JSON service"
    );
}
