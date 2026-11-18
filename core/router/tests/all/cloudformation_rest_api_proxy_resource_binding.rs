// SPDX-License-Identifier: CC0-1.0

//! The resource -> method -> `AWS_PROXY` integration BINDING a CloudFormation stack provisions
//! resolves a request on the deployed stage's invoke URL, whatever shape the resource path
//! takes and whichever verb the method declares.
//!
//! The rule under test: **a request to a deployed REST API stage is resolved by API Gateway's
//! own data plane to the method of the MOST SPECIFIC resource of that deployment whose path
//! template matches the request path — a literal segment beating a path parameter and a path
//! parameter beating a greedy catch-all — and the method's Lambda proxy integration is handed
//! that resource's own template, the api-relative path the client addressed, the verb, the
//! stage and the parameters the match bound; a method declared for `ANY` answers every verb.
//! Nothing about that resolution depends on HOW the records were created, so an api whose
//! resources, methods, integrations, deployment and stage CloudFormation created through API
//! Gateway's own public API serves the invoke URL the stack published exactly as one a client
//! created call by call, and the emulator's front-door refusal for an address it can attribute
//! to no service at all is never the answer. WHO API Gateway performs the integration as is
//! the choice `Integration$credentials` states: a role named there is assumed, so that role's
//! trust of `apigateway.amazonaws.com` and its own permissions admit the invocation, and an
//! integration naming none is admitted by the grant on the function instead.** Four crates
//! have to agree for that to hold (CloudFormation expands the template and makes every create
//! call, API Gateway holds the api and serves the invoke, AWS Lambda runs the function behind
//! the integration and IAM answers for the role the integration names), so the suite lives
//! here rather than in any one of them, and it drives the emulator's own assembled service
//! set.
//!
//! `cloudformation_rest_api_serves_its_stage.rs` owns WHICH declaration an api can come out of
//! (the SAM transform's implicit api, an `AWS::Serverless::Api`, the five types written out)
//! and `cloudformation_stage_invoke_answer_classes.rs` owns WHY an integration can or cannot be
//! performed at all; this suite owns the BINDING — which method of the deployment a request
//! resolves to and what the resolved match tells the integration.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the resource path (the
//! api's root, one literal segment, two nested literal segments, a path parameter, a greedy
//! catch-all, and a literal sibling of that catch-all, which it must beat), the verb the
//! method declares (a named verb and `ANY`), the verb the request is made with, whether the
//! request carries a body, and which alternative admits the integration's invocation (a
//! credentials role that trusts this service, and the grant on the function).
//!
//! Every expected value is computed here from what the template declared — the api id and the
//! stage's invoke base come out of the stack's own outputs, which is what a client deploying
//! the stack reads — never out of the answer being checked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, opt_element, outputs_of, physical_ids_of};
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for, and so the Region the stack's resources belong
/// to. It is the Region the shared stack poll helper signs its reads for.
const REGION: &str = "us-east-1";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the template deploys the api to.
const STAGE: &str = "live";

/// The service principal an api-side role has to trust for API Gateway to assume it, and the
/// action such a role has to allow for the integration's invocation to be made under it.
const API_GATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";
const INVOKE_FUNCTION: &str = "lambda:InvokeFunction";

/// The error code the emulator's front door answers a request it can attribute to no service
/// with, which the rule says a stage invoke is never answered with.
const ROUTER_REFUSAL: &str = "UnknownService";

/// How often the waits below look again, and how long they give up after. Generous: what is
/// waited on is a CONDITION another service owns, never an elapsed time this suite asserts on.
const LOOK: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(120);

/// The logical ids the template below declares, read back out of the stack's own resource list.
const API_ID: &str = "Api";
const FUNCTION_ID: &str = "Handler";
const ROLE_ID: &str = "ApiGatewayInvokeRole";

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// api CloudFormation creates is the api this suite then invokes, and the function behind its
/// integrations is the one AWS Lambda runs.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// Call one operation of an awsQuery service over its own wire.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// Read one API Gateway control-plane resource over its restJson1 wire: that service signs as
/// `apigateway` and binds every REST operation under `/restapis/…`.
async fn apigateway(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
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
/// The span a created function spends coming up is that service's own, so no fixed sleep names
/// the instant it ends: the wait is on the CONDITION, looking again until it holds.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let configuration =
            lambda(router, &format!("/2015-03-31/functions/{name}/configuration")).await;
        let state = configuration
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state == "Active" || Instant::now() >= giving_up_at {
            return state;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// What a request to the deployed stage was answered with.
struct Answer {
    status: u16,
    body: String,
}

/// Invoke the deployed stage, exactly as a client reaches AWS: the request's host is the api's
/// own `{rest-api-id}.execute-api.{region}.amazonaws.com` endpoint and its path is the stage
/// and the resource path.
///
/// The address is taken apart rather than rebuilt here, so what is driven is the value the
/// stack told the caller to use.
async fn invoke(router: &MultiServiceRouter, url: &str, verb: &str, body: &str) -> Answer {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let (host, path) = match without_scheme.split_once('/') {
        Some((host, path)) => (host.to_string(), format!("/{path}")),
        None => (without_scheme.to_string(), "/".to_string()),
    };
    let request = http::Request::builder()
        .method(verb)
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// The handler behind every method of the api: a Lambda proxy integration's own answer shape,
/// echoing what the event said about the RESOLVED match, so the response is evidence of which
/// method the request bound to and what that binding told the integration.
const PROXY_HANDLER: &str = r#"
import json


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "resource": event.get("resource"),
                "path": event.get("path"),
                "method": event.get("httpMethod"),
                "pathParameters": event.get("pathParameters"),
                "stage": (event.get("requestContext") or {}).get("stage"),
                "payload": event.get("body"),
            }
        ),
    }
"#;

/// One row: the request made on the deployed stage, and the match the deployment's binding
/// must resolve it to.
struct Row {
    case: &'static str,
    /// The path appended to the stage's invoke base, as a client addresses it.
    request_path: &'static str,
    /// The verb the request is made with.
    verb: &'static str,
    /// The body the request carries.
    body: &'static str,
    /// The resource path template of the method the request must resolve to.
    resource: &'static str,
    /// The api-relative path the integration's event must report.
    event_path: &'static str,
    /// The parameters the match must bind, as the event reports them; empty for a template
    /// with no parameter in it, which reports none at all.
    path_parameters: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "a GET method on the api's own root resource",
        request_path: "",
        verb: "GET",
        body: "",
        resource: "/",
        event_path: "/",
        path_parameters: &[],
    },
    Row {
        case: "a POST method on one literal segment, carrying a body",
        request_path: "/items",
        verb: "POST",
        body: r#"{"id":"one"}"#,
        resource: "/items",
        event_path: "/items",
        path_parameters: &[],
    },
    Row {
        case: "a GET method two nested literal segments deep",
        request_path: "/orders/open",
        verb: "GET",
        body: "",
        resource: "/orders/open",
        event_path: "/orders/open",
        path_parameters: &[],
    },
    Row {
        case: "a GET method on a path-parameter resource, which binds the segment",
        request_path: "/widgets/w-7",
        verb: "GET",
        body: "",
        resource: "/widgets/{widgetId}",
        event_path: "/widgets/w-7",
        path_parameters: &[("widgetId", "w-7")],
    },
    Row {
        case: "an ANY method on a greedy catch-all, reached with PUT and a body",
        request_path: "/files/a/b/c",
        verb: "PUT",
        body: r#"{"keep":true}"#,
        resource: "/files/{proxy+}",
        event_path: "/files/a/b/c",
        path_parameters: &[("proxy", "a/b/c")],
    },
    Row {
        case: "a literal sibling of the greedy catch-all, which the literal beats",
        request_path: "/files/pinned",
        verb: "GET",
        body: "",
        resource: "/files/pinned",
        event_path: "/files/pinned",
        path_parameters: &[],
    },
];

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

/// An `AWS::ApiGateway::Resource` under `parent`, which is either another declared resource or
/// the api's own root.
fn resource(parent: Value, path_part: &str) -> Value {
    json!({
        "Type": "AWS::ApiGateway::Resource",
        "Properties": {
            "RestApiId": { "Ref": API_ID },
            "ParentId": parent,
            "PathPart": path_part
        }
    })
}

/// An `AWS::ApiGateway::Method` with an `AWS_PROXY` integration on `resource_id`: `credentials`
/// says whether API Gateway performs it under the api-side role (so that role's trust and
/// permissions admit it) or as itself (so the grant on the function does).
fn proxy_method(resource_id: Value, verb: &str, credentials: bool) -> Value {
    let mut integration = json!({
        "Type": "AWS_PROXY",
        "IntegrationHttpMethod": "POST",
        "Uri": {
            "Fn::Sub": [
                "arn:aws:apigateway:${AWS::Region}:lambda:path/2015-03-31/functions/\
                 ${Target}/invocations",
                { "Target": { "Fn::GetAtt": [FUNCTION_ID, "Arn"] } }
            ]
        }
    });
    if credentials {
        integration["Credentials"] = json!({ "Fn::GetAtt": [ROLE_ID, "Arn"] });
    }
    json!({
        "Type": "AWS::ApiGateway::Method",
        "Properties": {
            "RestApiId": { "Ref": API_ID },
            "ResourceId": resource_id,
            "HttpMethod": verb,
            "AuthorizationType": "NONE",
            "MethodResponses": [{ "StatusCode": "200" }],
            "Integration": integration
        }
    })
}

/// The ONE stack every row is served by: the function and the two roles, the api, the resource
/// tree the rows address, one method per row, a deployment of them and the stage that serves
/// it.
fn template(stack: &str) -> String {
    let root = json!({ "Fn::GetAtt": [API_ID, "RootResourceId"] });
    let resources = json!({
        // The role the function itself runs as.
        "ExecutionRole": {
            "Type": "AWS::IAM::Role",
            "Properties": { "AssumeRolePolicyDocument": trust("lambda.amazonaws.com") }
        },
        // The api-side role: API Gateway assumes it to perform the integrations that name it,
        // so it trusts this service and allows the invocation of the function.
        ROLE_ID: {
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": trust(API_GATEWAY_PRINCIPAL),
                "Policies": [{
                    "PolicyName": "invoke-the-backend",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": INVOKE_FUNCTION,
                            "Resource": { "Fn::GetAtt": [FUNCTION_ID, "Arn"] }
                        }]
                    }
                }]
            }
        },
        FUNCTION_ID: {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Timeout": 30,
                "Role": { "Fn::GetAtt": ["ExecutionRole", "Arn"] },
                "Code": { "ZipFile": PROXY_HANDLER }
            }
        },
        // The alternative to the role: the grant on the FUNCTION, which admits the
        // integrations that name no credentials.
        "Permission": {
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "Action": INVOKE_FUNCTION,
                "FunctionName": { "Fn::GetAtt": [FUNCTION_ID, "Arn"] },
                "Principal": API_GATEWAY_PRINCIPAL
            }
        },
        API_ID: {
            "Type": "AWS::ApiGateway::RestApi",
            "Properties": {
                "Name": format!("{stack}-binding"),
                "EndpointConfiguration": { "Types": ["REGIONAL"] }
            }
        },
        "Items": resource(root.clone(), "items"),
        "Orders": resource(root.clone(), "orders"),
        "OrdersOpen": resource(json!({ "Ref": "Orders" }), "open"),
        "Widgets": resource(root.clone(), "widgets"),
        "WidgetId": resource(json!({ "Ref": "Widgets" }), "{widgetId}"),
        "Files": resource(root.clone(), "files"),
        "FilesProxy": resource(json!({ "Ref": "Files" }), "{proxy+}"),
        "FilesPinned": resource(json!({ "Ref": "Files" }), "pinned"),
        // One method per row. The rows alternate between the two alternatives that admit the
        // integration's invocation, so both are exercised across the resource shapes.
        "RootMethod": proxy_method(root, "GET", true),
        "ItemsMethod": proxy_method(json!({ "Ref": "Items" }), "POST", false),
        "OrdersOpenMethod": proxy_method(json!({ "Ref": "OrdersOpen" }), "GET", true),
        "WidgetIdMethod": proxy_method(json!({ "Ref": "WidgetId" }), "GET", false),
        "FilesProxyMethod": proxy_method(json!({ "Ref": "FilesProxy" }), "ANY", true),
        "FilesPinnedMethod": proxy_method(json!({ "Ref": "FilesPinned" }), "GET", false),
        "Deployment": {
            "Type": "AWS::ApiGateway::Deployment",
            "DependsOn": [
                "RootMethod", "ItemsMethod", "OrdersOpenMethod",
                "WidgetIdMethod", "FilesProxyMethod", "FilesPinnedMethod"
            ],
            "Properties": {
                "RestApiId": { "Ref": API_ID },
                "Description": "the deployment the stage serves"
            }
        },
        "Stage": {
            "Type": "AWS::ApiGateway::Stage",
            "Properties": {
                "RestApiId": { "Ref": API_ID },
                "DeploymentId": { "Ref": "Deployment" },
                "StageName": STAGE
            }
        }
    });

    json!({
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": API_ID } },
            "InvokeBase": {
                "Value": {
                    "Fn::Sub": format!(
                        "https://${{{API_ID}}}.execute-api.${{AWS::Region}}.amazonaws.com/{STAGE}"
                    )
                }
            }
        }
    })
    .to_string()
}

/// What the stack published once it settled: the api id, the stage's invoke base and the
/// logical-id -> physical-id map of what it created.
async fn deployed(
    router: &MultiServiceRouter,
    stack: &str,
) -> (String, String, BTreeMap<String, String>) {
    let body = template(stack);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            // The template declares roles, which is what CAPABILITY_IAM acknowledges.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from CreateStack, got {status} with body {answered}"
    );

    let (settled, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    let failures = if settled == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        settled, CREATE_COMPLETE,
        "expected the stack to reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );

    let outputs = outputs_of(&described);
    let api_id = outputs
        .get("ApiId")
        .cloned()
        .unwrap_or_else(|| panic!("the stack published no ApiId output in {described}"));
    let invoke_base = outputs
        .get("InvokeBase")
        .cloned()
        .unwrap_or_else(|| panic!("the stack published no InvokeBase output in {described}"));

    let (_, stack_resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    (api_id, invoke_base, physical_ids_of(&stack_resources))
}

/// The parameters a row expects, as the event reports them: an object of the bound segments,
/// or `null` where the matched template carries no parameter at all.
fn expected_parameters(row: &Row) -> Value {
    if row.path_parameters.is_empty() {
        return Value::Null;
    }
    let bound: Map<String, Value> = row
        .path_parameters
        .iter()
        .map(|(name, value)| ((*name).to_string(), json!(value)))
        .collect();
    Value::Object(bound)
}

/// RULE: every row's request resolves to the method of the most specific matching resource of
/// the deployment the stack created, and the proxy integration behind it is handed that match.
#[tokio::test]
async fn a_stacks_proxy_methods_bind_every_resource_shape_of_its_deployment() {
    let router = build_router();
    let stack = "rest-api-binding";
    let (api_id, invoke_base, physical) = deployed(&router, stack).await;

    // The api the stack published is one API Gateway HOLDS, and the stage it deployed points
    // at a deployment that service holds: the stack's own records are not evidence of either.
    let (status, api) = apigateway(&router, &format!("/restapis/{api_id}")).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetRestApi for the api the stack published ({api_id}), got \
         {status} with body {api}"
    );
    let (status, stage) = apigateway(&router, &format!("/restapis/{api_id}/stages/{STAGE}")).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetStage for {STAGE}, got {status} with body {stage}"
    );

    // The api-side role the credentials integrations name is one IAM holds, trusting this
    // service: that trust is what lets API Gateway perform those integrations at all.
    let role = physical
        .get(ROLE_ID)
        .cloned()
        .unwrap_or_else(|| panic!("the stack reported no physical id for {ROLE_ID}: {physical:?}"));
    let (status, reported) = query(
        &router,
        "iam",
        "2010-05-08",
        "GetRole",
        &[("RoleName", role.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from GetRole for {role}, got {status} with body {reported}"
    );
    let document = ::smithy_server_runtime::percent_decode(
        &opt_element(&reported, "AssumeRolePolicyDocument").unwrap_or_default(),
    )
    .to_string();
    assert!(
        document.contains(API_GATEWAY_PRINCIPAL),
        "expected IAM to hold {role} trusting {API_GATEWAY_PRINCIPAL}, got {document:?}"
    );

    let function = physical.get(FUNCTION_ID).cloned().unwrap_or_else(|| {
        panic!("the stack reported no physical id for {FUNCTION_ID}: {physical:?}")
    });
    let state = wait_until_active(&router, &function).await;
    assert_eq!(
        state, "Active",
        "expected Lambda to report {function} Active before the invokes, got {state:?}"
    );

    for row in ROWS {
        let case = row.case;
        let url = format!("{invoke_base}{}", row.request_path);
        let answered = invoke(&router, &url, row.verb, row.body).await;
        assert!(
            !answered.body.contains(ROUTER_REFUSAL),
            "case {case:?}: expected an answer from the api's own data plane at {url}, got the \
             front door's service-resolution error {:?}",
            answered.body
        );
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the deployed stage to answer HTTP 200 at {url}, got {} with \
             body {:?}",
            answered.status, answered.body
        );

        let payload: Value = serde_json::from_str(&answered.body).unwrap_or(Value::Null);
        assert_eq!(
            payload.get("resource").and_then(Value::as_str),
            Some(row.resource),
            "case {case:?}: expected the integration to be handed the resource template {:?}, \
             got {:?}",
            row.resource,
            answered.body
        );
        assert_eq!(
            payload.get("path").and_then(Value::as_str),
            Some(row.event_path),
            "case {case:?}: expected the integration to see the path {:?}, got {:?}",
            row.event_path,
            answered.body
        );
        assert_eq!(
            payload.get("method").and_then(Value::as_str),
            Some(row.verb),
            "case {case:?}: expected the integration to see the verb {:?}, got {:?}",
            row.verb,
            answered.body
        );
        let expected = expected_parameters(row);
        assert_eq!(
            payload.get("pathParameters").unwrap_or(&Value::Null),
            &expected,
            "case {case:?}: expected the match to bind {expected}, got {:?}",
            answered.body
        );
        assert_eq!(
            payload.get("stage").and_then(Value::as_str),
            Some(STAGE),
            "case {case:?}: expected the integration to see the stage {STAGE:?}, got {:?}",
            answered.body
        );
        let carried = payload.get("payload").and_then(Value::as_str);
        if row.body.is_empty() {
            assert!(
                carried.is_none_or(str::is_empty),
                "case {case:?}: expected the integration to see no body, got {carried:?}"
            );
        } else {
            assert_eq!(
                carried,
                Some(row.body),
                "case {case:?}: expected the integration to see the body {:?}, got {:?}",
                row.body,
                answered.body
            );
        }
    }
}
