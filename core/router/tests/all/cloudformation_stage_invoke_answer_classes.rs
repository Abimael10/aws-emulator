// SPDX-License-Identifier: CC0-1.0

//! Every request to the invoke URL of a CloudFormation-deployed REST API stage is attributed
//! to API Gateway and answered by that api's own data plane.
//!
//! The rule under test: **a request addressed to a deployed REST API stage's invoke URL is
//! attributed to API Gateway, which resolves the deployment, the resource and the method,
//! authorizes the call and performs its integration — so a correctly wired method answers
//! HTTP 200 carrying the integration's body, and a method whose integration cannot be
//! performed answers that api's own configuration-error response (HTTP 500,
//! `{"message":"Internal server error"}`, `x-amzn-errortype: InternalServerErrorException`).
//! The emulator's front-door refusal for an address it can attribute to no service at all
//! (`404` `UnknownService`) is never the answer, whichever way a client addresses the
//! stage.** Four crates have to agree for that to hold — CloudFormation makes the calls that
//! create the api, API Gateway serves the invoke, Lambda runs (or refuses to run) the
//! function behind the integration and IAM answers for the credentials role the integration
//! names — so the suite lives here rather than in any one of them, and it drives the
//! emulator's own assembled service set.
//!
//! The rows differ along what the rule generalises over: WHY the integration can or cannot be
//! performed — a function whose resource policy grants API Gateway the invocation, a function
//! that grants it nothing, a credentials role that trusts `apigateway.amazonaws.com` and may
//! invoke the function, a credentials role that trusts another principal, and an integration
//! `uri` naming a function that does not exist — and each row is driven over BOTH addressings
//! of the same stage a client on this machine uses: the api's own `execute-api` invoke host,
//! as the stack's own output publishes it, and the emulator's reserved data-plane path.
//!
//! Every expected value is computed here from what the stack declared — the api id and the
//! invoke URL come out of the stack's outputs, which is what a client deploying the stack
//! reads — never out of the answer being checked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

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

/// How long to wait between polls of a function's state, and how many polls to make.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 400;

/// The error code the emulator's front door answers a request it can attribute to no service
/// with, which the rule says a stage invoke is never answered with.
const ROUTER_REFUSAL: &str = "UnknownService";

/// The body API Gateway answers a method whose integration it could not perform with.
const CONFIGURATION_ERROR_BODY: &str = r#"{"message":"Internal server error"}"#;

/// The exception class that answer names, in the header an SDK reads an error code out of.
const INTERNAL_SERVER_ERROR_EXCEPTION: &str = "InternalServerErrorException";

/// See [`INTERNAL_SERVER_ERROR_EXCEPTION`].
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The stage the stack deploys its api to.
const STAGE: &str = "live";

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// api CloudFormation creates is the api this suite then invokes, and the functions behind its
/// integrations are the ones AWS Lambda runs.
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` read, for the wait below.
async fn lambda(router: &MultiServiceRouter, uri: &str) -> Value {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    serde_json::from_slice(response.body()).unwrap_or(Value::Null)
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`.
///
/// The span a function spends `Pending` is that service's own, so no fixed sleep names the
/// instant it ends; an invoke of the stage before then is an invoke of a function Lambda is
/// still bringing up. The wait is on that CONDITION, looked at again until it holds.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> String {
    let mut last = String::new();
    for _ in 0..POLLS {
        let configuration =
            lambda(router, &format!("/2015-03-31/functions/{name}/configuration")).await;
        let state = configuration
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state == "Active" {
            return state;
        }
        last = state;
        tokio::time::sleep(POLL).await;
    }
    last
}

/// What an invoke of the stage was answered with: the status, the exception class the answer
/// names, and the body.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: Option<String>,
    body: String,
}

/// Invoke a deployed stage at `url`, exactly as a client reaches one: the URL is taken apart
/// rather than rebuilt, so what is driven is the address the caller holds.
async fn invoke(router: &MultiServiceRouter, url: &str) -> Answer {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, path) = match without_scheme.split_once('/') {
        Some((host, path)) => (host.to_string(), format!("/{path}")),
        None => (without_scheme.to_string(), "/".to_string()),
    };
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json")
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        error_type: response
            .headers()
            .get(ERROR_TYPE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// The handler both of the stack's functions run: a Lambda proxy integration's own answer
/// shape, carrying what the event said about the request so the response proves the
/// integration was really reached.
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

/// An `AWS::ApiGateway::Method` with an `AWS_PROXY` integration, declared on its own resource
/// under the api's root: `uri` names the function the integration invokes, and `credentials`
/// the role API Gateway makes that call under where the row names one.
fn proxy_method(path: &str, uri: Value, credentials: Option<Value>) -> Value {
    let mut integration = json!({
        "Type": "AWS_PROXY",
        "IntegrationHttpMethod": "POST",
        "Uri": uri
    });
    if let Some(credentials) = credentials {
        integration["Credentials"] = credentials;
    }
    json!({
        "Type": "AWS::ApiGateway::Method",
        "Properties": {
            "RestApiId": { "Ref": "Api" },
            "ResourceId": { "Ref": path },
            "HttpMethod": "GET",
            "AuthorizationType": "NONE",
            "MethodResponses": [{ "StatusCode": "200" }],
            "Integration": integration
        }
    })
}

/// The `uri` of an integration that invokes the function `function_arn` names, in the form
/// `Integration$uri` documents for the Lambda function-invoking action.
fn invocation_uri(function_arn: Value) -> Value {
    json!({
        "Fn::Sub": [
            "arn:aws:apigateway:${AWS::Region}:lambda:path/2015-03-31/functions/${Target}/invocations",
            { "Target": function_arn }
        ]
    })
}

/// The one stack every row of the table is served by: one api, one resource per row, and the
/// functions and roles the rows' integrations name.
fn template(stack: &str) -> String {
    json!({
        "Resources": {
            // The role both functions run as.
            "ExecutionRole": {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": trust("lambda.amazonaws.com") }
            },
            // The role API Gateway CAN assume, and which may invoke the function.
            "TrustedRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust("apigateway.amazonaws.com"),
                    "Policies": [{
                        "PolicyName": "invoke-the-function",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "lambda:InvokeFunction",
                                "Resource": "*"
                            }]
                        }
                    }]
                }
            },
            // The same role, trusting a principal that is not API Gateway: nothing can be
            // called under it by this service.
            "UntrustedRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust("ec2.amazonaws.com"),
                    "Policies": [{
                        "PolicyName": "invoke-the-function",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "lambda:InvokeFunction",
                                "Resource": "*"
                            }]
                        }
                    }]
                }
            },
            "Echo": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": 30,
                    "Role": { "Fn::GetAtt": ["ExecutionRole", "Arn"] },
                    "Code": { "ZipFile": PROXY_HANDLER }
                }
            },
            // A second function, identical but for the one thing the rows differ on: no
            // `AWS::Lambda::Permission` grants API Gateway an invocation of it.
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
            "EchoPermission": {
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "Action": "lambda:InvokeFunction",
                    "FunctionName": { "Fn::GetAtt": ["Echo", "Arn"] },
                    "Principal": "apigateway.amazonaws.com"
                }
            },
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": format!("{stack}-api"),
                    "EndpointConfiguration": { "Types": ["REGIONAL"] }
                }
            },
            "wired": resource("wired"),
            "ungranted": resource("ungranted"),
            "trusted": resource("trusted"),
            "untrusted": resource("untrusted"),
            "absent": resource("absent"),
            "WiredMethod": proxy_method(
                "wired",
                invocation_uri(json!({ "Fn::GetAtt": ["Echo", "Arn"] })),
                None,
            ),
            "UngrantedMethod": proxy_method(
                "ungranted",
                invocation_uri(json!({ "Fn::GetAtt": ["Ungranted", "Arn"] })),
                None,
            ),
            "TrustedMethod": proxy_method(
                "trusted",
                invocation_uri(json!({ "Fn::GetAtt": ["Echo", "Arn"] })),
                Some(json!({ "Fn::GetAtt": ["TrustedRole", "Arn"] })),
            ),
            "UntrustedMethod": proxy_method(
                "untrusted",
                invocation_uri(json!({ "Fn::GetAtt": ["Echo", "Arn"] })),
                Some(json!({ "Fn::GetAtt": ["UntrustedRole", "Arn"] })),
            ),
            // A `uri` naming a function of this account that the stack never creates.
            "AbsentMethod": proxy_method(
                "absent",
                invocation_uri(json!({
                    "Fn::Sub": format!(
                        "arn:aws:lambda:${{AWS::Region}}:${{AWS::AccountId}}:function:{stack}-absent"
                    )
                })),
                None,
            ),
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": [
                    "WiredMethod",
                    "UngrantedMethod",
                    "TrustedMethod",
                    "UntrustedMethod",
                    "AbsentMethod"
                ],
                "Properties": { "RestApiId": { "Ref": "Api" } }
            },
            "Stage": {
                "Type": "AWS::ApiGateway::Stage",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "DeploymentId": { "Ref": "Deployment" },
                    "StageName": STAGE
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
            }
        }
    })
    .to_string()
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

/// One row of the table: the method's resource path and what the stage owes a request to it.
struct Row {
    case: &'static str,
    /// The path part of the resource the row's method is on.
    path: &'static str,
    /// The status the stage answers, and — for a performed integration — that the answer is
    /// the integration's own rather than the api's configuration-error envelope.
    status: u16,
    performed: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a proxy method whose function grants API Gateway the invocation",
        path: "wired",
        status: 200,
        performed: true,
    },
    Row {
        case: "a proxy method whose function grants API Gateway nothing",
        path: "ungranted",
        status: 500,
        performed: false,
    },
    Row {
        case: "a proxy method under a credentials role that trusts apigateway.amazonaws.com",
        path: "trusted",
        status: 200,
        performed: true,
    },
    Row {
        case: "a proxy method under a credentials role that trusts another principal",
        path: "untrusted",
        status: 500,
        performed: false,
    },
    Row {
        case: "a proxy method whose integration names a function that does not exist",
        path: "absent",
        status: 500,
        performed: false,
    },
];

#[tokio::test]
async fn a_cfn_deployed_stage_s_invoke_url_is_answered_by_the_api_s_own_data_plane() {
    let router = build_router();
    let stack = "stage-invoke-answer-classes";
    let template = template(stack);

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let (status, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    let failures = if status == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        status, CREATE_COMPLETE,
        "expected the stack to reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );

    let outputs = outputs_of(&described);
    let api_id = outputs
        .get("ApiId")
        .cloned()
        .unwrap_or_else(|| panic!("the stack published no ApiId output in {described}"));
    let invoke_url = outputs
        .get("InvokeUrl")
        .cloned()
        .unwrap_or_else(|| panic!("the stack published no InvokeUrl output in {described}"));

    // Both of the stack's functions are up before any invoke: a function Lambda is still
    // bringing up is not what any row is about.
    let (_, stack_resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    let physical: BTreeMap<String, String> = physical_ids_of(&stack_resources);
    for logical in ["Echo", "Ungranted"] {
        let function = physical.get(logical).cloned().unwrap_or_else(|| {
            panic!("no physical id for {logical} in {stack_resources}")
        });
        let state = wait_until_active(&router, &function).await;
        assert_eq!(
            state, "Active",
            "expected Lambda to report {function} Active before the invokes, got {state:?}"
        );
    }

    for row in ROWS {
        let case = row.case;
        // The two addressings of the same stage a client on this machine uses: the api's own
        // invoke host, as the stack published it, and the emulator's reserved data-plane path.
        let addressings: [(&str, String); 2] = [
            (
                "the api's own invoke host, as the stack published it",
                format!("{invoke_url}/{}", row.path),
            ),
            (
                "the emulator's reserved data-plane path",
                format!(
                    "https://localhost/_aws/execute-api/{api_id}/{STAGE}/{}",
                    row.path
                ),
            ),
        ];
        for (addressing, url) in addressings {
            let answered = invoke(&router, &url).await;
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
            if row.performed {
                let payload: Value = serde_json::from_str(&answered.body).unwrap_or(Value::Null);
                assert_eq!(
                    payload.get("path").and_then(Value::as_str),
                    Some(format!("/{}", row.path).as_str()),
                    "case {case:?} addressed by {addressing:?}: expected the integration to see \
                     the path /{}, got {:?}",
                    row.path,
                    answered.body
                );
                assert_eq!(
                    payload.get("stage").and_then(Value::as_str),
                    Some(STAGE),
                    "case {case:?} addressed by {addressing:?}: expected the integration to see \
                     the stage {STAGE}, got {:?}",
                    answered.body
                );
            } else {
                assert_eq!(
                    answered.body, CONFIGURATION_ERROR_BODY,
                    "case {case:?} addressed by {addressing:?}: expected the api's \
                     configuration-error body, got {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.error_type.as_deref(),
                    Some(INTERNAL_SERVER_ERROR_EXCEPTION),
                    "case {case:?} addressed by {addressing:?}: expected the answer to name the \
                     exception class, got {:?}",
                    answered.error_type
                );
            }
        }
    }
}
