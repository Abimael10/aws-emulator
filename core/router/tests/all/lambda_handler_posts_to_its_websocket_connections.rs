// SPDX-License-Identifier: CC0-1.0

//! What a Lambda handler's push to a WebSocket api's connections is addressed at and
//! authorized on: the api and STAGE its own SDK client was configured with, end to end over
//! the assembled router.
//!
//! > **RULE: an AWS SDK client a handler builds with an explicit endpoint sends its calls to
//! > THAT endpoint, so an api whose resource is named only by its endpoint — API Gateway's
//! > `@connections` surface, whose operations take no api and no stage member — is addressed
//! > at, and authorized on, the stage that endpoint names: the push is served only where a
//! > readable identity-based document allows `execute-api:ManageConnections` on
//! > `arn:aws:execute-api:{region}:{account}:{api-id}/{stage}/{method}/@connections/{connectionId}`
//! > — the route within that stage — and is refused `403 AccessDeniedException` otherwise,
//! > with nothing delivered.**
//!
//! Only a cross-service suite can pin it: the api, the stage and the deployment that make the
//! endpoint resolve a stage at all are API Gateway V2's records, the connection registry and
//! the decision are the API Gateway Management API's, the role the handler runs as is IAM's,
//! the session its calls are signed with is the token service's, and the handler and the SDK
//! seam it calls through are Lambda's. The Management API's own suites pin what an
//! unprivileged caller is answered when the stage is named in the REQUEST PATH
//! (`core/router/tests/all/apigatewaymanagementapi_manage_connections_authorization.rs`);
//! this suite owns the other way a caller names it — the endpoint its client was built with,
//! which is the only way a BACKEND names it.
//!
//! The rows differ along what the rule generalises over: the document in force on the
//! execution role (none at all, the action on every resource, on this very route, on the
//! stage's connections by wildcard, on another stage of the same api, another action of the
//! same service, and an explicit deny beside a grant of everything), the FORM of the endpoint
//! the client was built with (with and without a trailing slash on the stage path), and the
//! managed runtime whose SDK carries it (the Python client's `endpoint_url`, the JavaScript
//! client's `endpoint`). Every row is asserted on both halves: what the handler's own SDK was
//! answered, and what the registry holds for the connection afterwards.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

use crate::query_wire::{element, form_encode, outputs_of};

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for, and the one the api is created in.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, and the connection every push addresses — an id of the
/// shape API Gateway mints one in.
const STAGE: &str = "prod";
const CONNECTION_ID: &str = "L0SM9cOFyjUCIhA=";

/// The IAM action a `@connections` call asks for, the method `PostToConnection` is reached by,
/// and the code an `execute-api` endpoint words an authorization failure in.
const MANAGE_CONNECTIONS: &str = "execute-api:ManageConnections";
const POST: &str = "POST";
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The stack, the execution role and the two functions it declares — one per managed runtime
/// whose SDK this emulator supplies, because the endpoint is carried by each of those SDKs.
const STACK: &str = "websocket-pusher";
const ROLE: &str = "websocket-pusher-role";
const FUNCTION: &str = "websocket-pusher";
const NODE_FUNCTION: &str = "websocket-pusher-node";

/// The inline policy each row writes on the execution role.
const POLICY_NAME: &str = "manage-the-connections";

/// The payload every push carries.
const PAYLOAD: &str = "{\"action\":\"reply\"}";

/// The awsQuery API versions the setup services are called at.
const IAM_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// The stack status the setup waits for, and the function state an invocation needs.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ACTIVE: &str = "Active";

/// How long work the clock owes is waited for, and how often it is looked at. Generous: no row
/// measures a deadline.
const WAIT: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The handler the stack deploys: it pushes one payload to the connection the event names,
/// through a client built with the `@connections` ENDPOINT the event carries, and reports what
/// the SDK was answered.
const HANDLER: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    endpoint = event.get("endpoint") or None
    management = boto3.client("apigatewaymanagementapi", endpoint_url=endpoint)
    try:
        answered = management.post_to_connection(
            ConnectionId=event["connectionId"], Data=event["data"]
        )
    except botocore.exceptions.ClientError as error:
        return {
            "ok": True,
            "pushed": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            "endpoint": management.meta.endpoint_url,
        }
    return {
        "ok": True,
        "pushed": True,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "endpoint": management.meta.endpoint_url,
    }
"#;

/// The same handler on the JavaScript runtime, whose SDK carries the endpoint on the client's
/// own configuration (`new Client({ endpoint })`): the same rule, through the other SDK this
/// emulator supplies.
const NODE_HANDLER: &str = r#"
'use strict';

const {
    ApiGatewayManagementApiClient,
    PostToConnectionCommand,
} = require('@aws-sdk/client-apigatewaymanagementapi');

exports.handler = async (event) => {
    const client = new ApiGatewayManagementApiClient({ endpoint: event.endpoint });
    const reported = typeof client.config.endpoint === 'string' ? client.config.endpoint : '';
    try {
        const answered = await client.send(new PostToConnectionCommand({
            ConnectionId: event.connectionId,
            Data: event.data,
        }));
        return {
            ok: true,
            pushed: true,
            status: answered.$metadata.httpStatusCode,
            endpoint: reported,
        };
    } catch (error) {
        return {
            ok: true,
            pushed: false,
            code: error.name,
            message: error.message === undefined ? '' : error.message,
            status: error.$metadata === undefined ? 0 : error.$metadata.httpStatusCode,
            endpoint: reported,
        };
    }
};
"#;

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
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
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One JSON call of `service` at `host`, answered with the status and the parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(body) => Bytes::from(body.to_string()),
        None => Bytes::new(),
    };
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization(service))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One API Gateway V2 control-plane call, which must answer `expected`.
async fn control_plane(
    router: &MultiServiceRouter,
    operation: &str,
    path: &str,
    body: &Value,
    expected: u16,
) -> Value {
    let (status, answered) = json_call(
        router,
        "apigateway",
        &format!("apigateway.{REGION}.amazonaws.com"),
        "POST",
        path,
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        status, expected,
        "setup: {operation} must answer {expected}, got {status} with {answered}"
    );
    answered
}

/// One Lambda `restJson1` call.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    json_call(
        router,
        "lambda",
        &format!("lambda.{REGION}.amazonaws.com"),
        method,
        uri,
        body,
    )
    .await
}

/// A member of a JSON answer, as text.
fn as_str(what: &str, answered: &Value, member: &str) -> String {
    answered
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: {what} must report {member}, got {answered}"))
        .to_string()
}

/// A WEBSOCKET api with one DEPLOYED stage — what makes an `execute-api` endpoint serve that
/// stage at all. Answers the api's id.
async fn deployed_websocket_api(router: &MultiServiceRouter) -> String {
    let created = control_plane(
        router,
        "CreateApi",
        "/v2/apis",
        &json!({
            "name": "handler-pushes-to-its-connections",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
        201,
    )
    .await;
    let api_id = as_str("CreateApi", &created, "apiId");
    let integration = control_plane(
        router,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        &json!({ "integrationType": "MOCK" }),
        201,
    )
    .await;
    let integration_id = as_str("CreateIntegration", &integration, "integrationId");
    control_plane(
        router,
        "CreateRoute",
        &format!("/v2/apis/{api_id}/routes"),
        &json!({
            "routeKey": "$connect",
            "target": format!("integrations/{integration_id}")
        }),
        201,
    )
    .await;
    let deployment = control_plane(
        router,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        &json!({}),
        201,
    )
    .await;
    let deployment_id = as_str("CreateDeployment", &deployment, "deploymentId");
    control_plane(
        router,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        &json!({ "stageName": STAGE, "deploymentId": deployment_id }),
        201,
    )
    .await;
    api_id
}

/// The stack that declares the execution role and the function: the role trusts Lambda and
/// carries NO identity-based document of its own — what each row may do is written on it
/// through `iam:PutRolePolicy` below.
fn handler_stack() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "PusherRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "Pusher": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": {"Fn::GetAtt": ["PusherRole", "Arn"]},
                    "Timeout": 30,
                    "Code": {"ZipFile": HANDLER},
                },
            },
            "NodePusher": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": NODE_FUNCTION,
                    "Runtime": "nodejs22.x",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["PusherRole", "Arn"]},
                    "Timeout": 30,
                    "Code": {"ZipFile": NODE_HANDLER},
                },
            },
        },
        "Outputs": {
            "FunctionName": {"Value": {"Ref": "Pusher"}},
            "NodeFunctionName": {"Value": {"Ref": "NodePusher"}},
        },
    })
    .to_string()
}

/// What `DescribeStacks` reports for `stack`: its status and the whole body.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (_status, body) = query(
        router,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        "DescribeStacks",
        &[("StackName", stack)],
    )
    .await;
    (element(&body, "StackStatus"), body)
}

/// Create the stack, wait for `CREATE_COMPLETE` and answer its outputs.
async fn deployed_stack(
    router: &MultiServiceRouter,
    stack: &str,
    template: &str,
) -> BTreeMap<String, String> {
    let (status, body) = query(
        router,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({stack:?}) must be accepted, got {status} with {body}"
    );
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut reported = stack_status(router, stack).await;
    for _ in 0..looks {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
        reported = stack_status(router, stack).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {:?} with {}",
        reported.0, reported.1
    );
    outputs_of(&reported.1)
}

/// Wait until Lambda reports the function `Active` — the state an invocation needs. The span a
/// created function spends coming up is that service's own, so nothing here sleeps a fixed
/// span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut state = String::new();
    for _ in 0..looks {
        let (_status, body) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        state = body["State"].as_str().unwrap_or_default().to_string();
        if state == ACTIVE {
            return;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: the function {name:?} must reach {ACTIVE}, last reported {state:?}");
}

/// Put `document` on the execution role as its one inline policy, or take every policy off it
/// — what `iam:PutRolePolicy` and `iam:DeleteRolePolicy` do.
async fn given_policy(router: &MultiServiceRouter, document: Option<&str>) {
    match document {
        Some(document) => {
            let (status, body) = query(
                router,
                "iam",
                IAM_VERSION,
                "PutRolePolicy",
                &[
                    ("RoleName", ROLE),
                    ("PolicyName", POLICY_NAME),
                    ("PolicyDocument", document),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "setup: PutRolePolicy must be accepted, got {status} with {body}"
            );
        }
        // The role starts out carrying no inline policy at all, so the delete answers
        // `NoSuchEntity` for the first row that asks for none; what matters is the state it
        // leaves, which the rows below assert through what the call was answered.
        None => {
            query(
                router,
                "iam",
                IAM_VERSION,
                "DeleteRolePolicy",
                &[("RoleName", ROLE), ("PolicyName", POLICY_NAME)],
            )
            .await;
        }
    }
}

/// An identity-based policy with one statement.
fn statement(effect: &str, action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }]
    })
    .to_string()
}

/// The resource ARN a push on `api_id` addresses: the route within the stage, whose connection
/// id is the route's own parameter.
fn resource(api_id: &str) -> String {
    format!(
        "arn:aws:execute-api:{REGION}:{account}:{api_id}/{STAGE}/{POST}/@connections/\
         {{connectionId}}",
        account = engine::emulator_account_id()
    )
}

/// The resource a REFUSAL names: the same ARN with the account masked down to its last four
/// digits, which is how an `execute-api` endpoint reports a resource to a caller it just
/// refused. Spelled out here rather than taken from the emulator.
fn reported_resource(api_id: &str) -> String {
    let account = engine::emulator_account_id();
    let last_four = account
        .get(account.len().saturating_sub(4)..)
        .expect("the account id's last four digits");
    format!(
        "arn:aws:execute-api:{REGION}:********{last_four}:{api_id}/{STAGE}/{POST}/@connections/\
         {{connectionId}}"
    )
}

/// The clause closing a refusal no readable document allowed, and the one closing a refusal a
/// readable document DENIED.
fn no_grant() -> String {
    format!("because no identity-based policy allows the {MANAGE_CONNECTIONS} action")
}
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// One row: the document in force on the execution role, the endpoint its client is built with,
/// and whether the push is served.
struct Row {
    case: &'static str,
    /// The document, built from the api id, or `None` for a role carrying none.
    document: fn(&str) -> Option<String>,
    /// The endpoint the handler's client is built with, built from the api id.
    endpoint: fn(&str) -> String,
    served: bool,
    /// The clause the refusal of a row closes with, when the row is refused.
    reason: Option<&'static str>,
}

/// The `@connections` endpoint of `api_id`'s stage, as AWS publishes it and as a backend's
/// client is configured with it.
fn stage_endpoint(api_id: &str) -> String {
    format!("https://{api_id}.execute-api.{REGION}.amazonaws.com/{STAGE}")
}

const ROWS: &[Row] = &[
    Row {
        case: "no document at all",
        document: |_api| None,
        endpoint: stage_endpoint,
        served: false,
        reason: None,
    },
    Row {
        case: "the action on every resource",
        document: |_api| Some(statement("Allow", MANAGE_CONNECTIONS, "*")),
        endpoint: stage_endpoint,
        served: true,
        reason: None,
    },
    Row {
        case: "the action on this very route",
        document: |api| Some(statement("Allow", MANAGE_CONNECTIONS, &resource(api))),
        endpoint: stage_endpoint,
        served: true,
        reason: None,
    },
    Row {
        case: "the stage's connections by wildcard, with a trailing slash on the endpoint",
        document: |api| {
            Some(statement(
                "Allow",
                MANAGE_CONNECTIONS,
                &format!(
                    "arn:aws:execute-api:{REGION}:{account}:{api}/{STAGE}/{POST}/@connections/*",
                    account = engine::emulator_account_id()
                ),
            ))
        },
        endpoint: |api| format!("{}/", stage_endpoint(api)),
        served: true,
        reason: None,
    },
    Row {
        case: "the action on another stage of the same api",
        document: |api| {
            Some(statement(
                "Allow",
                MANAGE_CONNECTIONS,
                &format!(
                    "arn:aws:execute-api:{REGION}:{account}:{api}/other/{POST}/@connections/*",
                    account = engine::emulator_account_id()
                ),
            ))
        },
        endpoint: stage_endpoint,
        served: false,
        reason: None,
    },
    Row {
        case: "another action of the same service on every resource",
        document: |_api| Some(statement("Allow", "execute-api:Invoke", "*")),
        endpoint: stage_endpoint,
        served: false,
        reason: None,
    },
    Row {
        case: "an explicit deny beside a grant of everything",
        document: |_api| {
            Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Allow", "Action": MANAGE_CONNECTIONS, "Resource": "*" },
                        { "Effect": "Deny", "Action": MANAGE_CONNECTIONS, "Resource": "*" }
                    ]
                })
                .to_string(),
            )
        },
        endpoint: stage_endpoint,
        served: false,
        reason: Some(EXPLICIT_DENY),
    },
];

/// Register the connection every push addresses, through the seams that make a connection
/// exist and end: whatever the previous row left is closed first, so each row addresses a
/// connection of its own and the counters below are that row's.
fn register(engine: &Arc<engine::InMemoryEngine>) {
    let registry = mgmt::state::registry(&**engine).expect("setup: the connection registry");
    mgmt::state::close_connection(&registry, CONNECTION_ID);
    assert!(
        mgmt::state::register_connection(
            &**engine,
            CONNECTION_ID,
            mgmt::state::Identity {
                source_ip: "192.0.2.40".to_string(),
                user_agent: Some("agent/1".to_string()),
            },
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        ),
        "setup: the registration seam must register the connection"
    );
}

/// What the registry holds for the connection: how many payloads were delivered to it, and
/// the last of them as text.
fn delivered(engine: &Arc<engine::InMemoryEngine>) -> (u64, String) {
    let held = mgmt::state::connection(&**engine, CONNECTION_ID)
        .expect("the connection must still be registered");
    let last = held
        .messages
        .last()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    (held.delivered_count, last)
}

/// Drive ONE row against the function `function` on `runtime`: push through the handler and
/// assert both halves — what its own SDK was answered, and what the registry holds for the
/// connection afterwards.
async fn drive(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    runtime: &str,
    function: &str,
    api_id: &str,
    row: &Row,
) {
    let case = format!("{runtime}: {}", row.case);
    register(engine);
    given_policy(router, (row.document)(api_id).as_deref()).await;
    let endpoint = (row.endpoint)(api_id);

    let (status, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        Some(json!({
            "endpoint": endpoint,
            "connectionId": CONNECTION_ID,
            "data": PAYLOAD,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    assert_eq!(
        payload.get("ok").and_then(Value::as_bool),
        Some(true),
        "case {case:?}: the handler must have run to completion, got {payload}"
    );
    // The client reports the endpoint it was built with, which is where its calls go.
    assert_eq!(
        payload.get("endpoint").and_then(Value::as_str),
        Some(endpoint.as_str()),
        "case {case:?}: expected the client to report {endpoint:?}, got {payload}"
    );

    let (count, last) = delivered(engine);
    if row.served {
        assert_eq!(
            payload.get("pushed").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: the push must be served, got {payload}"
        );
        assert_eq!(
            payload.get("status").and_then(Value::as_i64),
            Some(200),
            "case {case:?}: expected the operation's own 200, got {payload}"
        );
        assert_eq!(
            (count, last.as_str()),
            (1, PAYLOAD),
            "case {case:?}: expected one delivery of {PAYLOAD:?}, got {count} with {last:?}"
        );
        return;
    }
    assert_eq!(
        payload.get("pushed").and_then(Value::as_bool),
        Some(false),
        "case {case:?}: the push must be refused, got {payload}"
    );
    assert_eq!(
        payload.get("status").and_then(Value::as_i64),
        Some(403),
        "case {case:?}: expected HTTP 403, got {payload}"
    );
    assert_eq!(
        payload.get("code").and_then(Value::as_str),
        Some(ACCESS_DENIED),
        "case {case:?}: expected the code {ACCESS_DENIED}, got {payload}"
    );
    let reason = row.reason.map_or_else(no_grant, str::to_string);
    let expected = format!(
        "is not authorized to perform: {MANAGE_CONNECTIONS} on resource: {} {reason}",
        reported_resource(api_id)
    );
    let message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        message.contains(&expected),
        "case {case:?}: expected a refusal carrying {expected:?}, got {message:?}"
    );
    // The decision precedes the connection store: a refused push delivers nothing.
    assert_eq!(
        count, 0,
        "case {case:?}: a refused push delivers nothing, got {count} deliveries"
    );
}

/// The function one output of the stack named, falling back on the name it was declared with.
fn named(outputs: &BTreeMap<String, String>, output: &str, declared: &str) -> String {
    outputs
        .get(output)
        .cloned()
        .unwrap_or_else(|| declared.to_string())
}

#[test]
fn a_handlers_push_is_authorized_on_the_stage_its_client_was_built_with() {
    std::thread::Builder::new()
        .name("lambda-handler-connections-endpoint".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // `enable_io`: a WEBSOCKET api's data plane is a socket the core opens when the api
            // is created, so the runtime this suite creates one on has to bind one.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
                let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
                // What is under test is the AUTHORIZATION of a credential, not its signature.
                router.set_request_authentication(false);

                let api_id = deployed_websocket_api(&router).await;
                let outputs = deployed_stack(&router, STACK, &handler_stack()).await;
                let python = named(&outputs, "FunctionName", FUNCTION);
                let node = named(&outputs, "NodeFunctionName", NODE_FUNCTION);
                wait_until_active(&router, &python).await;
                wait_until_active(&router, &node).await;

                for row in ROWS {
                    drive(&router, &engine, "python3.12", &python, &api_id, row).await;
                }
                // The same rule through the OTHER SDK this emulator supplies, which carries the
                // endpoint on the client's own configuration: one served row and one refused.
                for row in [&ROWS[0], &ROWS[1]] {
                    drive(&router, &engine, "nodejs22.x", &node, &api_id, row).await;
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
