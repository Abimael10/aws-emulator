// SPDX-License-Identifier: CC0-1.0

//! The DNS FIREWALL a Route 53 Profile pushes onto a VPC, observed the only way it can be: a
//! DNS query asked from INSIDE that VPC.
//!
//! RULE: a query a client inside a VPC sends to the Amazon-provided resolver — at the
//! link-local 169.254.169.253 or at the base of the VPC's range plus two — is decided by the
//! DNS Firewall rule groups that reach that VPC before any zone is read for it, and a rule
//! group reaches it through a Route 53 Profile associated with the VPC exactly as it does
//! through an association of its own; the first matching rule, in priority order, answers the
//! query its own `BlockResponse` way (`NXDOMAIN` as a name that does not exist, `NODATA` as a
//! successful answer with no record, `OVERRIDE` with the rule's own record), and a name no
//! rule's domain list covers is resolved as it was before the rule group existed.
//!
//! That is the only OBSERVABLE proof a Profile's push took effect: no API reports what a VPC's
//! resolver answers. Four crates have to agree for it to hold — Amazon EC2 owns the VPC, its
//! subnet and the interface the function is attached through, Route 53 Resolver owns the rule
//! groups, their rules and their domain lists, Route 53 Profiles owns the Profile and its two
//! kinds of association, and Lambda runs the handler whose datagram reaches the resolver — so
//! the suite lives here.
//!
//! The rows differ along what the rule generalises over: the `BlockResponse` of the rule that
//! matched (`NXDOMAIN`, `NODATA`, `OVERRIDE`), the FORM of the domain list entry that covers
//! the name (a plain domain, a wildcard entry, the domain under a wildcard entry), whether the
//! rule names a `Qtype` and whether the query asks that type, a name under no list at all, a
//! name of the VPC's own internal zone, and which of the resolver's two addresses the query was
//! sent to. Each name is asked THREE times — before any rule group reaches the VPC, while the
//! Profile carries one, and after the Profile is disassociated from the VPC — so what is
//! asserted is the DIFFERENCE the Profile's push makes and not one static answer.
//!
//! Every identifier below was minted by the service that owns it and read back out of that
//! service's own answer; the only values written by hand are the ones the request carries.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version an ec2Query form body names.
const EC2_API_VERSION: &str = "2016-11-15";

/// The network the function sits in, and the two addresses its VPC's resolver answers at: the
/// link-local one, and the base of the range plus two.
const VPC_CIDR: &str = "10.61.0.0/16";
const SUBNET_CIDR: &str = "10.61.1.0/24";
const ZONE: &str = "us-west-2a";
const LINK_LOCAL_RESOLVER: &str = "169.254.169.253";
const RANGE_RESOLVER: &str = "10.61.0.2";

/// An address of the VPC, and the Amazon-provided hostname of it — the name the VPC's own
/// internal zone answers, which is the row that proves a name on no domain list still resolves.
/// "AWS uses `<region>.compute.internal`" outside `us-east-1` (Amazon VPC User Guide, "DHCP
/// option sets").
const IN_VPC_ADDRESS: &str = "10.61.1.80";
const IN_VPC_HOSTNAME: &str = "ip-10-61-1-80.us-west-2.compute.internal";

/// The names the rows ask about, and the domain list entries that cover them.
const BLOCKED_NAME: &str = "blocked.example.com";
const QUIET_ENTRY: &str = "*.quiet.example.net";
const QUIET_DOMAIN: &str = "quiet.example.net";
const QUIET_SUBDOMAIN: &str = "feed.quiet.example.net";
const OVERRIDDEN_NAME: &str = "override.example.org";
const OVERRIDE_TARGET: &str = "safe.example.net";
const TYPED_NAME: &str = "typed.example.com";
const UNLISTED_NAME: &str = "control.example.net";

/// The TTL the `OVERRIDE` rule recommends for its own record, well inside
/// `BlockOverrideTtl`'s modelled `0..=604800` range.
const OVERRIDE_TTL: u64 = 42;

/// The DNS record types a probe asks for, as RFC 1035 numbers them.
const TYPE_A: u64 = 1;
const TYPE_AAAA: u64 = 28;

/// The response codes a reply carries, as RFC 1035 §4.1.1 numbers them: no error, a server
/// failure — what this VPC's resolver answers a name it would have to recurse for — and a name
/// that does not exist.
const RCODE_NOERROR: u64 = 0;
const RCODE_SERVFAIL: u64 = 2;
const RCODE_NXDOMAIN: u64 = 3;

/// The stack, the execution role and the function that carry the probe.
const STACK: &str = "xsvc-r53p-firewall-probe";
const EXEC_ROLE: &str = "xsvc-r53p-firewall-exec";
const FUNCTION: &str = "xsvc-r53p-firewall-probe-fn";

/// The `StackStatus` the probe stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The terminal status every resource of this suite must report before a query is asked: a rule
/// group, a rule and a domain list of Route 53 Resolver, and a Profile association of
/// Route 53 Profiles, all report `COMPLETE` once the work their create started is done.
const COMPLETE: &str = "COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is what the resolver answers and not how fast an
/// interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The memory the probe function is configured for, as its own `MemorySize`. No row tests a
/// memory limit, so this limit is given generous room. 3008 MiB is well inside the member's
/// documented 128..=10240 range.
const FUNCTION_MEMORY: u64 = 3_008;

/// How often to look while waiting for work a service's own clock owes — a stack completing, a
/// function reaching `Active`, an association settling — and how long to keep looking. Nothing
/// here sleeps for a fixed span: every wait is for the CONDITION, and the deadline is generous
/// so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(25);
/// See [`LOOK`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// The handler every phase runs: it frames one DNS query per probe the event names, sends it as
/// a datagram to the resolver address the event names, and reports the reply it read back.
///
/// The DNS messages are framed and read by the handler itself, so what is asserted is a real
/// exchange on a socket inside the VPC rather than a shape this emulator agreed with itself.
const PROBE_HANDLER: &str = r#"
import socket

# How long the handler waits for one reply. Generous for a reply that is served in this same
# process, and not a span any row measures: what is under test is what the resolver answers,
# never how fast it answers, and the invocation's own Timeout bounds every wait besides.
WAIT = 10


def _name_bytes(name):
    labels = [label for label in name.rstrip(".").split(".") if label]
    return b"".join(bytes([len(label)]) + label.encode("ascii") for label in labels) + b"\x00"


def _query(name, kind, qid):
    header = qid.to_bytes(2, "big") + b"\x01\x00" + b"\x00\x01" + b"\x00\x00" * 3
    return header + _name_bytes(name) + kind.to_bytes(2, "big") + b"\x00\x01"


def _skip_name(data, pos):
    while True:
        length = data[pos]
        if length == 0:
            return pos + 1
        if length & 0xC0:
            return pos + 2
        pos += 1 + length


def _read_name(data, pos):
    labels = []
    while True:
        length = data[pos]
        if length == 0:
            return ".".join(labels) + "."
        if length & 0xC0:
            pos = ((length & 0x3F) << 8) | data[pos + 1]
            continue
        labels.append(data[pos + 1 : pos + 1 + length].decode("ascii"))
        pos += 1 + length


def _records(reply):
    pos = 12
    for _ in range(int.from_bytes(reply[4:6], "big")):
        pos = _skip_name(reply, pos) + 4
    answered = []
    for _ in range(int.from_bytes(reply[6:8], "big")):
        pos = _skip_name(reply, pos)
        kind = int.from_bytes(reply[pos : pos + 2], "big")
        ttl = int.from_bytes(reply[pos + 4 : pos + 8], "big")
        length = int.from_bytes(reply[pos + 8 : pos + 10], "big")
        start = pos + 10
        pos = start + length
        if kind == 1 and length == 4:
            answered.append({"type": kind, "ttl": ttl, "value": ".".join(str(octet) for octet in reply[start:pos])})
        else:
            answered.append({"type": kind, "ttl": ttl, "value": _read_name(reply, start)})
    return answered


def _asked(resolver, name, kind, qid):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(WAIT)
    try:
        sock.sendto(_query(name, kind, qid), (resolver, 53))
        reply, peer = sock.recvfrom(512)
    except OSError as failed:
        return {"rcode": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()
    return {
        "id": int.from_bytes(reply[0:2], "big"),
        "rcode": reply[3] & 0x0F,
        "ancount": int.from_bytes(reply[6:8], "big"),
        "records": _records(reply),
        "peer": list(peer),
    }


def handler(event, context):
    resolver = event["resolver"]
    results = []
    for index, probe in enumerate(event["probes"]):
        results.append(_asked(resolver, probe["name"], int(probe["type"]), 0x500 + index))
    return {"results": results}
"#;

/// The emulator as it is ASSEMBLED, on one engine: every service this suite drives is the one
/// the router mounts, so the firewall the resolver consults is the one Route 53 Resolver
/// registered when it was mounted.
fn build_router() -> MultiServiceRouter {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form-encoded (`ec2Query` / `awsQuery`) call, as its status and its XML body.
async fn form_call(
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
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon EC2 action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = form_call(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One CloudFormation action.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "cloudformation", "2010-05-15", action, members).await
}

/// One Route 53 Resolver operation over its `awsJson1_1` wire, which must succeed.
async fn route53resolver(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> Value {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("route53resolver.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Route53Resolver.{operation}"))
        .header("authorization", authorization("route53resolver"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "setup: Route53Resolver.{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// One Route 53 Profiles operation over its `restJson1` wire, as its status and its body.
async fn route53profiles(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("route53profiles.{REGION}.amazonaws.com"))
        .header("authorization", authorization("route53profiles"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(
            body.map(Value::to_string).unwrap_or_default().into_bytes(),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// One Route 53 Profiles operation that must succeed.
async fn profiles_ok(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Value {
    let (status, payload) = route53profiles(router, method, uri, body).await;
    assert!(
        (200..300).contains(&status),
        "setup: {method} {uri} must succeed, got {status} with {payload}"
    );
    payload
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The network the function and the Profile both address.
struct Network {
    /// `Vpc$VpcId`, which is what the Profile is associated WITH.
    vpc: String,
    /// `Subnet$SubnetId` the function's interface is placed in.
    subnet: String,
    /// `SecurityGroup$GroupId` the function carries.
    group: String,
}

/// Create that network through Amazon EC2's own operations.
async fn ec2_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", SUBNET_CIDR),
                ("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    );
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("VpcId", vpc.as_str()),
                ("GroupName", "xsvc-r53p-firewall-group"),
                ("GroupDescription", "the group the probe function carries"),
            ],
        )
        .await,
        "groupId",
    );
    Network { vpc, subnet, group }
}

/// One rule of the group this suite builds: the domains its list holds, and the members of the
/// rule itself.
struct DeclaredRule {
    /// What the rule is called, which is also what its domain list is called.
    name: &'static str,
    /// The entries of its domain list.
    domains: &'static [&'static str],
    /// `CreateFirewallRule$Priority`.
    priority: u64,
    /// `CreateFirewallRule$BlockResponse`.
    block_response: &'static str,
    /// `CreateFirewallRule$Qtype`, where the rule evaluates one type only.
    qtype: Option<&'static str>,
}

/// The rules the Profile's rule group carries, one per block response the model declares plus
/// one scoped to a single query type.
const DECLARED_RULES: &[DeclaredRule] = &[
    DeclaredRule {
        name: "xsvc-blocked",
        domains: &[BLOCKED_NAME],
        priority: 10,
        block_response: "NXDOMAIN",
        qtype: None,
    },
    DeclaredRule {
        name: "xsvc-quiet",
        domains: &[QUIET_ENTRY],
        priority: 20,
        block_response: "NODATA",
        qtype: None,
    },
    DeclaredRule {
        name: "xsvc-overridden",
        domains: &[OVERRIDDEN_NAME],
        priority: 30,
        block_response: "OVERRIDE",
        qtype: None,
    },
    DeclaredRule {
        name: "xsvc-typed",
        domains: &[TYPED_NAME],
        priority: 40,
        block_response: "NXDOMAIN",
        qtype: Some("AAAA"),
    },
];

/// Create the rule group, its domain lists and its rules through Route 53 Resolver's own
/// operations, and answer the group's `Id` and `Arn` as that service minted them.
async fn plant_the_rule_group(router: &MultiServiceRouter) -> (String, String) {
    let group = route53resolver(
        router,
        "CreateFirewallRuleGroup",
        &json!({ "CreatorRequestId": "xsvc-r53p-group", "Name": "xsvc-r53p-firewall" }),
    )
    .await;
    let group = group
        .get("FirewallRuleGroup")
        .cloned()
        .unwrap_or_else(|| panic!("setup: the create must answer a FirewallRuleGroup, got {group}"));
    let group_id = string_of(&group, "Id");
    let group_arn = string_of(&group, "Arn");
    for rule in DECLARED_RULES {
        let list = route53resolver(
            router,
            "CreateFirewallDomainList",
            &json!({ "CreatorRequestId": rule.name, "Name": rule.name }),
        )
        .await;
        let list_id = string_of(
            list.get("FirewallDomainList").unwrap_or_else(|| {
                panic!("setup: the create must answer a FirewallDomainList, got {list}")
            }),
            "Id",
        );
        route53resolver(
            router,
            "UpdateFirewallDomains",
            &json!({
                "FirewallDomainListId": list_id,
                "Operation": "ADD",
                "Domains": rule.domains,
            }),
        )
        .await;
        let mut request = json!({
            "CreatorRequestId": format!("{}-rule", rule.name),
            "FirewallRuleGroupId": group_id,
            "FirewallDomainListId": list_id,
            "Name": rule.name,
            "Priority": rule.priority,
            "Action": "BLOCK",
            "BlockResponse": rule.block_response,
        });
        if rule.block_response == "OVERRIDE" {
            request["BlockOverrideDomain"] = json!(OVERRIDE_TARGET);
            request["BlockOverrideDnsType"] = json!("CNAME");
            request["BlockOverrideTtl"] = json!(OVERRIDE_TTL);
        }
        if let Some(qtype) = rule.qtype {
            request["Qtype"] = json!(qtype);
        }
        route53resolver(router, "CreateFirewallRule", &request).await;
    }
    (group_id, group_arn)
}

/// The string member `member` of `document`.
fn string_of(document: &Value, member: &str) -> String {
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: {document} must report {member}"))
        .to_string()
}

/// Wait until every resource the firewall is read off reports its terminal status: the rule
/// group, each of its rules, and the Profile resource association that carries the group to the
/// VPC.
async fn wait_until_the_firewall_has_settled(
    router: &MultiServiceRouter,
    group_id: &str,
    resource_association_id: &str,
) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let group = route53resolver(
            router,
            "GetFirewallRuleGroup",
            &json!({ "FirewallRuleGroupId": group_id }),
        )
        .await;
        let rules = route53resolver(
            router,
            "ListFirewallRules",
            &json!({ "FirewallRuleGroupId": group_id }),
        )
        .await;
        let association = profiles_ok(
            router,
            "GET",
            &format!("/profileresourceassociation/{resource_association_id}"),
            None,
        )
        .await;
        let group_settled = group
            .pointer("/FirewallRuleGroup/Status")
            .and_then(Value::as_str)
            == Some(COMPLETE);
        let rules_listed = rules
            .get("FirewallRules")
            .and_then(Value::as_array)
            .map(|listed| {
                listed.len() == DECLARED_RULES.len()
                    && listed.iter().all(|rule| {
                        rule.get("Status")
                            .and_then(Value::as_str)
                            .is_none_or(|status| status == COMPLETE)
                    })
            })
            .unwrap_or_default();
        let association_settled = association
            .pointer("/ProfileResourceAssociation/Status")
            .and_then(Value::as_str)
            == Some(COMPLETE);
        if group_settled && rules_listed && association_settled {
            // The association reports the resource it carries, as the one that pushes a rule
            // group to a VPC must.
            assert_eq!(
                association
                    .pointer("/ProfileResourceAssociation/ResourceType")
                    .and_then(Value::as_str),
                Some("FIREWALL_RULE_GROUP"),
                "setup: the resource association must report the kind of resource it carries, \
                 got {association}"
            );
            return;
        }
        let last = json!({ "group": group, "rules": rules, "association": association });
        assert!(
            Instant::now() < deadline,
            "setup: the firewall must settle, it still reports {last}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The template the suite deploys: the execution role, and the probe function ATTACHED to the
/// VPC the Profile is associated with. The inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist.
fn stack_template(network: &Network) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ExecRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": EXEC_ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "probe-grants",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "logs:CreateLogGroup",
                                    "logs:CreateLogStream",
                                    "logs:PutLogEvents",
                                    "ec2:CreateNetworkInterface",
                                    "ec2:DescribeNetworkInterfaces",
                                    "ec2:DeleteNetworkInterface",
                                ],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["ExecRole", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "MemorySize": FUNCTION_MEMORY,
                    "VpcConfig": {
                        "SubnetIds": [network.subnet],
                        "SecurityGroupIds": [network.group],
                    },
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy that stack and wait for `CREATE_COMPLETE`.
async fn deploy(router: &MultiServiceRouter, network: &Network) {
    let template = stack_template(network);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({STACK:?}) must succeed, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE {
            return;
        }
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            let (_, events) =
                cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
            panic!(
                "setup: the stack must reach {CREATE_COMPLETE}, it reports {reported} with \
                 resource failures {:?}",
                elements(&events, "ResourceStatusReason")
            );
        }
        assert!(
            Instant::now() < deadline,
            "setup: the stack must finish, it still reports {reported:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Wait until Lambda reports the probe function `Active`, which is when it can be invoked.
async fn wait_until_active(router: &MultiServiceRouter) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: Lambda must report {FUNCTION:?} Active, it still reports {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// One row of the table: the query, and what it must be answered with before, during and after
/// the Profile carries the rule group.
struct Row {
    case: &'static str,
    /// The name the query asks about.
    name: &'static str,
    /// The record type it asks for.
    qtype: u64,
    /// The response code and records the query is answered with while NO rule group reaches the
    /// VPC — which is also what it must be answered with again once the Profile is
    /// disassociated from it.
    unfiltered: (u64, Vec<(u64, String)>),
    /// The response code and records it is answered with while the Profile carries the rule
    /// group.
    filtered: (u64, Vec<(u64, String)>),
    /// The TTL every record of the answer must carry, where the rule that answered STATES one
    /// (`BlockOverrideTtl`); `None` for a row whose TTL is the resolver's own.
    ttl: Option<u64>,
}

/// The records a row expects: one `(type, value)` per record of the answer section.
fn no_records() -> Vec<(u64, String)> {
    Vec::new()
}

/// The `CNAME` record type, as RFC 1035 numbers it: the type an `OVERRIDE` block answers with.
const TYPE_CNAME: u64 = 5;

/// The rows: one per form the rule generalises over.
fn rows() -> Vec<Row> {
    // The answer the VPC's own internal zone holds for the Amazon-provided hostname of an
    // address in it: that address.
    let in_vpc_record = vec![(TYPE_A, IN_VPC_ADDRESS.to_string())];
    vec![
        Row {
            case: "a name a BLOCK rule's list holds exactly",
            name: BLOCKED_NAME,
            qtype: TYPE_A,
            unfiltered: (RCODE_SERVFAIL, no_records()),
            filtered: (RCODE_NXDOMAIN, no_records()),
            ttl: None,
        },
        Row {
            case: "a name under a wildcard entry, on a rule answering NODATA",
            name: QUIET_SUBDOMAIN,
            qtype: TYPE_A,
            unfiltered: (RCODE_SERVFAIL, no_records()),
            filtered: (RCODE_NOERROR, no_records()),
            ttl: None,
        },
        Row {
            case: "the domain of that wildcard entry itself",
            name: QUIET_DOMAIN,
            qtype: TYPE_A,
            unfiltered: (RCODE_SERVFAIL, no_records()),
            filtered: (RCODE_NOERROR, no_records()),
            ttl: None,
        },
        Row {
            case: "a name on a rule answering its own override record",
            name: OVERRIDDEN_NAME,
            qtype: TYPE_A,
            unfiltered: (RCODE_SERVFAIL, no_records()),
            filtered: (
                RCODE_NOERROR,
                // `BlockOverrideDnsType` CNAME, holding the rule's own domain.
                vec![(TYPE_CNAME, format!("{OVERRIDE_TARGET}."))],
            ),
            // The rule recommends how long its record may be cached, so the answer carries it.
            ttl: Some(OVERRIDE_TTL),
        },
        Row {
            case: "a name whose rule evaluates another query type, asked as A",
            name: TYPED_NAME,
            qtype: TYPE_A,
            unfiltered: (RCODE_SERVFAIL, no_records()),
            filtered: (RCODE_SERVFAIL, no_records()),
            ttl: None,
        },
        Row {
            case: "that same name asked as the type its rule evaluates",
            name: TYPED_NAME,
            qtype: TYPE_AAAA,
            unfiltered: (RCODE_SERVFAIL, no_records()),
            filtered: (RCODE_NXDOMAIN, no_records()),
            ttl: None,
        },
        Row {
            case: "a name no domain list of the group holds",
            name: UNLISTED_NAME,
            qtype: TYPE_A,
            unfiltered: (RCODE_SERVFAIL, no_records()),
            filtered: (RCODE_SERVFAIL, no_records()),
            ttl: None,
        },
        Row {
            case: "a hostname of the VPC's own internal zone, on no domain list",
            name: IN_VPC_HOSTNAME,
            qtype: TYPE_A,
            unfiltered: (RCODE_NOERROR, in_vpc_record.clone()),
            filtered: (RCODE_NOERROR, in_vpc_record),
            ttl: None,
        },
    ]
}

/// Invoke the probe once, asking every row's query of `resolver`, and answer the replies.
async fn probed(
    router: &MultiServiceRouter,
    phase: &str,
    resolver: &str,
    rows: &[Row],
) -> Vec<Value> {
    let probes: Vec<Value> = rows
        .iter()
        .map(|row| json!({ "name": row.name, "type": row.qtype }))
        .collect();
    let event = json!({ "resolver": resolver, "probes": probes });
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "phase {phase:?} at {resolver}: the invocation must be served, got {status} with {payload}"
    );
    assert_eq!(
        headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok()),
        None,
        "phase {phase:?} at {resolver}: the in-VPC querier must run to completion, got {payload}"
    );
    let results = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("phase {phase:?} at {resolver}: the handler must report one result per probe, \
                    got {payload}")
        });
    assert_eq!(
        results.len(),
        rows.len(),
        "phase {phase:?} at {resolver}: expected one result per probe, got {results:?}"
    );
    results
}

/// Hold every row's reply to what the phase expects.
fn assert_answers(
    phase: &str,
    resolver: &str,
    rows: &[Row],
    results: &[Value],
    expected: impl Fn(&Row) -> (u64, Vec<(u64, String)>),
) {
    for (row, result) in rows.iter().zip(results) {
        let (code, records) = expected(row);
        let case = row.case;
        assert_eq!(
            result.get("rcode").and_then(Value::as_u64),
            Some(code),
            "phase {phase:?} at {resolver}, case {case:?}: expected response code {code} for \
             {} ({}), got {result}",
            row.name,
            row.qtype
        );
        let held = result
            .get("records")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let reported: Vec<(u64, String)> = held
            .iter()
            .map(|record| {
                (
                    record.get("type").and_then(Value::as_u64).unwrap_or_default(),
                    record
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect();
        assert_eq!(
            reported, records,
            "phase {phase:?} at {resolver}, case {case:?}: expected the records {records:?} for \
             {}, got {result}",
            row.name
        );
        if let Some(ttl) = row.ttl.filter(|_| !records.is_empty()) {
            for record in &held {
                assert_eq!(
                    record.get("ttl").and_then(Value::as_u64),
                    Some(ttl),
                    "phase {phase:?} at {resolver}, case {case:?}: expected the record to carry \
                     the TTL {ttl} the rule states, got {result}"
                );
            }
        }
        assert_eq!(
            result.get("ancount").and_then(Value::as_u64),
            Some(records.len() as u64),
            "phase {phase:?} at {resolver}, case {case:?}: expected the answer count to agree \
             with the records, got {result}"
        );
        // The reply came back from the address the query was sent to, as a datagram's answer
        // does.
        assert_eq!(
            result
                .get("peer")
                .and_then(Value::as_array)
                .and_then(|peer| peer.first())
                .and_then(Value::as_str),
            Some(resolver),
            "phase {phase:?} at {resolver}, case {case:?}: expected the reply to come from \
             {resolver}, got {result}"
        );
    }
}

#[tokio::test]
async fn a_profiles_firewall_rule_group_decides_what_the_vpcs_resolver_answers() {
    let router = build_router();
    let network = ec2_network(&router).await;
    let (group_id, group_arn) = plant_the_rule_group(&router).await;
    let profile = profiles_ok(
        &router,
        "POST",
        "/profile",
        Some(&json!({ "ClientToken": "xsvc-r53p-profile", "Name": "xsvc-r53p-profile" })),
    )
    .await;
    let profile_id = string_of(
        profile
            .get("Profile")
            .unwrap_or_else(|| panic!("setup: CreateProfile must answer a Profile, got {profile}")),
        "Id",
    );
    deploy(&router, &network).await;
    wait_until_active(&router).await;
    let rows = rows();

    // Before anything reaches the VPC: the resolver answers out of its own zone alone.
    let before = probed(&router, "before the Profile carries a rule group", LINK_LOCAL_RESOLVER, &rows)
        .await;
    assert_answers(
        "before the Profile carries a rule group",
        LINK_LOCAL_RESOLVER,
        &rows,
        &before,
        |row| row.unfiltered.clone(),
    );

    // The rule group reaches the Profile, and the Profile the VPC.
    let resource_association = profiles_ok(
        &router,
        "POST",
        "/profileresourceassociation",
        Some(&json!({
            "ProfileId": profile_id,
            "Name": "xsvc-r53p-firewall-resource",
            "ResourceArn": group_arn,
            // "If you are adding a DNS Firewall rule group, include also a priority", which
            // must be "between 100 and 9900".
            "ResourceProperties": r#"{"priority": 101}"#,
        })),
    )
    .await;
    let resource_association_id = string_of(
        resource_association
            .get("ProfileResourceAssociation")
            .unwrap_or_else(|| {
                panic!(
                    "setup: the associate must answer a ProfileResourceAssociation, got \
                     {resource_association}"
                )
            }),
        "Id",
    );
    let vpc_association = profiles_ok(
        &router,
        "POST",
        "/profileassociation",
        Some(&json!({
            "ProfileId": profile_id,
            "Name": "xsvc-r53p-vpc-association",
            "ResourceId": network.vpc,
        })),
    )
    .await;
    assert_eq!(
        vpc_association
            .pointer("/ProfileAssociation/ResourceId")
            .and_then(Value::as_str),
        Some(network.vpc.as_str()),
        "setup: the association must name the VPC it was made for, got {vpc_association}"
    );
    wait_until_the_firewall_has_settled(&router, &group_id, &resource_association_id).await;

    // Both addresses the VPC's resolver answers at are filtered by the same rules.
    for resolver in [LINK_LOCAL_RESOLVER, RANGE_RESOLVER] {
        let filtered = probed(&router, "while the Profile carries the rule group", resolver, &rows)
            .await;
        assert_answers(
            "while the Profile carries the rule group",
            resolver,
            &rows,
            &filtered,
            |row| row.filtered.clone(),
        );
    }

    // The push is the ASSOCIATION's: dissociating the Profile from the VPC takes the filtering
    // away again, which is what makes it observable at all.
    profiles_ok(
        &router,
        "DELETE",
        &format!("/profileassociation/Profileid/{profile_id}/resourceid/{}", network.vpc),
        None,
    )
    .await;
    let after = probed(
        &router,
        "after the Profile is disassociated from the VPC",
        LINK_LOCAL_RESOLVER,
        &rows,
    )
    .await;
    assert_answers(
        "after the Profile is disassociated from the VPC",
        LINK_LOCAL_RESOLVER,
        &rows,
        &after,
        |row| row.unfiltered.clone(),
    );
}
