// SPDX-License-Identifier: CC0-1.0

//! Every class of integration a CloudFormation-deployed REST API stage carries is served by
//! that api's own data plane, at the id the stack reports for it.
//!
//! The rule under test: **a REST API a CloudFormation stack declared exists in Amazon API
//! Gateway under the id that service minted — which is what the stack's `Ref` publishes — and
//! the stage the stack deployed carries the methods, integrations and settings the template
//! declared, so a request to that stage's invoke URL is attributed to API Gateway, matched to
//! a method and answered by PERFORMING that method's integration: a performed integration
//! answers its own response (HTTP 200) whatever class it is, and one that cannot be performed
//! answers the api's configuration-error response (HTTP 500,
//! `{"message":"Internal server error"}`, `x-amzn-errortype: InternalServerErrorException`).
//! The emulator's front-door refusal for an address it can attribute to no service at all
//! (`404` `UnknownService`) is never the answer. The stage's declared logging is the logging
//! API Gateway holds, so every request that stage answers is also written as one record into
//! the log group the stack declared as its access-log destination.**
//!
//! Five crates have to agree for that to hold — CloudFormation makes the calls that create the
//! api, its account setting, the table, the log group and the roles; API Gateway serves the
//! invoke and delivers the record; Lambda runs (or refuses to run) the function behind a proxy
//! integration; DynamoDB performs the action a direct service integration names; CloudWatch
//! Logs holds the destination — so the suite lives here rather than in any one of them, and it
//! drives the emulator's own assembled service set.
//!
//! The rows differ along what the rule generalises over: the CLASS of the method's integration
//! and why it can or cannot be performed — a `MOCK` integration the api performs itself, an
//! `AWS_PROXY` integration at a function whose resource policy grants API Gateway the
//! invocation, an `AWS` integration making an Amazon DynamoDB action under a credentials role,
//! an `AWS_PROXY` integration at a function that grants API Gateway nothing, and an
//! `AWS_PROXY` integration whose `uri` names a function that does not exist — and each row is
//! driven over BOTH addressings of the same stage a client on this machine uses: the api's own
//! `execute-api` invoke host, as the stack's own output publishes it, and the emulator's
//! reserved data-plane path.
//!
//! What this suite adds over its siblings: `cloudformation_rest_api_serves_its_stage` pins
//! WHERE the api came from (written out, imported, SAM-synthesised) for one proxy integration,
//! and `cloudformation_stage_invoke_answer_classes` pins WHY a proxy integration at a Lambda
//! is or is not performed. Neither drives a non-proxy service integration or a stage's
//! declared access logging from a TEMPLATE, which is what the two rows and the delivery
//! assertion here are for.
//!
//! Every expected value is computed here from what the template declared — the api id, the
//! invoke URL, the table and the destination group come out of the stack's own outputs, which
//! is what a client deploying the stack reads — never out of the answer being checked.

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
const STACK: &str = "cfn-stage-integration-classes";

/// The stage the stack deploys its api to.
const STAGE: &str = "prod";

/// The table the direct service integration writes into, and the key schema it is declared
/// with.
const TABLE: &str = "cfn-stage-integration-notes";
const TABLE_KEY: &str = "pk";

/// The log group the stack declares as the stage's access-log destination.
const LOG_GROUP: &str = "/aws/apigateway/cfn-stage-integration-classes";

/// The access-log format the stage is declared with. `AccessLogSettings$format` requires that
/// a format name a request identifier, and `$context.requestId` is one; the record is rendered
/// as a JSON object so the assertion reads the fields rather than parsing a line.
const ACCESS_LOG_FORMAT: &str =
    r#"{"requestId":"$context.requestId","status":"$context.status","stage":"$context.stage"}"#;

/// The error code the emulator's front door answers a request it can attribute to no service
/// with, which the rule says a stage invoke is never answered with.
const ROUTER_REFUSAL: &str = "UnknownService";

/// The body API Gateway answers a method whose integration it could not perform with.
const CONFIGURATION_ERROR_BODY: &str = r#"{"message":"Internal server error"}"#;

/// The exception class that answer names, in the header an SDK reads an error code out of.
const INTERNAL_SERVER_ERROR_EXCEPTION: &str = "InternalServerErrorException";

/// See [`INTERNAL_SERVER_ERROR_EXCEPTION`].
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The header API Gateway reports the id of the request it answered in, which is the id a
/// record's `$context.requestId` renders.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How long to wait between looks at a condition another service owns, and the generous
/// budget one is given before the suite calls it a failure. Nothing here asserts on how long
/// anything took: every wait is on the CONDITION, looked at again until it holds.
const POLL: Duration = Duration::from_millis(25);

/// See [`POLL`].
const BUDGET: Duration = Duration::from_secs(90);

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// api CloudFormation creates is the api this suite then invokes, the functions behind its
/// proxy integrations are the ones AWS Lambda runs, the table its service integration writes
/// is Amazon DynamoDB's and the destination its records land in is CloudWatch Logs'.
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

/// What one call answered: the status, the two headers this suite reads, and the body.
#[derive(Debug)]
struct Answer {
    status: u16,
    request_id: Option<String>,
    error_type: Option<String>,
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
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    Answer {
        status: response.status().as_u16(),
        request_id: header(REQUEST_ID_HEADER),
        error_type: header(ERROR_TYPE_HEADER),
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

/// One Amazon DynamoDB call over its own `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
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

/// Invoke a deployed stage at `url`, exactly as a client reaches one: the URL is taken apart
/// rather than rebuilt, so what is driven is the address the caller holds, and the request
/// carries no credential at all, as a client of a method whose `authorizationType` is `NONE`
/// sends none.
async fn invoke(router: &MultiServiceRouter, verb: &str, url: &str, body: &str) -> Answer {
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
        verb,
        &host,
        &path,
        &[("content-type", "application/json")],
        Bytes::from(body.to_string()),
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

/// The handler the stack's proxy functions run: a Lambda proxy integration's own answer shape,
/// carrying what the event said about the request so the response proves the integration was
/// really reached.
const PROXY_HANDLER: &str = r#"
import json


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "path": event.get("path"),
                "stage": (event.get("requestContext") or {}).get("stage"),
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

/// One `AWS::ApiGateway::Resource` under the api's root, whose path part is `part`.
fn resource(part: &str) -> Value {
    json!({
        "Type": "AWS::ApiGateway::Resource",
        "Properties": {
            "RestApiId": { "Ref": "Api" },
            "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
            "PathPart": part
        }
    })
}

/// One `AWS::ApiGateway::Method` on the resource `logical_id` names, answering `verb` with the
/// integration `integration` declares.
fn method(logical_id: &str, verb: &str, integration: Value) -> Value {
    json!({
        "Type": "AWS::ApiGateway::Method",
        "Properties": {
            "RestApiId": { "Ref": "Api" },
            "ResourceId": { "Ref": logical_id },
            "HttpMethod": verb,
            "AuthorizationType": "NONE",
            "MethodResponses": [{ "StatusCode": "200" }],
            "Integration": integration
        }
    })
}

/// An `AWS_PROXY` integration invoking the function `function_arn` names.
///
/// The `uri` is the form `Integration$uri` documents for the Lambda function-invoking action.
fn proxy_integration(function_arn: Value) -> Value {
    json!({
        "Type": "AWS_PROXY",
        "IntegrationHttpMethod": "POST",
        "Uri": {
            "Fn::Sub": [
                "arn:aws:apigateway:${AWS::Region}:lambda:path/2015-03-31/functions/${Target}/invocations",
                { "Target": function_arn }
            ]
        }
    })
}

/// The one stack the whole table is served by: one api carrying one resource and one method
/// per row, the functions, roles, table and log group those methods and the stage name, and
/// the account setting that lets the stage's logs be written at all.
fn template() -> String {
    json!({
        "Resources": {
            // The destination of the stage's access-log records, which is a group of the
            // account's and so has to exist before a record can land in it.
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
            // The table the direct service integration writes into, and the role that
            // integration is performed under.
            "Table": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": TABLE,
                    "BillingMode": "PAY_PER_REQUEST",
                    "KeySchema": [{ "AttributeName": TABLE_KEY, "KeyType": "HASH" }],
                    "AttributeDefinitions": [
                        { "AttributeName": TABLE_KEY, "AttributeType": "S" }
                    ]
                }
            },
            "IntegrationRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust("apigateway.amazonaws.com"),
                    "Policies": [allow(
                        "write-the-table",
                        json!("dynamodb:PutItem"),
                        json!({ "Fn::GetAtt": ["Table", "Arn"] }),
                    )]
                }
            },
            // The role both functions run as, and the two functions themselves: identical but
            // for the one thing the rows differ on — whether an `AWS::Lambda::Permission`
            // grants API Gateway an invocation of it.
            "ExecutionRole": {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": trust("lambda.amazonaws.com") }
            },
            "Granted": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": 30,
                    "Role": { "Fn::GetAtt": ["ExecutionRole", "Arn"] },
                    "Code": { "ZipFile": PROXY_HANDLER }
                }
            },
            "Ungranted": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": 30,
                    "Role": { "Fn::GetAtt": ["ExecutionRole", "Arn"] },
                    "Code": { "ZipFile": PROXY_HANDLER }
                }
            },
            "GrantedPermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": { "Fn::GetAtt": ["Granted", "Arn"] },
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
            "mocked": resource("mocked"),
            "granted": resource("granted"),
            "ungranted": resource("ungranted"),
            "absent": resource("absent"),
            "notes": resource("notes"),
            "note": {
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ParentId": { "Ref": "notes" },
                    "PathPart": "{id}"
                }
            },
            // A `MOCK` integration: the api answers it itself, from the status the request
            // template states and the response template the integration response renders.
            "MockedMethod": method("mocked", "GET", json!({
                "Type": "MOCK",
                "RequestTemplates": { "application/json": "{\"statusCode\": 200}" },
                "IntegrationResponses": [{
                    "StatusCode": "200",
                    "ResponseTemplates": { "application/json": MOCK_RESPONSE }
                }]
            })),
            "GrantedMethod": method(
                "granted",
                "GET",
                proxy_integration(json!({ "Fn::GetAtt": ["Granted", "Arn"] })),
            ),
            "UngrantedMethod": method(
                "ungranted",
                "GET",
                proxy_integration(json!({ "Fn::GetAtt": ["Ungranted", "Arn"] })),
            ),
            // A `uri` naming a function of this account that the stack never creates.
            "AbsentMethod": method(
                "absent",
                "GET",
                proxy_integration(json!({
                    "Fn::Sub": format!(
                        "arn:aws:lambda:${{AWS::Region}}:${{AWS::AccountId}}:function:{STACK}-absent"
                    )
                })),
            ),
            // An `AWS` integration: the api MAKES the Amazon DynamoDB action its `uri` names,
            // under the role its `Credentials` names, with the rendered request template as
            // that action's request — the key from the path parameter the resource declares,
            // the stored value from the payload the client sent.
            "NoteMethod": method("note", "PUT", json!({
                "Type": "AWS",
                "IntegrationHttpMethod": "POST",
                "Uri": {
                    "Fn::Sub": "arn:aws:apigateway:${AWS::Region}:dynamodb:action/PutItem"
                },
                "Credentials": { "Fn::GetAtt": ["IntegrationRole", "Arn"] },
                "PassthroughBehavior": "NEVER",
                "RequestTemplates": { "application/json": put_item_template() },
                "IntegrationResponses": [{
                    "StatusCode": "200",
                    "ResponseTemplates": { "application/json": NOTE_RESPONSE }
                }]
            })),
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": [
                    "MockedMethod",
                    "GrantedMethod",
                    "UngrantedMethod",
                    "AbsentMethod",
                    "NoteMethod"
                ],
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
                    },
                    "MethodSettings": [{
                        "ResourcePath": "/*",
                        "HttpMethod": "*",
                        "LoggingLevel": LOGGING_LEVEL,
                        "MetricsEnabled": true
                    }]
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
            "TableName": { "Value": { "Ref": "Table" } },
            "LogGroupName": { "Value": { "Ref": "AccessLogs" } }
        }
    })
    .to_string()
}

/// The name the template declares the api under, which is what API Gateway's own read of the
/// reported id has to report back.
const API_NAME: &str = "cfn-stage-integration-classes-api";

/// The `loggingLevel` the stage's method settings declare, which is what `GetStage` has to
/// report for the method path that stands for every method of the stage.
const LOGGING_LEVEL: &str = "INFO";

/// The method path a setting stated for every method of the stage is keyed by
/// (`Stage$methodSettings`: "/\*/\* for overriding all methods in the stage").
const EVERY_METHOD: &str = "*/*";

/// The body the `MOCK` integration's response template renders.
const MOCK_RESPONSE: &str = r#"{"served":"mock"}"#;

/// The body the direct service integration's response template renders: the key the path
/// parameter carried, so the answer proves the action was made for the item the client
/// addressed.
const NOTE_RESPONSE: &str = r#"{"stored":"$input.params('id')"}"#;

/// The attribute the stored note is written under.
const NOTE_ATTRIBUTE: &str = "note";

/// The request the direct service integration makes: the `PutItem` document whose key comes
/// from the resource's path parameter and whose value comes from the request's payload.
fn put_item_template() -> String {
    format!(
        r#"{{"TableName":"{TABLE}","Item":{{"{TABLE_KEY}":{{"S":"$input.params('id')"}},"{NOTE_ATTRIBUTE}":{{"S":"$input.path('$.note')"}}}}}}"#
    )
}

/// What a row owes the client beside its status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owed {
    /// The `MOCK` integration's own mapped body.
    Mocked,
    /// The proxy function's own answer, which names the path and stage its event carried.
    Proxied,
    /// The direct service integration's mapped answer, with the item the action wrote
    /// readable in the table through DynamoDB's own read.
    Stored,
    /// The api's configuration-error response, for an integration that could not be
    /// performed.
    ConfigurationError,
}

/// One row of the table: the method's address under the stage and what the stage owes a
/// request to it.
#[derive(Debug)]
struct Row {
    case: &'static str,
    verb: &'static str,
    /// The resource path under the stage. `{key}` stands for the item key a row that writes
    /// one addresses, which differs per call so each call's own write is observable.
    path: &'static str,
    /// The payload the client sends, empty where it sends none.
    body: &'static str,
    status: u16,
    owed: Owed,
}

const ROWS: &[Row] = &[
    Row {
        case: "a MOCK integration the api performs itself",
        verb: "GET",
        path: "mocked",
        body: "",
        status: 200,
        owed: Owed::Mocked,
    },
    Row {
        case: "an AWS_PROXY integration at a function that grants API Gateway the invocation",
        verb: "GET",
        path: "granted",
        body: "",
        status: 200,
        owed: Owed::Proxied,
    },
    Row {
        case: "an AWS integration making a DynamoDB action under a credentials role",
        verb: "PUT",
        path: "notes/{key}",
        body: r#"{"note":"written through the stage"}"#,
        status: 200,
        owed: Owed::Stored,
    },
    Row {
        case: "an AWS_PROXY integration at a function that grants API Gateway nothing",
        verb: "GET",
        path: "ungranted",
        body: "",
        status: 500,
        owed: Owed::ConfigurationError,
    },
    Row {
        case: "an AWS_PROXY integration whose uri names a function that does not exist",
        verb: "GET",
        path: "absent",
        body: "",
        status: 500,
        owed: Owed::ConfigurationError,
    },
];

/// One request this suite drove, for the delivery assertion: the id API Gateway answered it
/// in, and the status it was answered with.
#[derive(Debug)]
struct Served {
    case: String,
    request_id: String,
    status: u16,
}

#[tokio::test]
async fn a_cfn_deployed_stage_serves_every_class_of_integration_it_carries() {
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
    let table = output("TableName");
    let group = output("LogGroupName");
    assert_eq!(
        table, TABLE,
        "expected the stack to publish the declared table name, got {table:?}"
    );
    assert_eq!(
        group, LOG_GROUP,
        "expected the stack to publish the declared log group name, got {group:?}"
    );

    // The api the stack reports is an api API Gateway HOLDS: the id the outputs carry is read
    // back through that service's own read, which is the call a client makes next.
    let read = apigateway(&router, &format!("/restapis/{api_id}")).await;
    assert_eq!(
        read.status, 200,
        "expected API Gateway to hold the api the stack reported as {api_id:?}, got HTTP {} \
         with body {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.json().get("name").and_then(Value::as_str),
        Some(API_NAME),
        "expected the held api to carry the declared name, got {:?}",
        read.body
    );

    // The stage's declared settings are the settings API Gateway holds for it.
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
        "expected the stage's access-log destination to name the declared group {LOG_GROUP}, \
         got {:?}",
        stage.body
    );
    assert_eq!(
        held["methodSettings"][EVERY_METHOD]["loggingLevel"].as_str(),
        Some(LOGGING_LEVEL),
        "expected the stage to carry the declared method setting under {EVERY_METHOD}, got {:?}",
        stage.body
    );

    // Both of the stack's functions are up before any invoke: a function Lambda is still
    // bringing up is not what any row is about.
    let (_, stack_resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", STACK)]).await;
    let physical: BTreeMap<String, String> = physical_ids_of(&stack_resources);
    for logical in ["Granted", "Ungranted"] {
        let function = physical
            .get(logical)
            .cloned()
            .unwrap_or_else(|| panic!("no physical id for {logical} in {stack_resources}"));
        let state = wait_until_active(&router, &function).await;
        assert_eq!(
            state, "Active",
            "expected Lambda to report {function} Active before the invokes, got {state:?}"
        );
    }

    let mut served: Vec<Served> = Vec::new();
    for row in ROWS {
        let case = row.case;
        // The two addressings of the same stage a client on this machine uses: the api's own
        // invoke host, as the stack published it, and the emulator's reserved data-plane path.
        let addressings: [(&str, String); 2] = [
            (
                "the api's own invoke host, as the stack published it",
                invoke_url.clone(),
            ),
            (
                "the emulator's reserved data-plane path",
                format!("https://localhost/_aws/execute-api/{api_id}/{STAGE}"),
            ),
        ];
        for (index, (addressing, base)) in addressings.into_iter().enumerate() {
            // A row that writes addresses its own item per call, so the write each call made
            // is observable on its own rather than overwriting another's.
            let key = format!("{NOTE_ATTRIBUTE}-{index}");
            let path = row.path.replace("{key}", &key);
            let answered = invoke(&router, row.verb, &format!("{base}/{path}"), row.body).await;
            assert!(
                !answered.body.contains(ROUTER_REFUSAL),
                "case {case:?} addressed by {addressing:?}: expected an answer from the api's \
                 own data plane, got the front door's refusal {:?}",
                answered.body
            );
            assert_eq!(
                answered.status, row.status,
                "case {case:?} addressed by {addressing:?}: expected HTTP {}, got {} with body \
                 {:?}",
                row.status, answered.status, answered.body
            );
            match row.owed {
                Owed::Mocked => assert_eq!(
                    answered.body, MOCK_RESPONSE,
                    "case {case:?} addressed by {addressing:?}: expected the integration's own \
                     mapped body, got {:?}",
                    answered.body
                ),
                Owed::Proxied => {
                    let payload = answered.json();
                    assert_eq!(
                        payload.get("path").and_then(Value::as_str),
                        Some(format!("/{path}").as_str()),
                        "case {case:?} addressed by {addressing:?}: expected the integration to \
                         see the path /{path}, got {:?}",
                        answered.body
                    );
                    assert_eq!(
                        payload.get("stage").and_then(Value::as_str),
                        Some(STAGE),
                        "case {case:?} addressed by {addressing:?}: expected the integration to \
                         see the stage {STAGE}, got {:?}",
                        answered.body
                    );
                }
                Owed::Stored => {
                    assert_eq!(
                        answered.json().get("stored").and_then(Value::as_str),
                        Some(key.as_str()),
                        "case {case:?} addressed by {addressing:?}: expected the mapped answer \
                         to name the item the request addressed, got {:?}",
                        answered.body
                    );
                    // The write is read back through DynamoDB's OWN read, so what passes is
                    // what that service holds rather than what the stage answered.
                    let item = dynamodb(
                        &router,
                        "GetItem",
                        json!({
                            "TableName": TABLE,
                            "Key": { TABLE_KEY: { "S": key.clone() } }
                        }),
                    )
                    .await;
                    assert_eq!(
                        item.status, 200,
                        "case {case:?} addressed by {addressing:?}: expected HTTP 200 from \
                         GetItem, got {} with body {:?}",
                        item.status, item.body
                    );
                    let note: Value = serde_json::from_str(row.body).expect("a JSON payload");
                    assert_eq!(
                        item.json()["Item"][NOTE_ATTRIBUTE]["S"].as_str(),
                        note.get(NOTE_ATTRIBUTE).and_then(Value::as_str),
                        "case {case:?} addressed by {addressing:?}: expected the table to hold \
                         what the request carried, got {:?}",
                        item.body
                    );
                }
                Owed::ConfigurationError => {
                    assert_eq!(
                        answered.body, CONFIGURATION_ERROR_BODY,
                        "case {case:?} addressed by {addressing:?}: expected the api's \
                         configuration-error body, got {:?}",
                        answered.body
                    );
                    assert_eq!(
                        answered.error_type.as_deref(),
                        Some(INTERNAL_SERVER_ERROR_EXCEPTION),
                        "case {case:?} addressed by {addressing:?}: expected the answer to name \
                         the exception class, got {:?}",
                        answered.error_type
                    );
                }
            }
            let request_id = answered.request_id.clone().unwrap_or_else(|| {
                panic!(
                    "case {case:?} addressed by {addressing:?}: expected the answer to carry a \
                     {REQUEST_ID_HEADER} header, got {answered:?}"
                )
            });
            served.push(Served {
                case: format!("{case} addressed by {addressing}"),
                request_id,
                status: row.status,
            });
        }
    }

    // Every request the stage answered is a record in the group the stack declared, rendered
    // by the format the stage declared. The delivery is another service's write, so the
    // CONDITION is looked at again until it holds rather than waited out.
    let delivered = delivered_records(&router, served.len()).await;
    for request in &served {
        let case = &request.case;
        let message = delivered.get(&request.request_id).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a record for request {} in {LOG_GROUP}, got records for \
                 {:?}",
                request.request_id,
                delivered.keys().collect::<Vec<_>>()
            )
        });
        let record: Value = serde_json::from_str(message).unwrap_or(Value::Null);
        assert_eq!(
            record.get("status").and_then(Value::as_str),
            Some(request.status.to_string().as_str()),
            "case {case:?}: expected the record to name the status the request was answered \
             with, got {message:?}"
        );
        assert_eq!(
            record.get("stage").and_then(Value::as_str),
            Some(STAGE),
            "case {case:?}: expected the record to name the stage that answered, got {message:?}"
        );
    }
}

/// The records the destination group holds, keyed by the request id each one renders, once it
/// holds at least `wanted` of them — or whatever it holds when the budget runs out, so the
/// assertions above report what was missing rather than this wait doing it.
async fn delivered_records(
    router: &MultiServiceRouter,
    wanted: usize,
) -> BTreeMap<String, String> {
    let deadline = Instant::now() + BUDGET;
    let mut records = BTreeMap::new();
    loop {
        let answered = logs(
            router,
            "FilterLogEvents",
            json!({ "logGroupName": LOG_GROUP }),
        )
        .await;
        if answered.status == 200 {
            records = answered
                .json()
                .get("events")
                .and_then(Value::as_array)
                .map(|events| {
                    events
                        .iter()
                        .filter_map(|event| event.get("message").and_then(Value::as_str))
                        .filter_map(|message| {
                            let parsed: Value = serde_json::from_str(message).ok()?;
                            let id = parsed.get("requestId").and_then(Value::as_str)?;
                            Some((id.to_string(), message.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        if records.len() >= wanted || Instant::now() >= deadline {
            return records;
        }
        tokio::time::sleep(POLL).await;
    }
}
