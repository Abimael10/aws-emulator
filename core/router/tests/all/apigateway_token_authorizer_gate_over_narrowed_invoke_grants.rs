// SPDX-License-Identifier: CC0-1.0

//! The `TOKEN` Lambda-authorizer gate of a deployed REST stage, driven over the invoke address
//! a client that cannot resolve AWS's own hostnames reaches it by, with each function's
//! invocation grant narrowed the way a client scripts it — and what the backend's DOWNSTREAM
//! store holds after every answer.
//!
//! RULE, one level above the recorded case: **a deployed method whose `authorizationType` is
//! `CUSTOM` is decided by the POLICY the Lambda function of its `Authorizer` returned for the
//! value the request carried at the authorizer's `identitySource`, evaluated against the ARN of
//! the method the request addressed, and that decision is taken BEFORE the integration is
//! dispatched: a policy allowing `execute-api:Invoke` on that method hands the request to the
//! integration and the client is served what the integration produced, while a policy that
//! does not admit it earns a terminal `403` whose body states API Gateway's own sentence for
//! that class and the integration NEVER runs. An admitted request has its integration performed
//! exactly ONCE. Which ARN a `lambda:InvokeFunction` grant to
//! `apigateway.amazonaws.com` names as its `aws:SourceArn` decides only WHETHER the gateway may
//! make each call — an authorizer's own ARN
//! (`arn:aws:execute-api:{region}:{account}:{api-id}/authorizers/{authorizer-id}`) for the
//! authorizer's function and the invoked METHOD's ARN
//! (`arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{verb}/{path}`, wildcards
//! included) for the integration's — and never WHICH answer the decision earns.**
//!
//! `Method$authorizationType` gives `CUSTOM` "for using a custom authorizer" and
//! `Authorizer$authorizerUri` is "the authorizer's Uniform Resource Identifier (URI) … For
//! `TOKEN` or `REQUEST` authorizers, this must be a well-formed Lambda function URI", so the
//! decision is a function's RETURN VALUE;
//! `Authorizer$authorizerResultTtlInSeconds` "if it equals 0, authorization caching is
//! disabled", so every row's own token reaches the function.
//!
//! What the neighbouring assembled-router suites already pin, and this one does not repeat:
//! [`crate::apigateway_token_authorizer_deny_leaves_the_backends_table_untouched`] pins the
//! same gate over a grant that names every method and stage of the api
//! (`{api-id}/*/*`) and addresses the stage at the api's own `execute-api` host, and
//! [`crate::apigateway_token_authorizer_deny_and_access_log`] pins the access-log record each
//! answer leaves. What only this suite reaches is the gate under grants narrowed to ONE
//! authorizer and ONE method — the scoping a client writes when it grants the gateway the least
//! it needs — over the `/_aws/execute-api/{api-id}/{stage}/{path}` address, and the member of
//! the backend's item that carries the path the proxy event named, so a gateway that cannot make
//! a narrowly granted call, or that hands the function the stage-qualified path, fails here
//! rather than passing because one broad grant covered everything.
//!
//! The rows differ along what the rule generalises over: WHAT the authorizer's function returned
//! for the presented token (an explicit `Deny` of the invoked method, an `Allow` naming it, an
//! `Allow` naming only a sibling method of the same api, so no statement admits the call) and
//! WHEN the row runs — the first `Deny` is answered before anything has ever been written and
//! the later refusals after the table already holds another request's item, so an absence that
//! only holds because the table is empty fails on those rows. Every row re-reads every key the
//! rows before it wrote. The last leg then DELETES the key an admitted call wrote and presents
//! the denied token again, so a write that only looked absent because the key had never been
//! used fails too.
//!
//! Four crates have to agree for that to hold — CloudFormation archives the inline handlers into
//! packages this emulator can run and provisions the table, Lambda runs both functions and
//! judges each invocation against the grant the client added, DynamoDB serves the table the
//! backend writes, and API Gateway resolves the stage, takes the decision and performs (or does
//! not perform) the integration — so the suite lives here rather than in any one of them.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling suites do.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, physical_ids_of};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request here is signed for, and so the Region of both functions, of the
/// table and of the ARNs the grants name. It is the Region the shared stack-settling helper
/// ([`wait_for_stack_status`]) reads a stack in.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every grant's `SourceArn` carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack holding the two functions and the table, and the status the setup waits for.
const STACK: &str = "narrowly-granted-authorizer";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical ids the template declares, which the physical names are read back by.
const BACKEND_RESOURCE: &str = "Backend";
const AUTHORIZER_RESOURCE: &str = "Gate";
const TABLE_RESOURCE: &str = "Written";

/// The stage the api is deployed to, the resource path of its one gated method and the verb it
/// is put under — the three values the method ARN a grant narrows to is built from.
///
/// `pub(crate)` because the sibling suite that drives the same gate on the STARTED emulator
/// ([`crate::apigateway_started_token_authorizer_gate`]) addresses the same method, so the two
/// cannot drift on what is deployed.
pub(crate) const STAGE: &str = "test";
pub(crate) const RESOURCE_PATH: &str = "guarded";
pub(crate) const VERB: &str = "GET";

/// The header the authorizer declares as its identity source, and the query parameter each row
/// names its own item by.
///
/// `pub(crate)` for the reason the stage's own names are; see [`STAGE`].
pub(crate) const TOKEN_HEADER: &str = "authorization";
const KEY_PARAMETER: &str = "key";

/// The table's hash key, and the member the handler records the `path` the proxy event named it
/// under.
///
/// `pub(crate)` for the reason the stage's own names are; see [`STAGE`].
pub(crate) const HASH_KEY: &str = "pk";
pub(crate) const PATH_MEMBER: &str = "path";

/// How long one invocation of either function is given, as its own `Timeout`: well past what a
/// run needs on any host, because what is under test is the decision and the write, not how fast
/// an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work another service owes — a stack reaching `CREATE_COMPLETE`, a
/// function reaching `Active`, a table reaching `ACTIVE` — and how long between two looks.
/// Generous on purpose: everything waited for happens in this process, so a loaded host does not
/// decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long ONE request to the deployed stage is given to be answered at all. The gate answers
/// out of records already in memory, so an answer takes a fraction of this; what the span has to
/// be is long enough that a loaded host never decides the outcome, and short enough that it
/// bites well before any cap the run itself is under, so a stage that has stopped answering is
/// reported as this row failing rather than as a hang.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// The sentences a refusal carries: the one API Gateway answers a call the returned policy
/// explicitly DENIES, and the one it answers a call no statement of that policy allows. Both are
/// fixed text for their class, and the clause naming the document that decided it is what tells
/// them apart — the same two sentences the sibling suites assert, asserted here so a gate reached
/// under narrow grants is shown to answer with them and not with some other refusal.
///
/// `EXPLICIT_DENY` is `pub(crate)` for the reason the stage's own names are; see [`STAGE`].
pub(crate) const EXPLICIT_DENY: &str = "User is not authorized to access this resource with an \
                                        explicit deny in an identity-based policy";
const NOT_AUTHORIZED: &str = "User is not authorized to access this resource because no \
                              identity-based policy allows the execute-api:Invoke action";

/// The member an authorization refusal states its sentence under, the class it names, and the
/// header that class travels in.
const MESSAGE_MEMBER: &str = "Message";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// The tokens the rows present. The authorizer's function chooses the policy it returns from the
/// token it was handed, so one function covers every policy shape the rows span.
const ALLOW_TOKEN: &str = "let-me-in";
const DENY_TOKEN: &str = "keep-me-out";
const ELSEWHERE_TOKEN: &str = "allow-elsewhere";

/// The authorizer's function: it names the method the call addressed as the policy's resource,
/// exactly as AWS's own Lambda-authorizer blueprint does, and for one token names a SIBLING
/// method of the same api instead, so no statement admits the invoked one.
///
/// `pub(crate)` because the sibling suite that drives the same gate on the STARTED emulator
/// ([`crate::apigateway_started_token_authorizer_gate`]) deploys the same handler, so the two
/// cannot drift on what the function returns.
pub(crate) const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    token = event.get("authorizationToken") or ""
    arn = event["methodArn"]
    elsewhere = arn.rsplit("/", 1)[0] + "/elsewhere"
    decided = {
        "let-me-in": ("Allow", arn),
        "keep-me-out": ("Deny", arn),
        "allow-elsewhere": ("Allow", elsewhere),
    }
    effect, resource = decided[token]
    return {
        "principalId": "probe-user",
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {"Action": "execute-api:Invoke", "Effect": effect, "Resource": resource}
            ],
        },
    }
"#;

/// The function behind the method: its ONLY observable effect is the item it puts in the table,
/// carrying the `path` the proxy event named — so a run of it is observable in DynamoDB, a call
/// that never reached it leaves nothing, and a call handed the wrong path writes the wrong item.
///
/// The write is CONDITIONAL on the key being free, so the item is also a record of HOW MANY TIMES
/// the integration was dispatched for one request: a second dispatch of the same request fails
/// the condition, the handler raises, and the row earns a `502` instead of the integration's own
/// answer. An admitted request has its integration performed exactly once, and that is what this
/// makes observable.
///
/// `pub(crate)` for the reason [`AUTHORIZER_HANDLER`] is.
pub(crate) const BACKEND_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    parameters = event.get("queryStringParameters") or {}
    key = parameters.get("key") or "no-key"
    boto3.client("dynamodb").put_item(
        TableName=os.environ["TABLE"],
        Item={"pk": {"S": key}, "path": {"S": event.get("path") or ""}},
        ConditionExpression="attribute_not_exists(pk)",
    )
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"ok": True}),
    }
"#;

/// One answered request: its status, its headers and its body verbatim.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine: both functions run on the Lambda this router
/// mounts, and the table the backend writes is the one this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<::engine::InMemoryEngine> = Arc::new(::engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification belongs to the emulator, not to this check: the credential scope is
    // what routes an SDK request, and an invoke of the deployed stage carries none at all, as a
    // client of that URL sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`. The signature itself is
/// never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One CloudFormation call over its awsQuery wire, which has to be accepted.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup: CloudFormation {action} must answer 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status,
        expected,
        "setup {what:?}: must answer HTTP {expected}, got {}",
        answered.summary()
    );
    answered.json()
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// The template the suite deploys: the two handlers, archived into deployment packages by
/// CloudFormation — which is how a function whose handler is written here comes to exist at all —
/// and the table the backend writes, named to it through the environment variable the transform
/// carries through.
fn template() -> String {
    let function = |source: &str, environment: Value, policies: Value| {
        json!({
            "Type": "AWS::Serverless::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Timeout": FUNCTION_TIMEOUT,
                "InlineCode": source,
                "Environment": environment,
                "Policies": policies,
            },
        })
    };
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            BACKEND_RESOURCE: function(
                BACKEND_HANDLER,
                json!({ "Variables": { "TABLE": { "Ref": TABLE_RESOURCE } } }),
                json!([{ "DynamoDBCrudPolicy": { "TableName": { "Ref": TABLE_RESOURCE } } }]),
            ),
            AUTHORIZER_RESOURCE: function(AUTHORIZER_HANDLER, json!({}), json!([])),
            TABLE_RESOURCE: {
                "Type": "AWS::Serverless::SimpleTable",
                "Properties": { "PrimaryKey": { "Name": HASH_KEY, "Type": "String" } },
            },
        },
    })
    .to_string()
}

/// The ARN Lambda reports for `name`, once it reports it `Active` — a transition that service
/// owes, so this looks until it reports it rather than sleeping a span on its behalf.
async fn active_function_arn(router: &MultiServiceRouter, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    let configuration = loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await
        .json();
        if reported.get("State").and_then(Value::as_str) == Some("Active")
            || Instant::now() >= deadline
        {
            break reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup: Lambda must report {name:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Look until DynamoDB first reports `table` `ACTIVE` — "you can perform read and write
/// operations only on an ACTIVE table" — which is that service's own transition.
async fn wait_until_table_active(router: &MultiServiceRouter, table: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": table }))
            .await
            .json();
        let status = described
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str);
        if status == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: DynamoDB must report {table:?} ACTIVE, got {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Grant `apigateway.amazonaws.com` the invocation of `name`, narrowed to exactly `source_arn` —
/// the `aws:SourceArn` such a grant is scoped by, as a client scripts the least it needs.
async fn grant(router: &MultiServiceRouter, name: &str, statement: &str, source_arn: &str) {
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        Some(json!({
            "StatementId": statement,
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": source_arn,
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission({name:?}, {source_arn:?}) must answer 201, got {}",
        granted.summary()
    );
}

/// The item `table` holds under `key`, read through DynamoDB's own strongly consistent `GetItem`,
/// or `None` when it holds none — which is what "the integration never ran" looks like to a
/// client of the backend's store.
async fn item_under(router: &MultiServiceRouter, table: &str, key: &str) -> Option<Value> {
    let read = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": table,
            "Key": { HASH_KEY: { "S": key } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "GetItem({key:?}) must answer 200, got {}",
        read.summary()
    );
    read.json().get("Item").cloned()
}

/// Drop whatever `table` holds under `key`, so an absence read afterwards is one the gate earned
/// rather than one a key that was never used gives for free.
async fn delete_item(router: &MultiServiceRouter, table: &str, key: &str) {
    let deleted = dynamodb(
        router,
        "DeleteItem",
        json!({ "TableName": table, "Key": { HASH_KEY: { "S": key } } }),
    )
    .await;
    assert_eq!(
        deleted.status,
        200,
        "DeleteItem({key:?}) must answer 200, got {}",
        deleted.summary()
    );
}

/// One row: what the request presents at the identity source, and what the stage owes it.
struct Row {
    case: &'static str,
    token: &'static str,
    status: u16,
    /// The sentence a refused answer states under [`MESSAGE_MEMBER`], or `None` for the row the
    /// integration serves.
    message: Option<&'static str>,
    /// Whether the method's integration — the backend function, and so its write — runs.
    integration_runs: bool,
}

/// The table the rule is asserted over, in order: the first `Deny` is answered before anything
/// has ever been written, and the later refusals after the table already holds another request's
/// item, so an absence that only holds because the table is empty fails on them.
const ROWS: &[Row] = &[
    Row {
        case: "a policy explicitly denying the invoked method, before anything has been written",
        token: DENY_TOKEN,
        status: 403,
        message: Some(EXPLICIT_DENY),
        integration_runs: false,
    },
    Row {
        case: "a policy allowing the invoked method",
        token: ALLOW_TOKEN,
        status: 200,
        message: None,
        integration_runs: true,
    },
    Row {
        case: "that same explicit deny, once the table already holds another request's item",
        token: DENY_TOKEN,
        status: 403,
        message: Some(EXPLICIT_DENY),
        integration_runs: false,
    },
    Row {
        case: "a policy allowing only another method of the same api, so none admits this one",
        token: ELSEWHERE_TOKEN,
        status: 403,
        message: Some(NOT_AUTHORIZED),
        integration_runs: false,
    },
    Row {
        case: "the allowing policy again, after the refused rows",
        token: ALLOW_TOKEN,
        status: 200,
        message: None,
        integration_runs: true,
    },
];

/// RULE: a `CUSTOM`-gated method of a deployed stage answers every request with what its
/// authorizer's function decided, takes that decision before dispatching the integration, and is
/// unaffected by how narrowly each function's invocation grant is scoped — so a refused request
/// leaves no item in the table the backend would have written, and an admitted one leaves the
/// item the proxy event's own path names.
#[test]
fn a_narrowly_granted_token_authorizer_decides_every_request_and_the_backends_write() {
    std::thread::Builder::new()
        .name("apigateway-narrow-grants".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on (`core/router/src/main.rs`),
            // because one invoke here drives two nested in-process calls on it — the authorizer's
            // invocation and the integration's — and the integration's handler drives a third,
            // its own write.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("build a multi-thread runtime");
            runtime.block_on(suite());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}

async fn suite() {
    let router = emulator();

    // 1. The stack: both functions and the table the backend writes.
    let body = template();
    cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND, and the
            // role the transform synthesizes for each function with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    let (reached, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described}"
    );
    let resources = cloudformation(&router, "DescribeStackResources", &[("StackName", STACK)]).await;
    let physical: BTreeMap<String, String> = physical_ids_of(&resources);
    let named = |logical: &str| {
        physical
            .get(logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: no physical id for {logical:?}, got {physical:?}"))
    };
    let backend = named(BACKEND_RESOURCE);
    let authorizer_function = named(AUTHORIZER_RESOURCE);
    let table = named(TABLE_RESOURCE);
    let backend_arn = active_function_arn(&router, &backend).await;
    let authorizer_arn = active_function_arn(&router, &authorizer_function).await;
    wait_until_table_active(&router, &table).await;

    // 2. The api, exactly as a CLI deployment builds one: the resource, the TOKEN authorizer, the
    //    gated method, the proxy integration, the two NARROW grants and the deployment that mints
    //    the stage.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "narrowly-granted-authorizer-api" })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &api_record, "id");
    let root = as_str("CreateRestApi", &api_record, "rootResourceId");
    let resource = apigw(
        &router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource_id = as_str("CreateResource", &resource, "id");
    let authorizer = apigw(
        &router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "token-gate",
            "type": "TOKEN",
            "identitySource": format!("method.request.header.{TOKEN_HEADER}"),
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{authorizer_arn}/\
                 invocations"
            ),
            // Nothing is HELD, so each row's own invocation decides it: the rows differ in the
            // policy the function returns, not in what a cache kept.
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");
    // The AUTHORIZER's function is granted its own authorizer's ARN, and the INTEGRATION's the
    // method ARN with only the stage wildcarded: the two scopes AWS documents for the two calls
    // the gateway makes, each naming one resource of this api and nothing else.
    grant(
        &router,
        &authorizer_function,
        "apigw-authorizer",
        &format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/authorizers/{authorizer_id}"),
    )
    .await;
    grant(
        &router,
        &backend,
        "apigw-backend",
        &format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/{VERB}/{RESOURCE_PATH}"),
    )
    .await;
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/{VERB}");
    apigw(
        &router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer_id })),
        201,
    )
    .await;
    apigw(
        &router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{backend_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;
    apigw(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    // 3. The rows, over the address a client that cannot resolve AWS's own hostnames reaches the
    //    stage by: each presents its own value at the identity source and names the item the
    //    backend would write for it, so what the table holds is read per row.
    let mut owed: Vec<(String, bool)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let key = format!("item-{index}");
        let answered = invoke(&router, &api, row.token, &key).await;
        assert_answer(case, &answered, row.status, row.message);
        owed.push((key, row.integration_runs));
        for (written, expected) in &owed {
            let held = item_under(&router, &table, written).await;
            let wanted = expected.then(|| {
                json!({
                    HASH_KEY: { "S": written },
                    // The path the PROXY EVENT named, which is the path within the stage.
                    PATH_MEMBER: { "S": format!("/{RESOURCE_PATH}") },
                })
            });
            assert_eq!(
                held, wanted,
                "case {case:?}: expected the table to hold {wanted:?} under {written:?}, got \
                 {held:?}"
            );
        }
    }

    // 4. The key an admitted call wrote is DROPPED and the denied token presented again under it:
    //    the refusal still leaves the key empty, so no row's absence rested on a key nothing had
    //    ever written.
    let reused = "item-1".to_string();
    delete_item(&router, &table, &reused).await;
    let case = "the explicit deny under a key an admitted call had already written and that was \
                then deleted";
    let answered = invoke(&router, &api, DENY_TOKEN, &reused).await;
    assert_answer(case, &answered, 403, Some(EXPLICIT_DENY));
    let held = item_under(&router, &table, &reused).await;
    assert_eq!(
        held, None,
        "case {case:?}: expected the table to hold nothing under {reused:?}, got {held:?}"
    );
}

/// One request to the deployed stage, over the `/_aws/execute-api/{api-id}/{stage}/{path}` address
/// this emulator answers for a client that cannot resolve `{api-id}.execute-api.{region}
/// .amazonaws.com`.
///
/// EVERY request earns a terminal answer: the gate answers its own refusals rather than leaving
/// the client waiting on an authorizer, so a request that never terminates is the calling row
/// FAILING with its own case named, not the suite hanging.
async fn invoke(router: &MultiServiceRouter, api: &str, token: &str, key: &str) -> Answered {
    tokio::time::timeout(
        ANSWER_DEADLINE,
        dispatch(
            router,
            VERB,
            &format!("apigateway.{REGION}.amazonaws.com"),
            &format!("/_aws/execute-api/{api}/{STAGE}/{RESOURCE_PATH}?{KEY_PARAMETER}={key}"),
            &[(TOKEN_HEADER, token)],
            Bytes::new(),
        ),
    )
    .await
    .unwrap_or_else(|_elapsed| {
        panic!("token {token:?}: the stage never answered the request at all")
    })
}

/// What one row owes: the status, and either the integration's own body or the sentence and class
/// of the refusal the decision earned.
fn assert_answer(case: &str, answered: &Answered, status: u16, message: Option<&str>) {
    assert_eq!(
        answered.status, status,
        "case {case:?}: expected HTTP {status} from the stage, got {}",
        answered.summary()
    );
    match message {
        Some(sentence) => {
            assert_eq!(
                answered.json().get(MESSAGE_MEMBER).and_then(Value::as_str),
                Some(sentence),
                "case {case:?}: expected {MESSAGE_MEMBER:?} to state {sentence:?}, got {}",
                answered.summary()
            );
            assert_eq!(
                answered.header(ERROR_TYPE_HEADER).as_deref(),
                Some(ACCESS_DENIED_EXCEPTION),
                "case {case:?}: expected {ERROR_TYPE_HEADER:?} to name \
                 {ACCESS_DENIED_EXCEPTION:?}, got {:?}",
                answered.header(ERROR_TYPE_HEADER)
            );
        }
        None => assert_eq!(
            answered.json(),
            json!({ "ok": true }),
            "case {case:?}: expected the integration's own answer, got {}",
            answered.summary()
        ),
    }
}
