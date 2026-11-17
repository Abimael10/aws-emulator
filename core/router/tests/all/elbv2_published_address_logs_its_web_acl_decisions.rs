// SPDX-License-Identifier: CC0-1.0

//! What a WAFv2 web ACL LOGS about a request a client made on the address an Application
//! Load Balancer publishes, on the emulator as it is ASSEMBLED.
//!
//! RULE: **the address a load balancer publishes as its `DNSName` is an address of this
//! emulator a client on this machine can dial, so a request sent there is decided by the web
//! ACL `AssociateWebACL` bound to that load balancer's own ARN and leaves ONE record about
//! that decision in the destination the web ACL's `LoggingConfiguration` names** — a record
//! naming the web ACL that decided, the rule that ended the evaluation and its kind, the
//! action applied, what each rule GROUP the web ACL ran did to the request, and the load
//! balancer the request arrived at.
//!
//! `LoadBalancer$DNSName` is "the public DNS name of the load balancer" — the address a client
//! is handed and then dials — and `CreateLoadBalancerInput$Scheme` documents that the nodes of
//! an `internet-facing` load balancer "have public IP addresses", so that is the family and
//! scheme a client on this machine addresses directly. `AssociateWebACLRequest$ResourceArn`
//! names an Application Load Balancer as the first protected resource form, and a
//! `LoggingConfiguration` "defines an association between logging destinations and a web ACL
//! resource, for logging from WAF" which `PutLoggingConfiguration` "enables … to start logging
//! from a web ACL" — so while a web ACL carries one, every request it decides leaves a record
//! at the destination it names, a request it blocked as much as one it let through.
//!
//! Why this suite lives on the assembled router rather than in either crate: the record is
//! only produced when the load balancer that opens the published address, the WAFv2 that holds
//! the binding and the CloudWatch Logs that holds the destination all run in ONE emulator, and
//! the request has to arrive on the SOCKET that address is served on. Every row here is a real
//! TCP connection to the published authority speaking HTTP/1.1, as a client of that address
//! does, and reads the record back out of the log group through that service's own reader — so
//! what a row observes is the record as a client reads it, not a value computed in process.
//! The sibling suite `elbv2_published_address_enforces_its_web_acl.rs` pins what the client is
//! ANSWERED on that address; this one pins what is LOGGED about the same request.
//!
//! The rows differ along what the rule generalises over: which rule decided the request (a
//! rule of a rule group this account owns, a rule of the web ACL itself, the web ACL's own
//! `DefaultAction`), what the group did to it (ended the evaluation, counted it, left it
//! alone), the request component the deciding rule inspects (a single header, the URI path),
//! the action applied (`BLOCK`, `ALLOW`), and a configuration that keeps components of the
//! request out of its records (`RedactedFields`).

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::query_wire::{element, form_encode};

/// The Region every control-plane call here is signed for.
const REGION: &str = "us-west-2";

/// The API versions the two query protocols carry.
const EC2_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The `Scope` a load balancer's web ACL lives in: a load balancer is a REGIONAL resource.
const SCOPE: &str = "REGIONAL";

/// The network the load balancer is placed in.
const VPC_CIDR: &str = "10.91.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.91.1.0/24", "us-west-2a"),
    ("10.91.2.0/24", "us-west-2b"),
];

/// The body the listener's default action answers with: the request reached the load
/// balancer's own configuration, so the web ACL let it through.
const LISTENER_BODY: &str = "LISTENER-OK";

/// The header every row's request carries, and the two values a row drives it with: one the
/// blocking rules match, one they do not.
const PROBE_HEADER: &str = "x-waf-probe";
const MATCHING_PROBE: &str = "deny-me";
const UNMATCHED_PROBE: &str = "let-me-in";

/// The rule INSIDE the rule group this account owns, which blocks what it matches.
const GROUP_RULE: &str = "deny-the-probe";

/// The web ACL rule that RUNS that group.
const RUNS_THE_GROUP: &str = "run-the-group";

/// A web ACL rule of its own, which blocks the path it matches and lives in no group.
const OWN_RULE: &str = "block-the-path";

/// The path that rule inspects.
const BLOCKED_PATH: &str = "/admin";

/// The `terminatingRuleId` of a request no rule terminated, which the web ACL's
/// `DefaultAction` decided.
const DEFAULT_ACTION_RULE: &str = "Default_Action";

/// The `terminatingRuleType` of a rule that inspects the request itself, and of the web ACL's
/// own `DefaultAction`, and the one of a rule that runs a customer-owned rule group.
const REGULAR_TYPE: &str = "REGULAR";
const GROUP_TYPE: &str = "GROUP";

/// What a redacted component shows instead of what the client sent, from
/// `LoggingConfiguration$RedactedFields`: "the `HEADER` field in the logs will be `REDACTED`".
const REDACTED: &str = "REDACTED";

/// The kind of resource WAF names as the source of a request that arrived at an Application
/// Load Balancer. The per-ARN-form catalogue of that identity is pinned by the wafv2 crate's
/// own `service::logs` tests; here it is the one source every row's request has.
const ALB_SOURCE: &str = "ALB";

/// The statuses a request is answered under: the one WAF serves a `Block` with, and the
/// listener's own fixed response for a request the web ACL let through.
const BLOCK_STATUS: u16 = 403;
const SERVED_STATUS: u16 = 200;

/// How long a wait for work the clock owes may take before it is a failure, and how long
/// between two looks. Generous on purpose: a row waits for a CONDITION the emulator reaches in
/// this process, so a loaded host does not decide the outcome.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long one dial and one exchange on the published address may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The emulator as it is ASSEMBLED, on one engine: the load balancer that opens the published
/// address, the WAFv2 that holds the binding and the CloudWatch Logs that holds the
/// destination are the ones this router mounts.
fn build_router() -> MultiServiceRouter {
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

/// One awsJson1_1 call of `service`, whose `X-Amz-Target` prefix is `target_prefix`.
async fn json_rpc(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    operation: &str,
    request: Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(service))
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
    (status, parsed)
}

/// One WAFv2 call that must succeed.
async fn waf_ok(router: &MultiServiceRouter, operation: &str, request: Value) -> Value {
    let (status, parsed) = json_rpc(router, "wafv2", "AWSWAF_20190729", operation, request).await;
    assert_eq!(
        status, 200,
        "setup: WAFv2 {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call that must succeed.
async fn logs_ok(router: &MultiServiceRouter, operation: &str, request: Value) -> Value {
    let (status, parsed) = json_rpc(router, "logs", "Logs_20140328", operation, request).await;
    assert_eq!(
        status, 200,
        "setup: CloudWatch Logs {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Create the log group `name` through CloudWatch Logs' own wire and answer the destination
/// ARN a logging configuration names it by, so the destination is a group the account holds.
async fn log_destination(router: &MultiServiceRouter, name: &str) -> String {
    logs_ok(router, "CreateLogGroup", json!({ "logGroupName": name })).await;
    format!(
        "arn:aws:logs:{REGION}:{}:log-group:{name}",
        engine::emulator_account_id()
    )
}

/// The records the log group `name` holds, each parsed as the JSON document a WAF log record
/// is, read through CloudWatch Logs' own reader.
async fn in_log_group(router: &MultiServiceRouter, name: &str) -> Vec<Value> {
    let read = logs_ok(router, "FilterLogEvents", json!({ "logGroupName": name })).await;
    read["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    serde_json::from_str(event["message"].as_str().unwrap_or_default())
                        .unwrap_or(Value::Null)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look in the log group until it holds a record, which is what a reader of the destination
/// waits for. Answers every record it holds then.
async fn delivered_records(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let records = in_log_group(router, name).await;
        if !records.is_empty() {
            return records;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the log group {name} never took a record about the request"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A minimal `VisibilityConfig`, as every web ACL, rule and rule group needs one.
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
fn byte_match(field: Value, search: &str, constraint: &str) -> Value {
    json!({
        "ByteMatchStatement": {
            "SearchString": aws_smithy_types::base64::encode(search),
            "FieldToMatch": field,
            "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
            "PositionalConstraint": constraint,
        }
    })
}

/// The statement the blocking rule of the rule group carries: the probe header holding the
/// value the rows that are meant to match send.
fn probe_statement() -> Value {
    byte_match(
        json!({ "SingleHeader": { "Name": PROBE_HEADER } }),
        MATCHING_PROBE,
        "EXACTLY",
    )
}

/// The rule group this account owns, whose one rule BLOCKS the probed request; answers its
/// ARN.
async fn blocking_rule_group(router: &MultiServiceRouter, name: &str) -> String {
    let created = waf_ok(
        router,
        "CreateRuleGroup",
        json!({
            "Name": name,
            "Scope": SCOPE,
            "Capacity": 100,
            "VisibilityConfig": visibility(name),
            "Rules": [{
                "Name": GROUP_RULE,
                "Priority": 0,
                "Statement": probe_statement(),
                "Action": { "Block": {} },
                "VisibilityConfig": visibility(GROUP_RULE),
            }],
        }),
    )
    .await;
    created["Summary"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateRuleGroup must answer Summary.ARN, got {created}"))
        .to_string()
}

/// A web ACL rule that runs the rule group `arn` this account owns, under `override_action`.
fn group_reference(arn: &str, override_action: Value) -> Value {
    json!([{
        "Name": RUNS_THE_GROUP,
        "Priority": 0,
        "Statement": { "RuleGroupReferenceStatement": { "ARN": arn } },
        "OverrideAction": override_action,
        "VisibilityConfig": visibility(RUNS_THE_GROUP),
    }])
}

/// The web ACL's `Rules` for the row whose blocking rule is the web ACL's own, over the URI
/// path.
///
/// Every row's rules are built from the ARN of the rule group this account owns; a web ACL
/// whose rule lives in no group has nothing to do with that ARN, so this one ignores it.
fn own_path_rule(_arn: &str) -> Value {
    json!([{
        "Name": OWN_RULE,
        "Priority": 0,
        "Statement": byte_match(json!({ "UriPath": {} }), BLOCKED_PATH, "EXACTLY"),
        "Action": { "Block": {} },
        "VisibilityConfig": visibility(OWN_RULE),
    }])
}

/// One `ruleGroupList` entry, as a row states what it expects.
fn group_entry(id: &str, terminating: Value, non_terminating: Value) -> Value {
    json!({
        "ruleGroupId": id,
        "terminatingRule": terminating,
        "nonTerminatingMatchingRules": non_terminating,
        "excludedRules": Value::Null,
    })
}

/// The network the load balancer is placed in, through EC2 itself: a VPC with an internet
/// gateway attached (an internet-facing load balancer can only be placed in a VPC that has
/// one), two subnets and a security group. Answers the subnet ids and the group id.
async fn network(router: &MultiServiceRouter) -> (Vec<String>, String) {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
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
                ("GroupName", "waf-logged-address"),
                ("GroupDescription", "the group of the load balancer"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    (subnets, security_group)
}

/// One internet-facing Application Load Balancer, its HTTP:80 listener answering
/// [`LISTENER_BODY`], and the address it publishes — reported once the load balancer carries
/// the state the clock owes it, which is what a client waits for before sending traffic.
async fn load_balancer(
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
    elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", arn.as_str()),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "fixed-response"),
            (
                "DefaultActions.member.1.FixedResponseConfig.StatusCode",
                "200",
            ),
            (
                "DefaultActions.member.1.FixedResponseConfig.MessageBody",
                LISTENER_BODY,
            ),
        ],
    )
    .await;
    wait_until_active(router, &arn).await;
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
/// is one of this emulator's own: a `host:port` on the LOOPBACK interface the core binds its
/// listeners on by default — `127.0.0.1`, or `[::1]` where that interface is IPv6 — rather
/// than a name in Elastic Load Balancing's own zone, which resolves to nothing on this
/// machine. The host is asserted here and the port is what the dial needs.
fn dialable_port(case: &str, published: &str) -> u16 {
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
    let port = port.parse::<u16>().unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the published address {published:?} names {port:?} where a port \
             belongs: {error}"
        )
    });
    port
}

/// What one answer on the published address carried: its status line and the rest of it.
struct Answered {
    status: u16,
    raw: String,
}

/// Dial the published address and exchange one request on it, as a client of that address
/// does: a connection in the clear, HTTP/1.1, closed after the one exchange. The host is the
/// loopback one the published authority names ([`dialable_port`]), so the connection goes to
/// this emulator's own listener and nowhere else.
async fn exchange(case: &str, published: &str, target: &str, probe: &str) -> Answered {
    let port = dialable_port(case, published);
    let mut stream = tokio::time::timeout(
        EXCHANGE_TIMEOUT,
        tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| panic!("case {case:?}: {published} did not accept a connection"))
    .unwrap_or_else(|error| panic!("case {case:?}: {published} refused a connection: {error}"));
    let request = format!(
        "GET {target} HTTP/1.1\r\nHost: {published}\r\nAccept: */*\r\n{PROBE_HEADER}: {probe}\r\n\
         Connection: close\r\n\r\n"
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

/// The identity a record names the protected resource by: the ACCOUNT that holds the load
/// balancer and the load balancer id its own ARN states after the resource type, as that ARN
/// carries both.
fn source_id(lb_arn: &str) -> String {
    let fields: Vec<&str> = lb_arn.splitn(6, ':').collect();
    let account = fields
        .get(4)
        .copied()
        .unwrap_or_else(|| panic!("the load balancer ARN {lb_arn} names no account"));
    let resource = fields
        .get(5)
        .copied()
        .unwrap_or_else(|| panic!("the load balancer ARN {lb_arn} names no resource"));
    let identity = resource
        .split_once('/')
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("the load balancer ARN {lb_arn} carries no resource type"));
    format!("{account}-{identity}")
}

/// The value one header of the record's `httpRequest` shows, or `None` when the record carries
/// no entry for it.
fn header_shown(record: &Value, name: &str) -> Option<String> {
    record["httpRequest"]["headers"]
        .as_array()?
        .iter()
        .find(|entry| {
            entry["name"]
                .as_str()
                .is_some_and(|written| written.eq_ignore_ascii_case(name))
        })
        .and_then(|entry| entry["value"].as_str().map(str::to_string))
}

/// One row: the web ACL in front of the load balancer, the request the client sends on the
/// published address, and what the record that request leaves must say.
struct Row {
    case: &'static str,
    /// The log group this row's records are delivered into, which doubles as the name every
    /// WAFv2 entity of the row is created under. A WAF logging destination's log group name
    /// has to begin with the prefix WAF requires of one.
    group: &'static str,
    /// The web ACL's `DefaultAction`.
    default_action: fn() -> Value,
    /// The web ACL's `Rules`, given the ARN of the rule group this account owns.
    rules: fn(&str) -> Value,
    /// `LoggingConfiguration$RedactedFields`, left off where the row configures none.
    redacted_fields: Option<fn() -> Value>,
    /// The request: its target, and the value the probe header carries.
    target: &'static str,
    probe: &'static str,
    /// The status the client is answered with on the published address.
    status: u16,
    /// What the record must say about the decision.
    decided: Decided,
    /// What the record's `httpRequest` must show for the path, the query string and the probe
    /// header the client sent.
    uri: &'static str,
    args: &'static str,
    header: &'static str,
}

/// What the record must say about the DECISION one request was given: the action applied, the
/// rule that ended the evaluation and its kind, and what each rule GROUP the web ACL ran did
/// to the request.
struct Decided {
    /// The `action` the record names.
    action: &'static str,
    /// The `terminatingRuleId` and `terminatingRuleType` the record names.
    terminating: &'static str,
    terminating_type: &'static str,
    /// What `ruleGroupList` must be, given the ARN of the rule group this account owns.
    rule_group_list: fn(&str) -> Value,
}

/// The decision a row expects, written as the record reports it.
fn decided(
    action: &'static str,
    terminating: &'static str,
    terminating_type: &'static str,
    rule_group_list: fn(&str) -> Value,
) -> Decided {
    Decided {
        action,
        terminating,
        terminating_type,
        rule_group_list,
    }
}

/// A row whose web ACL allows by default and configures no redaction, whose request is a plain
/// `GET /` carrying the probe header.
fn row(
    case: &'static str,
    group: &'static str,
    rules: fn(&str) -> Value,
    probe: &'static str,
    status: u16,
    decided: Decided,
) -> Row {
    Row {
        case,
        group,
        default_action: || json!({ "Allow": {} }),
        rules,
        redacted_fields: None,
        target: "/",
        probe,
        status,
        decided,
        uri: "/",
        args: "",
        header: probe,
    }
}

/// Every row of the table.
fn rows() -> Vec<Row> {
    vec![
        // The recorded pair: one request a rule of the rule group blocks, and one the same web
        // ACL's default action allows, each leaving its own record.
        row(
            "a rule of a rule group this account owns ends the evaluation",
            "aws-waf-logs-alb-group-blocked",
            |arn| group_reference(arn, json!({ "None": {} })),
            MATCHING_PROBE,
            BLOCK_STATUS,
            decided("BLOCK", RUNS_THE_GROUP, GROUP_TYPE, |arn| {
                json!([group_entry(
                    arn,
                    json!({ "ruleId": GROUP_RULE, "action": "BLOCK" }),
                    json!([]),
                )])
            }),
        ),
        row(
            "the web ACL's default action allows a request the same group left alone",
            "aws-waf-logs-alb-default-allowed",
            |arn| group_reference(arn, json!({ "None": {} })),
            UNMATCHED_PROBE,
            SERVED_STATUS,
            decided("ALLOW", DEFAULT_ACTION_RULE, REGULAR_TYPE, |arn| {
                json!([group_entry(arn, Value::Null, json!([]))])
            }),
        ),
        // The same group, counted whole by the web ACL rule that runs it: the request is
        // allowed by the default action and the record reports the group's rule as counting.
        row(
            "the group counted whole by the web ACL rule's OverrideAction",
            "aws-waf-logs-alb-group-counted",
            |arn| group_reference(arn, json!({ "Count": {} })),
            MATCHING_PROBE,
            SERVED_STATUS,
            decided("ALLOW", DEFAULT_ACTION_RULE, REGULAR_TYPE, |arn| {
                json!([group_entry(
                    arn,
                    Value::Null,
                    json!([{ "ruleId": GROUP_RULE, "action": "COUNT" }]),
                )])
            }),
        ),
        // A rule of the web ACL itself, inspecting another component of the request: it lives
        // in no group, so the record names no group at all.
        Row {
            target: BLOCKED_PATH,
            uri: BLOCKED_PATH,
            ..row(
                "a rule of the web ACL itself, on the URI path, ends the evaluation",
                "aws-waf-logs-alb-own-rule-blocked",
                own_path_rule,
                UNMATCHED_PROBE,
                BLOCK_STATUS,
                decided("BLOCK", OWN_RULE, REGULAR_TYPE, |_| json!([])),
            )
        },
        // The same web ACL and the path it does not match: the default action decides.
        row(
            "the same web ACL's own rule matching nothing, so the default action allows",
            "aws-waf-logs-alb-own-rule-allowed",
            own_path_rule,
            UNMATCHED_PROBE,
            SERVED_STATUS,
            decided("ALLOW", DEFAULT_ACTION_RULE, REGULAR_TYPE, |_| json!([])),
        ),
        // A web ACL with no rules at all, whose default action BLOCKS: the record reports the
        // default action as what terminated the evaluation.
        Row {
            default_action: || json!({ "Block": {} }),
            ..row(
                "a web ACL whose default action blocks a request no rule matched",
                "aws-waf-logs-alb-default-blocked",
                |_| json!([]),
                UNMATCHED_PROBE,
                BLOCK_STATUS,
                decided("BLOCK", DEFAULT_ACTION_RULE, REGULAR_TYPE, |_| json!([])),
            )
        },
        // The components the configuration names in `RedactedFields` are kept out of the
        // record, while the decision is the same one the first row's web ACL made.
        Row {
            redacted_fields: Some(|| {
                json!([
                    { "SingleHeader": { "Name": PROBE_HEADER } },
                    { "QueryString": {} },
                ])
            }),
            target: "/?token=secret",
            args: REDACTED,
            header: REDACTED,
            ..row(
                "the components the configuration redacts are kept out of the record",
                "aws-waf-logs-alb-redacted",
                |arn| group_reference(arn, json!({ "None": {} })),
                MATCHING_PROBE,
                BLOCK_STATUS,
                decided("BLOCK", RUNS_THE_GROUP, GROUP_TYPE, |arn| {
                    json!([group_entry(
                        arn,
                        json!({ "ruleId": GROUP_RULE, "action": "BLOCK" }),
                        json!([]),
                    )])
                }),
            )
        },
    ]
}

/// Create the web ACL `row` describes, put it in front of `lb_arn`, point its logging at
/// `destination`, and answer its ARN.
async fn web_acl_in_front(
    router: &MultiServiceRouter,
    row: &Row,
    group_arn: &str,
    lb_arn: &str,
    destination: &str,
) -> String {
    let created = waf_ok(
        router,
        "CreateWebACL",
        json!({
            "Name": row.group,
            "Scope": SCOPE,
            "DefaultAction": (row.default_action)(),
            "VisibilityConfig": visibility(row.group),
            "Rules": (row.rules)(group_arn),
        }),
    )
    .await;
    let arn = created["Summary"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateWebACL must answer Summary.ARN, got {created}"))
        .to_string();
    // A second association REPLACES the first: the resource carries at most one web ACL in
    // front of it, so each row's requests are decided by the row's own.
    waf_ok(
        router,
        "AssociateWebACL",
        json!({ "ResourceArn": lb_arn, "WebACLArn": arn }),
    )
    .await;
    let mut configuration = json!({
        "ResourceArn": arn,
        "LogDestinationConfigs": [destination],
    });
    if let Some(redacted) = row.redacted_fields {
        configuration["RedactedFields"] = redacted();
    }
    waf_ok(
        router,
        "PutLoggingConfiguration",
        json!({ "LoggingConfiguration": configuration }),
    )
    .await;
    arn
}

/// RULE: a request a client sends to the address a load balancer publishes is decided by the
/// web ACL bound to that load balancer and leaves ONE record about that decision in the
/// destination the web ACL's logging configuration names.
#[tokio::test]
async fn a_request_on_the_published_address_is_logged_by_the_web_acl_that_decided_it() {
    let router = build_router();
    let (subnets, security_group) = network(&router).await;
    let (lb_arn, published) =
        load_balancer(&router, "waf-logged-alb", &subnets, &security_group).await;
    let group_arn = blocking_rule_group(&router, "aws-waf-logs-alb-group").await;
    let expected_source = source_id(&lb_arn);

    for row in rows() {
        let case = row.case;
        let destination = log_destination(&router, row.group).await;
        let web_acl = web_acl_in_front(&router, &row, &group_arn, &lb_arn, &destination).await;

        let answered = exchange(case, &published, row.target, row.probe).await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected HTTP {} on the published address, got {} in {:?}",
            row.status, answered.status, answered.raw
        );
        let served = answered.raw.contains(LISTENER_BODY);
        assert_eq!(
            served,
            row.decided.action == "ALLOW",
            "case {case:?}: expected the listener's own answer only for a request the web ACL \
             let through, got {:?}",
            answered.raw
        );

        let delivered = delivered_records(&router, case, row.group).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: one decided request is one record, got {delivered:?}"
        );
        let record = &delivered[0];
        assert_eq!(
            record["webaclId"],
            json!(web_acl),
            "case {case:?}: the record must name the web ACL that decided, got {record}"
        );
        assert_eq!(
            record["action"],
            json!(row.decided.action),
            "case {case:?}: expected the action {:?}, got {record}",
            row.decided.action
        );
        assert_eq!(
            record["terminatingRuleId"],
            json!(row.decided.terminating),
            "case {case:?}: expected the terminating rule {:?}, got {record}",
            row.decided.terminating
        );
        assert_eq!(
            record["terminatingRuleType"],
            json!(row.decided.terminating_type),
            "case {case:?}: expected the terminating rule type {:?}, got {record}",
            row.decided.terminating_type
        );
        let groups = (row.decided.rule_group_list)(&group_arn);
        assert_eq!(
            record["ruleGroupList"], groups,
            "case {case:?}: expected the rule groups the web ACL ran {groups}, got {record}"
        );
        // And WHERE the request arrived: the kind of resource the association names, and that
        // resource as the service holding it identifies it.
        assert_eq!(
            record["httpSourceName"],
            json!(ALB_SOURCE),
            "case {case:?}: expected the source {ALB_SOURCE:?}, got {record}"
        );
        assert_eq!(
            record["httpSourceId"],
            json!(expected_source),
            "case {case:?}: expected the source id {expected_source:?}, got {record}"
        );
        // ... and the request itself, as the configuration's redaction leaves it.
        assert_eq!(
            record["httpRequest"]["uri"],
            json!(row.uri),
            "case {case:?}: expected the uri {:?}, got {record}",
            row.uri
        );
        assert_eq!(
            record["httpRequest"]["args"],
            json!(row.args),
            "case {case:?}: expected the query string {:?}, got {record}",
            row.args
        );
        let shown = header_shown(record, PROBE_HEADER);
        assert_eq!(
            shown.as_deref(),
            Some(row.header),
            "case {case:?}: expected the header {PROBE_HEADER} to show {:?}, got {record}",
            row.header
        );
    }
}

/// The record is what the LOGGING CONFIGURATION produces: the same web ACL in front of the
/// same published address decides a request and logs nothing until one is put, and logs it
/// from then on.
#[tokio::test]
async fn nothing_is_logged_about_the_published_address_until_a_configuration_is_put() {
    let router = build_router();
    let (subnets, security_group) = network(&router).await;
    let (lb_arn, published) =
        load_balancer(&router, "waf-unlogged-alb", &subnets, &security_group).await;
    let group = "aws-waf-logs-alb-enabled-later";
    let destination = log_destination(&router, group).await;

    let created = waf_ok(
        &router,
        "CreateWebACL",
        json!({
            "Name": group,
            "Scope": SCOPE,
            "DefaultAction": { "Allow": {} },
            "VisibilityConfig": visibility(group),
            "Rules": [{
                "Name": OWN_RULE,
                "Priority": 0,
                "Statement": probe_statement(),
                "Action": { "Block": {} },
                "VisibilityConfig": visibility(OWN_RULE),
            }],
        }),
    )
    .await;
    let web_acl = created["Summary"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateWebACL must answer Summary.ARN, got {created}"))
        .to_string();
    waf_ok(
        &router,
        "AssociateWebACL",
        json!({ "ResourceArn": lb_arn, "WebACLArn": web_acl }),
    )
    .await;

    // The web ACL decides the request — the client is refused at the edge — and the
    // destination it was never pointed at takes nothing: the record is delivered before the
    // client is answered, so an empty group here is an answer and not a race. The count
    // asserted after the configuration is put is what makes that hold either way: a record
    // about THIS request arriving late would make that count two rather than one.
    let case = "no logging configuration";
    let refused = exchange(case, &published, "/", MATCHING_PROBE).await;
    assert_eq!(
        refused.status, BLOCK_STATUS,
        "case {case:?}: expected HTTP {BLOCK_STATUS} from the web ACL, got {} in {:?}",
        refused.status, refused.raw
    );
    let records = in_log_group(&router, group).await;
    assert!(
        records.is_empty(),
        "case {case:?}: a web ACL carrying no logging configuration logs nothing, got {records:?}"
    );

    waf_ok(
        &router,
        "PutLoggingConfiguration",
        json!({
            "LoggingConfiguration": {
                "ResourceArn": web_acl,
                "LogDestinationConfigs": [destination],
            }
        }),
    )
    .await;

    let case = "the configuration put";
    let served = exchange(case, &published, "/", UNMATCHED_PROBE).await;
    assert_eq!(
        served.status, SERVED_STATUS,
        "case {case:?}: expected the listener's own answer, got {} in {:?}",
        served.status, served.raw
    );
    let delivered = delivered_records(&router, case, group).await;
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: only the request made after the configuration was put is recorded, got \
         {delivered:?}"
    );
    assert_eq!(
        delivered[0]["action"],
        json!("ALLOW"),
        "case {case:?}: expected the record of the allowed request, got {}",
        delivered[0]
    );
}
