// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING Lambda function's own SigV4-signed GET of a VPC Lattice service's ANNOUNCED
//! name is answered with, across the crates that own the halves of it: VPC Lattice announces
//! the name and holds the auth policy, the listener and the `LAMBDA` target group, IAM holds
//! the roles, STS mints the session the handler's request is made as, CloudFormation deploys
//! the two functions, and Lambda runs them — so the suite lives here.
//!
//! Two rules are pinned, which is what the whole path takes:
//!
//! * the name a service ANNOUNCES in its `dnsEntry` is a name this emulator ANSWERS that
//!   service at. A client that has read nothing but the control plane holds only that value,
//!   resolves it and dials it — which is what the function here does — so the announced name
//!   has to carry the service's own identity and a zone this emulator is reached in, be
//!   recorded for an in-process client to resolve, and be served by the service's own
//!   listener: the listener's `forward` selects the `LAMBDA` target group and the registered
//!   function's `{statusCode, headers, body}` becomes the HTTP response;
//! * the auth policy in force on a service whose `authType` is `AWS_IAM` decides that request
//!   before the listener acts. A policy that ALLOWS the caller's role for
//!   `vpc-lattice-svcs:Invoke` on the service has it served; replacing that statement with the
//!   identical one at `Effect=Deny` answers HTTP 403 whose body is the `AccessDeniedException`
//!   sentence naming the caller, the action and the service ARN with the request's path
//!   appended — whatever `Allow` the caller's own identity policy carries — and the target
//!   function is not invoked, so none of its answer reaches the client.
//!
//! The rows span what the second rule generalises over: the effect of the statement in force
//! (an `Allow`, the same statement at `Deny`, and the `Allow` put back), the resource it is
//! scoped to (every path of the service, and the exact path the request addresses), and the
//! principal it names (the caller's role and every principal), with the identity-based `Allow`
//! on the role held constant throughout — so a refusal can only be the auth policy's.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and the Region every ARN carries.
const REGION: &str = "us-west-2";

/// The account this emulator serves.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The service network and the service the probe reaches.
const NETWORK: &str = "xsvc-lattice-auth-net";
const SERVICE: &str = "xsvc-lattice-auth-shop";

/// The roles the two functions run as.
const TARGET_ROLE: &str = "xsvc-lattice-auth-target-role";
const DRIVER_ROLE: &str = "xsvc-lattice-auth-driver-role";

/// The functions that run as those roles: the target behind the listener, and the driver that
/// calls the announced name.
const TARGET_FUNCTION: &str = "xsvc-lattice-auth-target";
const DRIVER_FUNCTION: &str = "xsvc-lattice-auth-driver";

/// The stack that deploys both functions.
const STACK: &str = "xsvc-lattice-auth-stack";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status a Lattice resource serves its data plane in.
const ACTIVE: &str = "ACTIVE";

/// The `authType` values the two resources are created with
/// (`smithy_query(query="shape",shape="AuthType")`).
const AUTH_TYPE_NONE: &str = "NONE";
const AUTH_TYPE_AWS_IAM: &str = "AWS_IAM";

/// The action a client performs on a Lattice service's data plane, as an auth policy's
/// statements name it: the data plane's own action namespace and its one operation. Written
/// out here rather than read from the code under test.
const INVOKE_ACTION: &str = "vpc-lattice-svcs:Invoke";

/// The fixed clauses of the refusal AWS answers a denied request with. The variable parts —
/// the caller's ARN, the action and the resource — are the request's and the account's.
const DENIED_PREFIX: &str = "AccessDeniedException: User: ";
const DENIED_PERFORM: &str = " is not authorized to perform: ";
const DENIED_RESOURCE: &str = " on resource: ";
const DENIED_EXPLICIT_DENY: &str = " with an explicit deny in a service-based policy";

/// The nonce the target echoes, and the path the driver addresses: what the client is answered
/// with can only be the function's own if both come back in it.
const NONCE: &str = "lattice-auth-nonce";
const PROBE_PATH: &str = "/probe/lattice-auth-nonce";

/// How often a read looks again for work another component's CLOCK owes — a Lattice resource
/// settling into `ACTIVE`, a stack reaching [`CREATE_COMPLETE`] — and how many looks it takes.
/// Generous: the suite waits for the CONDITION and asserts nothing about how long it took.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the answer the data plane gives and not
/// how fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The handler behind the listener: it answers from the PATH the Lattice V2 event carries, so
/// its body can only reach the client if the forward reached it with this request.
const TARGET_HANDLER: &str = r#"
import os


def handler(event, context):
    return {
        "isBase64Encoded": False,
        "statusCode": 200,
        "statusDescription": "200 OK",
        "headers": {"Content-Type": "text/plain"},
        "body": os.environ["NONCE"] + ":" + event["path"],
    }
"#;

/// The handler that CALLS the service: it SIGNS one request of its own for the
/// `vpc-lattice-svcs` signing name with the credentials the runtime hands it, sends it to the
/// announced name it was given, and reports the status and body it was answered — so a refusal
/// is the probe's answer rather than an exception.
const DRIVER_HANDLER: &str = r#"
import os
import urllib.error
import urllib.request

import botocore.session
from botocore.auth import SigV4Auth
from botocore.awsrequest import AWSRequest


def handler(event, context):
    url = event["url"]
    signed = AWSRequest(method="GET", url=url, headers={"Host": event["host"]})
    credentials = botocore.session.get_session().get_credentials()
    SigV4Auth(credentials, "vpc-lattice-svcs", os.environ["AWS_REGION"]).add_auth(signed)
    sending = urllib.request.Request(signed.url, method="GET")
    for name, value in signed.headers.items():
        sending.add_header(name, value)
    try:
        with urllib.request.urlopen(sending) as answered:
            return {"status": answered.status, "body": answered.read().decode("utf-8")}
    except urllib.error.HTTPError as refused:
        return {"status": refused.code, "body": refused.read().decode("utf-8")}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the request the handler signs is dispatched
/// back into this same router, so the service it reaches is the service this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One IAM call that must succeed.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, members).await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} must succeed, got {status} with {body}"
    );
    body
}

/// One `restJson1` call to the service `signing` names, as its status and its parsed body.
async fn call(
    router: &MultiServiceRouter,
    signing: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| value.to_string().into_bytes())
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing}.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(signing))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    (status, parsed)
}

/// One VPC Lattice call.
async fn lattice(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    call(router, "vpc-lattice", method, uri, body).await
}

/// One VPC Lattice call that must answer `expected`.
async fn lattice_ok(
    router: &MultiServiceRouter,
    expected: u16,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Value {
    let (status, answered) = lattice(router, method, uri, body).await;
    assert_eq!(
        status, expected,
        "setup: vpc-lattice {method} {uri} must answer {expected}, got {status} with {answered}"
    );
    answered
}

/// The string member `member` of `value`.
fn member(case: &str, value: &Value, member: &str) -> String {
    value[member]
        .as_str()
        .unwrap_or_else(|| panic!("{case}: expected a string {member:?}, got {value}"))
        .to_string()
}

/// An ARN used as an `@httpLabel`, encoded the way an AWS SDK encodes a non-greedy label.
fn as_label(value: &str) -> String {
    value.replace('/', "%2F")
}

/// The ARN of a role of this account.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The trust policy a Lambda execution role needs.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The driver role's own identity-based grant: the inline `Allow` for the data plane's action
/// on every path of the service. It is held constant for every row, so a row that is refused
/// is refused by the AUTH POLICY and not by this.
fn identity_grant(service_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": INVOKE_ACTION,
            "Resource": format!("{service_arn}/*"),
        }],
    })
    .to_string()
}

/// One auth policy document: `effect` for `principal` on `resource`, over the data plane's own
/// action.
fn auth_policy(effect: &str, principal: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": {"AWS": [principal]},
            "Action": INVOKE_ACTION,
            "Resource": resource,
        }],
    })
    .to_string()
}

/// Look again until the Lattice resource at `uri` reports `ACTIVE`, or the looks run out.
async fn settled_active(router: &MultiServiceRouter, case: &str, uri: &str) {
    for _ in 0..LOOKS {
        let read = lattice_ok(router, 200, "GET", uri, None).await;
        if read["status"].as_str() == Some(ACTIVE) {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: {case} at {uri} never reported {ACTIVE}");
}

/// The control plane of the whole estate, in the order a client builds it: the service
/// network, the `LAMBDA` target group holding `target_arn`, the listener that forwards to it,
/// and the two associations that put a client in a position to reach the service at all.
/// Answers the name `GetService` announces for the service once it is `ACTIVE`.
async fn fronted_service(
    router: &MultiServiceRouter,
    service_id: &str,
    target_arn: &str,
) -> String {
    let network = lattice_ok(
        router,
        201,
        "POST",
        "/servicenetworks",
        Some(json!({ "name": NETWORK, "authType": AUTH_TYPE_NONE })),
    )
    .await;
    let network_id = member("setup CreateServiceNetwork", &network, "id");

    // The `LAMBDA` target group holding the function, and the listener whose default action
    // forwards to it.
    let group = lattice_ok(
        router,
        201,
        "POST",
        "/targetgroups",
        Some(json!({
            "name": "xsvc-lattice-auth-tg",
            "type": "LAMBDA",
            "config": { "lambdaEventStructureVersion": "V2" },
        })),
    )
    .await;
    let group_id = member("setup CreateTargetGroup", &group, "id");
    settled_active(router, "the target group", &format!("/targetgroups/{group_id}")).await;
    let registered = lattice_ok(
        router,
        200,
        "POST",
        &format!("/targetgroups/{group_id}/registertargets"),
        Some(json!({ "targets": [{ "id": target_arn }] })),
    )
    .await;
    let successful: Vec<String> = registered["successful"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|target| target["id"].as_str().map(str::to_string))
        .collect();
    assert!(
        successful.iter().any(|id| id == target_arn),
        "setup RegisterTargets must report the target function successful, got {registered}"
    );
    lattice_ok(
        router,
        201,
        "POST",
        &format!("/services/{service_id}/listeners"),
        Some(json!({
            "name": "http",
            "protocol": "HTTP",
            "port": 80,
            "defaultAction": {
                "forward": { "targetGroups": [{ "targetGroupIdentifier": group_id }] },
            },
        })),
    )
    .await;

    // The two associations that put a client in a position to reach the service at all.
    lattice_ok(
        router,
        200,
        "POST",
        "/servicenetworkserviceassociations",
        Some(json!({
            "serviceIdentifier": service_id,
            "serviceNetworkIdentifier": network_id,
        })),
    )
    .await;
    lattice_ok(
        router,
        200,
        "POST",
        "/servicenetworkvpcassociations",
        Some(json!({
            "serviceNetworkIdentifier": network_id,
            "vpcIdentifier": "vpc-0123456789abcdef0",
        })),
    )
    .await;
    settled_active(router, "the service", &format!("/services/{service_id}")).await;

    // What a client that has read nothing but the control plane holds.
    let read = lattice_ok(router, 200, "GET", &format!("/services/{service_id}"), None).await;
    let announced = read["dnsEntry"]["domainName"]
        .as_str()
        .unwrap_or_else(|| panic!("setup GetService must announce a dnsEntry.domainName, got {read}"))
        .to_string();
    assert!(
        announced.contains('.') && !announced.contains(':') && !announced.contains('/'),
        "setup GetService must announce a DNS name carrying no port and no path, got \
         {announced:?}"
    );
    announced
}

/// Create both roles and the stack that deploys the two functions.
async fn deployed_functions(router: &MultiServiceRouter, service_arn: &str) {
    for name in [TARGET_ROLE, DRIVER_ROLE] {
        let created = iam(
            router,
            "CreateRole",
            &[
                ("RoleName", name),
                ("AssumeRolePolicyDocument", &trust_policy()),
            ],
        )
        .await;
        assert_eq!(
            opt_element(&created, "Arn"),
            Some(role_arn(name)),
            "setup: the ARN IAM minted for {name:?} is the one the policies name, got {created}"
        );
    }
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", DRIVER_ROLE),
            ("PolicyName", "invoke-lattice"),
            ("PolicyDocument", &identity_grant(service_arn)),
        ],
    )
    .await;
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Target": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": TARGET_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn(TARGET_ROLE),
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {"NONCE": NONCE}},
                    "Code": {"ZipFile": TARGET_HANDLER},
                },
            },
            "Driver": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": DRIVER_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn(DRIVER_ROLE),
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": DRIVER_HANDLER},
                },
            },
        },
    })
    .to_string();
    let (status, created) = query(
        router,
        "cloudformation",
        "2010-05-15",
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
        "setup: CreateStack must be accepted, got {status} with {created}"
    );
    let mut described = String::new();
    for _ in 0..LOOKS {
        let (_, body) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        described = body;
        if opt_element(&described, "StackStatus").as_deref() == Some(CREATE_COMPLETE) {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: the functions' stack must reach {CREATE_COMPLETE}, got {described}");
}

/// The ARN Lambda minted for the function `name`.
async fn function_arn(router: &MultiServiceRouter, name: &str) -> String {
    let (status, read) = call(
        router,
        "lambda",
        "GET",
        &format!("/2015-03-31/functions/{name}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetFunction({name:?}) must succeed, got {status} with {read}"
    );
    member("setup GetFunction", &read["Configuration"], "FunctionArn")
}

/// What one probe reports: the status the data plane answered and the body it carried.
#[derive(Debug)]
struct Answered {
    status: u64,
    body: String,
    /// The whole payload, for a failure message.
    payload: Value,
}

/// Invoke the driver once against `url`, and answer what the data plane told it.
async fn probed(router: &MultiServiceRouter, case: &str, host: &str) -> Answered {
    let event = json!({
        "url": format!("http://{host}{PROBE_PATH}"),
        "host": host,
    });
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/2015-03-31/functions/{DRIVER_FUNCTION}/invocations"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(event.to_string().into_bytes()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let payload: Value = serde_json::from_slice(&body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()));
    assert_eq!(
        parts.status.as_u16(),
        200,
        "case {case:?}: the invocation must be served, got {} with {payload}",
        parts.status
    );
    let faulted = parts
        .headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        faulted, None,
        "case {case:?}: the handler must reach the announced name and run to the end, got \
         {faulted:?} with {payload}"
    );
    Answered {
        status: payload["status"].as_u64().unwrap_or_default(),
        body: payload["body"].as_str().unwrap_or_default().to_string(),
        payload,
    }
}

/// `PUT /authpolicy/{resourceIdentifier}`, whose `policy` is the document as JSON text.
async fn put_auth_policy(router: &MultiServiceRouter, case: &str, identifier: &str, policy: &str) {
    let answered = lattice_ok(
        router,
        200,
        "PUT",
        &format!("/authpolicy/{}", as_label(identifier)),
        Some(json!({ "policy": policy })),
    )
    .await;
    assert_eq!(
        answered["state"].as_str(),
        Some("Active"),
        "case {case:?}: a policy on an AWS_IAM service is Active, got {answered}"
    );
}

/// The whole path: the announced name reaches the service's Lambda target from inside a
/// running function, and the auth policy in force decides every such request.
#[tokio::test]
async fn an_auth_policy_decides_a_functions_signed_get_of_the_announced_name() {
    let router = build_router();
    // The service is created first: the driver's identity grant and every auth policy below
    // are written against the ARN its own create minted.
    let service = lattice_ok(
        &router,
        201,
        "POST",
        "/services",
        Some(json!({ "name": SERVICE, "authType": AUTH_TYPE_AWS_IAM })),
    )
    .await;
    let service_id = member("setup CreateService", &service, "id");
    let service_arn = member("setup CreateService", &service, "arn");
    // The roles and the two functions, and then the rest of the estate in front of the
    // service: the target group registers the ARN Lambda minted for the deployed target.
    deployed_functions(&router, &service_arn).await;
    let target_arn = function_arn(&router, TARGET_FUNCTION).await;
    let announced = fronted_service(&router, &service_id, &target_arn).await;

    let whole_service = format!("{service_arn}/*");
    let addressed = format!("{service_arn}{PROBE_PATH}");
    let expected_body = format!("{NONCE}:{PROBE_PATH}");

    /// What one row installs, and what the plane must answer it.
    struct Row {
        case: &'static str,
        effect: &'static str,
        /// `true` where the statement names every principal rather than the caller's role.
        every_principal: bool,
        /// `true` where the statement is scoped to the exact path the request addresses.
        exact_path: bool,
        status: u64,
    }
    let rows = &[
        Row {
            case: "an Allow naming the caller's role on every path of the service",
            effect: "Allow",
            every_principal: false,
            exact_path: false,
            status: 200,
        },
        Row {
            case: "the same statement at Effect=Deny",
            effect: "Deny",
            every_principal: false,
            exact_path: false,
            status: 403,
        },
        Row {
            case: "a Deny scoped to the exact path the request addresses",
            effect: "Deny",
            every_principal: false,
            exact_path: true,
            status: 403,
        },
        Row {
            case: "a Deny naming every principal",
            effect: "Deny",
            every_principal: true,
            exact_path: false,
            status: 403,
        },
        Row {
            case: "the Allow put back",
            effect: "Allow",
            every_principal: false,
            exact_path: false,
            status: 200,
        },
    ];

    for row in rows {
        let principal = if row.every_principal {
            "*".to_string()
        } else {
            role_arn(DRIVER_ROLE)
        };
        let resource = if row.exact_path {
            addressed.clone()
        } else {
            whole_service.clone()
        };
        put_auth_policy(
            &router,
            row.case,
            &service_id,
            &auth_policy(row.effect, &principal, &resource),
        )
        .await;

        let answered = probed(&router, row.case, &announced).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {} from the announced name {announced:?}, got {} with {}",
            row.case, row.status, answered.status, answered.payload
        );
        if row.status == 200 {
            // Served: the body is the TARGET's own, computed from the request the forward
            // handed it — it can reach the client no other way.
            assert_eq!(
                answered.body, expected_body,
                "case {:?}: expected the target's own body {expected_body:?}, got {} with {}",
                row.case, answered.body, answered.payload
            );
            continue;
        }
        // Refused: the sentence names the caller, the action and the service ARN with the
        // request's path appended, and the rule that refused it.
        let body = &answered.body;
        let session_prefix = format!("arn:aws:sts::{ACCOUNT}:assumed-role/{DRIVER_ROLE}/");
        let expected_tail = format!(
            "{DENIED_PERFORM}{INVOKE_ACTION}{DENIED_RESOURCE}{addressed}{DENIED_EXPLICIT_DENY}"
        );
        assert!(
            body.starts_with(&format!("{DENIED_PREFIX}{session_prefix}")),
            "case {:?}: the refusal must name the session of the role the driver runs as \
             ({session_prefix:?}), got {body:?}",
            row.case
        );
        assert!(
            body.ends_with(&expected_tail),
            "case {:?}: expected the refusal to end {expected_tail:?}, got {body:?}",
            row.case
        );
        assert!(
            !body.contains(&expected_body),
            "case {:?}: a refused request must not reach the target, yet its answer carries \
             the target's own body {expected_body:?}: {body:?}",
            row.case
        );
    }
}
