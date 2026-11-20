// SPDX-License-Identifier: CC0-1.0

//! The EXECUTION logs a deployed REST stage delivers once a CloudFormation stack has installed
//! the account's CloudWatch role.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > `Account$cloudwatchRoleArn` is "the ARN of an Amazon CloudWatch role for the current
//! > Account" — ONE setting of API Gateway per account and Region, shared by every REST API in
//! > it — so whichever caller writes it, the SAME setting is in force and `GetAccount` reports
//! > it: `UpdateAccount` with a `replace` of `/cloudwatchRoleArn`, and the
//! > `AWS::ApiGateway::Account` resource of a stack, which is that one call. While a role is in
//! > force, every request a stage serves whose `methodSettings` turn execution logging on for
//! > the method it reached is traced into the group API Gateway publishes for that stage,
//! > `API-Gateway-Execution-Logs_{rest-api-id}/{stage-name}` — a group that delivery brings
//! > into existence, with a stream carrying the events — whichever caller configured the stage.
//! > A stage whose settings turn execution logging on for no method is traced nowhere and has no
//! > such group at all, and while the account carries NO role nothing is delivered for any
//! > stage, however the stage is configured.
//!
//! Four services have to agree for that to hold — CloudFormation makes the calls the template
//! declares, IAM holds the role the account names and decides whether API Gateway may assume
//! it, API Gateway holds the account singleton and the api whose stage serves the request, and
//! CloudWatch Logs holds the group the trace lands in — so the suite lives here, with all four
//! mounted on ONE engine as `core/router/src/lib.rs` mounts them. Every delivery is read back
//! through CloudWatch Logs' own readers (`DescribeLogStreams` then `GetLogEvents`), so a trace
//! that never reached that store cannot pass.
//!
//! The rows differ along what the rule generalises over: WHERE the stage's logging level was
//! declared — a `MethodSettings` entry of the stack's own `AWS::ApiGateway::Stage` naming one
//! method, that resource's stage-wide entry, the `StageDescription` of the stack's
//! `AWS::ApiGateway::Deployment`, and a direct `UpdateStage` patch made after the stack — which
//! stage of the api the request reaches, and the resource path and verb of the method it
//! reaches (a child resource under one verb, a parameterised resource one level deeper under
//! another), plus the stage that turns execution logging on nowhere. The whole table is then
//! driven again with the one setting RE-POINTED by a direct `UpdateAccount` at a role created
//! through IAM's own call, and once more with that setting cleared.
//!
//! Every method here is a `MOCK` integration — "for testing without actually invoking the
//! backend" (`IntegrationType`) — so what a row shows is the stage's own logging and never a
//! backend's doing. Every wait is on the CONDITION, looked at again until it holds or a
//! generous budget passes; nothing asserts on how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, outputs_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every call here is signed for, every invoke host carries and every ARN names.
const REGION: &str = "us-east-1";

/// The stack the table is served by, and the name it declares its api under.
const STACK: &str = "apigw-stack-installed-account-role";
const API_NAME: &str = "apigw-stack-installed-account-role-api";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal the account's CloudWatch role has to trust: API Gateway writes the
/// account's logs under that role, so the assume is granted to that service.
const APIGATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// The AWS-managed policy the documented "API Gateway may push logs to CloudWatch" setup
/// attaches to that role (AWS Managed Policy Reference, filed under `/service-role/`).
const PUSH_TO_CLOUDWATCH_LOGS: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonAPIGatewayPushToCloudWatchLogs";

/// The role a direct `UpdateAccount` later re-points the one setting at, created through IAM's
/// own `CreateRole` rather than by the stack.
const SECOND_ROLE: &str = "apigw-account-role-repointed";

/// The `loggingLevel` every stage of the table that logs at all is set to: "valid values are
/// `OFF`, `ERROR`, and `INFO`" (`MethodSetting$loggingLevel`), and `INFO` is the one that writes
/// the whole trace of a request however the request was answered.
const INFO: &str = "INFO";

/// The stages the stack deploys the api to, one per way the logging level is configured.
const PER_METHOD_STAGE: &str = "test";
const WIDE_STAGE: &str = "wide";
const DESCRIBED_STAGE: &str = "described";
const PATCHED_STAGE: &str = "patched";
const QUIET_STAGE: &str = "quiet";

/// The two methods of the api, by the resource path each sits on and the verb it answers.
const PING_PATH: &str = "/ping";
const PING_VERB: &str = "GET";
const ITEM_PATH: &str = "/items/{id}";
const ITEM_VERB: &str = "POST";

/// The status every method answers with.
const SERVED_STATUS: u16 = 200;

/// The header API Gateway names the request a response answers in, lowercased as the wire
/// carries it; every record of a stage's execution log is prefixed with that id.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The prefix API Gateway publishes a stage's execution log group under.
const EXECUTION_LOG_GROUP_PREFIX: &str = "API-Gateway-Execution-Logs_";

/// How long to wait between looks at a condition another service owns, and the generous budget
/// one is given before the suite calls it a failure.
const POLL: Duration = Duration::from_millis(25);

/// See [`POLL`].
const BUDGET: Duration = Duration::from_secs(90);

/// What one call answered.
struct Answer {
    status: u16,
    request_id: Option<String>,
    body: String,
}

impl Answer {
    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// The four services this rule spans, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and an invoke of the published stage
    // carries none, as a client of a public method sends none.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
    );
    router
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
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
    let request_id = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Answer {
        status: response.status().as_u16(),
        request_id,
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// One form-encoded awsQuery call.
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
    (answered.status, answered.body)
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One IAM call over its awsQuery wire.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "iam", "2010-05-08", action, members).await
}

/// One API Gateway control-plane call, over that service's `restJson1` wire.
async fn apigateway(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
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
    .await
}

/// One API Gateway control-plane call that has to be accepted with HTTP 200.
async fn apigateway_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let answered = apigateway(router, method, path, body).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from {method} {path}, got {} with {:?}",
        answered.status, answered.body
    );
    answered.json()
}

/// One CloudWatch Logs call over its own `awsJson1_1` wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    dispatch(
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
    .await
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com`, the endpoint
/// `RestApi$disableExecuteApiEndpoint` documents — carrying no credential at all.
async fn invoke(
    router: &MultiServiceRouter,
    api: &str,
    stage: &str,
    verb: &str,
    path: &str,
) -> Answer {
    dispatch(
        router,
        verb,
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{stage}{path}"),
        &[("content-type", "application/json")],
        Bytes::new(),
    )
    .await
}

/// The role API Gateway reports in force for the account, or `None` where it reports none.
async fn account_role(router: &MultiServiceRouter, case: &str) -> Option<String> {
    let reported = apigateway_ok(router, case, "GET", "/account", None).await;
    reported
        .get("cloudwatchRoleArn")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Set the account's CloudWatch role through the one call API Gateway publishes for it: a
/// `replace` of `/cloudwatchRoleArn`, carrying the ARN to set or nothing at all to clear it.
async fn update_account_role(router: &MultiServiceRouter, case: &str, role_arn: &str) {
    apigateway_ok(
        router,
        case,
        "PATCH",
        "/account",
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/cloudwatchRoleArn", "value": role_arn },
            ],
        })),
    )
    .await;
}

/// The execution log group of one stage of one api.
fn execution_group(api: &str, stage: &str) -> String {
    format!("{EXECUTION_LOG_GROUP_PREFIX}{api}/{stage}")
}

/// One record of a stage's execution log as the group holds it: API Gateway prefixes each line
/// of a request's trace with that request's own id in parentheses.
fn about_request(request_id: &str, line: &str) -> String {
    format!("({request_id}) {line}")
}

/// What one log group holds, read exactly as a client reads it: the streams
/// `DescribeLogStreams` reports and every message `GetLogEvents` answers for them. `None` when
/// the account holds no such group at all — the `ResourceNotFoundException` a reader sees for a
/// group nothing has brought into existence.
struct Held {
    streams: Vec<String>,
    messages: Vec<String>,
}

/// See [`Held`].
async fn held(router: &MultiServiceRouter, group: &str) -> Option<Held> {
    let described = logs(
        router,
        "DescribeLogStreams",
        json!({
            "logGroupName": group,
            "orderBy": "LastEventTime",
            "descending": true,
            "limit": 5,
        }),
    )
    .await;
    if described.status != 200 {
        return None;
    }
    let streams: Vec<String> = described
        .json()
        .get("logStreams")
        .and_then(Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter_map(|stream| stream.get("logStreamName").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut messages = Vec::new();
    for stream in &streams {
        let read = logs(
            router,
            "GetLogEvents",
            json!({
                "logGroupName": group,
                "logStreamName": stream,
                "startFromHead": true,
            }),
        )
        .await;
        if read.status != 200 {
            continue;
        }
        if let Some(events) = read.json().get("events").and_then(Value::as_array) {
            messages.extend(
                events
                    .iter()
                    .filter_map(|event| event.get("message").and_then(Value::as_str))
                    .map(str::to_owned),
            );
        }
    }
    Some(Held { streams, messages })
}

/// Every record `group` holds about the request `request_id`, right now.
async fn trace_now(router: &MultiServiceRouter, group: &str, request_id: &str) -> Vec<String> {
    let prefix = about_request(request_id, "");
    held(router, group)
        .await
        .map(|held| {
            held.messages
                .into_iter()
                .filter(|message| message.starts_with(&prefix))
                .collect()
        })
        .unwrap_or_default()
}

/// The records `group` holds about `request_id` once it holds any — or whatever it holds when
/// the budget runs out, so the assertions report what was missing rather than this wait doing
/// it. The delivery is another service's write, so the CONDITION is looked at again until it
/// holds.
async fn trace_of(router: &MultiServiceRouter, group: &str, request_id: &str) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let trace = trace_now(router, group, request_id).await;
        if !trace.is_empty() || Instant::now() >= deadline {
            return trace;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// A trust policy granting `sts:AssumeRole` to one service principal, the form `CreateRole`'s
/// own modelled example carries.
fn trust_policy(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The body one method answers, which names the method that served the request.
fn served_body(label: &str) -> String {
    format!(r#"{{"served":"{label}"}}"#)
}

/// One `AWS::ApiGateway::Resource` under the resource `parent` names.
fn resource(parent: Value, part: &str) -> Value {
    json!({
        "Type": "AWS::ApiGateway::Resource",
        "Properties": { "RestApiId": { "Ref": "Api" }, "ParentId": parent, "PathPart": part },
    })
}

/// One `AWS::ApiGateway::Method` behind a `MOCK` integration answering [`SERVED_STATUS`] with
/// the body naming it.
fn mock_method(resource_logical_id: &str, verb: &str, label: &str) -> Value {
    json!({
        "Type": "AWS::ApiGateway::Method",
        "Properties": {
            "RestApiId": { "Ref": "Api" },
            "ResourceId": { "Ref": resource_logical_id },
            "HttpMethod": verb,
            "AuthorizationType": "NONE",
            "MethodResponses": [{ "StatusCode": SERVED_STATUS.to_string() }],
            "Integration": {
                "Type": "MOCK",
                "RequestTemplates": { "application/json": "{\"statusCode\":200}" },
                "IntegrationResponses": [{
                    "StatusCode": SERVED_STATUS.to_string(),
                    "ResponseTemplates": { "application/json": served_body(label) },
                }],
            },
        },
    })
}

/// One `AWS::ApiGateway::Stage` of the api, carrying the `MethodSettings` entries a stage of
/// the table declares. The account's role has to be in force before a declaration that turns
/// execution logging on is applied, which is what the dependency on the account resource says.
fn stage(name: &str, method_settings: Vec<Value>) -> Value {
    let mut properties = json!({
        "RestApiId": { "Ref": "Api" },
        "DeploymentId": { "Ref": "Deployment" },
        "StageName": name,
    });
    if !method_settings.is_empty() {
        if let Some(members) = properties.as_object_mut() {
            members.insert("MethodSettings".to_string(), Value::Array(method_settings));
        }
    }
    json!({
        "Type": "AWS::ApiGateway::Stage",
        "DependsOn": ["Account", "Deployment"],
        "Properties": properties,
    })
}

/// One `MethodSettings` entry raising the method it names to [`INFO`].
fn traced_method(resource_path: &str, http_method: &str, data_trace: bool) -> Value {
    json!({
        "ResourcePath": resource_path,
        "HttpMethod": http_method,
        "LoggingLevel": INFO,
        "DataTraceEnabled": data_trace,
    })
}

/// The one stack the table is served by: the role API Gateway writes this account's logs
/// under, the account setting naming it, and an api whose stages each state the logging level
/// in a different place.
fn template() -> String {
    json!({
        "Resources": {
            "CwRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(APIGATEWAY_PRINCIPAL),
                    "ManagedPolicyArns": [PUSH_TO_CLOUDWATCH_LOGS],
                },
            },
            "Account": {
                "Type": "AWS::ApiGateway::Account",
                "Properties": { "CloudWatchRoleArn": { "Fn::GetAtt": ["CwRole", "Arn"] } },
            },
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": API_NAME,
                    "EndpointConfiguration": { "Types": ["REGIONAL"] },
                },
            },
            "Ping": resource(json!({ "Fn::GetAtt": ["Api", "RootResourceId"] }), "ping"),
            "Items": resource(json!({ "Fn::GetAtt": ["Api", "RootResourceId"] }), "items"),
            "ItemId": resource(json!({ "Ref": "Items" }), "{id}"),
            "PingMethod": mock_method("Ping", PING_VERB, "ping"),
            "ItemMethod": mock_method("ItemId", ITEM_VERB, "item"),
            // The deployment carries a stage of its own, whose logging level its
            // `StageDescription` states beside the stage rather than per method — the
            // stage-wide setting of that declaration.
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": ["PingMethod", "ItemMethod", "Account"],
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "StageName": DESCRIBED_STAGE,
                    "StageDescription": { "LoggingLevel": INFO },
                },
            },
            "MethodStage": stage(
                PER_METHOD_STAGE,
                vec![traced_method(PING_PATH, PING_VERB, true)],
            ),
            "WideStage": stage(WIDE_STAGE, vec![traced_method("/*", "*", false)]),
            "PatchedStage": stage(PATCHED_STAGE, Vec::new()),
            "QuietStage": stage(QUIET_STAGE, Vec::new()),
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "RoleName": { "Value": { "Ref": "CwRole" } },
            "RoleArn": { "Value": { "Fn::GetAtt": ["CwRole", "Arn"] } },
        },
    })
    .to_string()
}

/// One row: a stage of the api, the request that reaches one of its methods, and whether the
/// settings in force for that method turn execution logging on.
struct Row {
    case: &'static str,
    stage: &'static str,
    verb: &'static str,
    /// The path within the stage the client addresses.
    request_path: &'static str,
    /// The resource path of the method that serves it, which the trace names.
    resource_path: &'static str,
    /// The body that method answers.
    label: &'static str,
    /// The `methodSettings` key the level in force is held under: the method path the patch
    /// POINTER the stack's provisioning sends spelled, which states the resource path as the
    /// path NAME `MethodSetting$ResourcePath` documents ("the initial slash must include a
    /// forward slash", every separator inside it escaped `~1`) — and the whole-stage path
    /// `/*/*` keyed `*/*`.
    settings_key: &'static str,
    /// Whether the level in force traces the request at all.
    traced: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the stack's Stage MethodSettings entry for that one method",
        stage: PER_METHOD_STAGE,
        verb: PING_VERB,
        request_path: "/ping",
        resource_path: PING_PATH,
        label: "ping",
        settings_key: "~1ping/GET",
        traced: true,
    },
    Row {
        case: "the stack's stage-wide Stage MethodSettings entry, on a parameterised resource",
        stage: WIDE_STAGE,
        verb: ITEM_VERB,
        request_path: "/items/7",
        resource_path: ITEM_PATH,
        label: "item",
        settings_key: "*/*",
        traced: true,
    },
    Row {
        case: "the StageDescription of the stack's Deployment",
        stage: DESCRIBED_STAGE,
        verb: PING_VERB,
        request_path: "/ping",
        resource_path: PING_PATH,
        label: "ping",
        settings_key: "*/*",
        traced: true,
    },
    Row {
        case: "a direct UpdateStage patch made after the stack",
        stage: PATCHED_STAGE,
        verb: ITEM_VERB,
        request_path: "/items/42",
        resource_path: ITEM_PATH,
        label: "item",
        settings_key: "*/*",
        traced: true,
    },
    Row {
        case: "a stage nothing turned execution logging on for",
        stage: QUIET_STAGE,
        verb: PING_VERB,
        request_path: "/ping",
        resource_path: PING_PATH,
        label: "ping",
        settings_key: "*/*",
        traced: false,
    },
];

/// Drive one request through the stage of a row and answer the id it was served under.
async fn served(router: &MultiServiceRouter, case: &str, api: &str, row: &Row) -> String {
    let answered = invoke(router, api, row.stage, row.verb, row.request_path).await;
    assert_eq!(
        (answered.status, answered.body.as_str()),
        (SERVED_STATUS, served_body(row.label).as_str()),
        "case {case:?}: expected the stage to answer HTTP {SERVED_STATUS} with the body of its \
         {} method, got {} with {:?}",
        row.resource_path,
        answered.status,
        answered.body
    );
    answered
        .request_id
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the answer carried no {REQUEST_ID_HEADER}"))
}

/// A request whose trace HAS been delivered, used as a barrier: a delivery pipeline that has
/// flushed a later request has had its chance at every earlier one. Answers the case text for a
/// message that names it.
async fn delivery_barrier(router: &MultiServiceRouter, api: &str) -> String {
    let case = "the delivery barrier";
    let request_id = served(router, case, api, &ROWS[0]).await;
    let group = execution_group(api, ROWS[0].stage);
    let trace = trace_of(router, &group, &request_id).await;
    assert!(
        !trace.is_empty(),
        "case {case:?}: expected the traced stage to deliver the trace of {request_id:?} into \
         {group:?}, got nothing"
    );
    request_id
}

/// One row under one role in force: the request is served, and the stage's execution log group
/// then holds the trace of exactly that request — or holds nothing, and does not exist at all,
/// for a stage no setting turned execution logging on for.
async fn one_row(router: &MultiServiceRouter, api: &str, row: &Row, installed: &str) {
    let case = format!("{} with {installed}", row.case);
    let case = case.as_str();
    let request_id = served(router, case, api, row).await;
    let group = execution_group(api, row.stage);

    if !row.traced {
        // A later request's delivery is the barrier: once it has landed, this one's would have.
        let barrier = delivery_barrier(router, api).await;
        let reported = held(router, &group).await;
        assert!(
            reported.is_none(),
            "case {case:?}: expected no {group:?} at all for a stage no setting traces, got {:?} \
             after the trace of {barrier:?} had landed",
            reported.map(|held| held.messages)
        );
        return;
    }

    let trace = trace_of(router, &group, &request_id).await;
    let held = held(router, &group)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the delivery to bring {group:?} into \
             existence, got no such group"));
    assert!(
        !held.streams.is_empty(),
        "case {case:?}: expected {group:?} to carry at least one stream, got {:?}",
        held.streams
    );
    // The line that OPENS a request's log, which is what names the request the records
    // following it are about.
    for line in [
        format!("Execution log for request {request_id}"),
        format!(
            "HTTP Method: {}, Resource Path: {}",
            row.verb, row.resource_path
        ),
        format!("Method completed with status: {SERVED_STATUS}"),
    ] {
        let expected = about_request(&request_id, &line);
        assert!(
            trace.contains(&expected),
            "case {case:?}: expected the record {expected:?} in this request's trace in {group:?}, \
             got {trace:?}"
        );
    }
}

/// RULE: the account's CloudWatch role is one setting whichever caller writes it, and while it
/// is in force every stage whose method settings turn execution logging on traces the requests
/// it serves into the group API Gateway publishes for that stage.
#[tokio::test]
async fn a_stack_installed_account_role_delivers_a_stages_execution_logs() {
    let router = emulator();

    // 1. Before the stack, the account carries no role: API Gateway has nothing to write the
    //    account's logs as, which is the state the stack's resource exists to change.
    let before = account_role(&router, "before the stack").await;
    assert_eq!(
        before, None,
        "case \"before the stack\": expected the account to carry no CloudWatch role, got \
         {before:?}"
    );

    // 2. The stack, which really makes the calls its resources are.
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let (reported, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "expected the stack to reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );
    let outputs = outputs_of(&described);
    let output = |name: &str| {
        outputs
            .get(name)
            .cloned()
            .unwrap_or_else(|| panic!("the stack published no {name} output in {described}"))
    };
    let api = output("ApiId");
    let role_name = output("RoleName");
    let role_arn = output("RoleArn");

    // 3. The identity behind the setting is really IAM's, and it admits exactly this service:
    //    one Allow statement, whose principal is API Gateway's own.
    let (status, role) = iam(&router, "GetRole", &[("RoleName", role_name.as_str())]).await;
    assert_eq!(
        status, 200,
        "expected IAM to hold the role {role_name:?} the stack created, got {status} {role}"
    );
    assert_eq!(
        element(&role, "Arn"),
        role_arn,
        "expected IAM to report the ARN the stack published, got {role}"
    );
    let trust = ::smithy_server_runtime::percent_decode(&element(
        &role,
        "AssumeRolePolicyDocument",
    ))
    .to_string();
    let document: Value = serde_json::from_str(&trust)
        .unwrap_or_else(|error| panic!("the reported trust policy is not JSON ({error}): {trust}"));
    let statements = document
        .get("Statement")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let admitting: Vec<&Value> = statements
        .iter()
        .filter(|statement| {
            statement.get("Effect").and_then(Value::as_str) == Some("Allow")
                && statement
                    .pointer("/Principal/Service")
                    .and_then(Value::as_str)
                    == Some(APIGATEWAY_PRINCIPAL)
        })
        .collect();
    assert_eq!(
        (statements.len(), admitting.len()),
        (1, 1),
        "expected exactly one Allow statement admitting {APIGATEWAY_PRINCIPAL}, got {statements:?}"
    );

    // 4. The account value the stack's resource wrote is the one API Gateway reports in force:
    //    the same singleton `UpdateAccount` sets.
    let installed = account_role(&router, "the role the stack installed").await;
    assert_eq!(
        installed.as_deref(),
        Some(role_arn.as_str()),
        "expected GetAccount to report the role the stack declared {role_arn:?}, got {installed:?}"
    );

    // 5. The one stage the stack left unconfigured is turned on by API Gateway's own call, which
    //    the account's role is now in force for.
    apigateway_ok(
        &router,
        "UpdateStage on the stage the stack left unconfigured",
        "PATCH",
        &format!("/restapis/{api}/stages/{PATCHED_STAGE}"),
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/*/*/logging/loglevel", "value": INFO },
            ],
        })),
    )
    .await;

    // The configuration each stage HOLDS is what gates its own logging, so it is read back
    // through that service's own reader before a single request is served: a trace can only be
    // about a stage whose settings turn execution logging on for the method it reached.
    for row in ROWS {
        let case = row.case;
        let held = apigateway_ok(
            &router,
            case,
            "GET",
            &format!("/restapis/{api}/stages/{}", row.stage),
            None,
        )
        .await;
        // Indexed rather than addressed by JSON Pointer: a method path is ONE key of the map
        // and carries the separator a pointer would read as a step of its own.
        let level = held
            .get("methodSettings")
            .and_then(|settings| settings.get(row.settings_key))
            .and_then(|setting| setting.get("loggingLevel"))
            .and_then(Value::as_str);
        assert_eq!(
            level,
            row.traced.then_some(INFO),
            "case {case:?}: expected the stage to hold the level its declaration states under \
             {:?}, got {held}",
            row.settings_key
        );
    }

    // 6. Every row, under the role the STACK installed.
    for row in ROWS {
        one_row(&router, &api, row, "the role the stack installed").await;
    }

    // 7. The SAME singleton re-pointed by a direct `UpdateAccount` at a role created through
    //    IAM's own call: one setting, either caller, and the table holds unchanged.
    let (status, created) = iam(
        &router,
        "CreateRole",
        &[
            ("RoleName", SECOND_ROLE),
            (
                "AssumeRolePolicyDocument",
                &trust_policy(APIGATEWAY_PRINCIPAL).to_string(),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected IAM to create {SECOND_ROLE}, got {status} {created}"
    );
    let second_arn = element(&created, "Arn");
    update_account_role(&router, "re-pointing the account's role", &second_arn).await;
    let repointed = account_role(&router, "the re-pointed role").await;
    assert_eq!(
        repointed.as_deref(),
        Some(second_arn.as_str()),
        "expected GetAccount to report the role UpdateAccount set {second_arn:?}, got {repointed:?}"
    );
    for row in ROWS {
        one_row(
            &router,
            &api,
            row,
            "the role UpdateAccount re-pointed the singleton at",
        )
        .await;
    }

    // 8. The setting CLEARED: every stage keeps its own configuration, and nothing is delivered
    //    for any of them, because there is no role to write as. The barrier is a request served
    //    after the role is back: once ITS trace has landed, the cleared request's would have.
    update_account_role(&router, "clearing the account's role", "").await;
    let cleared = account_role(&router, "the cleared account").await;
    assert_eq!(
        cleared, None,
        "expected GetAccount to report no role once the setting was cleared, got {cleared:?}"
    );
    let case = "a request served while the account carries no role";
    let untraced = served(&router, case, &api, &ROWS[0]).await;
    update_account_role(&router, "restoring the account's role", &second_arn).await;
    let barrier = delivery_barrier(&router, &api).await;
    let group = execution_group(&api, ROWS[0].stage);
    let trace = trace_now(&router, &group, &untraced).await;
    assert!(
        trace.is_empty(),
        "case {case:?}: expected no record about {untraced:?} in {group:?} once the trace of \
         {barrier:?} had landed, got {trace:?}"
    );
}
