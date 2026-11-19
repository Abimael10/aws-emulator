// SPDX-License-Identifier: CC0-1.0

//! What a client that DIALS the address an Application Load Balancer publishes is answered
//! with while a WAFv2 web ACL is associated with that load balancer, on the emulator as it is
//! ASSEMBLED.
//!
//! RULE: **the address a load balancer publishes as its `DNSName` is an address of the
//! emulator a client on this machine can dial, and a request that arrives on it is decided
//! FIRST by the web ACL `AssociateWebACL` bound to the load balancer's own ARN — a
//! terminating action other than `Allow` is answered at the edge, under that action's own
//! `CustomResponse` where it configures one and otherwise under the status WAF serves that
//! action with, and none of the listener's actions runs, so nothing behind the listener
//! answers; a request the web ACL lets through is served by the listener, carrying the
//! headers any `CustomRequestHandling` inserted.**
//!
//! `LoadBalancer$DNSName` is "the public DNS name of the load balancer" — the address a
//! client is handed and then dials — and `CreateLoadBalancerInput$Scheme` documents that the
//! nodes of an `internet-facing` load balancer "have public IP addresses", so that is the
//! family and scheme a client on this machine addresses directly. `AssociateWebACL`'s
//! `ResourceArn` names an Application Load Balancer as the first protected resource form, and
//! the statuses asserted here are the ones WAF serves its terminating actions under: `403`
//! for a `Block`, "the HTTP status code 405 Method Not Allowed" (`CaptchaAction`) and `202`
//! for a `Challenge`.
//!
//! Why this suite lives on the assembled router rather than in either crate: the enforcement
//! has to hold on the SOCKET the published address is served on, which only exists when the
//! load balancer that opened it and the WAFv2 that holds the binding run in ONE emulator.
//! Every row here is a real TCP connection to the published authority speaking HTTP/1.1, as a
//! client of that address does — the crate-local suites dispatch the same request into a
//! router of their own and so never touch the connection, the framing, or whether the
//! published address answers at all.
//!
//! The rows differ along what the rule generalises over: the request component the blocking
//! rule inspects (a single header, the URI path, the query string, the method), a matching and
//! a non-matching request for each, the terminating action (`Block`, `Captcha`, `Challenge`),
//! a block carrying its own custom response and one carrying none, a rule whose action is not
//! terminating, the web ACL's `DefaultAction` deciding instead of a rule, an `Allow` that
//! inserts a header the listener's own rule then matches, and no web ACL at all.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::query_wire::{element, form_encode};

/// The Region every control-plane call here is signed for.
const REGION: &str = "us-east-1";

/// The API versions the two query protocols carry.
const EC2_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The network the load balancers are placed in.
const VPC_CIDR: &str = "10.90.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [("10.90.1.0/24", "us-east-1a"), ("10.90.2.0/24", "us-east-1b")];

/// The body the listener's DEFAULT action answers with: the request reached the load
/// balancer's own configuration, so nothing refused it at the edge.
pub(crate) const LISTENER_BODY: &str = "LISTENER-OK";

/// The body the listener's one RULE answers with. It answers it only for a request carrying
/// the header a web ACL's `CustomRequestHandling` inserts, so this body is proof the inserted
/// header reached the listener's own rule evaluation.
const TAGGED_BODY: &str = "LISTENER-TAGGED";

/// The header the blocking rules of this suite inspect.
pub(crate) const BLOCK_HEADER: &str = "x-waf-block";

/// The header an inserting `Allow` configures, without the `x-amzn-waf-` prefix WAF puts on
/// it ("for the header name `sample`, WAF inserts the header `x-amzn-waf-sample`" —
/// `CustomHTTPHeader$Name`), the spelling it arrives under, and its value.
const INSERTED_HEADER: &str = "tag";
const INSERTED_HEADER_SENT: &str = "x-amzn-waf-tag";
const INSERTED_VALUE: &str = "edge";

/// The statuses WAF serves its terminating actions under.
pub(crate) const BLOCK_STATUS: u16 = 403;
const CAPTCHA_STATUS: u16 = 405;
const CHALLENGE_STATUS: u16 = 202;

/// The status and the body the one block that configures its own response serves.
const CUSTOM_STATUS: u16 = 503;
const CUSTOM_BODY: &str = "refused at the edge";

/// Neither body a load balancer of this suite answers with may appear in an answer the EDGE
/// served: the listener's actions did not run.
const NO_LISTENER_ANSWER: &[&str] = &[LISTENER_BODY, TAGGED_BODY];

/// Nothing to look for, for a row that asserts no absence.
const NOTHING: &[&str] = &[];

/// How long a wait for work the clock owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: the row waits for a CONDITION the emulator reaches
/// in this process, so a loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long one dial and one exchange on the published address may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The emulator as it is ASSEMBLED, on one engine: the load balancer that opens the published
/// address and the WAFv2 that holds the binding are the ones this router mounts.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form-encoded (query-protocol) call of `service`, as its status and its XML body.
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

/// One EC2 call that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "ec2", EC2_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One ELBv2 call that must succeed. The credential scope names THIS model's own routing key,
/// which is how an ELBv2 request is addressed at the model rather than at the Classic one it
/// shares an endpoint with.
async fn elbv2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(
        router,
        "elasticloadbalancingv2",
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

/// One WAFv2 call over its `awsJson1_1` wire that must succeed.
pub(crate) async fn waf_ok(
    router: &MultiServiceRouter,
    operation: &str,
    request: Value,
) -> Value {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("wafv2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSWAF_20190729.{operation}"))
        .header("authorization", authorization("wafv2"))
        .body(Bytes::from(
            serde_json::to_vec(&request).expect("the request serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "setup: WAFv2 {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// A minimal `VisibilityConfig`, as every web ACL and rule needs one.
fn visibility(metric: &str) -> Value {
    json!({
        "SampledRequestsEnabled": true,
        "CloudWatchMetricsEnabled": true,
        "MetricName": metric,
    })
}

/// A `ByteMatchStatement` over `field`, searching for `search` under `constraint`.
/// `ByteMatchStatement$SearchString` is a blob, which travels base64-encoded in an awsJson
/// body.
pub(crate) fn byte_match(field: Value, search: &str, constraint: &str) -> Value {
    json!({
        "ByteMatchStatement": {
            "SearchString": aws_smithy_types::base64::encode(search),
            "FieldToMatch": field,
            "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
            "PositionalConstraint": constraint,
        }
    })
}

/// One rule at priority 0 carrying `rule_action` over `statement`.
pub(crate) fn rule(name: &str, statement: Value, rule_action: Value) -> Value {
    json!({
        "Name": name,
        "Priority": 0,
        "Statement": statement,
        "Action": rule_action,
        "VisibilityConfig": visibility(name),
    })
}

/// The web ACL one row puts in front of the load balancer.
pub(crate) struct WebAclSpec {
    /// `WebACL$DefaultAction`.
    pub(crate) default_action: Value,
    /// `WebACL$Rules`.
    pub(crate) rules: Value,
    /// `WebACL$CustomResponseBodies`, left off the request where the row configures none (the
    /// member takes at least one entry).
    pub(crate) custom_response_bodies: Option<Value>,
}

impl WebAclSpec {
    /// A web ACL whose default action allows, carrying `rules`.
    pub(crate) fn allowing(rules: Value) -> Self {
        Self {
            default_action: json!({ "Allow": {} }),
            rules,
            custom_response_bodies: None,
        }
    }
}

/// Create the web ACL `spec` describes and answer its ARN.
pub(crate) async fn create_web_acl(
    router: &MultiServiceRouter,
    name: &str,
    spec: &WebAclSpec,
) -> String {
    let mut body = json!({
        "Name": name,
        "Scope": "REGIONAL",
        "DefaultAction": spec.default_action,
        "VisibilityConfig": visibility(name),
        "Rules": spec.rules,
    });
    if let Some(bodies) = &spec.custom_response_bodies {
        body["CustomResponseBodies"] = bodies.clone();
    }
    let created = waf_ok(router, "CreateWebACL", body).await;
    created["Summary"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateWebACL must answer Summary.ARN, got {created}"))
        .to_string()
}

/// A `Block` rule on the single header this suite's blocking rows carry.
fn header_block() -> Value {
    rule(
        "block-header",
        byte_match(json!({ "SingleHeader": { "Name": BLOCK_HEADER } }), "1", "EXACTLY"),
        json!({ "Block": {} }),
    )
}

/// The network the load balancers are placed in, through EC2 itself: a VPC with an internet
/// gateway attached (an internet-facing load balancer can only be placed in a VPC that has
/// one), two subnets and a security group. Answers the subnet ids and the group id.
pub(crate) async fn network(router: &MultiServiceRouter) -> (Vec<String>, String) {
    let vpc = element(&ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await, "vpcId");
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
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
            router,
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
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "waf-published-address"),
                ("GroupDescription", "the group of the load balancer"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    (subnets, security_group)
}

/// One internet-facing Application Load Balancer, its HTTP:80 listener, and the address it
/// publishes — reported once the load balancer carries the state the clock owes it, which is
/// what a client waits for before sending traffic.
///
/// The listener answers [`LISTENER_BODY`] by default and [`TAGGED_BODY`] for a request
/// carrying the header an inserting `Allow` puts on it.
pub(crate) async fn load_balancer(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[String],
    security_group: &str,
) -> (String, String) {
    let created = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", name),
            ("Scheme", "internet-facing"),
            ("Subnets.member.1", subnets[0].as_str()),
            ("Subnets.member.2", subnets[1].as_str()),
            ("SecurityGroups.member.1", security_group),
        ],
    )
    .await;
    let arn = element(&created, "LoadBalancerArn");
    let published = element(&created, "DNSName");

    let listener = element(
        &elbv2(
            router,
            "CreateListener",
            &[
                ("LoadBalancerArn", arn.as_str()),
                ("Protocol", "HTTP"),
                ("Port", "80"),
                ("DefaultActions.member.1.Type", "fixed-response"),
                ("DefaultActions.member.1.FixedResponseConfig.StatusCode", "200"),
                ("DefaultActions.member.1.FixedResponseConfig.MessageBody", LISTENER_BODY),
            ],
        )
        .await,
        "ListenerArn",
    );
    elbv2(
        router,
        "CreateRule",
        &[
            ("ListenerArn", listener.as_str()),
            ("Priority", "10"),
            ("Conditions.member.1.Field", "http-header"),
            (
                "Conditions.member.1.HttpHeaderConfig.HttpHeaderName",
                INSERTED_HEADER_SENT,
            ),
            (
                "Conditions.member.1.HttpHeaderConfig.Values.member.1",
                INSERTED_VALUE,
            ),
            ("Actions.member.1.Type", "fixed-response"),
            ("Actions.member.1.FixedResponseConfig.StatusCode", "200"),
            ("Actions.member.1.FixedResponseConfig.MessageBody", TAGGED_BODY),
        ],
    )
    .await;
    wait_until_active(router, &arn).await;
    // The address is the load balancer's own, not one call's answer: the read reports the
    // same `DNSName` the create published, which is the address a client that describes the
    // load balancer later is handed.
    let described = elbv2(
        router,
        "DescribeLoadBalancers",
        &[("LoadBalancerArns.member.1", arn.as_str())],
    )
    .await;
    assert_eq!(
        element(&described, "DNSName"),
        published,
        "case {name}: DescribeLoadBalancers must report the address the create published, \
         got {described}"
    );
    (arn, published)
}

/// Look at `DescribeLoadBalancers` until the load balancer reports the state the clock owes
/// it, which is what a client waits for before sending traffic.
async fn wait_until_active(router: &MultiServiceRouter, arn: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let body = elbv2(
            router,
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

/// The PORT of the address a load balancer published, read as the assertion that the address
/// is one of this emulator's own: a `host:port` on the interface the core binds — a bracketed
/// host where that interface is IPv6 — rather than a name in Elastic Load Balancing's own
/// zone, which resolves to nothing on this machine.
pub(crate) fn dialable_port(case: &str, published: &str) -> u16 {
    let (host, port) = published.rsplit_once(':').unwrap_or_else(|| {
        panic!(
            "case {case:?}: the published address {published:?} names no port at all, so no \
             client can dial it"
        )
    });
    let address: IpAddr = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the published address {published:?} names {host:?} where an \
                 address of this emulator belongs, not a host in the service's own zone which \
                 resolves to nothing on this machine: {error}"
            )
        });
    assert!(
        address.is_loopback(),
        "case {case:?}: the published address {published:?} must name the interface the core \
         binds its listeners on, which is loopback unless the operator names another, got \
         {host:?}"
    );
    port.parse::<u16>().unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the published address {published:?} names {port:?} where a port \
             belongs: {error}"
        )
    })
}

/// What one answer on the published address carried: its status line and the rest of it.
pub(crate) struct Answered {
    pub(crate) status: u16,
    pub(crate) raw: String,
}

/// Dial the published address and exchange one request on it, as a client of that address
/// does: a connection in the clear, HTTP/1.1, closed after the one exchange.
///
/// The PORT is taken from the published authority and asserted to be one the emulator itself
/// published; the core binds its listeners on loopback (`EMULATOR_BIND_ADDRESS`, `127.0.0.1`
/// by default), so the dial is to this machine.
pub(crate) async fn exchange(
    case: &str,
    published: &str,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
) -> Answered {
    let port = dialable_port(case, published);
    let mut stream = tokio::time::timeout(
        EXCHANGE_TIMEOUT,
        tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| panic!("case {case:?}: {published} did not accept a connection"))
    .unwrap_or_else(|error| panic!("case {case:?}: {published} refused a connection: {error}"));
    let mut request = format!("{method} {target} HTTP/1.1\r\nHost: {published}\r\nAccept: */*\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
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

/// One row: the web ACL in front of the load balancer (none at all for `web_acl: None`), the
/// request the client sends on the published address, and what must come back.
struct Row {
    case: &'static str,
    web_acl: Option<WebAclSpec>,
    method: &'static str,
    target: &'static str,
    headers: &'static [(&'static str, &'static str)],
    status: u16,
    /// Fragments the answer must carry, each a body or a header no other answer carries.
    present: &'static [&'static str],
    /// Fragments the answer must NOT carry.
    absent: &'static [&'static str],
}

/// A row whose request is a plain `GET /`.
fn row(
    case: &'static str,
    web_acl: Option<WebAclSpec>,
    status: u16,
    present: &'static [&'static str],
    absent: &'static [&'static str],
) -> Row {
    Row {
        case,
        web_acl,
        method: "GET",
        target: "/",
        headers: &[],
        status,
        present,
        absent,
    }
}

/// Every row of the table.
fn rows() -> Vec<Row> {
    vec![
        // No web ACL at all: the listener answers, which is what the published address serves
        // before any binding exists.
        row(
            "no web ACL in front of the load balancer",
            None,
            200,
            &[LISTENER_BODY],
            NOTHING,
        ),
        // A web ACL with nothing to match: its `DefaultAction` allows.
        row(
            "a web ACL with no rules and a default action that allows",
            Some(WebAclSpec::allowing(json!([]))),
            200,
            &[LISTENER_BODY],
            NOTHING,
        ),
        // The blocked request of the recorded case, and the two requests the same web ACL must
        // NOT refuse.
        Row {
            headers: &[(BLOCK_HEADER, "1")],
            ..row(
                "a Block rule on a single header, the header matching",
                Some(WebAclSpec::allowing(json!([header_block()]))),
                BLOCK_STATUS,
                NOTHING,
                NO_LISTENER_ANSWER,
            )
        },
        row(
            "a Block rule on a single header, the header absent",
            Some(WebAclSpec::allowing(json!([header_block()]))),
            200,
            &[LISTENER_BODY],
            NOTHING,
        ),
        Row {
            headers: &[(BLOCK_HEADER, "2")],
            ..row(
                "a Block rule on a single header, the header carrying another value",
                Some(WebAclSpec::allowing(json!([header_block()]))),
                200,
                &[LISTENER_BODY],
                NOTHING,
            )
        },
        // The same rule over the other request components a `FieldToMatch` names, each with a
        // matching and a non-matching request.
        Row {
            target: "/admin",
            ..row(
                "a Block rule on the URI path, the path matching",
                Some(WebAclSpec::allowing(json!([rule(
                    "block-path",
                    byte_match(json!({ "UriPath": {} }), "/admin", "EXACTLY"),
                    json!({ "Block": {} }),
                )]))),
                BLOCK_STATUS,
                NOTHING,
                NO_LISTENER_ANSWER,
            )
        },
        Row {
            target: "/public",
            ..row(
                "a Block rule on the URI path, another path",
                Some(WebAclSpec::allowing(json!([rule(
                    "block-path",
                    byte_match(json!({ "UriPath": {} }), "/admin", "EXACTLY"),
                    json!({ "Block": {} }),
                )]))),
                200,
                &[LISTENER_BODY],
                NOTHING,
            )
        },
        Row {
            target: "/?drop=all",
            ..row(
                "a Block rule on the query string, a query string containing the value",
                Some(WebAclSpec::allowing(json!([rule(
                    "block-query",
                    byte_match(json!({ "QueryString": {} }), "drop", "CONTAINS"),
                    json!({ "Block": {} }),
                )]))),
                BLOCK_STATUS,
                NOTHING,
                NO_LISTENER_ANSWER,
            )
        },
        Row {
            target: "/?keep=all",
            ..row(
                "a Block rule on the query string, another query string",
                Some(WebAclSpec::allowing(json!([rule(
                    "block-query",
                    byte_match(json!({ "QueryString": {} }), "drop", "CONTAINS"),
                    json!({ "Block": {} }),
                )]))),
                200,
                &[LISTENER_BODY],
                NOTHING,
            )
        },
        Row {
            method: "POST",
            ..row(
                "a Block rule on the method, the method matching",
                Some(WebAclSpec::allowing(json!([rule(
                    "block-method",
                    byte_match(json!({ "Method": {} }), "POST", "EXACTLY"),
                    json!({ "Block": {} }),
                )]))),
                BLOCK_STATUS,
                NOTHING,
                NO_LISTENER_ANSWER,
            )
        },
        row(
            "a Block rule on the method, another method",
            Some(WebAclSpec::allowing(json!([rule(
                "block-method",
                byte_match(json!({ "Method": {} }), "POST", "EXACTLY"),
                json!({ "Block": {} }),
            )]))),
            200,
            &[LISTENER_BODY],
            NOTHING,
        ),
        // The web ACL's own default action decides a request no rule matched.
        row(
            "a web ACL whose default action blocks",
            Some(WebAclSpec {
                default_action: json!({ "Block": {} }),
                rules: json!([]),
                custom_response_bodies: None,
            }),
            BLOCK_STATUS,
            NOTHING,
            NO_LISTENER_ANSWER,
        ),
        // A `Count` action is not terminating: the request goes on to the default action,
        // which allows it, so the listener answers.
        row(
            "a Count rule that matches, under a default action that allows",
            Some(WebAclSpec::allowing(json!([rule(
                "count-path",
                byte_match(json!({ "UriPath": {} }), "/", "EXACTLY"),
                json!({ "Count": {} }),
            )]))),
            200,
            &[LISTENER_BODY],
            NOTHING,
        ),
        // A block that configures its OWN response is served by WAF, with that response's
        // status, body and headers — the listener's actions are not consulted.
        Row {
            headers: &[(BLOCK_HEADER, "1")],
            ..row(
                "a Block rule carrying its own custom response",
                Some(WebAclSpec {
                    default_action: json!({ "Allow": {} }),
                    rules: json!([rule(
                        "block-header",
                        byte_match(
                            json!({ "SingleHeader": { "Name": BLOCK_HEADER } }),
                            "1",
                            "EXACTLY",
                        ),
                        json!({
                            "Block": {
                                "CustomResponse": {
                                    "ResponseCode": CUSTOM_STATUS,
                                    "CustomResponseBodyKey": "refused",
                                    "ResponseHeaders": [
                                        { "Name": "x-refused-by", "Value": "waf" }
                                    ],
                                }
                            }
                        }),
                    )]),
                    custom_response_bodies: Some(json!({
                        "refused": { "ContentType": "TEXT_PLAIN", "Content": CUSTOM_BODY }
                    })),
                }),
                CUSTOM_STATUS,
                &[CUSTOM_BODY, "x-refused-by: waf"],
                NO_LISTENER_ANSWER,
            )
        },
        // The other two terminating actions, each under the status WAF serves it with.
        Row {
            headers: &[(BLOCK_HEADER, "1")],
            ..row(
                "a Captcha rule that matches, with no token on the request",
                Some(WebAclSpec::allowing(json!([rule(
                    "captcha-header",
                    byte_match(
                        json!({ "SingleHeader": { "Name": BLOCK_HEADER } }),
                        "1",
                        "EXACTLY",
                    ),
                    json!({ "Captcha": {} }),
                )]))),
                CAPTCHA_STATUS,
                NOTHING,
                NO_LISTENER_ANSWER,
            )
        },
        Row {
            headers: &[(BLOCK_HEADER, "1")],
            ..row(
                "a Challenge rule that matches, with no token on the request",
                Some(WebAclSpec::allowing(json!([rule(
                    "challenge-header",
                    byte_match(
                        json!({ "SingleHeader": { "Name": BLOCK_HEADER } }),
                        "1",
                        "EXACTLY",
                    ),
                    json!({ "Challenge": {} }),
                )]))),
                CHALLENGE_STATUS,
                NOTHING,
                NO_LISTENER_ANSWER,
            )
        },
        // An `Allow` that inserts a header: the request reaches the listener on the published
        // address, whose own rule matches that header and answers its own body.
        row(
            "an Allow rule inserting a header the listener's rule matches",
            Some(WebAclSpec::allowing(json!([rule(
                "tag-everything",
                byte_match(json!({ "UriPath": {} }), "/", "EXACTLY"),
                json!({
                    "Allow": {
                        "CustomRequestHandling": {
                            "InsertHeaders": [
                                { "Name": INSERTED_HEADER, "Value": INSERTED_VALUE }
                            ]
                        }
                    }
                }),
            )]))),
            200,
            &[TAGGED_BODY],
            NOTHING,
        ),
        // The same request with the binding taken away: the listener's rule does not match, so
        // the inserted header above is the only thing that made it match.
        row(
            "no web ACL, so nothing inserts that header",
            None,
            200,
            &[LISTENER_BODY],
            &[TAGGED_BODY],
        ),
    ]
}

/// RULE: the address a load balancer publishes is dialable on this machine, and a request
/// that arrives on it is decided first by the web ACL bound to the load balancer's ARN — the
/// edge's own answer for a terminating action, the listener's for a request it lets through.
#[tokio::test]
async fn the_published_address_is_decided_by_the_web_acl_in_front_of_it() {
    let router = build_router();
    let (subnets, security_group) = network(&router).await;
    let (lb_arn, published) =
        load_balancer(&router, "waf-published-alb", &subnets, &security_group).await;

    let mut bound = false;
    for (index, row) in rows().into_iter().enumerate() {
        match &row.web_acl {
            Some(spec) => {
                let web_acl =
                    create_web_acl(&router, &format!("published-acl-{index}"), spec).await;
                // A second association REPLACES the first: the resource carries at most one
                // web ACL in front of it.
                waf_ok(
                    &router,
                    "AssociateWebACL",
                    json!({ "ResourceArn": lb_arn, "WebACLArn": web_acl }),
                )
                .await;
                bound = true;
            }
            None => {
                if bound {
                    waf_ok(
                        &router,
                        "DisassociateWebACL",
                        json!({ "ResourceArn": lb_arn }),
                    )
                    .await;
                    bound = false;
                }
            }
        }
        let answered = exchange(row.case, &published, row.method, row.target, row.headers).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} in {:?}",
            row.case, row.status, answered.status, answered.raw
        );
        let seen = answered.raw.to_ascii_lowercase();
        for fragment in row.present {
            assert!(
                seen.contains(&fragment.to_ascii_lowercase()),
                "case {:?}: expected the answer to carry {fragment:?}, got {:?}",
                row.case,
                answered.raw
            );
        }
        for fragment in row.absent {
            assert!(
                !seen.contains(&fragment.to_ascii_lowercase()),
                "case {:?}: expected the answer NOT to carry {fragment:?}, got {:?}",
                row.case,
                answered.raw
            );
        }
    }
}

/// The binding is keyed by the PROTECTED RESOURCE's own ARN: two load balancers publish two
/// addresses of this emulator, and the web ACL in front of one decides nothing about the
/// traffic that arrives on the other's.
#[tokio::test]
async fn a_web_acl_bound_to_one_load_balancer_decides_nothing_on_another() {
    let router = build_router();
    let (subnets, security_group) = network(&router).await;
    let (protected_arn, protected) =
        load_balancer(&router, "waf-protected-alb", &subnets, &security_group).await;
    let (_other_arn, other) =
        load_balancer(&router, "waf-unprotected-alb", &subnets, &security_group).await;
    assert_ne!(
        protected, other,
        "case two load balancers: each must publish an address of its own, got {protected:?} for \
         the protected one and {other:?} for the other"
    );

    let web_acl = create_web_acl(
        &router,
        "one-resource-acl",
        &WebAclSpec::allowing(json!([header_block()])),
    )
    .await;
    waf_ok(
        &router,
        "AssociateWebACL",
        json!({ "ResourceArn": protected_arn, "WebACLArn": web_acl }),
    )
    .await;

    let blocked: &[(&str, &str)] = &[(BLOCK_HEADER, "1")];
    let refused = exchange(
        "the address the web ACL is in front of",
        &protected,
        "GET",
        "/",
        blocked,
    )
    .await;
    assert_eq!(
        refused.status, BLOCK_STATUS,
        "case the address the web ACL is in front of: expected HTTP {BLOCK_STATUS}, got {} in {:?}",
        refused.status, refused.raw
    );
    assert!(
        !refused.raw.contains(LISTENER_BODY),
        "case the address the web ACL is in front of: the listener's action must not have run, \
         got {:?}",
        refused.raw
    );

    let served = exchange("another load balancer's address", &other, "GET", "/", blocked).await;
    assert_eq!(
        served.status, 200,
        "case another load balancer's address: expected HTTP 200, got {} in {:?}",
        served.status, served.raw
    );
    assert!(
        served.raw.contains(LISTENER_BODY),
        "case another load balancer's address: expected the listener's own answer, got {:?}",
        served.raw
    );
}
