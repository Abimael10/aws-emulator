// SPDX-License-Identifier: CC0-1.0

//! A `lambda` target behind an Application Load Balancer on a router that VERIFIES request
//! signatures — the configuration the emulator serves traffic in.
//!
//! RULE under test: **a request a client sends to the address a load balancer publishes is
//! served by INVOKING the function its `lambda` target group registers; the client is
//! answered from the object that function returned only while that object satisfies the
//! Lambda-target response contract (a readable status code, members of the shapes the
//! contract gives them, within the response size a Lambda target may answer with), and a
//! function that breaks the contract — or whose run raised — earns the LOAD BALANCER's own
//! HTTP 502 with its payload never passed on. The function RAN either way, exactly once per
//! request, and a contract violation costs that one request and no more: the registration is
//! still reported afterwards and the next well-formed request is served again.**
//!
//! `LoadBalancer$DNSName` is "the public DNS name of the load balancer",
//! `TargetGroup$TargetType` gives `lambda` as "Register a single Lambda function as a
//! target", and `TargetDescription$Id` is "the ARN of the Lambda function" for such a group
//! (`smithy_query(query="shape", shape="TargetGroup,TargetDescription")` on the
//! elasticloadbalancingv2 model); the 502 the load balancer generates for a response it
//! cannot interpret is recorded behaviour, as is the ALB→Lambda response contract itself.
//!
//! What this suite adds to the two that already pin that rule
//! (`elbv2_lambda_target_published_address.rs` here, and the crate-local
//! `alb_lambda_target_published_address.rs`) is the axis neither covers: both turn request
//! authentication OFF, so neither can see whether the two in-process calls this rule rests on
//! are accepted by a router that authenticates every request — the load balancer's own
//! `lambda:Invoke` of the registered function, and the SDK call the HANDLER makes while it
//! runs, which is how the suite knows the function ran at all. Every call below is signed
//! with the keys this process publishes, exactly as a client's SDK signs one.
//!
//! That the function ran is asserted where nothing of ELBv2 can fake it: the handler reports
//! every invocation to an Amazon SQS queue through its own SDK, as a session of its execution
//! role, BEFORE it decides what to return — so a row answered without the handler having been
//! reached leaves the queue empty and fails. The control leg takes that reading before any
//! load balancer exists, through Lambda's own `Invoke`.
//!
//! The rows differ along what the rule generalises over: the CLASS of contract violation (no
//! status code, a payload that is no response object at all, a status outside the HTTP range,
//! a response past the size limit, a run that raised) and the shape of a well-formed answer
//! (a body and media type of the function's own, no body at all).
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::query_wire::{element, form_encode, physical_ids_of};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for and addressed in.
const REGION: &str = "us-west-2";

/// The API versions the query protocols below carry.
const CFN_VERSION: &str = "2010-05-15";
const EC2_VERSION: &str = "2016-11-15";
/// The ELBv2 API version: the awsQuery envelope's `Version` is what names this model behind
/// the one Elastic Load Balancing endpoint both models are published on (see
/// `elb_classic_and_v2_signing_name_routing.rs`).
const ELB_VERSION: &str = "2015-12-01";

/// The signing names the calls below are signed for. Elastic Load Balancing publishes ONE
/// (`smithy_query(query="service_info")`: `sigv4Name` `elasticloadbalancing`), which is what
/// an SDK for either API signs, and what the router verifies a signature against.
const ELB_SIGNING_NAME: &str = "elasticloadbalancing";

/// The stack the queue, the execution role and the function are deployed as, and the names it
/// declares them under. CloudFormation archives the inline handler into a deployment package,
/// which is how a function whose source is written here comes to exist at all.
const STACK: &str = "authenticated-alb-lambda-estate";
const QUEUE: &str = "authenticated-alb-lambda-side-channel";
const ROLE: &str = "authenticated-alb-lambda-exec";
const FUNCTION: &str = "authenticated-alb-lambda-target";

/// The load balancer, the target group and the statement the invoke grant is written under.
const LOAD_BALANCER: &str = "auth-alb-lambda-lb";
const TARGET_GROUP: &str = "auth-alb-lambda-tg";
const STATEMENT: &str = "elbv2-invoke";

/// The status a load balancer answers for a Lambda-target response its contract refuses.
const BAD_GATEWAY: u16 = 502;

/// The body a well-formed answer carries, which is the function's OWN and must never be what
/// a refused response is answered with.
const TARGET_BODY: &str = "TARGET-OK";

/// The bodies the malformed answers carry, neither of which the load balancer may pass on.
const NO_STATUS_BODY: &str = "NO-STATUS-CODE";
const NON_JSON_PAYLOAD: &str = "not an alb response object at all";

/// The network the load balancer is placed in.
const VPC_CIDR: &str = "10.90.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.90.1.0/24", "us-west-2a"),
    ("10.90.2.0/24", "us-west-2b"),
];

/// The path the control leg's direct `Invoke` names, which is what the side channel must
/// report for it.
const CONTROL_PATH: &str = "/direct";

/// How long a wait for work the clock owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: everything waited for happens in this process, so
/// a loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long one dial and one exchange on the published address may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The handler every leg runs. It reports the invocation to the side channel FIRST — so a leg
/// served without the handler having run leaves the queue empty — and then decides what to
/// return from the LAST SEGMENT of the path the event carries, so one package serves every
/// row.
const HANDLER_SOURCE: &str = r#"
import os

import boto3

LIMIT = 1024 * 1024


def handler(event, context):
    whole = event.get("path") or "/"
    path = whole.rsplit("/", 1)[-1]
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["QUEUE_URL"], MessageBody=whole
    )
    if path == "nostatus":
        return {"statusDescription": "200 OK", "body": "NO-STATUS-CODE"}
    if path == "nonjson":
        return "not an alb response object at all"
    if path == "badstatus":
        return {"statusCode": 7000, "body": "TARGET-OK"}
    if path == "toolarge":
        return {"statusCode": 200, "body": "x" * (LIMIT + 1)}
    if path == "raise":
        raise RuntimeError("the handler refused to answer")
    if path == "nobody":
        return {"statusCode": 204}
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "text/plain"},
        "body": "TARGET-OK",
    }
"#;

/// The credential every call below signs with: the pair this process publishes once request
/// authentication is on.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One signed form-encoded (query-protocol) call, as its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    signer: &Signer,
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
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.into_bytes())
        .expect("a well-formed request");
    let (parts, payload) = signer.sign(built, signing_name).into_parts();
    let response = router
        .dispatch(http::Request::from_parts(
            parts,
            bytes::Bytes::from(payload),
        ))
        .await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must succeed.
async fn cloudformation(
    router: &MultiServiceRouter,
    signer: &Signer,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        signer,
        "cloudformation",
        CFN_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CloudFormation {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One EC2 call that must succeed.
async fn ec2(
    router: &MultiServiceRouter,
    signer: &Signer,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, signer, "ec2", EC2_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One Elastic Load Balancing v2 call, as its status and its XML body: signed for the one
/// signing name the API publishes, with the envelope `Version` that names this model.
async fn elbv2_call(
    router: &MultiServiceRouter,
    signer: &Signer,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        signer,
        ELB_SIGNING_NAME,
        ELB_VERSION,
        action,
        members,
    )
    .await
}

/// One Elastic Load Balancing v2 call that must succeed.
async fn elbv2(
    router: &MultiServiceRouter,
    signer: &Signer,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = elbv2_call(router, signer, action, members).await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One signed Lambda `restJson1` call: its status, its parsed body and the function-error
/// header an invocation that failed carries.
async fn lambda(
    router: &MultiServiceRouter,
    signer: &Signer,
    method: &str,
    path: &str,
    body: &Value,
) -> (u16, Value, Option<String>) {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the request body serializes")
    };
    let host = format!("lambda.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    let (parts, payload) = signer.sign(built, "lambda").into_parts();
    let response = router
        .dispatch(http::Request::from_parts(
            parts,
            bytes::Bytes::from(payload),
        ))
        .await;
    let status = response.status().as_u16();
    let function_error = response
        .headers()
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(response.body()).to_string())
        })
    };
    (status, parsed, function_error)
}

/// One signed Amazon SQS call over its `awsJson1_0` wire.
async fn sqs(
    router: &MultiServiceRouter,
    signer: &Signer,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("sqs.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .body(body.to_string().into_bytes())
        .expect("a well-formed request");
    let (parts, payload) = signer.sign(built, "sqs").into_parts();
    let response = router
        .dispatch(http::Request::from_parts(
            parts,
            bytes::Bytes::from(payload),
        ))
        .await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The template the suite deploys: the side-channel queue, the execution role carrying the one
/// grant the handler's own call needs, and the function itself.
fn stack_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "SideChannel": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": QUEUE },
            },
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "report-invocations",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "sqs:SendMessage",
                                "Resource": { "Fn::GetAtt": ["SideChannel", "Arn"] },
                            }],
                        },
                    }],
                },
            },
            "Target": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Timeout": 30,
                    "Environment": {
                        "Variables": { "QUEUE_URL": { "Ref": "SideChannel" } },
                    },
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string()
}

/// Deploy that stack, wait for `CREATE_COMPLETE` and answer the physical id of each resource.
async fn deploy(router: &MultiServiceRouter, signer: &Signer) -> BTreeMap<String, String> {
    let template = stack_template();
    cloudformation(
        router,
        signer,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let described =
            cloudformation(router, signer, "DescribeStacks", &[("StackName", STACK)]).await;
        let reported = element(&described, "StackStatus");
        if reported == "CREATE_COMPLETE" {
            break;
        }
        assert!(
            reported == "CREATE_IN_PROGRESS" && Instant::now() < deadline,
            "setup: the stack must reach CREATE_COMPLETE, it reports {reported} in {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    let resources =
        cloudformation(router, signer, "DescribeStackResources", &[("StackName", STACK)]).await;
    physical_ids_of(&resources)
}

/// Every message the side channel holds, deleted as it is read so the next leg starts from an
/// empty queue — once at least one has arrived and one further look finds none, else every
/// message the budget allowed.
async fn reported(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
) -> Vec<String> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, received) = sqs(
            router,
            signer,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ReceiveMessage to answer 200, got {status} {received}"
        );
        let batch = received["Messages"].as_array().cloned().unwrap_or_default();
        for message in &batch {
            seen.push(message["Body"].as_str().unwrap_or_default().to_string());
            let handle = message["ReceiptHandle"].as_str().unwrap_or_default();
            let (status, deleted) = sqs(
                router,
                signer,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DeleteMessage to answer 200, got {status} {deleted}"
            );
        }
        if !seen.is_empty() && batch.is_empty() {
            return seen;
        }
        if Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look at `GetFunctionConfiguration` until Lambda reports the function `Active`, which is
/// what a caller waits for before invoking a function it has just created.
async fn wait_until_function_is_active(router: &MultiServiceRouter, signer: &Signer) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, configuration, _) = lambda(
            router,
            signer,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup GetFunctionConfiguration: expected 200, got {status} {configuration}"
        );
        if configuration["State"].as_str() == Some("Active") {
            return configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| panic!("setup: no FunctionArn in {configuration}"))
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "setup: the function never became Active, last answer {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look at `DescribeLoadBalancers` until the load balancer reports the state the clock owes
/// it, which is what a client waits for before sending traffic.
async fn wait_until_active(router: &MultiServiceRouter, signer: &Signer, arn: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = elbv2(
            router,
            signer,
            "DescribeLoadBalancers",
            &[("LoadBalancerArns.member.1", arn)],
        )
        .await;
        if body.contains("<Code>active</Code>") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the load balancer never became active, last answer {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What one answer on the published address carried: its status, and the whole answer as it
/// came off the connection.
struct Answered {
    status: u16,
    raw: String,
}

/// Dial the published address and exchange one request on it, as a client of that address
/// does: a connection in the clear, HTTP/1.1, carrying no AWS credential at all.
///
/// Only the PORT is taken from the published authority: the core binds its listeners on
/// loopback (`EMULATOR_BIND_ADDRESS`, `127.0.0.1` by default), so the dial is to this machine.
async fn exchange(case: &str, published: &str, target: &str) -> Answered {
    let port = published
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the published address {published:?} carries no port to dial")
        });
    let mut stream = tokio::time::timeout(
        EXCHANGE_TIMEOUT,
        tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| panic!("case {case:?}: {published} did not accept a connection"))
    .unwrap_or_else(|error| panic!("case {case:?}: {published} refused a connection: {error}"));
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {published}\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the published address did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    let raw = String::from_utf8_lossy(&answer).to_string();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: no status line in {raw:?}"));
    Answered { status, raw }
}

/// One row: the path the client asks for, what the load balancer must answer, and whether the
/// answer may carry the function's own well-formed body.
struct Row {
    case: &'static str,
    target: &'static str,
    status: u16,
    serves_target_body: bool,
}

/// Every row's answer must come from the records, and every row must have invoked the function
/// exactly once. The paths carry a PREFIX, as a client's own path does.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a response naming no statusCode",
            target: "/behind/nostatus",
            status: BAD_GATEWAY,
            serves_target_body: false,
        },
        Row {
            case: "a payload that is no response object at all",
            target: "/behind/nonjson",
            status: BAD_GATEWAY,
            serves_target_body: false,
        },
        Row {
            case: "a statusCode outside the HTTP range",
            target: "/behind/badstatus",
            status: BAD_GATEWAY,
            serves_target_body: false,
        },
        Row {
            case: "a response past the size a Lambda target may answer with",
            target: "/behind/toolarge",
            status: BAD_GATEWAY,
            serves_target_body: false,
        },
        Row {
            case: "a run that raised",
            target: "/behind/raise",
            status: BAD_GATEWAY,
            serves_target_body: false,
        },
        Row {
            case: "a well-formed response naming no body, which the contract allows",
            target: "/behind/nobody",
            status: 204,
            serves_target_body: false,
        },
    ]
}

/// RULE: on a router that verifies request signatures, the address a load balancer publishes
/// serves a `lambda` target group by invoking the registered function — the load balancer's
/// own 502 for a response its contract refuses, the function's own answer otherwise, the
/// invocation having happened either way, and the registration surviving every refusal.
#[test]
fn an_alb_serves_its_lambda_target_and_generates_the_502_on_an_authenticating_router() {
    std::thread::Builder::new()
        .name("elbv2-lambda-authenticated".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(drive());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// The whole scenario, against ONE assembled, authenticating router.
async fn drive() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // Every call below runs with request authentication ON, as the emulator serves traffic.
    router.set_request_authentication(true);
    // The address the emulator publishes its data port at, which is what every URL a service
    // hands a caller is built from — the queue URL this suite's handler is given among them.
    ::emulator_server::EmulatorService::serving_at(&router, "https://127.0.0.1:9297");
    let published_keys = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published_keys.access_key_id.clone(),
        secret: published_keys.secret_access_key.clone(),
    };

    let resources = deploy(&router, &signer).await;
    let queue_url = resources
        .get("SideChannel")
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack reports no queue, got {resources:?}"));
    let function_arn = wait_until_function_is_active(&router, &signer).await;

    // THE CONTROL LEG, before any load balancer exists: a synchronous `Invoke` runs the
    // handler to completion, so the report it makes through its own SDK — a call signed as a
    // session of its execution role, against this authenticating router — is on the queue by
    // the time the call answers.
    let (status, answered, function_error) = lambda(
        &router,
        &signer,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        &json!({ "path": CONTROL_PATH, "httpMethod": "POST" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "control: expected Invoke to answer 200, got {status} {answered}"
    );
    assert_eq!(
        function_error, None,
        "control: expected the run to report no function error, got {function_error:?} with \
         {answered}"
    );
    let seen = reported(&router, &signer, "control", &queue_url).await;
    assert_eq!(
        seen,
        vec![CONTROL_PATH.to_string()],
        "control: expected exactly one invocation reported for {CONTROL_PATH:?}, got {seen:?}"
    );

    // The network the load balancer is placed in, through EC2 itself. An internet-facing load
    // balancer can only be placed in a VPC that has an internet gateway attached.
    let vpc = element(
        &ec2(&router, &signer, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let gateway = element(
        &ec2(&router, &signer, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        &router,
        &signer,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let mut subnets: Vec<String> = Vec::new();
    for (cidr, zone) in SUBNET_CIDRS {
        let created = ec2(
            &router,
            &signer,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&created, "subnetId"));
    }
    let security_group = element(
        &ec2(
            &router,
            &signer,
            "CreateSecurityGroup",
            &[
                ("GroupName", "auth-alb-lambda-target"),
                ("GroupDescription", "the group of the load balancer"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );

    // The load balancer and the address it publishes for a client to dial.
    let created = elbv2(
        &router,
        &signer,
        "CreateLoadBalancer",
        &[
            ("Name", LOAD_BALANCER),
            ("Scheme", "internet-facing"),
            ("Subnets.member.1", subnets[0].as_str()),
            ("Subnets.member.2", subnets[1].as_str()),
            ("SecurityGroups.member.1", security_group.as_str()),
        ],
    )
    .await;
    let load_balancer_arn = element(&created, "LoadBalancerArn");
    let published = element(&created, "DNSName");

    // The `lambda` target group, the grant that admits the invocation, the registration of the
    // function and the listener that forwards to the group.
    let target_group = element(
        &elbv2(
            &router,
            &signer,
            "CreateTargetGroup",
            &[("Name", TARGET_GROUP), ("TargetType", "lambda")],
        )
        .await,
        "TargetGroupArn",
    );
    let (status, granted, _) = lambda(
        &router,
        &signer,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        &json!({
            "StatementId": STATEMENT,
            "Action": "lambda:InvokeFunction",
            "Principal": "elasticloadbalancing.amazonaws.com",
            "SourceArn": target_group,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission: expected 201, got {status} {granted}"
    );
    elbv2(
        &router,
        &signer,
        "RegisterTargets",
        &[
            ("TargetGroupArn", target_group.as_str()),
            ("Targets.member.1.Id", function_arn.as_str()),
        ],
    )
    .await;
    elbv2(
        &router,
        &signer,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer_arn.as_str()),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "forward"),
            (
                "DefaultActions.member.1.TargetGroupArn",
                target_group.as_str(),
            ),
        ],
    )
    .await;
    wait_until_active(&router, &signer, &load_balancer_arn).await;

    // The well-formed control request THROUGH the load balancer: the function's own answer,
    // and one invocation reported for the path the client asked for.
    let well_formed = "/behind/ok";
    let answer = exchange("a well-formed response", &published, well_formed).await;
    assert_eq!(
        answer.status, 200,
        "case {:?}: expected HTTP 200, got {} in {:?}",
        "a well-formed response", answer.status, answer.raw
    );
    assert!(
        answer.raw.contains(TARGET_BODY),
        "case {:?}: expected the function's own body {TARGET_BODY:?}, got {:?}",
        "a well-formed response",
        answer.raw
    );
    let seen = reported(&router, &signer, "a well-formed response", &queue_url).await;
    assert_eq!(
        seen,
        vec![well_formed.to_string()],
        "case {:?}: expected exactly one invocation reported for {well_formed:?}, got {seen:?}",
        "a well-formed response"
    );

    for row in rows() {
        let answered = exchange(row.case, &published, row.target).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} in {:?}",
            row.case, row.status, answered.status, answered.raw
        );
        assert_eq!(
            answered.raw.contains(TARGET_BODY),
            row.serves_target_body,
            "case {:?}: expected the function's own body to be served={}, got {:?}",
            row.case,
            row.serves_target_body,
            answered.raw
        );
        // A refused response is the LOAD BALANCER's own answer: neither the payload the
        // function returned nor its size reaches the client.
        if row.status == BAD_GATEWAY {
            assert!(
                !answered.raw.contains(NO_STATUS_BODY)
                    && !answered.raw.contains(NON_JSON_PAYLOAD),
                "case {:?}: the refused payload must not be proxied, got {:?}",
                row.case,
                answered.raw
            );
        }
        // The function RAN, exactly once, whatever the load balancer answered — and the event
        // it was handed named the path THE CLIENT asked for.
        let seen = reported(&router, &signer, row.case, &queue_url).await;
        assert_eq!(
            seen,
            vec![row.target.to_string()],
            "case {:?}: expected exactly one invocation reported for the client's own path {:?}, \
             got {seen:?}",
            row.case,
            row.target
        );
    }

    // A contract violation costs that one request and no more: the registration is still the
    // one the target group holds, and the next well-formed request is served again.
    let health = elbv2(
        &router,
        &signer,
        "DescribeTargetHealth",
        &[("TargetGroupArn", target_group.as_str())],
    )
    .await;
    assert!(
        health.contains(&function_arn),
        "after every refusal: expected DescribeTargetHealth to still report {function_arn}, got \
         {health}"
    );
    let answer = exchange("after every refusal", &published, well_formed).await;
    assert_eq!(
        answer.status, 200,
        "case {:?}: expected HTTP 200, got {} in {:?}",
        "after every refusal", answer.status, answer.raw
    );
    assert!(
        answer.raw.contains(TARGET_BODY),
        "case {:?}: expected the function's own body {TARGET_BODY:?}, got {:?}",
        "after every refusal",
        answer.raw
    );
    let seen = reported(&router, &signer, "after every refusal", &queue_url).await;
    assert_eq!(
        seen,
        vec![well_formed.to_string()],
        "case {:?}: expected exactly one invocation reported for {well_formed:?}, got {seen:?}",
        "after every refusal"
    );
}
