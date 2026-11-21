// SPDX-License-Identifier: CC0-1.0

//! The ACCESS LOGS a REST stage a CloudFormation stack declared delivers into the CloudWatch
//! Logs log group that same stack declared.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > A stack declaring a REST API, a log group and a stage whose `AccessLogSetting` names that
//! > group as its `DestinationArn` beside a `Format` reaches `CREATE_COMPLETE` with the stage
//! > really configured — `GetStage` reports both members — and the stage then SERVES its
//! > deployment's methods at the api's own invoke endpoint, leaving for every request it
//! > answers exactly ONE log event in the log group that ARN names: that format's `$context`
//! > references rendered against the request the stage answered, a reference the call has no
//! > value for rendered as the single placeholder character, in a stream of that group its own
//! > readers report. A destination is a log group of the ACCOUNT's, so a stage pointed at a
//! > name CloudWatch Logs holds no group under delivers nothing and brings no group into
//! > existence, and a stage carrying no `AccessLogSetting` delivers nothing at all. Each
//! > stage's records land in the group its OWN destination names and in no other.
//!
//! Four services have to agree for that to hold — CloudFormation makes the calls the template's
//! resources are, IAM holds the role the account's logs are written under and decides whether
//! API Gateway may assume it, API Gateway holds the api whose stage both carries the
//! configuration and serves the request, and CloudWatch Logs holds the group, the stream and
//! the event — so the suite lives here with all four mounted on ONE engine, as
//! `core/router/src/lib.rs` mounts them. Every delivery is read back through CloudWatch Logs'
//! own readers (`FilterLogEvents`, then `DescribeLogStreams` for the stream each event names),
//! so a record that never reached that store cannot pass, and every call is made exactly as a
//! client makes it: the control-plane calls over each service's own wire, and the invoke at the
//! api's published `execute-api` host carrying no credential at all.
//!
//! The rows differ along what the rule generalises over: the FORMAT a stage renders its records
//! by (a space-separated line, a JSON object, and a Common-Log-shaped line naming a variable
//! the call has no value for), the published ARN FORM its destination is named by (the group
//! with everything under it, which `Fn::GetAtt … Arn` answers for an `AWS::Logs::LogGroup`, and
//! the group itself), the SHAPE of the group's name (a path and a flat name), the resource path
//! and verb of the method the request reaches (a child resource, and a parameterised resource
//! one level deeper under another verb), and HOW MANY requests are served under one
//! configuration. Two rows carry no delivery at all: a destination naming a group the account
//! holds none of, and a stage declared with no `AccessLogSetting`.
//!
//! Every method is a `MOCK` integration — "for testing without actually invoking the backend"
//! (`IntegrationType`) — so what a row shows is the stage's own logging and never a backend's
//! doing. Every wait is on the CONDITION, looked at again until it holds or a generous budget
//! passes; nothing asserts on how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every call is signed for, every invoke host carries and every ARN names.
const REGION: &str = "us-east-1";

/// The one stack the table is served by, and the name it declares its api under.
const STACK: &str = "apigw-declared-stage-access-logs";
const API_NAME: &str = "apigw-declared-stage-access-logs-api";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The capability a stack declaring an IAM resource is created with.
const CAPABILITY_IAM: &str = "CAPABILITY_IAM";

/// The service principal the account's CloudWatch role has to trust: API Gateway writes the
/// account's logs under that role, so the assume is granted to that service.
const APIGATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// The AWS-managed policy the documented "API Gateway may push logs to CloudWatch" setup
/// attaches to that role (AWS Managed Policy Reference, filed under `/service-role/`).
const PUSH_TO_CLOUDWATCH_LOGS: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonAPIGatewayPushToCloudWatchLogs";

/// The two methods of the api, by the resource path each sits on and the verb it answers.
const PING_PATH: &str = "/ping";
const PING_VERB: &str = "GET";
const ITEM_PATH: &str = "/items/{id}";
const ITEM_VERB: &str = "POST";

/// The status every method answers with.
const SERVED_STATUS: u16 = 200;

/// The header API Gateway names the request a response answers in, lowercased as the wire
/// carries it: `$context.requestId` is that same id, which is what ties a record to its request.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The stages the stack deploys the api to, one per row of the table.
const LINE_STAGE: &str = "line";
const JSON_STAGE: &str = "json";
const CLF_STAGE: &str = "clf";
const ABSENT_STAGE: &str = "absent";
const QUIET_STAGE: &str = "quiet";

/// The log groups the stack declares, in the two shapes a group's name takes: a path, and a
/// flat name.
const LINE_GROUP: &str = "/aws/apigateway/declared-access-logs/line";
const JSON_GROUP: &str = "apigw-declared-access-logs-json";
const CLF_GROUP: &str = "/aws/apigateway/declared-access-logs/clf";
const QUIET_GROUP: &str = "apigw-declared-access-logs-quiet";

/// The name one row's destination carries that NO resource of the stack declares, so CloudWatch
/// Logs holds no group under it.
const ABSENT_GROUP: &str = "/aws/apigateway/declared-access-logs/absent";

/// What a `$context` reference the call has no value for renders as: AWS writes an access-log
/// field it has no value for as the Common Log Format's own single-hyphen placeholder, so a
/// line keeps its field count whatever the call carried.
const UNRESOLVED: &str = "-";

/// "The request protocol, for example, HTTP/1.1" (`$context.protocol`): every request this data
/// plane is handed arrives over the emulator's own HTTP/1.1 front door.
const PROTOCOL: &str = "HTTP/1.1";

/// The error CloudWatch Logs answers a reader of a group it holds none of with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// How long to wait between looks at a condition another service owns, and the generous budget
/// one is given before the suite calls it a failure. The record is owed from the instant the
/// stage answered, which the answer already establishes, so the budget is a BOUND rather than a
/// span any passing run waits out.
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

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
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
    (answered.status, answered.body)
}

/// One API Gateway control-plane call that has to be answered with HTTP 200, over that
/// service's `restJson1` wire.
async fn apigateway_ok(router: &MultiServiceRouter, case: &str, path: &str) -> Value {
    let answered = dispatch(
        router,
        "GET",
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from GET {path}, got {} with {:?}",
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

/// The `(message, logStreamName)` of every event `FilterLogEvents` reports for `group`, or
/// `None` where CloudWatch Logs holds no group of that name at all.
async fn events_of(router: &MultiServiceRouter, group: &str) -> Option<Vec<(String, String)>> {
    let answered = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
    if answered.status != 200 {
        let reported = answered.json();
        let kind = reported
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            kind.contains(RESOURCE_NOT_FOUND),
            "FilterLogEvents({group:?}): expected either a page of events or \
             {RESOURCE_NOT_FOUND}, got {} with {reported}",
            answered.status
        );
        return None;
    }
    Some(
        answered
            .json()
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .map(|event| {
                        (
                            event
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            event
                                .get("logStreamName")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Every event `group` holds that NAMES `request_id`, right now. Every format of the table
/// renders that id — `AccessLogSettings$format` requires a format to select it — so it is what
/// separates one request's record from another's in a group one stage keeps writing into.
async fn records_now(
    router: &MultiServiceRouter,
    group: &str,
    request_id: &str,
) -> Vec<(String, String)> {
    events_of(router, group)
        .await
        .map(|events| {
            events
                .into_iter()
                .filter(|(message, _)| message.contains(request_id))
                .collect()
        })
        .unwrap_or_default()
}

/// The events `group` holds about `request_id` once it holds any — or whatever it holds when the
/// budget runs out, so the assertions report what was missing rather than this wait doing it.
/// The delivery is another service's write, so the CONDITION is looked at again until it holds.
async fn records_of(
    router: &MultiServiceRouter,
    group: &str,
    request_id: &str,
) -> Vec<(String, String)> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let records = records_now(router, group, request_id).await;
        if !records.is_empty() || Instant::now() >= deadline {
            return records;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The streams `DescribeLogStreams` reports for `group`, which is what says a stream an event
/// names is really a stream of that group.
async fn streams_of(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let answered = logs(
        router,
        "DescribeLogStreams",
        json!({ "logGroupName": group }),
    )
    .await;
    if answered.status != 200 {
        return Vec::new();
    }
    answered
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
        .unwrap_or_default()
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

/// One `AWS::Logs::LogGroup` of the stack, declared under the name a row's destination names.
fn log_group(name: &str) -> Value {
    json!({
        "Type": "AWS::Logs::LogGroup",
        "Properties": { "LogGroupName": name },
    })
}

/// The destination ARN of a group named by its OWN `Fn::GetAtt … Arn`, which for an
/// `AWS::Logs::LogGroup` is the group with everything under it (the `:*` form the type's own
/// return value documents).
fn group_arn_of(logical_id: &str) -> Value {
    json!({ "Fn::GetAtt": [logical_id, "Arn"] })
}

/// The destination ARN naming the GROUP itself, built by the template out of the account and
/// Region it is deployed into — the other form `accessLogSettings` takes.
fn bare_group_arn(name: &str) -> Value {
    json!({
        "Fn::Sub": format!("arn:aws:logs:${{AWS::Region}}:${{AWS::AccountId}}:log-group:{name}"),
    })
}

/// One `AWS::ApiGateway::Stage` of the api carrying `AccessLogSetting`. The account's role has
/// to be in force before a stage that logs is applied, which is what the dependency on the
/// account resource says; the group has to exist before a request is served, which is what the
/// dependency on it says.
fn logging_stage(name: &str, destination: Value, format: &str, depends_on: &[&str]) -> Value {
    let mut dependencies = vec![json!("Account"), json!("Deployment")];
    dependencies.extend(depends_on.iter().map(|logical_id| json!(logical_id)));
    json!({
        "Type": "AWS::ApiGateway::Stage",
        "DependsOn": dependencies,
        "Properties": {
            "RestApiId": { "Ref": "Api" },
            "DeploymentId": { "Ref": "Deployment" },
            "StageName": name,
            "AccessLogSetting": { "DestinationArn": destination, "Format": format },
        },
    })
}

/// One `AWS::ApiGateway::Stage` carrying no `AccessLogSetting` at all.
fn quiet_stage(name: &str) -> Value {
    json!({
        "Type": "AWS::ApiGateway::Stage",
        "DependsOn": ["Account", "Deployment"],
        "Properties": {
            "RestApiId": { "Ref": "Api" },
            "DeploymentId": { "Ref": "Deployment" },
            "StageName": name,
        },
    })
}

/// The one stack the table is served by: the role API Gateway writes this account's logs under,
/// the account setting naming it, the log groups the destinations name, and an api deployed to
/// one stage per row.
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
            "LineGroup": log_group(LINE_GROUP),
            "JsonGroup": log_group(JSON_GROUP),
            "ClfGroup": log_group(CLF_GROUP),
            "QuietGroup": log_group(QUIET_GROUP),
            // A deployment is the snapshot taken when it is created, so the methods have to
            // exist before that snapshot is taken to be in it.
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": ["PingMethod", "ItemMethod"],
                "Properties": { "RestApiId": { "Ref": "Api" } },
            },
            "LineStage": logging_stage(
                LINE_STAGE,
                group_arn_of("LineGroup"),
                Format::Line.declared(),
                &["LineGroup"],
            ),
            "JsonStage": logging_stage(
                JSON_STAGE,
                bare_group_arn(JSON_GROUP),
                Format::Json.declared(),
                &["JsonGroup"],
            ),
            "ClfStage": logging_stage(
                CLF_STAGE,
                group_arn_of("ClfGroup"),
                Format::Clf.declared(),
                &["ClfGroup"],
            ),
            "AbsentStage": logging_stage(
                ABSENT_STAGE,
                bare_group_arn(ABSENT_GROUP),
                Format::Line.declared(),
                &[],
            ),
            "QuietStage": quiet_stage(QUIET_STAGE),
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            // Each destination as the STACK publishes it, which is what a client deploying this
            // template reads — never a value taken out of the answer being checked.
            "LineDestination": { "Value": group_arn_of("LineGroup") },
            "JsonDestination": { "Value": bare_group_arn(JSON_GROUP) },
            "ClfDestination": { "Value": group_arn_of("ClfGroup") },
            "AbsentDestination": { "Value": bare_group_arn(ABSENT_GROUP) },
        },
    })
    .to_string()
}

/// What one request the stage answered renders a format against: the values of the call and of
/// the answer a `$context` reference of the formats below reads.
struct Call<'a> {
    api: &'a str,
    stage: &'a str,
    request_id: &'a str,
    verb: &'a str,
    /// The resource path of the METHOD that served it, which `$context.resourcePath` reports —
    /// a parameterised resource's own template rather than the path asked for.
    resource_path: &'a str,
    /// The path the client addressed within the stage, which `$context.path` reports
    /// stage-qualified.
    request_path: &'a str,
    /// The body the method answered, whose length `$context.responseLength` reports.
    body: &'a str,
}

impl Call<'_> {
    /// `$context.path`: "for a non-proxy request URL of
    /// `https://{rest-api-id}.execute-api.{region}.amazonaws.com/{stage-name}/root/child`, the
    /// `$context.path` value is `/{stage-name}/root/child`".
    fn path(&self) -> String {
        format!("/{}{}", self.stage, self.request_path)
    }
}

/// What a row's stage renders its records by.
#[derive(Clone, Copy)]
enum Format {
    /// A space-separated line of the id, the verb, the method's resource path and the status.
    Line,
    /// A JSON object, the shape a client that parses its access logs configures.
    Json,
    /// A Common-Log-shaped line, which names `$context.identity.user` — a member API Gateway
    /// has a value for only where the method's authorization authenticated a principal, so a
    /// public method's record renders the placeholder there.
    Clf,
}

impl Format {
    /// The format string the template declares, as `AccessLogSettings$format` takes it. Every
    /// one selects `$context.requestId`: "the format must include at least
    /// $context.requestId".
    fn declared(self) -> &'static str {
        match self {
            Self::Line => "$context.requestId $context.httpMethod $context.resourcePath \
                           $context.status",
            Self::Json => {
                r#"{"requestId":"$context.requestId","apiId":"$context.apiId","stage":"$context.stage","path":"$context.path","status":"$context.status"}"#
            }
            Self::Clf => {
                r#"$context.requestId $context.identity.user "$context.httpMethod $context.path $context.protocol" $context.status $context.responseLength"#
            }
        }
    }

    /// The ONE record that format renders for `call` — computed from what the stack declared and
    /// what the stage answered, never read out of the group being checked.
    fn rendered(self, call: &Call<'_>) -> String {
        match self {
            Self::Line => format!(
                "{} {} {} {SERVED_STATUS}",
                call.request_id, call.verb, call.resource_path
            ),
            Self::Json => format!(
                r#"{{"requestId":"{}","apiId":"{}","stage":"{}","path":"{}","status":"{SERVED_STATUS}"}}"#,
                call.request_id,
                call.api,
                call.stage,
                call.path()
            ),
            Self::Clf => format!(
                "{} {UNRESOLVED} \"{} {} {PROTOCOL}\" {SERVED_STATUS} {}",
                call.request_id,
                call.verb,
                call.path(),
                call.body.len()
            ),
        }
    }
}

/// One row: a stage of the api the stack deployed, the request that reaches one of its methods,
/// and where that request's record is owed.
struct Row {
    case: &'static str,
    stage: &'static str,
    verb: &'static str,
    request_path: &'static str,
    resource_path: &'static str,
    /// The body the method it reaches answers.
    label: &'static str,
    /// The log group its destination names.
    group: &'static str,
    /// Whether CloudWatch Logs holds a group of that name at all: a destination is a log group
    /// of the account's, and one row names a group no resource of the stack declares.
    held_group: bool,
    /// The stack output publishing the destination ARN the stage's settings carry, or `None`
    /// for a stage declared with no `AccessLogSetting`.
    destination_output: Option<&'static str>,
    /// The format its settings carry, or `None` for a stage declared with none.
    format: Option<Format>,
    /// How many requests are served under this one configuration.
    requests: usize,
}

impl Row {
    /// Whether this row's requests are owed a record at all.
    fn delivers(&self) -> bool {
        self.format.is_some() && self.held_group
    }
}

const ROWS: &[Row] = &[
    Row {
        case: "a line format, the destination named by the group ARN the stack published, a \
               path-shaped group name, two requests",
        stage: LINE_STAGE,
        verb: PING_VERB,
        request_path: "/ping",
        resource_path: PING_PATH,
        label: "ping",
        group: LINE_GROUP,
        held_group: true,
        destination_output: Some("LineDestination"),
        format: Some(Format::Line),
        requests: 2,
    },
    Row {
        case: "a JSON format, the destination naming the group itself, a flat group name, a \
               parameterised resource under another verb",
        stage: JSON_STAGE,
        verb: ITEM_VERB,
        request_path: "/items/7",
        resource_path: ITEM_PATH,
        label: "item",
        group: JSON_GROUP,
        held_group: true,
        destination_output: Some("JsonDestination"),
        format: Some(Format::Json),
        requests: 1,
    },
    Row {
        case: "a Common-Log-shaped format naming a variable the call has no value for",
        stage: CLF_STAGE,
        verb: PING_VERB,
        request_path: "/ping",
        resource_path: PING_PATH,
        label: "ping",
        group: CLF_GROUP,
        held_group: true,
        destination_output: Some("ClfDestination"),
        format: Some(Format::Clf),
        requests: 1,
    },
    Row {
        case: "a destination naming a log group the account holds none of",
        stage: ABSENT_STAGE,
        verb: PING_VERB,
        request_path: "/ping",
        resource_path: PING_PATH,
        label: "ping",
        group: ABSENT_GROUP,
        held_group: false,
        destination_output: Some("AbsentDestination"),
        format: Some(Format::Line),
        requests: 1,
    },
    Row {
        case: "a stage the stack declared with no AccessLogSetting at all",
        stage: QUIET_STAGE,
        verb: ITEM_VERB,
        request_path: "/items/9",
        resource_path: ITEM_PATH,
        label: "item",
        group: QUIET_GROUP,
        held_group: true,
        destination_output: None,
        format: None,
        requests: 1,
    },
];

/// Drive one request through the stage of a row, answering the id it was served under.
async fn served(router: &MultiServiceRouter, case: &str, api: &str, row: &Row) -> String {
    let answered = invoke(router, api, row.stage, row.verb, row.request_path).await;
    assert_eq!(
        (answered.status, answered.body.as_str()),
        (SERVED_STATUS, served_body(row.label).as_str()),
        "case {case:?}: expected the deployed stage to answer HTTP {SERVED_STATUS} with the body \
         of its {} method, got {} with {:?}",
        row.resource_path,
        answered.status,
        answered.body
    );
    answered
        .request_id
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the answer carried no {REQUEST_ID_HEADER}"))
}

/// A request whose record HAS been delivered, used as a barrier: a delivery pipeline that has
/// flushed a later request has had its chance at every earlier one.
async fn delivery_barrier(router: &MultiServiceRouter, api: &str) -> String {
    let case = "the delivery barrier";
    let barrier = &ROWS[0];
    let request_id = served(router, case, api, barrier).await;
    let records = records_of(router, barrier.group, &request_id).await;
    assert!(
        !records.is_empty(),
        "case {case:?}: expected the logging stage to deliver the record of {request_id:?} into \
         {:?}, got nothing",
        barrier.group
    );
    request_id
}

/// The configuration one row's stage HOLDS, read back through API Gateway's own reader before a
/// single request is served: a record can only be about a stage that really carries the
/// settings its declaration stated.
async fn assert_configured(router: &MultiServiceRouter, api: &str, row: &Row, destination: &str) {
    let case = row.case;
    let reported = apigateway_ok(router, case, &format!("/restapis/{api}/stages/{}", row.stage)).await;
    let settings = reported.get("accessLogSettings");
    match row.format {
        None => assert!(
            settings.is_none_or(Value::is_null),
            "case {case:?}: expected the stage to carry no accessLogSettings, got {reported}"
        ),
        Some(format) => {
            let carried = settings.cloned().unwrap_or(Value::Null);
            assert_eq!(
                (
                    carried.get("destinationArn").and_then(Value::as_str),
                    carried.get("format").and_then(Value::as_str),
                ),
                (Some(destination), Some(format.declared())),
                "case {case:?}: expected the stage to carry the destination and format the stack \
                 declared, got {reported}"
            );
        }
    }
}

/// One row: its requests are served, and the group its destination names then holds exactly one
/// record per request — that row's format rendered against that request, in a stream the group's
/// own reader reports — or holds nothing at all for a row owed no delivery.
async fn one_row(router: &MultiServiceRouter, api: &str, row: &Row) -> Vec<String> {
    let case = row.case;
    let mut request_ids = Vec::new();
    for _ in 0..row.requests {
        request_ids.push(served(router, case, api, row).await);
    }

    if !row.delivers() {
        // A later request's delivery is the barrier: once it has landed, this one's would have.
        let barrier = delivery_barrier(router, api).await;
        let held = events_of(router, row.group).await;
        if row.held_group {
            // A group the stack declared stays as the stack left it: empty.
            let events = held.as_ref().map(Vec::len);
            assert_eq!(
                events,
                Some(0),
                "case {case:?}: expected {:?} to hold no event at all once the record of \
                 {barrier:?} had landed, got {held:?}",
                row.group
            );
        } else {
            // A destination is a log group of the account's: one that is not there is not
            // brought into existence by a delivery.
            assert!(
                held.is_none(),
                "case {case:?}: expected CloudWatch Logs to hold no group {:?} at all once the \
                 record of {barrier:?} had landed, got {held:?}",
                row.group
            );
        }
        return request_ids;
    }

    let format = row.format.expect("a delivering row carries a format");
    // The records are awaited first, so the streams read after them are the ones they are in.
    for request_id in &request_ids {
        let _ = records_of(router, row.group, request_id).await;
    }
    let streams = streams_of(router, row.group).await;
    for request_id in &request_ids {
        let records = records_of(router, row.group, request_id).await;
        let expected = format.rendered(&Call {
            api,
            stage: row.stage,
            request_id,
            verb: row.verb,
            resource_path: row.resource_path,
            request_path: row.request_path,
            body: &served_body(row.label),
        });
        assert_eq!(
            records.len(),
            1,
            "case {case:?}: expected exactly one record about {request_id:?} in {:?}, got \
             {records:?}",
            row.group
        );
        let (message, stream) = &records[0];
        assert_eq!(
            message, &expected,
            "case {case:?}: expected the record {expected:?} in {:?}, got {message:?}",
            row.group
        );
        assert!(
            streams.contains(stream),
            "case {case:?}: expected the event's stream {stream:?} to be one DescribeLogStreams \
             reports for {:?}, got {streams:?}",
            row.group
        );
    }
    request_ids
}

/// RULE: a stack that declares a REST stage carrying `AccessLogSetting` beside the log group it
/// names reaches `CREATE_COMPLETE` with the stage configured, serves that stage's deployment at
/// the api's own invoke endpoint, and leaves exactly one rendered record per served request in
/// that group — while a destination naming no group of the account's, and a stage carrying no
/// settings, deliver nothing.
#[tokio::test]
async fn a_stack_declared_stage_serves_and_delivers_its_access_logs() {
    let router = emulator();

    // 1. The stack, which really makes the calls its resources are.
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template()),
            ("Capabilities.member.1", CAPABILITY_IAM),
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

    // 2. Every stage really CARRIES the settings its declaration stated, as API Gateway reports
    //    them, and every group the template declared is really there, as CloudWatch Logs reports
    //    it: a delivery below is then into a group the stack brought into existence.
    for row in ROWS {
        let destination = row.destination_output.map(output).unwrap_or_default();
        assert_configured(&router, &api, row, &destination).await;
        let held = events_of(&router, row.group).await;
        assert_eq!(
            held.is_some(),
            row.held_group,
            "case {:?}: expected CloudWatch Logs to {} a group {:?} before any request, got \
             {held:?}",
            row.case,
            if row.held_group { "hold" } else { "hold no" },
            row.group
        );
    }

    // 3. Every row: the stage serves, and its own destination holds one rendered record per
    //    request it answered.
    let mut delivered: Vec<(&Row, Vec<String>)> = Vec::new();
    for row in ROWS {
        let request_ids = one_row(&router, &api, row).await;
        delivered.push((row, request_ids));
    }

    // 4. Each stage's records land in the group its OWN destination names and in no other: a
    //    sibling stage's group holds nothing about this stage's requests.
    for (row, request_ids) in &delivered {
        for (other, _) in &delivered {
            if other.group == row.group || !other.held_group {
                continue;
            }
            for request_id in request_ids {
                let strays = records_now(&router, other.group, request_id).await;
                assert!(
                    strays.is_empty(),
                    "case {:?}: expected no record about {request_id:?} in another stage's \
                     destination {:?}, got {strays:?}",
                    row.case,
                    other.group
                );
            }
        }
    }
}
