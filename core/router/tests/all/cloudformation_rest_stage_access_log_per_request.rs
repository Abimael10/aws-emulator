// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation-deployed REST stage carrying access logging leaves behind for EACH
//! request it served, when the requests are identical to one another.
//!
//! The rule under test: **a REST stage whose `accessLogSettings` name a log-group
//! `destinationArn` and a `format` writes exactly ONE access-log event per request it
//! answered into that group — one per request and no fewer, so N identical requests leave N
//! records — and each record is that stage's `format` with every `$context` reference
//! resolved against the request it is about, so the record naming a request carries the very
//! id API Gateway reported that request under, which is also the id the backend invocation
//! behind it saw. A repeat of a request is a request of its own: nothing about the stage
//! collapses, dedupes or caches away the record of the second identical call.**
//!
//! Four crates have to agree for that to hold — CloudFormation makes the calls that create
//! the group, the push role, the account setting, the function and the api; API Gateway
//! serves the invoke and delivers the record; AWS Lambda runs the proxy integration behind
//! it; CloudWatch Logs holds the destination and answers the read — so the suite lives here
//! and drives the emulator's own assembled service set.
//!
//! The rows differ along what the rule generalises over: HOW MANY identical requests are
//! issued before the destination is read (one, two, then four), each row's requests being
//! byte-for-byte the same call as the row before it, and the records are counted
//! CUMULATIVELY, so a stage that stopped logging after the first request, logged only the
//! last of a burst, or wrote a record twice fails here.
//!
//! The sibling `cloudformation_rest_api_stage_integration_classes` drives one request per
//! integration CLASS and pins what one record contains;
//! `apigateway_stage_access_log_destination_delivery` pins which destinations a record is
//! delivered to. Neither drives the SAME request repeatedly, which is what this suite adds.
//!
//! Every expected value is computed from what the template declared and from the ids the
//! answers reported, never out of the records being checked, and every wait is on the
//! CONDITION, looked at again until it holds.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack the whole table is served by.
const STACK: &str = "cfn-stage-access-log-per-request";

/// The stage the stack deploys its api to.
const STAGE: &str = "prod";

/// The path part of the api's one resource, and the path within the stage every request below
/// addresses.
const PATH_PART: &str = "ping";

/// The resource path of the matched method, as `$context.resourcePath` reports it.
const RESOURCE_PATH: &str = "/ping";

/// The verb every request carries, and the status the stage answers it with.
const VERB: &str = "GET";
const SERVED_STATUS: u16 = 200;

/// The log group the stack declares as the stage's access-log destination.
const LOG_GROUP: &str = "/aws/apigateway/cfn-stage-access-log-per-request";

/// The name the template declares the api under.
const API_NAME: &str = "cfn-stage-access-log-per-request-api";

/// The access-log format the stage is declared with. `AccessLogSettings$format` requires that
/// a format name a request identifier, and `$context.requestId` is one; the record is rendered
/// as a JSON object so the assertions read its fields rather than parsing a line.
const ACCESS_LOG_FORMAT: &str = concat!(
    r#"{"requestId":"$context.requestId","httpMethod":"$context.httpMethod","#,
    r#""resourcePath":"$context.resourcePath","status":"$context.status","#,
    r#""stage":"$context.stage"}"#
);

/// The error code the emulator's front door answers a request it can attribute to no service
/// with, which an invoke of a deployed stage is never answered with.
const ROUTER_REFUSAL: &str = "UnknownService";

/// The header API Gateway reports the id of the request it answered in, which is the id a
/// record's `$context.requestId` renders.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How long to wait between looks at a condition another service owns, and the generous
/// budget one is given before the suite calls it a failure. Nothing here asserts on how long
/// anything took.
const POLL: Duration = Duration::from_millis(25);

/// See [`POLL`].
const BUDGET: Duration = Duration::from_secs(90);

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// api CloudFormation creates is the api this suite invokes, the function behind its proxy
/// integration is the one AWS Lambda runs, and the destination its records land in is
/// CloudWatch Logs'.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// What one call answered: the status, the request id it was reported under, and the body.
#[derive(Debug)]
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
        .uri(path)
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

/// Call one CloudFormation operation over its awsQuery wire.
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

/// One API Gateway control-plane read, over that service's `restJson1` wire.
async fn apigateway(router: &MultiServiceRouter, path: &str) -> Answer {
    dispatch(
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
    .await
}

/// One AWS Lambda `restJson1` read.
async fn lambda(router: &MultiServiceRouter, path: &str) -> Value {
    dispatch(
        router,
        "GET",
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::new(),
    )
    .await
    .json()
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

/// Invoke the deployed stage at `url`, exactly as a client reaches one: the URL is taken apart
/// rather than rebuilt, and the request carries no credential at all, as a client of a method
/// whose `authorizationType` is `NONE` sends none.
async fn invoke(router: &MultiServiceRouter, url: &str) -> Answer {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, path) = match without_scheme.split_once('/') {
        Some((host, path)) => (host.to_string(), format!("/{path}")),
        None => (without_scheme.to_string(), "/".to_string()),
    };
    dispatch(
        router,
        VERB,
        &host,
        &path,
        &[("content-type", "application/json")],
        Bytes::new(),
    )
    .await
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`.
///
/// The span a function spends `Pending` is that service's own, so no fixed sleep names the
/// instant it ends; an invoke of the stage before then is an invoke of a function Lambda is
/// still bringing up. The wait is on that CONDITION, looked at again until it holds.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    let mut last = String::new();
    while Instant::now() < deadline {
        let configuration =
            lambda(router, &format!("/2015-03-31/functions/{name}/configuration")).await;
        last = configuration
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if last == "Active" {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    last
}

/// The handler the stack's function runs: a Lambda proxy integration's own answer shape,
/// carrying the id the event said API Gateway is serving this request under — so the answer
/// proves the integration was reached for THIS request and the record can be correlated with
/// the invocation behind it.
const PROXY_HANDLER: &str = r#"
import json


def handler(event, context):
    request = event.get("requestContext") or {}
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "requestId": request.get("requestId"),
                "path": event.get("path"),
            }
        ),
    }
"#;

/// A trust policy granting `sts:AssumeRole` to one service principal.
fn trust(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole"
        }]
    })
}

/// One inline policy allowing `actions` on `resource`.
fn allow(name: &str, actions: Value, resource: Value) -> Value {
    json!({
        "PolicyName": name,
        "PolicyDocument": {
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }]
        }
    })
}

/// The one stack this suite is served by: the destination group, the role API Gateway writes
/// the account's logs under and the account setting naming it, the function behind the
/// method's proxy integration, and the api whose stage declares the access logging.
fn template() -> String {
    json!({
        "Resources": {
            // The destination of the stage's records, which is a group of the account's and so
            // has to exist before a record can land in it.
            "AccessLogs": {
                "Type": "AWS::Logs::LogGroup",
                "Properties": { "LogGroupName": LOG_GROUP }
            },
            // The role API Gateway writes this account's logs under, and the account setting
            // naming it: a stage whose logging is on while the account carries no role writes
            // nothing.
            "LogPushRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust("apigateway.amazonaws.com"),
                    "Policies": [allow(
                        "push-access-logs",
                        json!(["logs:CreateLogStream", "logs:PutLogEvents"]),
                        json!({ "Fn::GetAtt": ["AccessLogs", "Arn"] }),
                    )]
                }
            },
            "Account": {
                "Type": "AWS::ApiGateway::Account",
                "Properties": { "CloudWatchRoleArn": { "Fn::GetAtt": ["LogPushRole", "Arn"] } }
            },
            "ExecutionRole": {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": trust("lambda.amazonaws.com") }
            },
            "Backend": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": 30,
                    "Role": { "Fn::GetAtt": ["ExecutionRole", "Arn"] },
                    "Code": { "ZipFile": PROXY_HANDLER }
                }
            },
            "BackendPermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": { "Fn::GetAtt": ["Backend", "Arn"] },
                    "Principal": "apigateway.amazonaws.com"
                }
            },
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": API_NAME,
                    "EndpointConfiguration": { "Types": ["REGIONAL"] }
                }
            },
            "Ping": {
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                    "PathPart": PATH_PART
                }
            },
            "PingMethod": {
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ResourceId": { "Ref": "Ping" },
                    "HttpMethod": VERB,
                    "AuthorizationType": "NONE",
                    "MethodResponses": [{ "StatusCode": "200" }],
                    "Integration": {
                        "Type": "AWS_PROXY",
                        "IntegrationHttpMethod": "POST",
                        "Uri": {
                            "Fn::Sub": [
                                "arn:aws:apigateway:${AWS::Region}:lambda:path/2015-03-31/functions/${Target}/invocations",
                                { "Target": { "Fn::GetAtt": ["Backend", "Arn"] } }
                            ]
                        }
                    }
                }
            },
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": ["PingMethod"],
                "Properties": { "RestApiId": { "Ref": "Api" } }
            },
            "Stage": {
                "Type": "AWS::ApiGateway::Stage",
                // The account's role has to be in force before the stage's logging is set.
                "DependsOn": ["Account"],
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "DeploymentId": { "Ref": "Deployment" },
                    "StageName": STAGE,
                    "AccessLogSetting": {
                        "DestinationArn": { "Fn::GetAtt": ["AccessLogs", "Arn"] },
                        "Format": ACCESS_LOG_FORMAT
                    }
                }
            }
        },
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "InvokeUrl": {
                "Value": {
                    "Fn::Sub": format!(
                        "https://${{Api}}.execute-api.${{AWS::Region}}.amazonaws.com/{STAGE}"
                    )
                }
            },
            "LogGroupName": { "Value": { "Ref": "AccessLogs" } }
        }
    })
    .to_string()
}

/// One row: how many identical requests are issued before the destination is read again.
struct Row {
    case: &'static str,
    requests: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "the first request the stage serves",
        requests: 1,
    },
    Row {
        case: "two more requests identical to it",
        requests: 2,
    },
    Row {
        case: "a burst of four more identical requests",
        requests: 4,
    },
];

/// RULE: every request a stage carrying `accessLogSettings` answers leaves exactly one record
/// in the group its `destinationArn` names, rendered from the stage's `format` against that
/// request — so N identical requests leave N records, one per request id.
#[tokio::test]
async fn each_request_a_cfn_deployed_stage_serves_leaves_one_access_log_record() {
    let router = build_router();
    let template = template();

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
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
    let api_id = output("ApiId");
    let invoke_url = output("InvokeUrl");
    let group = output("LogGroupName");
    assert_eq!(
        group, LOG_GROUP,
        "expected the stack to publish the declared log group name, got {group:?}"
    );

    // The logging configuration the template declared is the configuration API Gateway HOLDS
    // for the stage: a record can only be about a stage whose own settings name the
    // destination and the format.
    let stage = apigateway(&router, &format!("/restapis/{api_id}/stages/{STAGE}")).await;
    assert_eq!(
        stage.status, 200,
        "expected API Gateway to hold the stage the stack deployed, got HTTP {} with body {:?}",
        stage.status, stage.body
    );
    let held = stage.json();
    assert_eq!(
        held["accessLogSettings"]["format"].as_str(),
        Some(ACCESS_LOG_FORMAT),
        "expected the stage to carry the declared access-log format, got {:?}",
        stage.body
    );
    assert_eq!(
        held["accessLogSettings"]["destinationArn"]
            .as_str()
            .map(|arn| arn.contains(LOG_GROUP)),
        Some(true),
        "expected the stage's access-log destination to name the declared group {LOG_GROUP}, got \
         {:?}",
        stage.body
    );

    // The function behind the method is up before any invoke: a function Lambda is still
    // bringing up is not what this suite is about.
    let (_, stack_resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", STACK)]).await;
    let physical: BTreeMap<String, String> = physical_ids_of(&stack_resources);
    let function = physical
        .get("Backend")
        .cloned()
        .unwrap_or_else(|| panic!("no physical id for Backend in {stack_resources}"));
    let state = wait_until_active(&router, &function).await;
    assert_eq!(
        state, "Active",
        "expected Lambda to report {function} Active before the invokes, got {state:?}"
    );

    let url = format!("{invoke_url}/{PATH_PART}");
    // Every request id the stage reported, in the order the requests were made: the records
    // are counted against this list, cumulatively, after each row.
    let mut served: Vec<String> = Vec::new();
    for row in ROWS {
        let case = row.case;
        for _ in 0..row.requests {
            let answered = invoke(&router, &url).await;
            assert!(
                !answered.body.contains(ROUTER_REFUSAL),
                "case {case:?}: expected an answer from the api's own data plane, got the front \
                 door's refusal {:?}",
                answered.body
            );
            assert_eq!(
                answered.status, SERVED_STATUS,
                "case {case:?}: expected HTTP {SERVED_STATUS} from the stage, got {} with body \
                 {:?}",
                answered.status, answered.body
            );
            let request_id = answered.request_id.clone().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the answer to carry a {REQUEST_ID_HEADER} header, \
                     got {answered:?}"
                )
            });
            // The id the BACKEND saw for this request is the id the answer reported, which is
            // what makes a record correlatable with the invocation behind it.
            assert_eq!(
                answered.json().get("requestId").and_then(Value::as_str),
                Some(request_id.as_str()),
                "case {case:?}: expected the proxy integration to have seen the request id \
                 {request_id:?}, got {:?}",
                answered.body
            );
            assert!(
                !served.contains(&request_id),
                "case {case:?}: expected each request to be reported under an id of its own, got \
                 {request_id:?} twice in {served:?}"
            );
            served.push(request_id);
        }

        // The destination holds one record per request made SO FAR. The delivery is another
        // service's write, so the CONDITION is looked at again until it holds.
        let delivered = delivered_records(&router, served.len()).await;
        assert_eq!(
            delivered.len(),
            served.len(),
            "case {case:?}: expected {} record(s) in {LOG_GROUP} after {} request(s), got \
             {delivered:?}",
            served.len(),
            served.len()
        );
        for request_id in &served {
            let about: Vec<&String> = delivered
                .iter()
                .filter(|record| {
                    serde_json::from_str::<Value>(record)
                        .ok()
                        .as_ref()
                        .and_then(|parsed| parsed.get("requestId").and_then(Value::as_str))
                        == Some(request_id.as_str())
                })
                .collect();
            assert_eq!(
                about.len(),
                1,
                "case {case:?}: expected exactly one record about request {request_id:?} in \
                 {LOG_GROUP}, got {about:?} among {delivered:?}"
            );
            let record: Value = serde_json::from_str(about[0]).unwrap_or(Value::Null);
            let field = |name: &str| record.get(name).and_then(Value::as_str).map(str::to_owned);
            assert_eq!(
                field("httpMethod").as_deref(),
                Some(VERB),
                "case {case:?}: expected the record about {request_id:?} to name the verb the \
                 client sent, got {:?}",
                about[0]
            );
            assert_eq!(
                field("resourcePath").as_deref(),
                Some(RESOURCE_PATH),
                "case {case:?}: expected the record about {request_id:?} to name the matched \
                 resource path, got {:?}",
                about[0]
            );
            assert_eq!(
                field("status").as_deref(),
                Some(SERVED_STATUS.to_string().as_str()),
                "case {case:?}: expected the record about {request_id:?} to name the status the \
                 stage answered with, got {:?}",
                about[0]
            );
            assert_eq!(
                field("stage").as_deref(),
                Some(STAGE),
                "case {case:?}: expected the record about {request_id:?} to name the stage that \
                 answered, got {:?}",
                about[0]
            );
            assert!(
                !about[0].contains("$context."),
                "case {case:?}: expected every reference of the format to be resolved in the \
                 record about {request_id:?}, got {:?}",
                about[0]
            );
        }
    }
}

/// Every record the destination group holds, once it holds at least `wanted` of them — or
/// whatever it holds when the budget runs out, so the assertions above report what was missing
/// rather than this wait doing it.
async fn delivered_records(router: &MultiServiceRouter, wanted: usize) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let answered = logs(
            router,
            "FilterLogEvents",
            json!({ "logGroupName": LOG_GROUP }),
        )
        .await;
        let records: Vec<String> = if answered.status == 200 {
            answered
                .json()
                .get("events")
                .and_then(Value::as_array)
                .map(|events| {
                    events
                        .iter()
                        .filter_map(|event| event.get("message").and_then(Value::as_str))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if records.len() >= wanted || Instant::now() >= deadline {
            return records;
        }
        tokio::time::sleep(POLL).await;
    }
}
