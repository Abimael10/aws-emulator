// SPDX-License-Identifier: CC0-1.0

//! WHEN the access-log record of a request a Lambda-proxy REST stage served becomes readable
//! in the CloudWatch Logs group its `accessLogSettings` name: the instant each delivered
//! event carries, and what a TIME-BOUNDED read of that group reports.
//!
//! RULE: **with the account naming a CloudWatch role API Gateway can write as
//! (`Account$cloudwatchRoleArn`), every request a deployed stage carrying `accessLogSettings`
//! answers is delivered into the log group its `destinationArn` names as exactly ONE log
//! event, whose message is that stage's `format` — "a single line format of the access logs
//! of data, as specified by selected $context variables" — resolved against THAT request, and
//! whose event timestamp is the instant the record was delivered. So a client watching the
//! destination with `FilterLogEvents` bounded by `startTime` ("the start of the time range …
//! Events with a timestamp before this time are not returned") is reported exactly the
//! records of the requests served from that instant on: a window opened before the traffic
//! reports all of it, and a window opened after the last record was written reports none.
//! Which integration answered decides none of it — a method run by a Lambda function through
//! an `AWS_PROXY` integration is logged like any other, with `$context.status` the status that
//! run returned, `$context.httpMethod` the verb the request was served under and
//! `$context.resourcePath` the resource template that matched.**
//!
//! A time-bounded read is how a client of a destination actually watches it: an unbounded
//! filter answers a page of everything the group ever took, so an event stamped with an
//! instant the client is not watching (an unstamped event, or one stamped from some other
//! clock) is invisible to the read a client makes and its request looks unlogged forever.
//! That is the half neither sibling suite reaches.
//!
//! Only a cross-service suite can pin it: AWS IAM holds the function's execution role and the
//! account's CloudWatch role, CloudFormation archives the inline handler into a deployment
//! package this emulator can run, AWS Lambda runs it, API Gateway resolves the stage, serves
//! the request through the proxy integration and delivers the record, and CloudWatch Logs
//! holds the group every assertion reads — all of them on ONE engine, as
//! `aws_emulator_router::build_router` assembles the emulator a client really dials.
//!
//! What the neighbouring assembled-router suites already pin, and this one does not repeat:
//! [`crate::apigateway_stage_access_log_destination_delivery`] pins the DELIVERY of a `MOCK`
//! method's records — one event per answered request, and the stream and page readings of the
//! destination — and [`crate::apigateway_token_authorizer_deny_and_access_log`] pins what a
//! stage records about a request its Lambda authorizer refused. What only this suite reaches
//! is the TIME dimension of the delivery, over a method a function RAN.
//!
//! The traffic rows differ along what the rule generalises over: the VERB the method is served
//! under, the SHAPE of the resource the request matched (one segment, parameterised), whether
//! a query string was sent, the STATUS the function's run returned (a success, a created, a
//! not-found), and the REPETITION of one request. The window rows are generated from the
//! instants the records were actually written, and span a bound before all of them, each bound
//! that is one of them, and a bound past the last.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every call is signed for, every ARN names and the api's invoke host carries.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the destination group's ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The access key id this emulator publishes, which speaks for the account's own identity.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The function behind every method of the api, the role it runs as, and the stack that
/// archives its inline handler into a deployment package.
const FUNCTION: &str = "xsvc-access-log-window-echo";
const EXECUTION_ROLE: &str = "xsvc-access-log-window-exec";
const STACK: &str = "xsvc-access-log-window-stack";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The role the ACCOUNT writes its logs under, and the inline document embedded on it.
const PUSH_ROLE: &str = "xsvc-access-log-window-push";
const PUSH_POLICY: &str = "vended-logs";

/// The stage the api is deployed to, and the destination its access logging is pointed at.
const STAGE: &str = "prod";
const DESTINATION_GROUP: &str = "/aws/apigateway/access-log-window";

/// The format the stage renders its records by: the `$context` references the rule names, as
/// an operator writes a JSON access log. It selects `$context.requestId`, which
/// `AccessLogSettings$format` requires of a format at all ("the format must include at least
/// $context.requestId").
const FORMAT: &str = concat!(
    r#"{"requestId":"$context.requestId","httpMethod":"$context.httpMethod","#,
    r#""resourcePath":"$context.resourcePath","status":"$context.status"}"#
);

/// The header API Gateway reports the id of the request it processed in, which a record's
/// `$context.requestId` has to equal.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the record and not how fast an interpreter
/// gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a delivered record is given to become readable in its destination, and how long to
/// wait between looks. The suite waits for the RECORDS rather than for any duration: it fails
/// only where they never arrive at all, and the deadline is many times over what the write
/// needs so a loaded host cannot decide the outcome.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(45);
const DELIVERY_POLL: Duration = Duration::from_millis(20);

/// How long a created function is given to report itself `Active`, and how often to look. The
/// span is Lambda's own, so no fixed sleep names the instant it ends — the wait is on the
/// CONDITION, with the same generous room.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(90);
const LOOK: Duration = Duration::from_millis(100);

/// The handler behind every method: the query string says which status the run answers with,
/// so one package serves every row and each record's status is one a RUN returned.
const ECHO_HANDLER: &str = r#"
import json


def handler(event, context):
    query = event.get("queryStringParameters") or {}
    status = int(query.get("status") or "200")
    return {
        "statusCode": status,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"answered": status}),
    }
"#;

/// One answered request: its status, the request id it was reported under and its body.
struct Answered {
    status: u16,
    request_id: Option<String>,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text())
    }
}

/// The emulator as it is ASSEMBLED, on one engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the invoke of the published stage
    // URL carries no credential at all, as a client of a public method sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
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
    let request_id = parts
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Answered {
        status: parts.status.as_u16(),
        request_id,
        body,
    }
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
        "setup {what:?}: expected HTTP {expected} from API Gateway, got {}",
        answered.summary()
    );
    answered.json()
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

/// One CloudWatch Logs call over its `awsJson1_1` wire, answering its status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let answered = dispatch(
        router,
        "POST",
        &format!("logs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("authorization", &authorization("logs")),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    (answered.status, answered.json())
}

/// One CloudWatch Logs call that has to succeed.
async fn logs_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = logs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "setup {operation}: expected HTTP 200 from CloudWatch Logs, got {status} with {parsed}"
    );
    parsed
}

/// One `awsQuery` call of `service` that has to be accepted, answering its XML body.
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
        "setup {service} {action}: expected HTTP 200, got {}",
        answered.summary()
    );
    answered.text()
}

/// One unsigned request to the deployed stage's own invoke URL, as a client of a method whose
/// `authorizationType` is `NONE` makes it.
async fn invoke(router: &MultiServiceRouter, api: &str, verb: &str, asked_for: &str) -> Answered {
    dispatch(
        router,
        verb,
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{asked_for}"),
        &[("content-type", "application/json")],
        Bytes::new(),
    )
    .await
}

/// A required, non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what:?}: expected a non-empty {member:?}, got {body}"))
}

/// The instant now, in the epoch milliseconds a log event's `timestamp` and a filter's
/// `startTime` are both expressed in.
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("a clock after the epoch")
            .as_millis(),
    )
    .expect("an instant within the epoch-millisecond range")
}

/// A trust policy naming one AWS service as the principal that may assume the role.
fn service_trust_policy(service_principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service_principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create a role carrying `trust_policy` and one inline document, answering the ARN IAM minted
/// for it.
async fn role_with(
    router: &MultiServiceRouter,
    name: &str,
    trust_policy: &str,
    policy: &str,
    document: &str,
) -> String {
    let created = accepted_query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", trust_policy),
        ],
    )
    .await;
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup CreateRole({name:?}): expected the role's ARN"));
    accepted_query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", policy),
            ("PolicyDocument", document),
        ],
    )
    .await;
    arn
}

/// The document a writer of the account's log groups carries: the vended-logs actions a
/// delivery is made with, and the readers of a group.
fn log_write_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogGroup",
                "logs:CreateLogStream",
                "logs:DescribeLogGroups",
                "logs:DescribeLogStreams",
                "logs:PutLogEvents",
                "logs:GetLogEvents",
                "logs:FilterLogEvents",
            ],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Point the ACCOUNT at `role_arn`, which is what gives API Gateway something to write the
/// account's logs as, and read it back.
async fn account_logs_under(router: &MultiServiceRouter, role_arn: &str) {
    let reported = apigw(
        router,
        "UpdateAccount",
        "PATCH",
        "/account",
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/cloudwatchRoleArn", "value": role_arn },
            ],
        })),
        200,
    )
    .await;
    assert_eq!(
        reported["cloudwatchRoleArn"].as_str(),
        Some(role_arn),
        "setup UpdateAccount: expected the account to report the push role {role_arn:?}, got \
         {reported}"
    );
}

/// Deploy the stack declaring the function: the inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all. Answers the ARN Lambda reports for it, once it reports it `Active`.
async fn deployed_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Echo": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": ECHO_HANDLER },
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
        "setup CreateStack: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );

    // The invoke the integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
    let deadline = Instant::now() + ACTIVE_DEADLINE;
    let mut configuration;
    loop {
        configuration = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active")
            || Instant::now() >= deadline
        {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup GetFunctionConfiguration: expected Lambda to report {FUNCTION:?} Active, got \
         {configuration}"
    );
    as_str("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// `CreateResource` of `path_part` under `parent`, answering the created resource's id.
async fn child(router: &MultiServiceRouter, api: &str, parent: &str, path_part: &str) -> String {
    let created = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{parent}"),
        Some(json!({ "pathPart": path_part })),
        201,
    )
    .await;
    as_str("CreateResource", &created, "id")
}

/// One open method on `resource`, served by `function_arn` through an `AWS_PROXY` integration —
/// "for integrating the method request with the Lambda function-invoking action with the client
/// request passed through as-is" (`IntegrationType`).
async fn proxy_method(
    router: &MultiServiceRouter,
    api: &str,
    resource: &str,
    verb: &str,
    function_arn: &str,
) {
    let method = format!("/restapis/{api}/resources/{resource}/methods/{verb}");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;
}

/// The api, its methods all proxying to `function_arn`, the grant that admits the invocation
/// and the deployment that releases it to [`STAGE`]. Answers the api id.
async fn deployed_api(router: &MultiServiceRouter, function_arn: &str) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "access-log-window-api" })),
        201,
    )
    .await;
    let api = as_str("CreateRestApi", &created, "id");
    let root = as_str("CreateRestApi", &created, "rootResourceId");

    let echo = child(router, &api, &root, "echo").await;
    for verb in ["GET", "POST", "DELETE"] {
        proxy_method(router, &api, &echo, verb, function_arn).await;
    }
    let items = child(router, &api, &root, "items").await;
    let item = child(router, &api, &items, "{id}").await;
    proxy_method(router, &api, &item, "GET", function_arn).await;

    // "Grants a principal permission to use a function" (`AddPermission`): the grant that
    // admits the invocation API Gateway makes as its own service principal.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/*/*"),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission: expected HTTP 201 from Lambda, got {}",
        granted.summary()
    );

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

/// Point the stage's access logging at [`DESTINATION_GROUP`], rendered by [`FORMAT`], as an
/// operator configures it: the group ARN without the trailing wildcard, which is one of the two
/// published forms of a log group's ARN.
async fn configure_access_logging(router: &MultiServiceRouter, api: &str) {
    let destination = format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{DESTINATION_GROUP}");
    let reported = apigw(
        router,
        "UpdateStage accessLogSettings",
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": "/accessLogSettings/destinationArn",
                    "value": destination,
                },
                { "op": "replace", "path": "/accessLogSettings/format", "value": FORMAT },
            ],
        })),
        200,
    )
    .await;
    assert_eq!(
        reported["accessLogSettings"]["destinationArn"].as_str(),
        Some(destination.as_str()),
        "setup UpdateStage: expected the stage to report the destination {destination:?}, got \
         {reported}"
    );
    assert_eq!(
        reported["accessLogSettings"]["format"].as_str(),
        Some(FORMAT),
        "setup UpdateStage: expected the stage to report the configured format, got {reported}"
    );
}

/// The `(timestamp, message)` of every event `FilterLogEvents` reports for the destination,
/// bounded by `start_time` where one is given.
async fn filtered(router: &MultiServiceRouter, start_time: Option<i64>) -> Vec<(i64, String)> {
    let mut request = json!({ "logGroupName": DESTINATION_GROUP });
    if let Some(start) = start_time {
        request["startTime"] = json!(start);
    }
    let body = logs_ok(router, "FilterLogEvents", request).await;
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    (
                        event["timestamp"].as_i64().unwrap_or_default(),
                        event["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The destination's events once at least `expected` of them are readable there, looking again
/// until they are or [`DELIVERY_DEADLINE`] passes: the wait is on the RECORDS, never on a
/// duration.
async fn events_settled(
    router: &MultiServiceRouter,
    case: &str,
    expected: usize,
) -> Vec<(i64, String)> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let events = filtered(router, None).await;
        if events.len() >= expected {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected at least {expected} access-log event(s) in \
             {DESTINATION_GROUP:?}, got {events:?}"
        );
        tokio::time::sleep(DELIVERY_POLL).await;
    }
}

/// One request the stage serves, and what the record it leaves has to say about it.
struct Row {
    case: &'static str,
    /// The verb the request is sent under, which `$context.httpMethod` reports.
    verb: &'static str,
    /// What the client asks for within the stage, query string included.
    asked_for: &'static str,
    /// The `$context.resourcePath` the record has to carry: the template that matched.
    resource_path: &'static str,
    /// The status the function's run answers with, which `$context.status` reports in decimal.
    status: u16,
}

/// The traffic table: the verbs, the resource shapes, the statuses a run returned and a repeat
/// of one request.
const ROWS: &[Row] = &[
    Row {
        case: "a GET the function answers 200, on a one-segment resource",
        verb: "GET",
        asked_for: "/echo",
        resource_path: "/echo",
        status: 200,
    },
    Row {
        case: "the same request again, which is a second record of its own",
        verb: "GET",
        asked_for: "/echo",
        resource_path: "/echo",
        status: 200,
    },
    Row {
        case: "a POST whose run answers 201, with a query string",
        verb: "POST",
        asked_for: "/echo?status=201",
        resource_path: "/echo",
        status: 201,
    },
    Row {
        case: "a DELETE whose run answers 404, which the stage still served",
        verb: "DELETE",
        asked_for: "/echo?status=404",
        resource_path: "/echo",
        status: 404,
    },
    Row {
        case: "a parameterised resource, where the template and the path asked for differ",
        verb: "GET",
        asked_for: "/items/42",
        resource_path: "/items/{id}",
        status: 200,
    },
];

/// `records` keyed by the `requestId` each of them carries, which is how a client attributes a
/// delivered record to the request it made.
fn by_request_id(case: &str, records: &[String]) -> BTreeMap<String, Value> {
    let mut keyed = BTreeMap::new();
    for record in records {
        let parsed: Value = serde_json::from_str(record).unwrap_or_else(|error| {
            panic!("case {case:?}: the record {record:?} is not the JSON the format is: {error}")
        });
        let id = parsed["requestId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the record {record:?} names no requestId"))
            .to_string();
        let already = keyed.insert(id.clone(), parsed);
        assert!(
            already.is_none(),
            "case {case:?}: expected one record per request, got two for request {id:?}"
        );
    }
    keyed
}

/// `messages` in a fixed order, for the comparisons about WHICH records a read reports rather
/// than about the order one reader reports them in.
fn sorted(messages: &[String]) -> Vec<String> {
    let mut ordered = messages.to_vec();
    ordered.sort();
    ordered
}

/// RULE: every request a proxy-integrated stage serves is one timestamped record in the
/// destination its `accessLogSettings` name, resolved against that request — and a
/// `startTime`-bounded read of that group reports exactly the records written from that instant
/// on.
#[tokio::test]
async fn each_served_request_leaves_one_timestamped_record_a_bounded_read_reports() {
    let router = emulator();

    // 1. The function, running as its own execution role, behind a deployed api whose methods
    //    all proxy to it.
    let execution_role = role_with(
        &router,
        EXECUTION_ROLE,
        &service_trust_policy("lambda.amazonaws.com"),
        "logging",
        &log_write_document(),
    )
    .await;
    let function_arn = deployed_function(&router, &execution_role).await;
    let api = deployed_api(&router, &function_arn).await;

    // 2. The account's CloudWatch role, which is what API Gateway writes the account's logs as,
    //    and the destination group, which is a log group of the ACCOUNT's: API Gateway delivers
    //    into one that is there and brings none into existence.
    let push_role = role_with(
        &router,
        PUSH_ROLE,
        &service_trust_policy("apigateway.amazonaws.com"),
        PUSH_POLICY,
        &log_write_document(),
    )
    .await;
    account_logs_under(&router, &push_role).await;
    logs_ok(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": DESTINATION_GROUP }),
    )
    .await;
    configure_access_logging(&router, &api).await;

    // 3. The window the client opens BEFORE any of its traffic, as a client watching a
    //    destination does, and then the traffic itself.
    let window_start = now_ms();
    let mut expected: BTreeMap<String, Value> = BTreeMap::new();
    for row in ROWS {
        let case = row.case;
        let answered = invoke(&router, &api, row.verb, row.asked_for).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} from the stage, got {}",
            row.status,
            answered.summary()
        );
        let request_id = answered
            .request_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the response named no request id"));
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected a non-empty request id, got {request_id:?}"
        );
        let already = expected.insert(
            request_id.clone(),
            json!({
                "requestId": request_id,
                "httpMethod": row.verb,
                "resourcePath": row.resource_path,
                "status": row.status.to_string(),
            }),
        );
        assert!(
            already.is_none(),
            "case {case:?}: expected each request to be reported under its own id, got \
             {request_id:?} twice"
        );
    }

    // 4. One record per request, each the stage's format resolved against THAT request.
    let case = "the whole traffic";
    let events = events_settled(&router, case, ROWS.len()).await;
    let messages: Vec<String> = events.iter().map(|(_, message)| message.clone()).collect();
    assert_eq!(
        events.len(),
        ROWS.len(),
        "case {case:?}: expected exactly one record per request in {DESTINATION_GROUP:?}, got \
         {messages:?}"
    );
    assert!(
        messages.iter().all(|message| !message.contains("$context.")),
        "case {case:?}: expected every reference of the format to be resolved, got {messages:?}"
    );
    assert_eq!(
        by_request_id(case, &messages),
        expected,
        "case {case:?}: expected the stage's format resolved against each request it served, \
         got {messages:?}"
    );

    // 5. Each record is stamped with the instant it was delivered, which is inside the window
    //    the client opened before the traffic and not after the read.
    let read_at = now_ms();
    for (timestamp, message) in &events {
        assert!(
            (window_start..=read_at).contains(timestamp),
            "case {case:?}: expected the record {message:?} to be stamped within the window the \
             traffic was served in ({window_start}..={read_at}), got {timestamp}"
        );
    }

    // 6. The window table, generated from the instants the records were actually written: a
    //    bound before all of them, each bound that IS one of them, and a bound past the last.
    let stamps: BTreeSet<i64> = events.iter().map(|(timestamp, _)| *timestamp).collect();
    let last = *stamps
        .iter()
        .next_back()
        .expect("the delivered records carry instants");
    let mut bounds: Vec<(String, i64)> = vec![
        (
            "the window the client opened before its traffic".to_string(),
            window_start,
        ),
        ("one millisecond before that window".to_string(), window_start - 1),
    ];
    for stamp in &stamps {
        bounds.push((format!("the instant {stamp} a record was written at"), *stamp));
    }
    bounds.push((
        "the instant after the last record was written".to_string(),
        last + 1,
    ));
    for (case, start) in &bounds {
        // "Events with a timestamp before this time are not returned" — so exactly the records
        // stamped at or after the bound, and nothing else.
        let owed: Vec<String> = events
            .iter()
            .filter(|(timestamp, _)| timestamp >= start)
            .map(|(_, message)| message.clone())
            .collect();
        let reported = filtered(&router, Some(*start)).await;
        let got: Vec<String> = reported
            .iter()
            .map(|(_, message)| message.clone())
            .collect();
        assert_eq!(
            sorted(&got),
            sorted(&owed),
            "case {case:?}: expected FilterLogEvents(startTime={start}) to report the \
             {} record(s) written at or after it, got {got:?}",
            owed.len()
        );
    }
}
