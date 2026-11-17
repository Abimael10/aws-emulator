// SPDX-License-Identifier: CC0-1.0

//! `routing.http.desync_mitigation_mode` on the wire, in front of a `lambda` target, on the
//! assembled router that VERIFIES request signatures — the configuration the emulator serves
//! traffic in.
//!
//! RULE under test: **a load balancer reads the FRAMING of every request that arrives on the
//! address it publishes — the header fields that say where the body ends — and answers, with
//! its own `400` and before any rule of its listener is evaluated or any target is invoked,
//! exactly the framings its `routing.http.desync_mitigation_mode` refuses: `monitor` refuses
//! none, `defensive` (the value the attribute is carried at) refuses a framing two recipients
//! can read two different messages out of, and `strictest` refuses every framing that is not
//! written as HTTP/1.1 states it. A framing it does not refuse is forwarded ONCE, and the body
//! the target is handed is the one the transfer coding frames where a request carries one,
//! whatever a length beside it says.**
//!
//! `LoadBalancerAttribute$Key` carries the switch: "`routing.http.desync_mitigation_mode` -
//! Determines how the load balancer handles requests that might pose a security risk to your
//! application. The possible values are monitor, defensive, and strictest. The default is
//! defensive" (`smithy_query(query="shape", shape="LoadBalancerAttribute")` on the
//! elasticloadbalancingv2 model). Which framings "might pose a security risk" is RFC 7230
//! §3.3.2 and §3.3.3: a request carrying both a transfer coding and a length, or a coding list
//! that does not end in the chunked one, can be read two ways — "the Transfer-Encoding
//! overrides the Content-Length", and a recipient that reads the other way reads a second
//! request out of the same bytes — while one length written twice with the same value is a
//! length every recipient reads alike, in a form the grammar does not admit.
//!
//! What this suite drives that the crate-local
//! `services/service-com-amazonaws-elasticloadbalancingv2-elasticloadbalancing-v10/tests/all/alb_desync_mitigation_and_forwarded_for.rs`
//! does not: the two axes a crate-local suite cannot see. First, whether the TARGET was
//! invoked at all — read where nothing of ELBv2 can fake it, on an Amazon SQS queue the
//! handler reports to through its own SDK as a session of its execution role, before it
//! decides what to return; a refused framing must leave that queue EMPTY. Second, the framings
//! whose two readings consume DIFFERENT bytes (a length that disagrees with the chunked body
//! beside it), which is the framing a smuggled request actually travels in, and the body the
//! target is handed for them. Every call is signed with the keys this process publishes, as a
//! client's SDK signs one, and the data-plane requests are written BYTE FOR BYTE on a real
//! connection to the published address — the only place a client's own framing exists.
//!
//! The rows differ along what the rule generalises over: the framing class (one length, no
//! framing header at all, the chunked coding alone, a length beside a coding whose two
//! readings end the body in different places, in both orders, and one length written twice),
//! each driven under every value the attribute's domain admits, plus the value it is carried
//! at before anything is written.
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
/// the one Elastic Load Balancing endpoint both models are published on.
const ELB_VERSION: &str = "2015-12-01";

/// The signing name Elastic Load Balancing publishes, which is what an SDK signs for.
const ELB_SIGNING_NAME: &str = "elasticloadbalancing";

/// The attribute under test.
const DESYNC_ATTRIBUTE: &str = "routing.http.desync_mitigation_mode";

/// The stack the queue, the execution role and the function are deployed as, and the names it
/// declares them under.
const STACK: &str = "desync-alb-lambda-estate";
const QUEUE: &str = "desync-alb-side-channel";
const ROLE: &str = "desync-alb-lambda-exec";
const FUNCTION: &str = "desync-alb-lambda-target";

/// The load balancer, the target group and the statement the invoke grant is written under.
const LOAD_BALANCER: &str = "desync-alb-lb";
const TARGET_GROUP: &str = "desync-alb-tg";
const STATEMENT: &str = "elbv2-invoke";

/// The status a load balancer answers a request whose framing its mode refuses, and the whole
/// status line it answers it on — which is what a client reading the connection sees first.
const BAD_REQUEST: u16 = 400;
const BAD_REQUEST_STATUS_LINE: &str = "HTTP/1.1 400 Bad Request";

/// The body the function answers with, which is the TARGET's own: no answer the load balancer
/// generates itself may carry it.
const TARGET_BODY: &str = "TARGET-REACHED";

/// The path every data-plane request asks for, and the path the control leg's direct `Invoke`
/// names.
const PROBE_PATH: &str = "/probe";
const CONTROL_PATH: &str = "/direct";

/// The network the load balancer is placed in.
const VPC_CIDR: &str = "10.91.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.91.1.0/24", "us-west-2a"),
    ("10.91.2.0/24", "us-west-2b"),
];

/// How long a wait for work the clock owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: everything waited for happens in this process, so a
/// loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long one dial and one exchange on the published address may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a look at the side channel waits for a report it expects: a client reading a queue
/// long-polls for the message it is waiting for rather than spinning on empty answers.
const LONG_POLL_SECONDS: i64 = 5;

/// The handler every row runs. It reports the invocation to the side channel FIRST — so a row
/// the load balancer answered itself leaves the queue empty — reporting the path it was handed
/// AND the body, so a row also reads which of the two readings of its own bytes the target was
/// handed; then it answers with its own body.
const HANDLER_SOURCE: &str = r#"
import os

import boto3

QUEUE_URL = os.environ["QUEUE_URL"]


def handler(event, context):
    path = event.get("path") or "/"
    body = event.get("body") or ""
    boto3.client("sqs").send_message(QueueUrl=QUEUE_URL, MessageBody=path + "|" + body)
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "text/plain"},
        "body": "TARGET-REACHED",
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

/// One Elastic Load Balancing v2 call that must succeed.
async fn elbv2(
    router: &MultiServiceRouter,
    signer: &Signer,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        signer,
        ELB_SIGNING_NAME,
        ELB_VERSION,
        action,
        members,
    )
    .await;
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

/// Every message the side channel holds, deleted as it is read so the next row starts from an
/// empty queue — once `expected` of them have arrived and one further look finds none, else
/// every message the budget allowed.
///
/// A row the load balancer answered itself expects NONE of them, and the first look settles it
/// with no waiting: the handler reports before it returns and the load balancer answers after
/// the invocation, so the queue's contents are already final when a row's answer is in hand.
async fn reports(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
    expected: usize,
) -> Vec<String> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + BUDGET;
    loop {
        // A consumer waiting for a report it expects long-polls for it, as a client reading a
        // side channel does; one that expects none asks for whatever is there and no more.
        let wait = i64::from(seen.is_empty() && expected > 0) * LONG_POLL_SECONDS;
        let (status, received) = sqs(
            router,
            signer,
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": wait,
            }),
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
        if seen.len() >= expected && batch.is_empty() {
            return seen;
        }
        if Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look at `GetFunctionConfiguration` until Lambda reports the function `Active`, which is what
/// a caller waits for before invoking a function it has just created.
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

/// Look at `DescribeLoadBalancers` until the load balancer reports the state the clock owes it,
/// which is what a client waits for before sending traffic.
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

/// What `DescribeLoadBalancerAttributes` reports the attribute under test at.
async fn described(router: &MultiServiceRouter, signer: &Signer, arn: &str) -> String {
    let body = elbv2(
        router,
        signer,
        "DescribeLoadBalancerAttributes",
        &[("LoadBalancerArn", arn)],
    )
    .await;
    let mut rest = body.as_str();
    while let Some(at) = rest.find("<member>") {
        rest = &rest[at + "<member>".len()..];
        let Some(end) = rest.find("</member>") else {
            break;
        };
        let member = &rest[..end];
        if element(member, "Key") == DESYNC_ATTRIBUTE {
            return element(member, "Value");
        }
        rest = &rest[end..];
    }
    panic!("DescribeLoadBalancerAttributes reports no {DESYNC_ATTRIBUTE} in {body}");
}

/// Write one value of the attribute, which must be accepted, and read it back, so every row is
/// driven against a value the control plane REPORTS rather than one this suite believes it
/// wrote.
async fn written(router: &MultiServiceRouter, signer: &Signer, arn: &str, value: &str) {
    elbv2(
        router,
        signer,
        "ModifyLoadBalancerAttributes",
        &[
            ("LoadBalancerArn", arn),
            ("Attributes.member.1.Key", DESYNC_ATTRIBUTE),
            ("Attributes.member.1.Value", value),
        ],
    )
    .await;
    let reported = described(router, signer, arn).await;
    assert_eq!(
        reported, value,
        "case {value:?}: expected {DESYNC_ATTRIBUTE} to be reported at {value:?}, got \
         {reported:?}"
    );
}

/// Dial the published address and exchange ONE request written byte for byte, as a client that
/// frames its own request does. Answers the whole answer as it came off the connection.
///
/// Only the PORT is taken from the published authority: the core binds its listeners on
/// loopback (`EMULATOR_BIND_ADDRESS`, `127.0.0.1` by default), so the dial is to this machine.
async fn raw_exchange(case: &str, published: &str, request: &str) -> String {
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
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the published address did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    String::from_utf8_lossy(&answer).to_string()
}

/// One framing a client can write a request with: the header lines that frame it, the bytes
/// that follow, which modes refuse it, and the body a recipient reading it as HTTP/1.1 states
/// it ends up with — the one the target must be handed where the request is forwarded.
struct Framing {
    case: &'static str,
    headers: &'static str,
    bytes: &'static str,
    /// Written as HTTP/1.1 states it, so refused by no mode.
    compliant: bool,
    /// Readable two ways, so refused by `defensive` as well as by `strictest`.
    ambiguous: bool,
    /// What the transfer coding, else the length, frames as the body.
    body: &'static str,
}

/// The framings each mode is driven against, spanning the forms the two framing headers take.
/// Every one of them is a message this address can read, so what decides the answer is the
/// attribute and nothing else.
fn framings() -> Vec<Framing> {
    vec![
        Framing {
            case: "a body framed by one length",
            headers: "Content-Length: 8\r\n",
            bytes: "SMUGGLED",
            compliant: true,
            ambiguous: false,
            body: "SMUGGLED",
        },
        Framing {
            case: "no framing header at all",
            headers: "",
            bytes: "",
            compliant: true,
            ambiguous: false,
            body: "",
        },
        Framing {
            case: "the chunked coding alone",
            headers: "Transfer-Encoding: chunked\r\n",
            bytes: "4\r\nhi!!\r\n0\r\n\r\n",
            compliant: true,
            ambiguous: false,
            body: "hi!!",
        },
        Framing {
            // RFC 7230 §3.3.3: the transfer coding overrides the length for one recipient and
            // not for another. The two readings end the body in DIFFERENT places here — the
            // coding ends it at once, the length claims eight more bytes — which is the framing
            // a smuggled request travels in.
            case: "a length that disagrees with the chunked body beside it",
            headers: "Content-Length: 8\r\nTransfer-Encoding: chunked\r\n",
            bytes: "0\r\n\r\n",
            compliant: false,
            ambiguous: true,
            body: "",
        },
        Framing {
            // The same disagreement in the other order, and with the coding framing bytes the
            // length says are not there at all.
            case: "a transfer coding carrying a body the length beside it denies",
            headers: "Transfer-Encoding: chunked\r\nContent-Length: 0\r\n",
            bytes: "4\r\nhi!!\r\n0\r\n\r\n",
            compliant: false,
            ambiguous: true,
            body: "hi!!",
        },
        Framing {
            // RFC 7230 §3.3.2: one length written twice is a length every recipient reads the
            // same way, in a form the grammar does not admit — so nothing can be hidden inside
            // it, and only `strictest` refuses it.
            case: "the same length written twice",
            headers: "Content-Length: 8\r\nContent-Length: 8\r\n",
            bytes: "SMUGGLED",
            compliant: false,
            ambiguous: false,
            body: "SMUGGLED",
        },
    ]
}

/// RULE: the framings a load balancer answers itself are exactly the ones its
/// `routing.http.desync_mitigation_mode` refuses; a refused request reaches no target at all,
/// and a forwarded one hands the target the body HTTP/1.1 frames out of its bytes.
#[test]
fn a_load_balancers_desync_mitigation_mode_decides_which_framings_reach_its_lambda_target() {
    std::thread::Builder::new()
        .name("elbv2-desync-mitigation".to_string())
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

    // THE CONTROL LEG, before any load balancer exists: a synchronous `Invoke` runs the handler
    // to completion, so the report it makes through its own SDK is on the queue by the time the
    // call answers — which is what makes an EMPTY queue conclusive later on.
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
    let seen = reports(&router, &signer, "control", &queue_url, 1).await;
    assert_eq!(
        seen,
        vec![format!("{CONTROL_PATH}|")],
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
                ("GroupName", "desync-alb-target"),
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

    // The value the attribute's own documentation carries it at, with nothing written: the rows
    // under it are driven against the mode a load balancer is in before any client touches it.
    let carried = described(&router, &signer, &load_balancer_arn).await;
    assert_eq!(
        carried, "defensive",
        "case the attribute as it is carried: expected {DESYNC_ATTRIBUTE} at \"defensive\", got \
         {carried:?}"
    );

    // Each mode against each framing: `monitor` refuses none, the carried `defensive` refuses
    // what reads two ways, `strictest` refuses everything not written as the grammar has it —
    // and `monitor` again last, so a mode is seen to be read afresh for every request rather
    // than latched by the first one that was refused.
    type Mode = (&'static str, bool, fn(&Framing) -> bool);
    let modes: [Mode; 4] = [
        ("defensive", false, |framing: &Framing| framing.ambiguous),
        ("monitor", true, |_| false),
        ("strictest", true, |framing: &Framing| !framing.compliant),
        ("monitor", true, |_| false),
    ];
    for (mode, write, refuses) in modes {
        if write {
            written(&router, &signer, &load_balancer_arn, mode).await;
        }
        for framing in framings() {
            let case = format!("{mode:?} with {:?}", framing.case);
            let request = format!(
                "POST {PROBE_PATH} HTTP/1.1\r\nHost: {published}\r\n{}Connection: close\r\n\r\n{}",
                framing.headers, framing.bytes
            );
            let raw = raw_exchange(&case, &published, &request).await;
            let refused = refuses(&framing);
            if refused {
                // The load balancer answers it ITSELF, on the status line a client reads first.
                assert!(
                    raw.starts_with(BAD_REQUEST_STATUS_LINE),
                    "case {case}: expected the answer to open {BAD_REQUEST_STATUS_LINE:?}, got \
                     {raw:?}"
                );
                assert!(
                    !raw.contains(TARGET_BODY),
                    "case {case}: a refused request carries nothing of the target's own body, \
                     got {raw:?}"
                );
            } else {
                let status = raw
                    .split_whitespace()
                    .nth(1)
                    .and_then(|code| code.parse::<u16>().ok())
                    .unwrap_or_else(|| panic!("case {case}: no status line in {raw:?}"));
                assert_eq!(
                    status, 200,
                    "case {case}: expected the target's own HTTP 200, got {status} in {raw:?}"
                );
                assert!(
                    raw.contains(TARGET_BODY),
                    "case {case}: expected the target's own body {TARGET_BODY:?}, got {raw:?}"
                );
                assert_ne!(
                    status, BAD_REQUEST,
                    "case {case}: this mode refuses no such framing, got {raw:?}"
                );
            }
            // What the TARGET saw: nothing at all for a refused framing, and exactly one
            // invocation — carrying the path the client asked for and the body HTTP/1.1 frames
            // out of its bytes — for a forwarded one.
            let expected: Vec<String> = if refused {
                Vec::new()
            } else {
                vec![format!("{PROBE_PATH}|{}", framing.body)]
            };
            let seen = reports(&router, &signer, &case, &queue_url, expected.len()).await;
            assert_eq!(
                seen, expected,
                "case {case}: expected the side channel to report {expected:?}, got {seen:?}"
            );
        }
    }
}
