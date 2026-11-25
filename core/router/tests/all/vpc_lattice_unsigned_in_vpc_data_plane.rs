// SPDX-License-Identifier: CC0-1.0

//! The address a VPC Lattice service ANNOUNCES, dialled by a running Lambda function in the
//! associated VPC, across the services that own the halves of it: VPC Lattice announces the
//! address and holds the listener, the target group and the associations, CloudFormation and
//! IAM deploy the two functions, and Lambda runs them — so the suite lives here.
//!
//! The RULE under test, in one sentence: the address a service's control plane ANNOUNCES for
//! clients to reach it at (`GetService`'s `dnsEntry.domainName`, "the public DNS name of the
//! service") is an address this emulator ANSWERS that service's traffic at, so an ordinary
//! unsigned request a client of the associated VPC sends to it is matched by the listener's
//! `defaultAction`, forwarded to the `LAMBDA` target group and answered from what the
//! registered function returned.
//!
//! The rows differ along what the rule generalises over: the path the client addresses, the
//! method it sends, and the ADDRESS form it dials (the announced name on its own, and the
//! same name with the listener's port written out) — each a different way a client writes
//! the one address the control plane handed it.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The account this emulator serves.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The role both functions run as.
const ROLE: &str = "xsvc-lattice-unsigned-role";

/// The functions: the TARGET behind the listener, and the DRIVER that dials the announced
/// address from inside the VPC.
const TARGET_FUNCTION: &str = "xsvc-lattice-unsigned-target";
const DRIVER_FUNCTION: &str = "xsvc-lattice-unsigned-driver";

/// The stack that deploys them.
const STACK: &str = "xsvc-lattice-unsigned-stack";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The terminal status the lattice resources must reach before the path is servable.
const ACTIVE: &str = "ACTIVE";

/// The value the target echoes back, so a body a row asserts on can only have come from the
/// function's own run.
const NONCE: &str = "n0nce-7f3a";

/// How often a read looks again for work another component's CLOCK owes — a stack reaching
/// [`CREATE_COMPLETE`], a service or a target group reaching [`ACTIVE`] — and how many looks
/// it takes. Generous: the suite waits for the CONDITION and asserts nothing about how long
/// it took.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the answer the data plane gives and not
/// how fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The TARGET the listener forwards to: it answers the `{statusCode, headers, body}` object
/// the forward's contract gives it, with a body computed from the event's own path — so the
/// body the client reads can only come from a run that was handed this request.
const TARGET_HANDLER: &str = r#"
import os


def handler(event, context):
    return {
        "isBase64Encoded": False,
        "statusCode": 200,
        "statusDescription": "200 OK",
        "headers": {"Content-Type": "text/plain"},
        "body": os.environ["NONCE"] + ":" + (event.get("path") or ""),
    }
"#;

/// The DRIVER: a function in the associated VPC that sends ONE ordinary, unsigned HTTP
/// request to the address it is handed and reports what came back — so a refusal is the
/// probe's answer rather than an exception that hides it.
const DRIVER_HANDLER: &str = r#"
import urllib.error
import urllib.request


def handler(event, context):
    sending = urllib.request.Request(event["url"], method=event.get("method") or "GET")
    sending.add_header("Host", event["host"])
    try:
        with urllib.request.urlopen(sending) as answered:
            return {"status": answered.status, "body": answered.read().decode("utf-8")}
    except urllib.error.HTTPError as refused:
        return {"status": refused.code, "body": refused.read().decode("utf-8")}
    except Exception as error:
        return {"status": 0, "body": "%s: %s" % (type(error).__name__, error)}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the request the driver sends is dispatched
/// back into this same router, so the service it reaches is the service this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
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

/// One `restJson1` call of the service `signing` names, as its status and parsed body.
async fn rest(
    router: &MultiServiceRouter,
    signing: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
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

/// One VPC Lattice call that must succeed with `expected`.
async fn lattice_ok(
    router: &MultiServiceRouter,
    expected: u16,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Value {
    let (status, answered) = rest(router, "vpc-lattice", method, uri, body).await;
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
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member} in {value}"))
        .to_string()
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

/// Create the execution role and the stack that deploys both functions.
async fn deployed_functions(router: &MultiServiceRouter) {
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", ROLE),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole must succeed, got {status} with {created}"
    );
    let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}");
    let function = |name: &str, source: &str| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Role": role_arn,
                "Timeout": FUNCTION_TIMEOUT,
                "Environment": {"Variables": {"NONCE": NONCE}},
                "Code": {"ZipFile": source},
            },
        })
    };
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Target": function(TARGET_FUNCTION, TARGET_HANDLER),
            "Driver": function(DRIVER_FUNCTION, DRIVER_HANDLER),
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

/// The function ARN Lambda minted for `name`.
async fn function_arn(router: &MultiServiceRouter, name: &str) -> String {
    let (status, read) = rest(
        router,
        "lambda",
        "GET",
        &format!("/2015-03-31/functions/{name}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetFunction {name} must succeed, got {status} with {read}"
    );
    read["Configuration"]["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: GetFunction {name} reported no ARN: {read}"))
        .to_string()
}

/// Look again until the lattice resource at `path` reports [`ACTIVE`].
async fn settled_active(router: &MultiServiceRouter, path: &str) {
    for _ in 0..LOOKS {
        let read = lattice_ok(router, 200, "GET", path, None).await;
        if read["status"].as_str() == Some(ACTIVE) {
            return;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: {path} never reported {ACTIVE}");
}

/// The whole control plane of the recorded shape, answering the address `GetService`
/// announces for clients to reach the service at.
async fn announced_address(router: &MultiServiceRouter) -> String {
    let target = function_arn(router, TARGET_FUNCTION).await;
    let group = lattice_ok(
        router,
        201,
        "POST",
        "/targetgroups",
        Some(&json!({
            "name": "xsvc-lattice-unsigned-tg",
            "type": "LAMBDA",
            "config": {"lambdaEventStructureVersion": "V2"},
        })),
    )
    .await;
    let group_id = member("the target group", &group, "id");
    settled_active(router, &format!("/targetgroups/{group_id}")).await;
    let registered = lattice_ok(
        router,
        200,
        "POST",
        &format!("/targetgroups/{group_id}/registertargets"),
        Some(&json!({ "targets": [{ "id": target }] })),
    )
    .await;
    assert_eq!(
        registered["successful"][0]["id"].as_str(),
        Some(target.as_str()),
        "setup: RegisterTargets must report the function ARN successful, got {registered}"
    );

    let network = lattice_ok(
        router,
        201,
        "POST",
        "/servicenetworks",
        Some(&json!({ "name": "xsvc-lattice-unsigned-net", "authType": "NONE" })),
    )
    .await;
    let network_id = member("the service network", &network, "id");
    let service = lattice_ok(
        router,
        201,
        "POST",
        "/services",
        Some(&json!({ "name": "xsvc-lattice-unsigned-svc", "authType": "NONE" })),
    )
    .await;
    let service_id = member("the service", &service, "id");
    settled_active(router, &format!("/services/{service_id}")).await;
    lattice_ok(
        router,
        201,
        "POST",
        &format!("/services/{service_id}/listeners"),
        Some(&json!({
            "name": "http",
            "protocol": "HTTP",
            "port": 80,
            "defaultAction": {
                "forward": { "targetGroups": [{ "targetGroupIdentifier": group_id }] },
            },
        })),
    )
    .await;
    let association = lattice_ok(
        router,
        200,
        "POST",
        "/servicenetworkserviceassociations",
        Some(&json!({
            "serviceIdentifier": service_id,
            "serviceNetworkIdentifier": network_id,
        })),
    )
    .await;
    let association_id = member("the service association", &association, "id");
    let vpc = lattice_ok(
        router,
        200,
        "POST",
        "/servicenetworkvpcassociations",
        Some(&json!({
            "serviceNetworkIdentifier": network_id,
            "vpcIdentifier": "vpc-0123456789abcdef0",
        })),
    )
    .await;
    let vpc_id = member("the VPC association", &vpc, "id");
    settled_active(
        router,
        &format!("/servicenetworkserviceassociations/{association_id}"),
    )
    .await;
    settled_active(
        router,
        &format!("/servicenetworkvpcassociations/{vpc_id}"),
    )
    .await;

    let read = lattice_ok(router, 200, "GET", &format!("/services/{service_id}"), None).await;
    let announced = read["dnsEntry"]["domainName"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: GetService announced no domainName: {read}"))
        .to_string();
    assert!(
        !announced.is_empty(),
        "setup: GetService must announce a domain name, got {read}"
    );
    announced
}

/// What the driver reported for one probe.
#[derive(Debug)]
struct Probed {
    status: u64,
    body: String,
}

/// Invoke the driver once against `url`, with `host` as the request's `Host`.
async fn probed(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    url: &str,
    host: &str,
) -> Probed {
    let event = json!({ "url": url, "host": host, "method": method });
    let (status, payload) = rest(
        router,
        "lambda",
        "POST",
        &format!("/2015-03-31/functions/{DRIVER_FUNCTION}/invocations"),
        Some(&event),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the driver invocation must be served, got {status} with {payload}"
    );
    Probed {
        status: payload["status"].as_u64().unwrap_or_default(),
        body: payload["body"].as_str().unwrap_or_default().to_string(),
    }
}

/// The rule: the announced address answers the service's own traffic, unsigned, from a
/// function in the associated VPC.
#[tokio::test]
async fn the_announced_address_answers_an_unsigned_in_vpc_request_from_its_lambda_target() {
    let router = build_router();
    deployed_functions(&router).await;
    let announced = announced_address(&router).await;

    // (case, method, the path the client addresses, the address form it dials)
    let rows: Vec<(&str, &str, String, String)> = vec![
        (
            "the announced name on its own",
            "GET",
            format!("/probe/{NONCE}"),
            announced.clone(),
        ),
        (
            "the same name with the listener's port written out",
            "GET",
            format!("/probe/{NONCE}"),
            format!("{announced}:80"),
        ),
        (
            "another path of the same service",
            "GET",
            "/orders/42".to_string(),
            announced.clone(),
        ),
        (
            "the root path",
            "GET",
            "/".to_string(),
            announced.clone(),
        ),
        (
            "a method the default rule forwards all the same",
            "POST",
            format!("/probe/{NONCE}"),
            announced.clone(),
        ),
    ];

    for (case, method, path, dialled) in rows {
        let url = format!("http://{dialled}{path}");
        let answered = probed(&router, case, method, &url, &dialled).await;
        assert_eq!(
            (answered.status, answered.body.as_str()),
            (200, format!("{NONCE}:{path}").as_str()),
            "case {case:?}: the unsigned request to {url:?} must be answered by the Lambda \
             target, got {answered:?}"
        );
    }
}
