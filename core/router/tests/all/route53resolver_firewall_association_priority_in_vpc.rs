// SPDX-License-Identifier: CC0-1.0

//! Which DNS FIREWALL rule decides a query asked INSIDE a VPC when more than one rule group
//! reaches it — read the only way it can be read: from a program running in that VPC.
//!
//! RULE: a query a client inside a VPC sends to the Amazon-provided resolver is decided by the
//! first matching rule of the DNS Firewall rule groups associated with that VPC, taken in
//! ascending order of the `Priority` their associations carry and then of the rules' own
//! `Priority` inside a group; the rule that decides answers a `BLOCK` its own `BlockResponse`
//! way (`NXDOMAIN` as a name that does not exist, `NODATA` as a successful answer with no
//! record, `OVERRIDE` with the rule's own record), a rule naming a `Qtype` decides only a query
//! asking that type, and the order is read off the associations as they stand — so changing an
//! association's `Priority`, or taking it away, changes which rule decides.
//!
//! The program that asks is a Lambda function attached to the VPC, and its handler module is
//! built out of the standard library — including the modules that touch the platform's FORK
//! machinery, which the standard library registers callbacks with as they are imported. A
//! module like that imports cleanly, so the invocation answers the querier's own result with no
//! `X-Amz-Function-Error`: a handler that cannot be imported is no vantage point at all, and
//! nothing about a VPC's resolver is observable without one.
//!
//! The rows differ along what the rule generalises over: which of two associated rule groups
//! holds the matching rule, the `BlockResponse` of the rule that decides (`NXDOMAIN`,
//! `OVERRIDE`, `NODATA`), the FORM of the domain list entry that covers the name (a plain
//! domain, a wildcard entry, the domain under a wildcard entry), whether the deciding rule names
//! a `Qtype` and whether the query asks that type, a name no domain list covers, and a name of
//! the VPC's own internal zone. Each row is asked in FIVE phases — before any rule group is
//! associated, while one group is associated ahead of the other, after the two associations'
//! priorities are reordered, after the second group is disassociated, and after both are — so
//! what is asserted is the DIFFERENCE the order of the associations makes and never one static
//! answer.
//!
//! Only an assembled emulator can pin it: Amazon EC2 owns the VPC, its subnet and the interface
//! the function's queries leave from, Route 53 Resolver owns the rule groups, their rules, their
//! domain lists and the associations that bring them to the VPC, CloudFormation and IAM deploy
//! the function, and AWS Lambda runs it inside the VPC.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};
// The querier itself is the one the sibling in-VPC firewall suite drives, so both suites read
// this resolver through the same DNS framing.
use crate::route53profiles_firewall_in_vpc_resolution::PROBE_HANDLER;

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version an ec2Query form body names.
const EC2_API_VERSION: &str = "2016-11-15";

/// The network the function sits in, and the resolver address its queries go to: the link-local
/// one the Amazon-provided DNS server answers at.
const VPC_CIDR: &str = "10.72.0.0/16";
/// See [`VPC_CIDR`].
const SUBNET_CIDR: &str = "10.72.1.0/24";
/// See [`VPC_CIDR`].
const ZONE: &str = "us-west-2a";
/// See [`VPC_CIDR`].
const LINK_LOCAL_RESOLVER: &str = "169.254.169.253";

/// An address of the VPC and the Amazon-provided hostname of it — the name the VPC's own
/// internal zone answers, which is the row proving a name on no domain list still resolves as it
/// did. "AWS uses `<region>.compute.internal`" outside `us-east-1` (Amazon VPC User Guide, "DHCP
/// option sets").
const IN_VPC_ADDRESS: &str = "10.72.1.80";
/// See [`IN_VPC_ADDRESS`].
const IN_VPC_HOSTNAME: &str = "ip-10-72-1-80.us-west-2.compute.internal";

/// The name BOTH rule groups hold a rule for, which is how the order of their associations
/// becomes observable: the two rules answer it different ways.
const SHARED_NAME: &str = "shared.example.com";

/// The wildcard entry only the second rule group holds, the domain it stands for and a name
/// under it.
const QUIET_ENTRY: &str = "*.quiet.example.net";
/// See [`QUIET_ENTRY`].
const QUIET_DOMAIN: &str = "quiet.example.net";
/// See [`QUIET_ENTRY`].
const QUIET_SUBDOMAIN: &str = "feed.quiet.example.net";

/// The name whose rule evaluates ONE query type, and a name no domain list of either group
/// holds.
const TYPED_NAME: &str = "typed.example.com";
/// See [`TYPED_NAME`].
const UNLISTED_NAME: &str = "control.example.net";

/// The record the `OVERRIDE` rule answers with, and the TTL it recommends for it — well inside
/// `BlockOverrideTtl`'s modelled `0..=604800` range.
const OVERRIDE_TARGET: &str = "safe.example.net";
/// See [`OVERRIDE_TARGET`].
const OVERRIDE_TTL: u64 = 42;

/// The DNS record types a probe asks for, as RFC 1035 numbers them.
const TYPE_A: u64 = 1;
/// See [`TYPE_A`].
const TYPE_AAAA: u64 = 28;
/// See [`TYPE_A`].
const TYPE_CNAME: u64 = 5;

/// The response codes a reply carries, as RFC 1035 §4.1.1 numbers them: no error, a server
/// failure — what this VPC's resolver answers a name it would have to recurse for — and a name
/// that does not exist.
const RCODE_NOERROR: u64 = 0;
/// See [`RCODE_NOERROR`].
const RCODE_SERVFAIL: u64 = 2;
/// See [`RCODE_NOERROR`].
const RCODE_NXDOMAIN: u64 = 3;

/// The stack, the execution role and the function that carry the querier.
const STACK: &str = "xsvc-r53r-priority-probe";
/// See [`STACK`].
const EXEC_ROLE: &str = "xsvc-r53r-priority-exec";
/// See [`STACK`].
const FUNCTION: &str = "xsvc-r53r-priority-probe-fn";

/// The `StackStatus` the probe stack must reach, and the suffix of every status that is an
/// operation still in flight.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The terminal status every resource the firewall is read off reports once the work its create
/// or update started is done: a rule group, a rule and a rule-group association all settle to
/// `COMPLETE`.
const COMPLETE: &str = "COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`, and how much memory it is
/// configured with. Both generous: what is under test is what the resolver answers, never how
/// fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;
/// See [`FUNCTION_TIMEOUT`].
const FUNCTION_MEMORY: u64 = 3_008;

/// How often to look while waiting for work a service's own clock owes — a stack completing, a
/// function reaching `Active`, an association settling — and how long to keep looking. Nothing
/// here sleeps for a fixed span: every wait is for the CONDITION, and the deadline is generous so
/// a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(25);
/// See [`LOOK`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// What the querier's module imports BEFORE the DNS framing it is built out of: the
/// standard-library modules that touch the platform's fork machinery, which is what a dig-style
/// helper pulls in. They are imported by the module BODY, so a handler that runs at all is a
/// clean import of every one of them.
const FORK_MACHINERY: &str = "\
import concurrent.futures\n\
import logging\n\
import random\n\
\n\
logging.getLogger(__name__).addHandler(logging.NullHandler())\n\
_JITTER = random.Random(0).random()\n\
";

/// The source the probe function is deployed with: the fork machinery above, then the querier
/// the sibling suite drives.
fn probe_source() -> String {
    format!("{FORK_MACHINERY}{PROBE_HANDLER}")
}

/// The emulator as it is ASSEMBLED, on one engine: every service this suite drives is the one the
/// router mounts, so the firewall the resolver consults is the one Route 53 Resolver registered
/// when it was mounted.
fn build_router() -> MultiServiceRouter {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
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

/// One Route 53 Resolver operation over its `awsJson1_1` wire, as its status and its parsed body.
async fn resolver_call(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
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
    (status, payload)
}

/// One Route 53 Resolver operation that must succeed.
async fn resolver(router: &MultiServiceRouter, operation: &str, request: &Value) -> Value {
    let (status, payload) = resolver_call(router, operation, request).await;
    assert_eq!(
        status, 200,
        "setup: Route53Resolver.{operation} must succeed for {request}, got {status} with \
         {payload}"
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

/// The network the function and the rule-group associations both address.
struct Network {
    /// `Vpc$VpcId`, which is what a rule group is associated WITH.
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
                ("GroupName", "xsvc-r53r-priority-group"),
                ("GroupDescription", "the group the probe function carries"),
            ],
        )
        .await,
        "groupId",
    );
    Network { vpc, subnet, group }
}

/// One rule of a rule group this suite builds: the entries of its own domain list, and the
/// members of the rule itself.
struct DeclaredRule {
    /// What the rule is called, which is also what its domain list is called.
    name: &'static str,
    /// The entries of its domain list.
    domains: &'static [&'static str],
    /// `CreateFirewallRule$Priority`, unique inside its group.
    priority: u64,
    /// `CreateFirewallRule$BlockResponse`.
    block_response: &'static str,
    /// `CreateFirewallRule$Qtype`, where the rule evaluates one type only.
    qtype: Option<&'static str>,
}

/// One rule group this suite builds, and the rules it carries.
struct DeclaredGroup {
    /// `CreateFirewallRuleGroup$Name`, which is also its `CreatorRequestId`.
    name: &'static str,
    /// Its rules.
    rules: &'static [DeclaredRule],
}

/// The group whose rule for [`SHARED_NAME`] answers `NXDOMAIN`, and which also holds the one rule
/// scoped to a single query type.
const NXDOMAIN_GROUP: DeclaredGroup = DeclaredGroup {
    name: "xsvc-r53r-nxdomain-group",
    rules: &[
        DeclaredRule {
            name: "xsvc-r53r-shared-nxdomain",
            domains: &[SHARED_NAME],
            priority: 100,
            block_response: "NXDOMAIN",
            qtype: None,
        },
        DeclaredRule {
            name: "xsvc-r53r-typed",
            domains: &[TYPED_NAME],
            priority: 200,
            block_response: "NXDOMAIN",
            qtype: Some("AAAA"),
        },
    ],
};

/// The group whose rule for [`SHARED_NAME`] answers the rule's own record, and which also holds
/// the wildcard entry no rule of the other group covers.
const OVERRIDE_GROUP: DeclaredGroup = DeclaredGroup {
    name: "xsvc-r53r-override-group",
    rules: &[
        DeclaredRule {
            name: "xsvc-r53r-shared-override",
            domains: &[SHARED_NAME],
            priority: 100,
            block_response: "OVERRIDE",
            qtype: None,
        },
        DeclaredRule {
            name: "xsvc-r53r-quiet",
            domains: &[QUIET_ENTRY],
            priority: 200,
            block_response: "NODATA",
            qtype: None,
        },
    ],
};

/// A rule group as this suite reads it back: the id Route 53 Resolver minted for it and how many
/// rules it holds.
struct Planted {
    id: String,
    rules: usize,
}

/// Create one rule group, its domain lists and its rules through Route 53 Resolver's own
/// operations.
async fn plant(router: &MultiServiceRouter, declared: &DeclaredGroup) -> Planted {
    let answered = resolver(
        router,
        "CreateFirewallRuleGroup",
        &json!({ "CreatorRequestId": declared.name, "Name": declared.name }),
    )
    .await;
    let group = answered.get("FirewallRuleGroup").cloned().unwrap_or_else(|| {
        panic!("setup: the create must answer a FirewallRuleGroup, got {answered}")
    });
    let group_id = string_of(&group, "Id");
    for rule in declared.rules {
        let list = resolver(
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
        resolver(
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
        resolver(router, "CreateFirewallRule", &request).await;
    }
    Planted {
        id: group_id,
        rules: declared.rules.len(),
    }
}

/// The string member `member` of `document`.
fn string_of(document: &Value, member: &str) -> String {
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: {document} must report {member}"))
        .to_string()
}

/// Associate `group` with the VPC at `priority`, and answer the id of the association.
async fn associate(
    router: &MultiServiceRouter,
    group: &Planted,
    vpc: &str,
    priority: u64,
    name: &str,
) -> String {
    let answered = resolver(
        router,
        "AssociateFirewallRuleGroup",
        &json!({
            "CreatorRequestId": name,
            "FirewallRuleGroupId": group.id,
            "VpcId": vpc,
            "Priority": priority,
            "Name": name,
        }),
    )
    .await;
    let association = answered
        .get("FirewallRuleGroupAssociation")
        .cloned()
        .unwrap_or_else(|| {
            panic!("setup: the associate must answer a FirewallRuleGroupAssociation, got {answered}")
        });
    assert_eq!(
        association.get("VpcId").and_then(Value::as_str),
        Some(vpc),
        "setup: the association must name the VPC it was made for, got {association}"
    );
    string_of(&association, "Id")
}

/// Wait until every resource a decision is read off has settled: each rule group and each of its
/// rules, and each association named, which must also report the priority it was last asked for.
async fn wait_until_settled(
    router: &MultiServiceRouter,
    groups: &[&Planted],
    associations: &[(&str, u64)],
) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut settled = true;
        let mut last = Vec::new();
        for group in groups {
            let read = resolver(
                router,
                "GetFirewallRuleGroup",
                &json!({ "FirewallRuleGroupId": group.id }),
            )
            .await;
            let rules = resolver(
                router,
                "ListFirewallRules",
                &json!({ "FirewallRuleGroupId": group.id }),
            )
            .await;
            let group_settled =
                read.pointer("/FirewallRuleGroup/Status").and_then(Value::as_str) == Some(COMPLETE);
            let rules_settled = rules
                .get("FirewallRules")
                .and_then(Value::as_array)
                .map(|listed| {
                    listed.len() == group.rules
                        && listed.iter().all(|rule| {
                            rule.get("Status")
                                .and_then(Value::as_str)
                                .is_none_or(|status| status == COMPLETE)
                        })
                })
                .unwrap_or_default();
            settled &= group_settled && rules_settled;
            last.push(json!({ "group": read, "rules": rules }));
        }
        for (association_id, priority) in associations {
            let read = resolver(
                router,
                "GetFirewallRuleGroupAssociation",
                &json!({ "FirewallRuleGroupAssociationId": association_id }),
            )
            .await;
            let reported = read.pointer("/FirewallRuleGroupAssociation/Status");
            if reported.and_then(Value::as_str) == Some(COMPLETE) {
                // The association carries the order it was last asked for, which is what the
                // decisions below are read in.
                assert_eq!(
                    read.pointer("/FirewallRuleGroupAssociation/Priority")
                        .and_then(Value::as_u64),
                    Some(*priority),
                    "setup: the settled association must report the priority it was asked for, \
                     got {read}"
                );
            } else {
                settled = false;
            }
            last.push(read);
        }
        if settled {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the firewall must settle, it still reports {last:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The template the suite deploys: the execution role, and the probe function ATTACHED to the VPC
/// the rule groups are associated with. The inline source is archived into the deployment package
/// by CloudFormation, which is how a function whose handler is written here comes to exist.
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
                    "Code": {"ZipFile": probe_source()},
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
        if !reported.is_empty() && !reported.ends_with(IN_PROGRESS) {
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

/// What one query must be answered with: the response code, the records of the answer section as
/// `(type, value)` pairs, and the TTL every record must carry where the rule that answered STATES
/// one (`BlockOverrideTtl`).
#[derive(Clone, Debug)]
struct Answer {
    rcode: u64,
    records: Vec<(u64, String)>,
    ttl: Option<u64>,
}

/// A name this VPC's resolver would have to recurse for, which it has no answer for.
fn unresolved() -> Answer {
    Answer {
        rcode: RCODE_SERVFAIL,
        records: Vec::new(),
        ttl: None,
    }
}

/// A `BLOCK` answered `NXDOMAIN`: "the domain name that's in the query doesn't exist".
fn nxdomain() -> Answer {
    Answer {
        rcode: RCODE_NXDOMAIN,
        records: Vec::new(),
        ttl: None,
    }
}

/// A `BLOCK` answered `NODATA`: "the query was successful, but no response is available for it".
fn nodata() -> Answer {
    Answer {
        rcode: RCODE_NOERROR,
        records: Vec::new(),
        ttl: None,
    }
}

/// A `BLOCK` answered `OVERRIDE`: the rule's own record, carrying the TTL it recommends.
fn overridden() -> Answer {
    Answer {
        rcode: RCODE_NOERROR,
        records: vec![(TYPE_CNAME, format!("{OVERRIDE_TARGET}."))],
        ttl: Some(OVERRIDE_TTL),
    }
}

/// The answer the VPC's own internal zone holds for the Amazon-provided hostname of an address in
/// it: that address.
fn in_vpc_hostname() -> Answer {
    Answer {
        rcode: RCODE_NOERROR,
        records: vec![(TYPE_A, IN_VPC_ADDRESS.to_string())],
        ttl: None,
    }
}

/// The phases every row is asked in, in order.
const PHASES: [&str; 5] = [
    "before either rule group is associated with the VPC",
    "while the NXDOMAIN group is associated ahead of the OVERRIDE group",
    "after the NXDOMAIN group's association is moved behind the OVERRIDE group's",
    "after the OVERRIDE group is disassociated, leaving the NXDOMAIN group alone",
    "after both rule groups are disassociated",
];

/// One row of the table: a query, and what it must be answered with in each phase.
struct Row {
    case: &'static str,
    /// The name the query asks about.
    name: &'static str,
    /// The record type it asks for.
    qtype: u64,
    /// One answer per entry of [`PHASES`].
    phases: Vec<Answer>,
}

/// The rows: one per form the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The whole ordering story in one row: both groups hold a rule for this name, so
            // which answer comes back names the association that decided.
            case: "a name both groups hold a rule for, asked as A",
            name: SHARED_NAME,
            qtype: TYPE_A,
            phases: vec![
                unresolved(),
                nxdomain(),
                overridden(),
                nxdomain(),
                unresolved(),
            ],
        },
        Row {
            case: "a name under a wildcard entry only the second group holds",
            name: QUIET_SUBDOMAIN,
            qtype: TYPE_A,
            phases: vec![unresolved(), nodata(), nodata(), unresolved(), unresolved()],
        },
        Row {
            case: "the domain of that wildcard entry itself",
            name: QUIET_DOMAIN,
            qtype: TYPE_A,
            phases: vec![unresolved(), nodata(), nodata(), unresolved(), unresolved()],
        },
        Row {
            case: "a name whose only rule evaluates another query type, asked as A",
            name: TYPED_NAME,
            qtype: TYPE_A,
            phases: vec![
                unresolved(),
                unresolved(),
                unresolved(),
                unresolved(),
                unresolved(),
            ],
        },
        Row {
            case: "that same name asked as the type its rule evaluates",
            name: TYPED_NAME,
            qtype: TYPE_AAAA,
            phases: vec![
                unresolved(),
                nxdomain(),
                nxdomain(),
                nxdomain(),
                unresolved(),
            ],
        },
        Row {
            case: "a name no domain list of either group holds",
            name: UNLISTED_NAME,
            qtype: TYPE_A,
            phases: vec![
                unresolved(),
                unresolved(),
                unresolved(),
                unresolved(),
                unresolved(),
            ],
        },
        Row {
            case: "a hostname of the VPC's own internal zone, on no domain list",
            name: IN_VPC_HOSTNAME,
            qtype: TYPE_A,
            phases: vec![
                in_vpc_hostname(),
                in_vpc_hostname(),
                in_vpc_hostname(),
                in_vpc_hostname(),
                in_vpc_hostname(),
            ],
        },
    ]
}

/// Ask every row's query from inside the VPC, in one invocation, and hold the replies to what the
/// phase expects.
async fn probe_phase(router: &MultiServiceRouter, phase: usize, rows: &[Row]) {
    let case = PHASES[phase];
    let probes: Vec<Value> = rows
        .iter()
        .map(|row| json!({ "name": row.name, "type": row.qtype }))
        .collect();
    let event = json!({ "resolver": LINK_LOCAL_RESOLVER, "probes": probes });
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "phase {case:?}: the invocation must be served, got {status} with {payload}"
    );
    // A querier whose module is built out of the standard library imports cleanly, so the
    // invocation answers the handler's own result: an import failure here is no vantage point
    // inside the VPC at all.
    assert_eq!(
        headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok()),
        None,
        "phase {case:?}: the in-VPC querier must run to completion, got {payload}"
    );
    let results = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("phase {case:?}: the handler must report one result per probe, got {payload}")
        });
    assert_eq!(
        results.len(),
        rows.len(),
        "phase {case:?}: expected one result per probe, got {results:?}"
    );
    for (row, result) in rows.iter().zip(&results) {
        let expected = &row.phases[phase];
        let row_case = row.case;
        assert_eq!(
            result.get("rcode").and_then(Value::as_u64),
            Some(expected.rcode),
            "phase {case:?}, case {row_case:?}: expected response code {} for {} ({}), got \
             {result}",
            expected.rcode,
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
            reported, expected.records,
            "phase {case:?}, case {row_case:?}: expected the records {:?} for {}, got {result}",
            expected.records, row.name
        );
        if let Some(ttl) = expected.ttl {
            for record in &held {
                assert_eq!(
                    record.get("ttl").and_then(Value::as_u64),
                    Some(ttl),
                    "phase {case:?}, case {row_case:?}: expected the record to carry the TTL \
                     {ttl} the rule states, got {result}"
                );
            }
        }
        assert_eq!(
            result.get("ancount").and_then(Value::as_u64),
            Some(expected.records.len() as u64),
            "phase {case:?}, case {row_case:?}: expected the answer count to agree with the \
             records, got {result}"
        );
        // The reply came back from the address the query was sent to, as a datagram's answer
        // does.
        assert_eq!(
            result
                .get("peer")
                .and_then(Value::as_array)
                .and_then(|peer| peer.first())
                .and_then(Value::as_str),
            Some(LINK_LOCAL_RESOLVER),
            "phase {case:?}, case {row_case:?}: expected the reply to come from \
             {LINK_LOCAL_RESOLVER}, got {result}"
        );
    }
}

#[tokio::test]
async fn the_association_priority_decides_which_rule_answers_an_in_vpc_query() {
    let router = build_router();
    let network = ec2_network(&router).await;
    let nxdomain_group = plant(&router, &NXDOMAIN_GROUP).await;
    let override_group = plant(&router, &OVERRIDE_GROUP).await;
    deploy(&router, &network).await;
    wait_until_active(&router).await;
    let rows = rows();

    // Nothing filters the VPC's queries yet: the resolver answers out of its own zone alone.
    probe_phase(&router, 0, &rows).await;

    // Both rule groups reach the VPC, the one answering NXDOMAIN first.
    let ahead = associate(
        &router,
        &nxdomain_group,
        &network.vpc,
        101,
        "xsvc-r53r-nxdomain-association",
    )
    .await;
    let behind = associate(
        &router,
        &override_group,
        &network.vpc,
        200,
        "xsvc-r53r-override-association",
    )
    .await;
    let groups = [&nxdomain_group, &override_group];
    wait_until_settled(&router, &groups, &[(&ahead, 101), (&behind, 200)]).await;
    probe_phase(&router, 1, &rows).await;

    // The order is the ASSOCIATIONS': moving the first one behind the second hands the shared
    // name to the other group's rule, with nothing about either rule changed.
    resolver(
        &router,
        "UpdateFirewallRuleGroupAssociation",
        &json!({ "FirewallRuleGroupAssociationId": ahead, "Priority": 300 }),
    )
    .await;
    wait_until_settled(&router, &groups, &[(&ahead, 300), (&behind, 200)]).await;
    probe_phase(&router, 2, &rows).await;

    // Taking the second group away leaves its rules deciding nothing, and the group still
    // associated decides the shared name again.
    resolver(
        &router,
        "DisassociateFirewallRuleGroup",
        &json!({ "FirewallRuleGroupAssociationId": behind }),
    )
    .await;
    probe_phase(&router, 3, &rows).await;

    // With neither group associated, every name is answered as it was before any of them existed.
    resolver(
        &router,
        "DisassociateFirewallRuleGroup",
        &json!({ "FirewallRuleGroupAssociationId": ahead }),
    )
    .await;
    probe_phase(&router, 4, &rows).await;
}
