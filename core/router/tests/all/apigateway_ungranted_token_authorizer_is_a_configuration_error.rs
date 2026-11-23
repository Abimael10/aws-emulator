// SPDX-License-Identifier: CC0-1.0

//! What a client of a DEPLOYED REST stage is answered when the method behind it is gated by a
//! `TOKEN` Lambda authorizer whose function the api MAY NOT INVOKE, and what changes when the
//! grant that admits the invocation is added — asserted on the emulator as it is ASSEMBLED.
//!
//! RULE UNDER TEST, one level above the recorded case: **a request to a deployed method whose
//! `authorizationType` is `CUSTOM` is decided by the RETURN VALUE of the Lambda function its
//! `Authorizer` names, so a call whose authorizer function this api may not invoke — no
//! statement of that function's resource policy admits `lambda:InvokeFunction` for
//! `apigateway.amazonaws.com` on the AUTHORIZER's own `aws:SourceArn` — has no decision at
//! all: it is answered through the `AUTHORIZER_CONFIGURATION_ERROR` gateway response, `500`
//! naming `AuthorizerConfigurationException` in `x-amzn-errortype` with that response's own
//! default body — `{"message":null}`, the quoted value of a message the gateway sets none of —
//! and NOT through the generic server-fault response a method whose own integration cannot be
//! performed earns (`InternalServerErrorException`, `{"message":"Internal server error"}`).
//! The method's integration is never performed for such a call, so the backend's write to
//! another service's store does not happen. A grant whose `aws:SourceArn` covers that
//! authorizer makes the IDENTICAL request succeed — the policy the function returns is
//! evaluated, the integration runs and the client is served what it produced — which is what
//! tells a gateway that ENFORCES the grant from one that is simply broken.**
//!
//! `Authorizer$authorizerUri` is "the authorizer's Uniform Resource Identifier (URI) … For
//! `TOKEN` or `REQUEST` authorizers, this must be a well-formed Lambda function URI", so the
//! decision is a function's return value and the invocation of that function is an ordinary
//! Lambda invocation its resource policy decides; `Authorizer$authorizerResultTtlInSeconds`
//! "if it equals 0, authorization caching is disabled", so every row's own request reaches the
//! function rather than a held result. `AddPermission` takes the `SourceArn` as "for AWS
//! services, the ARN of the AWS resource that invokes the function", which for an authorizer
//! is the authorizer itself.
//!
//! The rows differ along what the rule generalises over: WHICH resource the grant on the
//! authorizer's function is scoped to — none at all, the invoked METHOD of the same api (a
//! grant that exists but is about something else), the AUTHORIZER itself, and a wildcard
//! covering every resource of the api — and WHEN the row runs, so the two refused rows are
//! read both before anything has ever been written and after the table already holds an
//! earlier row's item. Every row re-reads every key the rows before it addressed, so a
//! refusal that is observed as "nothing was written" only because writes never land fails
//! here.
//!
//! What the neighbouring assembled-router suites already pin, and this one does not repeat:
//! [`crate::apigateway_token_authorizer_deny_leaves_the_backends_table_untouched`] pins what
//! the POLICY the function returned decides (`Allow`, explicit `Deny`, a statement about
//! another method) and that a refused decision spares the backend's table, and
//! [`crate::apigateway_token_authorizer_deny_and_access_log`] pins the three answers a
//! `CUSTOM` gate gives and the access-log record each leaves. What only this suite reaches is
//! the leg BEFORE any decision exists: that an un-invocable authorizer is the authorizer's own
//! configuration failure rather than the generic gateway `500`, that it spares the backend
//! too, and that the grant — in each of the forms a client scripts it in — is what flips the
//! identical request to the integration's own answer.
//!
//! Four crates have to agree for that to hold — CloudFormation archives the inline handlers
//! into packages this emulator can run and provisions the table, Lambda runs both functions
//! and holds the resource policy that admits (or refuses) each invocation, DynamoDB serves the
//! table the backend writes, and API Gateway resolves the stage, invokes the authorizer and
//! renders the refusal — so the suite lives here rather than in any one of them.
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
/// table and of the api's own invoke host. It is the Region the shared stack-settling helper
/// ([`wait_for_stack_status`]) reads a stack in.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every grant's `SourceArn` carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack holding the two functions and the table, and the status the setup waits for.
const STACK: &str = "ungranted-authorizer-gate";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical ids the template declares, which the physical names are read back by.
const BACKEND_RESOURCE: &str = "Backend";
const AUTHORIZER_RESOURCE: &str = "Gate";
const TABLE_RESOURCE: &str = "Written";

/// The stage the api is deployed to, and the resource path of its one gated method.
const STAGE: &str = "live";
const RESOURCE_PATH: &str = "secure";

/// The header the authorizer declares as its identity source, the value every row presents at
/// it — the rows differ in the GRANT, not in the token — and the query parameter each row names
/// its own item by.
const TOKEN_HEADER: &str = "authorization";
const PRESENTED_TOKEN: &str = "let-me-in";
const KEY_PARAMETER: &str = "key";

/// The table's hash key, and the member the handler records the verb it was handed under.
const HASH_KEY: &str = "pk";
const VERB_MEMBER: &str = "verb";

/// The statement ids the grants below are added under: one on the authorizer's function, which
/// rows add and remove, and one on the backend's, which stands for the whole suite so a refused
/// row is never refused because the INTEGRATION could not be invoked.
const AUTHORIZER_STATEMENT: &str = "apigw-authorizer";
const BACKEND_STATEMENT: &str = "apigw-backend";

/// How long one invocation of either function is given, as its own `Timeout`: well past what a
/// run needs on any host, because what is under test is the grant and the refusal, not how fast
/// an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long to wait for work another service owes — a stack reaching `CREATE_COMPLETE`, a
/// function reaching `Active`, a table reaching `ACTIVE` — and how long between two looks.
/// Generous on purpose: everything waited for happens in this process, so a loaded host does
/// not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long ONE request to the deployed stage is given to be answered at all. The gate answers
/// out of records already in memory, so an answer takes a fraction of this; what the span has
/// to be is long enough that a loaded host never decides the outcome, and short enough that it
/// bites well before any cap the run itself is under, so a stage that has stopped answering is
/// reported as this row failing rather than as a hang.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

/// The header API Gateway names the exception class of a refused request in, lowercased as HTTP
/// field names compare.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The class and the body a call NO authorizer decided earns: the authorizer is the part of the
/// configuration its operator has to fix, and the gateway sets no error message of its own for
/// that class, so the response type's default template (`{"message":$context.error.messageString}`)
/// renders the quoted value of nothing — the JSON literal `null`.
const AUTHORIZER_CONFIGURATION_EXCEPTION: &str = "AuthorizerConfigurationException";
const NO_DECISION_BODY: &str = r#"{"message":null}"#;

/// The class and body of the GENERIC server fault, which is what the rows above must NOT be
/// answered with: a method whose own integration cannot be performed earns this one, and a
/// client that cannot tell the two apart retries a permanent configuration fault.
const INTERNAL_SERVER_ERROR_EXCEPTION: &str = "InternalServerErrorException";
const INTERNAL_SERVER_ERROR_BODY: &str = r#"{"message":"Internal server error"}"#;

/// The authorizer's function: it names the method the call addressed as the policy's resource,
/// exactly as AWS's own Lambda-authorizer blueprint does, so an invocation of it that HAPPENS
/// admits the call and one that is refused is the only reason a row can earn no decision.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    return {
        "principalId": "probe-user",
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Action": "execute-api:Invoke",
                    "Effect": "Allow",
                    "Resource": event["methodArn"],
                }
            ],
        },
    }
"#;

/// The function behind the method: its ONLY observable effect is the item it puts in the table,
/// so a run of it is observable in DynamoDB and a call that never reached it leaves nothing.
const BACKEND_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    parameters = event.get("queryStringParameters") or {}
    key = parameters.get("key") or "no-key"
    boto3.client("dynamodb").put_item(
        TableName=os.environ["TABLE"],
        Item={"pk": {"S": key}, "verb": {"S": event.get("httpMethod") or ""}},
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
    // Signature verification belongs to the emulator, not to this check: the credential scope
    // is what routes an SDK request, and an invoke of the deployed stage carries none at all,
    // as a client of that URL sends none.
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
/// CloudFormation — which is how a function whose handler is written here comes to exist at all
/// — and the table the backend writes, named to it through the environment variable the
/// transform carries through.
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

/// Grant `apigateway.amazonaws.com` the invocation of `name`, narrowed to `source_arn` — the
/// `aws:SourceArn` such a grant is scoped by, as a client scripts it.
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

/// Withdraw the statement `statement` from `name`'s resource policy, so the next row is decided
/// by the grant IT adds rather than by the one before it.
async fn withdraw(router: &MultiServiceRouter, name: &str, statement: &str) {
    let removed = lambda(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{name}/policy/{statement}"),
        None,
    )
    .await;
    assert_eq!(
        removed.status,
        204,
        "setup: RemovePermission({name:?}, {statement:?}) must answer 204, got {}",
        removed.summary()
    );
}

/// The item `table` holds under `key`, read through DynamoDB's own strongly consistent
/// `GetItem`, or `None` when it holds none — which is what "the integration never ran" looks
/// like to a client of the backend's store.
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

/// What the grant on the authorizer's function is scoped to for one row.
enum Scoped {
    /// The function carries no grant for `apigateway.amazonaws.com` at all.
    Nothing,
    /// A grant whose `aws:SourceArn` is the invoked METHOD of this api — a grant that exists
    /// and is about something other than the authorizer.
    TheMethod,
    /// A grant whose `aws:SourceArn` is the AUTHORIZER itself, the resource an authorizer's
    /// invocation is made for.
    TheAuthorizer,
    /// A grant covering every resource of the api, the wildcard form a client scripts when it
    /// does not narrow the permission.
    EveryResourceOfTheApi,
}

/// One row: how the authorizer's function is granted, and what the stage owes the identical
/// request under that grant.
struct Row {
    case: &'static str,
    scoped: Scoped,
    status: u16,
    body: &'static str,
    error_type: Option<&'static str>,
    /// Whether the method's integration — the backend function, and so its write — runs.
    integration_runs: bool,
}

/// The table the rule is asserted over, in order: the un-granted row is answered before
/// anything has ever been written, and the second refused row after the table already holds an
/// admitted request's item, so an absence that only holds because the table is empty fails on
/// it.
const ROWS: &[Row] = &[
    Row {
        case: "no grant at all on the authorizer's function",
        scoped: Scoped::Nothing,
        status: 500,
        body: NO_DECISION_BODY,
        error_type: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        integration_runs: false,
    },
    Row {
        case: "a grant scoped to the authorizer itself",
        scoped: Scoped::TheAuthorizer,
        status: 200,
        body: r#"{"ok":true}"#,
        error_type: None,
        integration_runs: true,
    },
    Row {
        case: "a grant that exists but is scoped to the invoked method, not the authorizer",
        scoped: Scoped::TheMethod,
        status: 500,
        body: NO_DECISION_BODY,
        error_type: Some(AUTHORIZER_CONFIGURATION_EXCEPTION),
        integration_runs: false,
    },
    Row {
        case: "a grant covering every resource of the api",
        scoped: Scoped::EveryResourceOfTheApi,
        status: 200,
        body: r#"{"ok":true}"#,
        error_type: None,
        integration_runs: true,
    },
];

/// RULE: a `CUSTOM`-gated method whose authorizer function the api may not invoke is answered
/// the authorizer's own configuration failure — `500`, `AuthorizerConfigurationException`,
/// `{"message":null}` — with the integration never performed, and the grant that admits the
/// invocation is what flips the identical request to the integration's own answer.
#[test]
fn an_un_invocable_token_authorizer_is_an_authorizer_configuration_error() {
    std::thread::Builder::new()
        .name("apigateway-ungranted-authorizer".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // The MULTI-THREADED flavour the emulator serves on (`core/router/src/main.rs`),
            // because one invoke here drives two nested in-process calls on it — the
            // authorizer's invocation and the integration's — and the integration's handler
            // drives a third, its own write.
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
            // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND, and
            // the role the transform synthesizes for each function with CAPABILITY_IAM.
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

    // 2. The api, exactly as a CLI deployment builds one: the resource, the TOKEN authorizer,
    //    the gated method, the proxy integration and the deployment that mints the stage. The
    //    authorizer's function is granted NOTHING yet — the rows below decide that.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "ungranted-authorizer-api" })),
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
            // grant that admits that invocation, not in what a cache kept.
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
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

    // The METHOD's own grant stands for the whole suite, scoped to the method the rows address:
    // a refused row is then refused because of the AUTHORIZER's grant and nothing else.
    let method_arn = format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/GET/{RESOURCE_PATH}");
    let authorizer_source_arn =
        format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/authorizers/{authorizer_id}");
    let every_resource = format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*");
    grant(&router, &backend, BACKEND_STATEMENT, &method_arn).await;

    // 3. The rows: each scopes the grant on the authorizer's function its own way and presents
    //    the same token at the identity source, so what differs in the answer is the grant.
    let invoke_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    // Every key the rows have addressed so far, with whether the table owes an item under it.
    let mut owed: Vec<(String, bool)> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let key = format!("item-{index}");
        let scoped_to = match row.scoped {
            Scoped::Nothing => None,
            Scoped::TheMethod => Some(method_arn.clone()),
            Scoped::TheAuthorizer => Some(authorizer_source_arn.clone()),
            Scoped::EveryResourceOfTheApi => Some(every_resource.clone()),
        };
        if let Some(source_arn) = &scoped_to {
            grant(
                &router,
                &authorizer_function,
                AUTHORIZER_STATEMENT,
                source_arn,
            )
            .await;
        }

        // EVERY request earns a terminal answer: the gate answers its own refusals rather than
        // leaving the client waiting on an authorizer, so a request that never terminates is
        // this row FAILING with its own case named, not the suite hanging.
        let answered = tokio::time::timeout(
            ANSWER_DEADLINE,
            dispatch(
                &router,
                "GET",
                &invoke_host,
                &format!("/{STAGE}/{RESOURCE_PATH}?{KEY_PARAMETER}={key}"),
                &[(TOKEN_HEADER, PRESENTED_TOKEN)],
                Bytes::new(),
            ),
        )
        .await
        .unwrap_or_else(|_elapsed| {
            panic!("case {case:?}: the stage never answered the request at all")
        });

        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} from the stage, got {}",
            row.status,
            answered.summary()
        );
        assert_eq!(
            answered.json(),
            serde_json::from_str::<Value>(row.body).expect("the row's own body is JSON"),
            "case {case:?}: expected the body {}, got {}",
            row.body,
            answered.summary()
        );
        assert_eq!(
            answered.header(ERROR_TYPE_HEADER).as_deref(),
            row.error_type,
            "case {case:?}: expected {ERROR_TYPE_HEADER:?} to name {:?}, got {:?}",
            row.error_type,
            answered.header(ERROR_TYPE_HEADER)
        );
        // The generic server fault is the answer this refusal must NOT be rendered as: a
        // client keyed on the class or on the sentence must be able to tell an authorizer it
        // has to fix from a backend that broke behind an admitted call.
        if row.error_type == Some(AUTHORIZER_CONFIGURATION_EXCEPTION) {
            assert_ne!(
                answered.header(ERROR_TYPE_HEADER).as_deref(),
                Some(INTERNAL_SERVER_ERROR_EXCEPTION),
                "case {case:?}: the refusal must not name the generic server fault, got {}",
                answered.summary()
            );
            assert_ne!(
                answered.text(),
                INTERNAL_SERVER_ERROR_BODY,
                "case {case:?}: the refusal must not carry the generic server-fault body, got {}",
                answered.summary()
            );
        }

        // What the BACKEND did: the item this row names is in the table only where the
        // authorizer's invocation was admitted and the policy it returned admitted the call.
        // The answer above is terminal — the integration has either been performed or refused
        // by the time the client holds it — so the reading waits on nothing.
        owed.push((key, row.integration_runs));
        for (written, expected) in &owed {
            let held = item_under(&router, &table, written).await;
            let wanted = expected
                .then(|| json!({ HASH_KEY: { "S": written }, VERB_MEMBER: { "S": "GET" } }));
            assert_eq!(
                held, wanted,
                "case {case:?}: expected the table to hold {wanted:?} under {written:?}, got \
                 {held:?}"
            );
        }

        if scoped_to.is_some() {
            withdraw(&router, &authorizer_function, AUTHORIZER_STATEMENT).await;
        }
    }
}
