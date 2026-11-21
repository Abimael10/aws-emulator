// SPDX-License-Identifier: CC0-1.0

//! What a client of a DEPLOYED REST stage is served when the `AWS_PROXY` integration behind the
//! method invokes a function whose handler BLOCKS before it returns.
//!
//! RULE: an `AWS_PROXY` integration invokes its function synchronously and holds the client's
//! request open for that whole run — the response IS the envelope the function returned, so every
//! effect the run had before returning is already in place when the client is answered, and the
//! span the client waits is at least the span the run itself spent. That wait is bounded by the
//! integration's own stored deadline ("Custom timeout between 50 and 29,000 milliseconds. The
//! default value is 29,000 milliseconds or 29 seconds", `Integration$timeoutInMillis`): a run that
//! has not answered by then costs the request the function's answer, and the stage answers through
//! the api's `INTEGRATION_TIMEOUT` gateway response (`504`) — rendered from the template the api's
//! own customization carries, with `$context.error.responseType` standing for the response type
//! being answered — while the invocation itself is NOT cancelled and still runs to completion.
//!
//! Four crates have to agree for that to hold (IAM holds the execution role and the grant its run
//! writes with, CloudFormation archives the inline handler into a deployment package this emulator
//! can run, Lambda runs the handler and spends the wait it asked for, DynamoDB holds the item that
//! run writes, and API Gateway resolves the stage, bounds the integration and renders the gateway
//! response), so the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: where the integration's stored deadline
//! sits relative to the span the function's own run takes — the documented default, which no
//! stored number overrides; a stored number comfortably past the run; one well short of it; and
//! the shortest number the member admits. Every row asserts the status, the body and what the run
//! left in the table, so a stage that answers before its backend has done the work, one that
//! cannot be made to miss any deadline at all, and one that kills the run it stopped waiting for
//! each fail here.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region of the table, the function and
/// the api's own invoke host.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the table's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The table the handler writes the record of its own run into, and the two attributes it holds:
/// the leg that ran, and the span that run measured for itself.
const TABLE: &str = "xsvc-proxy-deadline-runs";
const LEG_ATTRIBUTE: &str = "leg";
const MEASURED_ATTRIBUTE: &str = "measured_ms";

/// The function every method of the api is integrated with, the role it runs as, the one inline
/// document that role carries, and the stack that archives the handler into a package.
const FUNCTION: &str = "xsvc-proxy-deadline-handler";
const EXECUTION_ROLE: &str = "xsvc-proxy-deadline-role";
const EXECUTION_POLICY: &str = "run-record";
const STACK: &str = "xsvc-proxy-deadline-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// How long one invocation is given, as the function's own `Timeout`: well past the wait the
/// handler asks for below, so what stops a row is the INTEGRATION's deadline and never the
/// function's own.
const FUNCTION_TIMEOUT: u64 = 60;

/// The span the handler BLOCKS for before it writes its record and returns, in milliseconds. It is
/// the one quantity the rows are stated against: a deadline shorter than this cannot be met and a
/// deadline longer than it can.
///
/// It is stated at twice the shortest deadline a row stores, so the window between a row's timeout
/// being answered and that row's run writing its record is as long as the wait that row's deadline
/// cut — what a reading taken right after the timeout lands inside is then a matter of the rule and
/// not of how this host happened to schedule it. Otherwise as short as the rows allow, so the suite
/// holds this host for no longer than stating the rule needs.
const ASKED_WAIT_MS: u64 = 2_000;

/// How often to look while waiting for work another service's clock owes — a created table
/// reaching `ACTIVE`, a created function reaching `Active`, the record of a run that is still in
/// flight — and how many looks to take. The spans are those services' own, so no fixed sleep names
/// the instant one ends, and the deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 600;

/// The gateway response type a request whose integration did not answer in time is served
/// through, which is also the fixed text `$context.error.responseType` stands for in that
/// response's own template.
const INTEGRATION_TIMEOUT: &str = "INTEGRATION_TIMEOUT";

/// The status that gateway response carries, from the API Gateway REST API reference's table of
/// generated gateway responses.
const INTEGRATION_TIMEOUT_STATUS: u16 = 504;

/// The media type a gateway response's template is keyed by, and the one its rendering is served
/// with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// A marker of the api's OWN customization of that response: text no refusal of this emulator
/// words by itself, so a body carrying it can only have come from the template below.
const TEMPLATE_MARKER: &str = "the-apis-own-timeout-template";

/// The `application/json` template the api's `INTEGRATION_TIMEOUT` customization carries: the
/// marker above, and a reference to the response type being answered.
fn timeout_template() -> String {
    format!(r#"{{"gw":"{TEMPLATE_MARKER}","type":"$context.error.responseType"}}"#)
}

/// What that template renders to for this response type: the reference resolved to the type's own
/// fixed name, and nothing else changed.
fn timeout_body() -> String {
    format!(r#"{{"gw":"{TEMPLATE_MARKER}","type":"{INTEGRATION_TIMEOUT}"}}"#)
}

/// The handler every method is integrated with: it BLOCKS for the span the request asks for, then
/// records what that block really cost it and returns the proxy envelope the client is served.
///
/// The span it measures is the handler's OWN reading of the clock around its own blocking wait, so
/// a run whose wait costs no time at all reports it here. `TABLE_NAME` is substituted before the
/// stack is deployed.
const BLOCKING_HANDLER: &str = r#"
import json
import time

import boto3


def handler(event, context):
    asked = (event.get("queryStringParameters") or {}).get("wait", "0")
    leg = (event.get("queryStringParameters") or {}).get("leg", "unnamed")
    started = time.monotonic()
    time.sleep(float(asked))
    measured_ms = int((time.monotonic() - started) * 1000)
    boto3.client("dynamodb").put_item(
        TableName="TABLE_NAME",
        Item={"leg": {"S": leg}, "measured_ms": {"N": str(measured_ms)}},
    )
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"leg": leg, "measuredMs": measured_ms}),
    }
"#;

/// The handler source as it is deployed: the table it writes named in it.
fn handler_source() -> String {
    BLOCKING_HANDLER.replace("TABLE_NAME", TABLE)
}

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

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
/// into this same router, so the table it writes is the table this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential scope
    // so the router can name the target service, and the invoke of the deployed stage carries no
    // credential at all, as a client of a public method sends none.
    router.set_request_authentication(false);
    router
}

/// The access key id this emulator publishes, which resolves to no session and so speaks for the
/// account's own identity.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
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

/// One awsQuery call of `service` that has to be accepted, answered with its XML body.
async fn accepted_query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        &format!("{service}.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(service)),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup: {service} {action} must answer 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
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

/// A required string member of a JSON response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: must carry a non-empty {member:?}, got {body}"))
}

/// The ARN of the table the handler writes, which its execution role's grant names.
fn table_arn() -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}")
}

/// Create the table the handler writes and wait until DynamoDB reports it `ACTIVE` — "you can
/// perform read and write operations only on an ACTIVE table".
async fn create_table(router: &MultiServiceRouter) {
    let created = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": LEG_ATTRIBUTE, "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": LEG_ATTRIBUTE, "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateTable({TABLE:?}) must succeed, got {}",
        created.summary()
    );
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = dynamodb(router, "DescribeTable", &json!({"TableName": TABLE}))
            .await
            .json();
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        described["Table"]["TableStatus"].as_str(),
        Some("ACTIVE"),
        "setup: DynamoDB must report {TABLE:?} ACTIVE, got {described}"
    );
}

/// The execution role the function runs as, with the one document its run needs: the log actions
/// every run writes with, and the write it makes into the table above.
async fn create_execution_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    let role_arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole({EXECUTION_ROLE:?}) must answer the ARN"));
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": "dynamodb:PutItem",
                "Resource": table_arn(),
            },
        ],
    })
    .to_string();
    accepted_query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyName", EXECUTION_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    role_arn
}

/// Deploy the stack declaring the function: the inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to exist
/// at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deploy_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Blocker": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": handler_source()},
                },
            },
        },
    })
    .to_string();
    accepted_query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described}"
    );

    // The invocation the integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        configuration = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup: Lambda must report {FUNCTION:?} Active, got {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// Grant API Gateway the invocation of that function: "grants a principal permission to use a
/// function" (`AddPermission`), which is what admits the integration's call.
async fn grant_api_gateway(router: &MultiServiceRouter) {
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission must answer 201, got {}",
        granted.summary()
    );
}

/// The api every row calls: one resource per row, each with a `GET` method whose `AWS_PROXY`
/// integration carries that row's own stored deadline, the api's own customization of the
/// `INTEGRATION_TIMEOUT` gateway response, and a deployment serving it all at [`STAGE`].
async fn deployed_api(router: &MultiServiceRouter, function_arn: &str) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "proxy-integration-deadline" })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &created, "id");
    let root = as_str("CreateRestApi", &created, "rootResourceId");

    for row in ROWS {
        let resource = apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": row.leg })),
            201,
        )
        .await;
        let resource = as_str("CreateResource", &resource, "id");
        let method = format!("/restapis/{api}/resources/{resource}/methods/GET");
        apigw(
            router,
            "PutMethod",
            "PUT",
            &method,
            Some(json!({ "authorizationType": "NONE" })),
            201,
        )
        .await;
        // `integrationHttpMethod` travels under its `jsonName`, which is what
        // `smithy_query(query="operation", operation="PutIntegration")` reports as `httpMethod`.
        let mut integration = json!({
            "type": "AWS_PROXY",
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
        });
        if let Some(stored) = row.timeout_in_millis {
            integration["timeoutInMillis"] = json!(stored);
        }
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("{method}/integration"),
            Some(integration),
            201,
        )
        .await;
    }

    // The api's own customization of the response a request whose integration did not answer in
    // time is served through: a template of the developer's, which is what the rows below read the
    // rendered `$context.error.responseType` out of.
    apigw(
        router,
        "PutGatewayResponse",
        "PUT",
        &format!("/restapis/{api}/gatewayresponses/{INTEGRATION_TIMEOUT}"),
        Some(json!({
            "statusCode": INTEGRATION_TIMEOUT_STATUS.to_string(),
            "responseTemplates": { JSON_CONTENT_TYPE: timeout_template() },
        })),
        201,
    )
    .await;

    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    api
}

/// The leg the warm-up invocation below names, which no row asserts anything about.
const WARM_UP_LEG: &str = "warm-up";

/// Invoke the function ONCE, through Lambda's own `Invoke`, with an event that asks for no wait at
/// all, and assert nothing about the answer beyond its having been given.
///
/// The invocation pays for the execution environment the rows are then served in — the interpreter,
/// the runtime and the import of the handler's own module, which the function's `Timeout` does not
/// bound — so no row asserted below carries that cost inside the deadline its integration stores.
async fn warm_the_execution_environment(router: &MultiServiceRouter) {
    let answered = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        Some(json!({
            "queryStringParameters": { "leg": WARM_UP_LEG, "wait": "0" },
        })),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup: the warm-up invocation must answer 200, got {}",
        answered.summary()
    );
}

/// One request to the deployed stage's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` — carrying no credential at all, as a client
/// of a public method sends none. The query string names the leg and the span the handler blocks
/// for.
async fn call_stage(router: &MultiServiceRouter, api: &str, leg: &str) -> Answered {
    let waited_seconds = ASKED_WAIT_MS as f64 / 1_000.0;
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}/{leg}?leg={leg}&wait={waited_seconds}"),
        &[],
        Bytes::new(),
    )
    .await
}

/// The record the run of `leg` wrote, read through DynamoDB's own strongly consistent `GetItem`,
/// or `None` while that run has written nothing.
async fn run_record(router: &MultiServiceRouter, case: &str, leg: &str) -> Option<Value> {
    let read = dynamodb(
        router,
        "GetItem",
        &json!({
            "TableName": TABLE,
            "Key": { LEG_ATTRIBUTE: { "S": leg } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetItem for {leg:?} must answer 200, got {}",
        read.summary()
    );
    read.json().get("Item").cloned()
}

/// The same record, looked for until the run that writes it has written it or the looks run out.
async fn awaited_run_record(router: &MultiServiceRouter, case: &str, leg: &str) -> Option<Value> {
    for _ in 0..LOOKS {
        if let Some(held) = run_record(router, case, leg).await {
            return Some(held);
        }
        tokio::time::sleep(LOOK).await;
    }
    None
}

/// The span the run recorded for its own blocking wait, out of the record it wrote.
fn measured_ms(case: &str, record: &Value) -> u64 {
    record
        .pointer(&format!("/{MEASURED_ATTRIBUTE}/N"))
        .and_then(Value::as_str)
        .and_then(|number| number.parse::<u64>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the run's record must carry {MEASURED_ATTRIBUTE:?}, got {record}")
        })
}

/// What the client is owed by a row.
#[derive(Debug, Clone, Copy)]
enum Owed {
    /// The stored deadline is longer than the run takes: the client is served the function's own
    /// envelope, and the run's record is already in the table when that answer arrives.
    TheFunctionsAnswer,
    /// The stored deadline falls first: the client is served the api's own `INTEGRATION_TIMEOUT`
    /// rendering, and the run — which was never cancelled — finishes and records itself after the
    /// client already holds that answer.
    TheGatewayTimeout {
        /// Whether the run is asserted to complete afterwards. A deadline so short that the
        /// request is given up on before the invocation has been made names no run to finish, so
        /// the row that pins the published MINIMUM asserts the answer alone.
        completes_afterwards: bool,
    },
}

/// One row: the deadline the integration stores, and what that makes of the call.
struct Row {
    case: &'static str,
    /// The resource path part the row calls, which is also the key its run records itself under.
    leg: &'static str,
    /// `Integration$timeoutInMillis` as the row stores it, or `None` for a row that stores none
    /// and so gets the value the member documents as its default.
    timeout_in_millis: Option<i32>,
    owed: Owed,
}

/// The rows, in the order they run: the ones that are SERVED first, so the deadline rows below are
/// judged against an execution environment that is already standing.
///
/// The deadline a row stores is stated against [`ASKED_WAIT_MS`], which is what the handler blocks
/// for: `1_000` is short of that block and `29_000` is well past it.
const ROWS: &[Row] = &[
    Row {
        case: "no stored deadline at all, so the documented default of 29,000 ms applies",
        leg: "default-deadline",
        timeout_in_millis: None,
        owed: Owed::TheFunctionsAnswer,
    },
    Row {
        case: "the largest deadline the member admits, which the run fits well inside",
        leg: "widest-deadline",
        timeout_in_millis: Some(29_000),
        owed: Owed::TheFunctionsAnswer,
    },
    Row {
        case: "a stored deadline shorter than the run's own wait",
        leg: "short-deadline",
        timeout_in_millis: Some(1_000),
        owed: Owed::TheGatewayTimeout {
            completes_afterwards: true,
        },
    },
    Row {
        case: "the smallest deadline the member admits, which pins the bound and nothing past it",
        leg: "shortest-deadline",
        timeout_in_millis: Some(50),
        owed: Owed::TheGatewayTimeout {
            completes_afterwards: false,
        },
    },
];

/// Every row, over one assembled emulator.
///
/// Driven on a MULTI-THREADED runtime, as the server serves a request: the chain is three services
/// deep — the stage's request awaits the invocation, whose handler makes its own call back into
/// this process — and a run the stage stopped waiting for is still going while the next row is
/// served, so the suite runs it the way the server runs it rather than on the single-threaded
/// default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_proxy_integration_waits_out_its_run_and_bounds_it_by_its_own_stored_deadline() {
    let router = emulator();
    create_table(&router).await;
    let role_arn = create_execution_role(&router).await;
    let function_arn = deploy_function(&router, &role_arn).await;
    grant_api_gateway(&router).await;
    let api = deployed_api(&router, &function_arn).await;
    warm_the_execution_environment(&router).await;

    for row in ROWS {
        let case = row.case;
        let leg = row.leg;
        let answered = call_stage(&router, &api, leg).await;
        match row.owed {
            Owed::TheFunctionsAnswer => {
                assert_eq!(
                    answered.status,
                    200,
                    "case {case:?}: the function's own envelope must be served, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.header("content-type"),
                    Some(JSON_CONTENT_TYPE.to_string()),
                    "case {case:?}: expected the envelope's own content type, got {:?}",
                    answered.header("content-type")
                );
                let body = answered.json();
                assert_eq!(
                    body.get("leg").and_then(Value::as_str),
                    Some(leg),
                    "case {case:?}: expected the handler's own body for this leg, got {body}"
                );
                // The handler's OWN reading of the clock around its blocking wait: a lower bound,
                // so a host of any speed satisfies it and only an environment whose blocking wait
                // costs the run no time at all fails it.
                let reported = body
                    .get("measuredMs")
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: the body must carry measuredMs, got {body}")
                    });
                assert!(
                    reported >= ASKED_WAIT_MS,
                    "case {case:?}: the run blocked for {ASKED_WAIT_MS} ms, so its own reading of \
                     that wait must be at least that, got {reported}"
                );
                // The request was held open for the WHOLE run: the record the handler wrote before
                // returning is already there when the client holds the answer, with no looking
                // again.
                let record = run_record(&router, case, leg).await.unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the run's record must already be in the table when its \
                         answer reaches the client"
                    )
                });
                let recorded = measured_ms(case, &record);
                assert!(
                    recorded >= ASKED_WAIT_MS,
                    "case {case:?}: the recorded wait must be at least {ASKED_WAIT_MS} ms, got \
                     {recorded} in {record}"
                );
            }
            Owed::TheGatewayTimeout {
                completes_afterwards,
            } => {
                assert_eq!(
                    answered.status,
                    INTEGRATION_TIMEOUT_STATUS,
                    "case {case:?}: a run that outlasts the stored deadline must be answered \
                     through the api's {INTEGRATION_TIMEOUT} response, got {}",
                    answered.summary()
                );
                assert_eq!(
                    answered.text(),
                    timeout_body(),
                    "case {case:?}: expected the api's own template rendered with the response \
                     type, got {:?}",
                    answered.text()
                );
                // The run the stage stopped waiting for had not finished: its record cannot be
                // there yet, because the handler writes it only after the wait it asked for.
                let early = run_record(&router, case, leg).await;
                assert!(
                    early.is_none(),
                    "case {case:?}: the client holds the timeout before the run's own effect, got \
                     {early:?}"
                );
                if completes_afterwards {
                    // The timed-out request did not CANCEL the invocation: the run goes on and
                    // records itself after the client already holds the timeout.
                    let record = awaited_run_record(&router, case, leg)
                        .await
                        .unwrap_or_else(|| {
                            panic!(
                                "case {case:?}: the invocation must still complete after the \
                                 client was answered"
                            )
                        });
                    let recorded = measured_ms(case, &record);
                    assert!(
                        recorded >= ASKED_WAIT_MS,
                        "case {case:?}: the recorded wait must be at least {ASKED_WAIT_MS} ms, \
                         got {recorded} in {record}"
                    );
                }
            }
        }
    }
}
