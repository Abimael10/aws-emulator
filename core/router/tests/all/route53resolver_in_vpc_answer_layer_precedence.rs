// SPDX-License-Identifier: CC0-1.0

//! What the Amazon-provided resolver of a VPC answers when EVERY layer of the account's own
//! records for that VPC bears on one query — read the only way such an answer can be read: from a
//! program running inside that VPC.
//!
//! RULE: a query a client inside a VPC sends to the Amazon-provided resolver — at the link-local
//! address or at the base of the VPC's range plus two — is answered by composing the records the
//! account holds for that VPC in a fixed order. The DNS Firewall rule groups associated with the
//! VPC decide it first, a matched `BLOCK` answering its own `BlockResponse` way and a rule naming
//! a `Qtype` deciding only a query asking that type; the Resolver rules associated with the VPC
//! decide it next, the most specific `DomainName` enclosing the name deciding, a `FORWARD` rule
//! sending the query to the resolvers its `TargetIps` name and a `SYSTEM` rule leaving it to the
//! VPC's own resolver; the private hosted zones associated with the VPC answer after them, with
//! their own records and TTLs; and only a name no layer of the three covers is left to what the
//! public namespace fixes — a name a DNS standard reserves for documentation answered out of that
//! standard, a name under a top-level domain the root zone never delegates as one that does not
//! exist, and any other as one this resolver cannot answer. A layer whose association the account
//! removes decides nothing any more, which hands the query to the next layer down.
//!
//! The ORDER is the whole subject: no API reports it, and each pair of layers is only
//! distinguishable where both cover one name. So every name below is held by a private hosted
//! zone of the VPC, and the rows differ along which layers cover it besides — a firewall rule and
//! the zone, a firewall rule and a forwarding rule and the zone, a forwarding rule and the zone,
//! a `SYSTEM` rule under that forwarding rule, a firewall rule scoped to one query type, and no
//! rule at all — beside the names outside every zone that only the public namespace answers.
//! Each row is asked in FOUR phases: before any association exists, with the firewall group and
//! both Resolver rules associated, after the firewall association is removed, and after the
//! Resolver rule associations are removed too — so what is asserted is the DIFFERENCE each layer
//! makes and never one static answer.
//!
//! Only an assembled emulator can pin it: Amazon EC2 owns the VPC, its subnets and the interface
//! the function's queries leave from, Route 53 owns the private hosted zone and its record sets,
//! Route 53 Resolver owns the rule groups, the domain lists, the Resolver rules, the outbound
//! endpoint and every association that brings them to the VPC, CloudFormation and IAM deploy the
//! querier, and AWS Lambda runs it inside the VPC.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};
// The querier is the one the sibling in-VPC firewall suites drive, so every suite reading this
// resolver reads it through the same DNS framing.
use crate::route53profiles_firewall_in_vpc_resolution::PROBE_HANDLER;

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version an `ec2Query` form body names.
const EC2_API_VERSION: &str = "2016-11-15";

/// The network the querier sits in. Two subnets in two Availability Zones, because an outbound
/// Resolver endpoint — which a `FORWARD` rule routes through — needs "at least two IP addresses,
/// in two different Availability Zones" (`CreateResolverEndpointRequest$IpAddresses`).
const VPC_CIDR: &str = "10.74.0.0/16";
/// See [`VPC_CIDR`].
const SUBNET_CIDRS: [&str; 2] = ["10.74.1.0/24", "10.74.2.0/24"];
/// The addresses the outbound endpoint is created on.
const OUTBOUND_IPS: [&str; 2] = ["10.74.1.10", "10.74.2.10"];

/// The two addresses the Amazon-provided DNS server of that VPC answers at: "the
/// 169.254.169.253 IP address, or the reserved IP address at the base of the VPC network range
/// plus two" (`ModifyVpcAttributeRequest$EnableDnsSupport`).
const LINK_LOCAL_RESOLVER: &str = "169.254.169.253";
/// See [`LINK_LOCAL_RESOLVER`].
const RANGE_RESOLVER: &str = "10.74.0.2";

/// An address inside the VPC that NOTHING answers DNS at, which is where the forwarding rule
/// sends the queries its domain covers.
const DEAD_TARGET: &str = "10.74.9.250";

/// The private hosted zone associated with the VPC, and the TTL its record sets carry — inside
/// `ResourceRecordSet$TTL`'s modelled `0..=172800`.
const ZONE: &str = "layers.internal";
/// See [`ZONE`].
const RECORD_TTL: u64 = 300;

/// A name of the zone no rule of any layer covers: the control row.
const OPEN: &str = "open.layers.internal";
/// See [`OPEN`].
const OPEN_ADDRESS: &str = "10.74.1.40";

/// A name of the zone a `BLOCK` rule answering `NXDOMAIN` also covers.
const BLOCKED: &str = "blocked.layers.internal";
/// See [`BLOCKED`].
const BLOCKED_ADDRESS: &str = "10.74.1.41";

/// A name of the zone a `BLOCK` rule answering `OVERRIDE` also covers.
const SWAPPED: &str = "swapped.layers.internal";
/// See [`SWAPPED`].
const SWAPPED_ADDRESS: &str = "10.74.1.42";

/// A name of the zone whose only firewall rule evaluates ONE query type.
const TYPED: &str = "typed.layers.internal";
/// See [`TYPED`].
const TYPED_ADDRESS: &str = "10.74.1.43";

/// The name the forwarding rule's `DomainName` is, which the zone also holds a record at.
const SENT: &str = "sent.layers.internal";
/// See [`SENT`].
const SENT_ADDRESS: &str = "10.74.1.44";

/// A name UNDER the forwarding rule's domain that a `BLOCK` rule answering `NODATA` also covers:
/// the one row where the firewall and a Resolver rule both reach the same query.
const FENCED: &str = "fenced.sent.layers.internal";
/// See [`FENCED`].
const FENCED_ADDRESS: &str = "10.74.1.45";

/// A name under the forwarding rule's domain that a more specific `SYSTEM` rule covers.
const PROCESSED: &str = "inner.sent.layers.internal";
/// See [`PROCESSED`].
const PROCESSED_ADDRESS: &str = "10.74.1.46";

/// A name of the zone the zone holds no record set at.
const ABSENT: &str = "nothing.layers.internal";

/// The record the `OVERRIDE` rule answers with, and the TTL it recommends — well inside
/// `BlockOverrideTtl`'s modelled `0..=604800` range.
const OVERRIDE_TARGET: &str = "safe.example.net";
/// See [`OVERRIDE_TARGET`].
const OVERRIDE_TTL: u64 = 42;

/// The public names no zone of the VPC holds, which only the public namespace answers: a
/// second-level domain RFC 2606 §3 reserves for documentation, the one host label published
/// beside another of them, a name under a top-level domain RFC 6761 §6 keeps undelegated, and a
/// name under a domain somebody owns.
const DOCUMENTATION_DOMAIN: &str = "example.com";
/// See [`DOCUMENTATION_DOMAIN`].
const DOCUMENTATION_HOST: &str = "www.example.org";
/// See [`DOCUMENTATION_DOMAIN`].
const NEVER_DELEGATED_NAME: &str = "ledger.nothing-here.invalid";
/// See [`DOCUMENTATION_DOMAIN`].
const OWNED_NAME: &str = "broker.example.com";
/// The address such a documentation name is answered with: the first address of the block
/// RFC 5737 §3 reserves for documentation (TEST-NET-1).
const DOCUMENTATION_ADDRESS: &str = "192.0.2.1";

/// The DNS record types a probe asks for, as RFC 1035 numbers them.
const TYPE_A: u64 = 1;
/// See [`TYPE_A`].
const TYPE_AAAA: u64 = 28;
/// See [`TYPE_A`].
const TYPE_CNAME: u64 = 5;

/// The response codes a reply carries, as RFC 1035 §4.1.1 numbers them.
const RCODE_NOERROR: u64 = 0;
/// See [`RCODE_NOERROR`].
const RCODE_SERVFAIL: u64 = 2;
/// See [`RCODE_NOERROR`].
const RCODE_NXDOMAIN: u64 = 3;

/// The stack, the execution role and the function that carry the querier.
const STACK: &str = "xsvc-r53r-layers-probe";
/// See [`STACK`].
const EXEC_ROLE: &str = "xsvc-r53r-layers-exec";
/// See [`STACK`].
const FUNCTION: &str = "xsvc-r53r-layers-probe-fn";

/// The `StackStatus` the probe stack must reach, and the suffix of every status that is an
/// operation still in flight.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// The terminal status the firewall resources and every association report once the work their
/// create started is done.
const COMPLETE: &str = "COMPLETE";

/// The status that makes a Resolver endpoint serve: "`OPERATIONAL` … the Resolver endpoint is
/// ready to be used".
const OPERATIONAL: &str = "OPERATIONAL";

/// How long one invocation is given, as the function's own `Timeout`, and how much memory it is
/// configured with. Both generous: what is under test is what the resolver answers, never how
/// fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;
/// See [`FUNCTION_TIMEOUT`].
const FUNCTION_MEMORY: u64 = 3_008;

/// How often to look while waiting for work a service's own clock owes — a stack completing, a
/// function reaching `Active`, an endpoint or an association settling — and how long to keep
/// looking. Nothing here sleeps for a fixed span: every wait is for the CONDITION, and the
/// deadline is generous, far past what any step takes, so a loaded host cannot decide an outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// The emulator as it is ASSEMBLED, on one engine: every service this suite drives is the one the
/// router mounts, so the firewall, the rules and the zones the resolver consults are the records
/// those services' own operations wrote.
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
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization(service))
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One Amazon EC2 action that must be served, as its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = form_call(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: Amazon EC2 {action} must be served, got {status} with {body}"
    );
    body
}

/// One CloudFormation action, as its status and its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "cloudformation", "2010-05-15", action, members).await
}

/// One Route 53 call over its `restXml` wire, as its status and its body.
async fn route53(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    let host = "route53.amazonaws.com";
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("route53"))
        .header(http::header::CONTENT_TYPE, "application/xml")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One Route 53 Resolver operation over its `awsJson1_1` wire, as its status and its parsed body.
async fn resolver(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let host = format!("route53resolver.{REGION}.amazonaws.com");
    let http_request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("route53resolver"))
        .header(http::header::CONTENT_TYPE, "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Route53Resolver.{operation}"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Route 53 Resolver operation that must be served, as its parsed body.
async fn resolver_ok(router: &MultiServiceRouter, operation: &str, request: &Value) -> Value {
    let (status, answered) = resolver(router, operation, request).await;
    assert_eq!(
        status, 200,
        "setup: Route53Resolver.{operation} must be served for {request}, got {status} with \
         {answered}"
    );
    answered
}

/// One AWS Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, http::HeaderMap, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("lambda"))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The string member `member` of `document`.
fn string_of(document: &Value, member: &str) -> String {
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: {document} must report {member}"))
        .to_string()
}

/// The estate every phase queries from.
struct Estate {
    router: MultiServiceRouter,
    /// `Vpc$VpcId` of the VPC the querier sits in, which every association names.
    vpc: String,
    /// The outbound endpoint a `FORWARD` rule of this VPC routes through.
    outbound_endpoint: String,
}

/// Create the VPC, its two subnets and a security group, and answer the ids.
async fn network(router: &MultiServiceRouter) -> (String, Vec<String>, String) {
    let created = ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await;
    let vpc = opt_element(&created, "vpcId")
        .unwrap_or_else(|| panic!("setup: CreateVpc must mint a VPC, got {created}"));
    let mut subnets = Vec::new();
    for (index, cidr) in SUBNET_CIDRS.iter().enumerate() {
        let zone = format!("{REGION}{}", ["a", "b"][index]);
        let answered = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr),
                ("AvailabilityZone", zone.as_str()),
            ],
        )
        .await;
        subnets.push(
            opt_element(&answered, "subnetId")
                .unwrap_or_else(|| panic!("setup: CreateSubnet must mint a subnet, {answered}")),
        );
    }
    let answered = ec2(
        router,
        "CreateSecurityGroup",
        &[
            ("VpcId", vpc.as_str()),
            ("GroupName", "xsvc-r53r-layers-group"),
            ("GroupDescription", "the group the querier carries"),
        ],
    )
    .await;
    let group = opt_element(&answered, "groupId")
        .unwrap_or_else(|| panic!("setup: CreateSecurityGroup must mint a group, {answered}"));
    (vpc, subnets, group)
}

/// Create the private hosted zone associated with `vpc` and write one `A` record set per entry of
/// `records`, through Route 53's own operations.
async fn private_zone(router: &MultiServiceRouter, vpc: &str, records: &[(&str, &str)]) {
    let (status, created) = route53(
        router,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <Name>{ZONE}</Name><CallerReference>{STACK}-{ZONE}</CallerReference>\
             <HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>\
             <VPC><VPCRegion>{REGION}</VPCRegion><VPCId>{vpc}</VPCId></VPC>\
             </CreateHostedZoneRequest>"
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateHostedZone must answer 201, got {status} with {created}"
    );
    let zone_id = opt_element(&created, "Id")
        .unwrap_or_else(|| panic!("setup: the create must answer the zone's Id, got {created}"))
        .trim_start_matches("/hostedzone/")
        .to_string();
    let changes: String = records
        .iter()
        .map(|(name, address)| {
            format!(
                "<Change><Action>UPSERT</Action><ResourceRecordSet><Name>{name}</Name>\
                 <Type>A</Type><TTL>{RECORD_TTL}</TTL><ResourceRecords><ResourceRecord>\
                 <Value>{address}</Value></ResourceRecord></ResourceRecords>\
                 </ResourceRecordSet></Change>"
            )
        })
        .collect();
    let (status, submitted) = route53(
        router,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
        &format!(
            "<ChangeResourceRecordSetsRequest \
             xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <ChangeBatch><Comment>the zone's records</Comment><Changes>{changes}</Changes>\
             </ChangeBatch></ChangeResourceRecordSetsRequest>"
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ChangeResourceRecordSets must be served, got {status} with {submitted}"
    );
}

/// Create the outbound Resolver endpoint on [`OUTBOUND_IPS`], wait for the status that makes it
/// serve, and answer its id.
async fn outbound_endpoint(
    router: &MultiServiceRouter,
    group: &str,
    subnets: &[String],
) -> String {
    let created = resolver_ok(
        router,
        "CreateResolverEndpoint",
        &json!({
            "CreatorRequestId": format!("{STACK}-outbound"),
            "Name": "outbound",
            "Direction": "OUTBOUND",
            "SecurityGroupIds": [group],
            "IpAddresses": [
                {"SubnetId": subnets[0], "Ip": OUTBOUND_IPS[0]},
                {"SubnetId": subnets[1], "Ip": OUTBOUND_IPS[1]},
            ],
        }),
    )
    .await;
    let endpoint = string_of(
        created.get("ResolverEndpoint").unwrap_or_else(|| {
            panic!("setup: the create must answer a ResolverEndpoint, got {created}")
        }),
        "Id",
    );
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = resolver_ok(
            router,
            "GetResolverEndpoint",
            &json!({"ResolverEndpointId": endpoint}),
        )
        .await;
        if read.pointer("/ResolverEndpoint/Status").and_then(Value::as_str) == Some(OPERATIONAL) {
            return endpoint;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: the outbound endpoint must reach {OPERATIONAL}, it still reports {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// One rule of the firewall rule group this suite plants: the entries of its own domain list and
/// the members of the rule itself.
struct DeclaredRule {
    /// What the rule is called, which is also what its domain list is called.
    name: &'static str,
    /// The entries of its domain list.
    domains: &'static [&'static str],
    /// `CreateFirewallRule$Priority`, unique inside the group.
    priority: u64,
    /// `CreateFirewallRule$BlockResponse`.
    block_response: &'static str,
    /// `CreateFirewallRule$Qtype`, where the rule evaluates one query type only.
    qtype: Option<&'static str>,
}

/// The rules of the one rule group: each covers a name a zone of the VPC also holds, so which
/// answer comes back names the layer that decided.
const DECLARED_RULES: &[DeclaredRule] = &[
    DeclaredRule {
        name: "xsvc-r53r-layers-blocked",
        domains: &[BLOCKED],
        priority: 100,
        block_response: "NXDOMAIN",
        qtype: None,
    },
    DeclaredRule {
        name: "xsvc-r53r-layers-swapped",
        domains: &[SWAPPED],
        priority: 200,
        block_response: "OVERRIDE",
        qtype: None,
    },
    DeclaredRule {
        name: "xsvc-r53r-layers-fenced",
        domains: &[FENCED],
        priority: 300,
        block_response: "NODATA",
        qtype: None,
    },
    DeclaredRule {
        name: "xsvc-r53r-layers-typed",
        domains: &[TYPED],
        priority: 400,
        block_response: "NXDOMAIN",
        qtype: Some("AAAA"),
    },
];

/// Plant the rule group, its domain lists and its rules through Route 53 Resolver's own
/// operations, and answer the id the service minted for the group.
async fn plant_rule_group(router: &MultiServiceRouter) -> String {
    let answered = resolver_ok(
        router,
        "CreateFirewallRuleGroup",
        &json!({ "CreatorRequestId": STACK, "Name": STACK }),
    )
    .await;
    let group_id = string_of(
        answered.get("FirewallRuleGroup").unwrap_or_else(|| {
            panic!("setup: the create must answer a FirewallRuleGroup, got {answered}")
        }),
        "Id",
    );
    for rule in DECLARED_RULES {
        let created = resolver_ok(
            router,
            "CreateFirewallDomainList",
            &json!({ "CreatorRequestId": rule.name, "Name": rule.name }),
        )
        .await;
        let list_id = string_of(
            created.get("FirewallDomainList").unwrap_or_else(|| {
                panic!("setup: the create must answer a FirewallDomainList, got {created}")
            }),
            "Id",
        );
        resolver_ok(
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
        resolver_ok(router, "CreateFirewallRule", &request).await;
    }
    group_id
}

/// Associate the rule group with the VPC and answer the id of the association.
async fn associate_rule_group(estate: &Estate, group: &str) -> String {
    let answered = resolver_ok(
        &estate.router,
        "AssociateFirewallRuleGroup",
        &json!({
            "CreatorRequestId": format!("{STACK}-association"),
            "FirewallRuleGroupId": group,
            "VpcId": estate.vpc,
            "Priority": 101,
            "Name": format!("{STACK}-association"),
        }),
    )
    .await;
    let association = answered
        .get("FirewallRuleGroupAssociation")
        .cloned()
        .unwrap_or_else(|| {
            panic!("setup: the associate must answer an association, got {answered}")
        });
    assert_eq!(
        association.get("VpcId").and_then(Value::as_str),
        Some(estate.vpc.as_str()),
        "setup: the association must name the VPC it was made for, got {association}"
    );
    string_of(&association, "Id")
}

/// Wait until every firewall record a decision is read off has settled: the rule group, each of
/// its rules, and the association with the VPC.
async fn wait_until_firewall_settled(estate: &Estate, group: &str, association: &str) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = resolver_ok(
            &estate.router,
            "GetFirewallRuleGroup",
            &json!({ "FirewallRuleGroupId": group }),
        )
        .await;
        let rules = resolver_ok(
            &estate.router,
            "ListFirewallRules",
            &json!({ "FirewallRuleGroupId": group }),
        )
        .await;
        let associated = resolver_ok(
            &estate.router,
            "GetFirewallRuleGroupAssociation",
            &json!({ "FirewallRuleGroupAssociationId": association }),
        )
        .await;
        let group_settled =
            read.pointer("/FirewallRuleGroup/Status").and_then(Value::as_str) == Some(COMPLETE);
        let rules_settled = rules
            .get("FirewallRules")
            .and_then(Value::as_array)
            .is_some_and(|listed| {
                listed.len() == DECLARED_RULES.len()
                    && listed.iter().all(|rule| {
                        rule.get("Status")
                            .and_then(Value::as_str)
                            .is_none_or(|status| status == COMPLETE)
                    })
            });
        let association_settled = associated
            .pointer("/FirewallRuleGroupAssociation/Status")
            .and_then(Value::as_str)
            == Some(COMPLETE);
        if group_settled && rules_settled && association_settled {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: the firewall must settle, it still reports {read}, {rules} and {associated}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Create a Resolver rule of `rule_type` on `domain`, routing to `targets` through the estate's
/// outbound endpoint where it names any, and answer its id.
async fn resolver_rule(
    estate: &Estate,
    name: &str,
    rule_type: &str,
    domain: &str,
    targets: &[&str],
) -> String {
    let mut request = json!({
        "CreatorRequestId": format!("{STACK}-{name}"),
        "Name": name,
        "RuleType": rule_type,
        "DomainName": domain,
    });
    if !targets.is_empty() {
        request["ResolverEndpointId"] = json!(estate.outbound_endpoint);
        request["TargetIps"] = json!(targets
            .iter()
            .map(|address| json!({"Ip": address, "Port": 53}))
            .collect::<Vec<Value>>());
    }
    let created = resolver_ok(&estate.router, "CreateResolverRule", &request).await;
    assert_eq!(
        created.pointer("/ResolverRule/DomainName").and_then(Value::as_str),
        Some(format!("{domain}.").as_str()),
        "setup: the rule must hold the domain it was created for, got {created}"
    );
    string_of(
        created
            .get("ResolverRule")
            .unwrap_or_else(|| panic!("setup: the create must answer a ResolverRule, {created}")),
        "Id",
    )
}

/// Associate `rule` with the estate's VPC and wait for the association to settle: "`COMPLETE` …
/// the Resolver rule was successfully associated with the VPC".
async fn associate_rule(estate: &Estate, rule: &str) {
    let associated = resolver_ok(
        &estate.router,
        "AssociateResolverRule",
        &json!({"ResolverRuleId": rule, "VPCId": estate.vpc, "Name": "layers"}),
    )
    .await;
    let association = string_of(
        associated.get("ResolverRuleAssociation").unwrap_or_else(|| {
            panic!("setup: the associate must answer an association, got {associated}")
        }),
        "Id",
    );
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = resolver_ok(
            &estate.router,
            "GetResolverRuleAssociation",
            &json!({"ResolverRuleAssociationId": association}),
        )
        .await;
        if read
            .pointer("/ResolverRuleAssociation/Status")
            .and_then(Value::as_str)
            == Some(COMPLETE)
        {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: the association of {rule} must reach {COMPLETE}, it still reports {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Deploy the querier: the execution role and the function ATTACHED to the VPC. The inline source
/// is archived into a deployment package by CloudFormation, which is how a function whose handler
/// is written here comes to exist.
async fn deploy_querier(router: &MultiServiceRouter, subnets: &[String], group: &str) {
    let template = json!({
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
                    // The interface calls attaching a function to a VPC takes, which is what the
                    // managed `AWSLambdaVPCAccessExecutionRole` policy grants.
                    "Policies": [{
                        "PolicyName": "vpc-access",
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
                        "SubnetIds": [subnets[0], subnets[1]],
                        "SecurityGroupIds": [group],
                    },
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
    })
    .to_string();
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
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, described) = cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE {
            break;
        }
        assert!(
            reported.ends_with(IN_PROGRESS) && Instant::now() < giving_up_at,
            "setup: the stack must reach {CREATE_COMPLETE}, it reports {reported:?} with \
             {described}"
        );
        tokio::time::sleep(LOOK).await;
    }
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda must report {FUNCTION:?} Active, it still reports {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Build the estate: the network, the private hosted zone and its records, the outbound endpoint
/// a forwarding rule routes through, and the querier inside the VPC.
async fn estate() -> Estate {
    let router = build_router();
    let (vpc, subnets, group) = network(&router).await;
    private_zone(
        &router,
        &vpc,
        &[
            (OPEN, OPEN_ADDRESS),
            (BLOCKED, BLOCKED_ADDRESS),
            (SWAPPED, SWAPPED_ADDRESS),
            (TYPED, TYPED_ADDRESS),
            (SENT, SENT_ADDRESS),
            (FENCED, FENCED_ADDRESS),
            (PROCESSED, PROCESSED_ADDRESS),
        ],
    )
    .await;
    let outbound_endpoint = outbound_endpoint(&router, &group, &subnets).await;
    deploy_querier(&router, &subnets, &group).await;
    Estate {
        router,
        vpc,
        outbound_endpoint,
    }
}

/// What one query must be answered with: the response code, the records of the answer section as
/// `(type, value)` pairs, and the TTL every record must carry where the layer that answered
/// STATES one (a record set's `TTL`, a rule's `BlockOverrideTtl`).
#[derive(Clone, Debug)]
struct Answer {
    rcode: u64,
    records: Vec<(u64, String)>,
    ttl: Option<u64>,
}

/// The answer a private hosted zone of the VPC holds for one of its names: the address, under the
/// TTL its record set carries.
fn from_the_zone(address: &str) -> Answer {
    Answer {
        rcode: RCODE_NOERROR,
        records: vec![(TYPE_A, address.to_string())],
        ttl: Some(RECORD_TTL),
    }
}

/// A name a zone of the VPC holds nothing at: the zone is the authority, so the name does not
/// exist.
fn absent() -> Answer {
    Answer {
        rcode: RCODE_NXDOMAIN,
        records: Vec::new(),
        ttl: None,
    }
}

/// A name that exists carrying no record of the type asked about.
fn no_record_of_that_type() -> Answer {
    Answer {
        rcode: RCODE_NOERROR,
        records: Vec::new(),
        ttl: None,
    }
}

/// A `BLOCK` answered `NXDOMAIN`: "the domain name that's in the query doesn't exist".
fn blocked_nxdomain() -> Answer {
    Answer {
        rcode: RCODE_NXDOMAIN,
        records: Vec::new(),
        ttl: None,
    }
}

/// A `BLOCK` answered `NODATA`: "the query was successful, but no response is available for it".
fn blocked_nodata() -> Answer {
    Answer {
        rcode: RCODE_NOERROR,
        records: Vec::new(),
        ttl: None,
    }
}

/// A `BLOCK` answered `OVERRIDE`: the rule's own record, carrying the TTL the rule recommends.
fn blocked_override() -> Answer {
    Answer {
        rcode: RCODE_NOERROR,
        records: vec![(TYPE_CNAME, format!("{OVERRIDE_TARGET}."))],
        ttl: Some(OVERRIDE_TTL),
    }
}

/// A query a `FORWARD` rule sent to resolvers that answered nothing: the client is left with no
/// answer for it.
fn forwarded_unanswered() -> Answer {
    Answer {
        rcode: RCODE_SERVFAIL,
        records: Vec::new(),
        ttl: None,
    }
}

/// A public name a DNS standard reserves for documentation, answered out of that standard. The
/// TTL is the resolver's own, so no row holds it to a figure.
fn from_the_standard() -> Answer {
    Answer {
        rcode: RCODE_NOERROR,
        records: vec![(TYPE_A, DOCUMENTATION_ADDRESS.to_string())],
        ttl: None,
    }
}

/// A name the public namespace establishes does not exist.
fn established_absent() -> Answer {
    Answer {
        rcode: RCODE_NXDOMAIN,
        records: Vec::new(),
        ttl: None,
    }
}

/// A name under a zone somebody owns, which nothing here can resolve.
fn unresolvable() -> Answer {
    Answer {
        rcode: RCODE_SERVFAIL,
        records: Vec::new(),
        ttl: None,
    }
}

/// The phases every row is asked in, in order.
const PHASES: [&str; 4] = [
    "before any rule group or Resolver rule is associated with the VPC",
    "with the rule group and both Resolver rules associated",
    "after the rule group's association is removed",
    "after the Resolver rule associations are removed too",
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

/// The rows: one per combination of layers that reaches a single query.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a name of the zone no rule of any layer covers",
            name: OPEN,
            qtype: TYPE_A,
            phases: vec![
                from_the_zone(OPEN_ADDRESS),
                from_the_zone(OPEN_ADDRESS),
                from_the_zone(OPEN_ADDRESS),
                from_the_zone(OPEN_ADDRESS),
            ],
        },
        Row {
            case: "a name the zone holds nothing at",
            name: ABSENT,
            qtype: TYPE_A,
            phases: vec![absent(), absent(), absent(), absent()],
        },
        Row {
            // The firewall ahead of the zone: the zone holds an address for this name and the
            // rule answers it as absent anyway.
            case: "a name a BLOCK rule answering NXDOMAIN covers, which the zone holds",
            name: BLOCKED,
            qtype: TYPE_A,
            phases: vec![
                from_the_zone(BLOCKED_ADDRESS),
                blocked_nxdomain(),
                from_the_zone(BLOCKED_ADDRESS),
                from_the_zone(BLOCKED_ADDRESS),
            ],
        },
        Row {
            case: "a name a BLOCK rule answering OVERRIDE covers, which the zone holds",
            name: SWAPPED,
            qtype: TYPE_A,
            phases: vec![
                from_the_zone(SWAPPED_ADDRESS),
                blocked_override(),
                from_the_zone(SWAPPED_ADDRESS),
                from_the_zone(SWAPPED_ADDRESS),
            ],
        },
        Row {
            case: "a name whose only rule evaluates another query type, asked as A",
            name: TYPED,
            qtype: TYPE_A,
            phases: vec![
                from_the_zone(TYPED_ADDRESS),
                from_the_zone(TYPED_ADDRESS),
                from_the_zone(TYPED_ADDRESS),
                from_the_zone(TYPED_ADDRESS),
            ],
        },
        Row {
            case: "that same name asked as the type its rule evaluates",
            name: TYPED,
            qtype: TYPE_AAAA,
            phases: vec![
                no_record_of_that_type(),
                blocked_nxdomain(),
                no_record_of_that_type(),
                no_record_of_that_type(),
            ],
        },
        Row {
            // The Resolver rules ahead of the zone.
            case: "the forwarding rule's own domain, which the zone holds",
            name: SENT,
            qtype: TYPE_A,
            phases: vec![
                from_the_zone(SENT_ADDRESS),
                forwarded_unanswered(),
                forwarded_unanswered(),
                from_the_zone(SENT_ADDRESS),
            ],
        },
        Row {
            // The firewall ahead of the Resolver rules: both reach this query, and the arm that
            // comes back is the firewall rule's.
            case: "a name under the forwarding rule's domain that a BLOCK rule also covers",
            name: FENCED,
            qtype: TYPE_A,
            phases: vec![
                from_the_zone(FENCED_ADDRESS),
                blocked_nodata(),
                forwarded_unanswered(),
                from_the_zone(FENCED_ADDRESS),
            ],
        },
        Row {
            case: "a name a more specific SYSTEM rule leaves to the VPC's own resolver",
            name: PROCESSED,
            qtype: TYPE_A,
            phases: vec![
                from_the_zone(PROCESSED_ADDRESS),
                from_the_zone(PROCESSED_ADDRESS),
                from_the_zone(PROCESSED_ADDRESS),
                from_the_zone(PROCESSED_ADDRESS),
            ],
        },
        Row {
            case: "a reserved documentation domain, under no zone of the VPC",
            name: DOCUMENTATION_DOMAIN,
            qtype: TYPE_A,
            phases: vec![
                from_the_standard(),
                from_the_standard(),
                from_the_standard(),
                from_the_standard(),
            ],
        },
        Row {
            case: "the host label published beside another reserved domain",
            name: DOCUMENTATION_HOST,
            qtype: TYPE_A,
            phases: vec![
                from_the_standard(),
                from_the_standard(),
                from_the_standard(),
                from_the_standard(),
            ],
        },
        Row {
            case: "a name under a top-level domain the root zone never delegates",
            name: NEVER_DELEGATED_NAME,
            qtype: TYPE_A,
            phases: vec![
                established_absent(),
                established_absent(),
                established_absent(),
                established_absent(),
            ],
        },
        Row {
            case: "a name under a domain somebody owns",
            name: OWNED_NAME,
            qtype: TYPE_A,
            phases: vec![
                unresolvable(),
                unresolvable(),
                unresolvable(),
                unresolvable(),
            ],
        },
    ]
}

/// Ask every row's query from inside the VPC, in one invocation addressed to `server`, and hold
/// each reply to what the phase expects.
async fn probe_phase(estate: &Estate, phase: usize, rows: &[Row], server: &str) {
    let case = PHASES[phase];
    let probes: Vec<Value> = rows
        .iter()
        .map(|row| json!({ "name": row.name, "type": row.qtype }))
        .collect();
    let event = json!({ "resolver": server, "probes": probes });
    let (status, headers, payload) = lambda(
        &estate.router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        Some(&event),
    )
    .await;
    assert_eq!(
        status, 200,
        "phase {case:?} at {server}: the invocation must be served, got {status} with {payload}"
    );
    assert_eq!(
        headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok()),
        None,
        "phase {case:?} at {server}: the in-VPC querier must run to completion, got {payload}"
    );
    let results = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("phase {case:?} at {server}: expected one result per probe, got {payload}")
        });
    assert_eq!(
        results.len(),
        rows.len(),
        "phase {case:?} at {server}: expected one result per probe, got {results:?}"
    );
    for (row, result) in rows.iter().zip(&results) {
        check_row(case, server, row, &row.phases[phase], result);
    }
}

/// Hold one reply to what the phase expects of it.
fn check_row(case: &str, server: &str, row: &Row, expected: &Answer, result: &Value) {
    let row_case = row.case;
    let name = row.name;
    let qtype = row.qtype;
    assert_eq!(
        result.get("rcode").and_then(Value::as_u64),
        Some(expected.rcode),
        "phase {case:?} at {server}, case {row_case:?}: expected response code {} for {name} \
         ({qtype}), got {result}",
        expected.rcode
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
        "phase {case:?} at {server}, case {row_case:?}: expected the records {:?} for {name} \
         ({qtype}), got {result}",
        expected.records
    );
    if let Some(ttl) = expected.ttl {
        for record in &held {
            assert_eq!(
                record.get("ttl").and_then(Value::as_u64),
                Some(ttl),
                "phase {case:?} at {server}, case {row_case:?}: expected every record to carry \
                 the TTL {ttl} the layer that answered states, got {result}"
            );
        }
    }
    assert_eq!(
        result.get("ancount").and_then(Value::as_u64),
        Some(expected.records.len() as u64),
        "phase {case:?} at {server}, case {row_case:?}: expected the answer count to agree with \
         the records, got {result}"
    );
    // The reply came back from the address the query was sent to, as a datagram's answer does.
    assert_eq!(
        result
            .get("peer")
            .and_then(Value::as_array)
            .and_then(|peer| peer.first())
            .and_then(Value::as_str),
        Some(server),
        "phase {case:?} at {server}, case {row_case:?}: expected the reply to come from \
         {server}, got {result}"
    );
}

/// RULE: a query asked inside a VPC is answered by composing the account's records for that VPC
/// in one order — the DNS Firewall rule groups associated with it first, then the Resolver rules
/// associated with it, then the private hosted zones associated with it, and only a name none of
/// them covers out of what the public namespace fixes — and a layer whose association is removed
/// hands the query to the next one down.
#[tokio::test]
async fn the_firewall_the_resolver_rules_and_the_zones_decide_in_that_order() {
    let estate = estate().await;
    let rows = rows();

    // Nothing but the zone reaches the VPC yet: every name it holds is answered out of it, and
    // the names under no zone out of the public namespace.
    probe_phase(&estate, 0, &rows, LINK_LOCAL_RESOLVER).await;

    // All three layers in force: the rule group associated with the VPC, a FORWARD rule sending
    // one domain to an address nothing answers DNS at, and a SYSTEM rule under it.
    let group = plant_rule_group(&estate.router).await;
    let association = associate_rule_group(&estate, &group).await;
    wait_until_firewall_settled(&estate, &group, &association).await;
    let diverting = resolver_rule(&estate, "diverting", "FORWARD", SENT, &[DEAD_TARGET]).await;
    let processed = resolver_rule(&estate, "processed", "SYSTEM", PROCESSED, &[]).await;
    associate_rule(&estate, &diverting).await;
    associate_rule(&estate, &processed).await;
    probe_phase(&estate, 1, &rows, LINK_LOCAL_RESOLVER).await;
    // The composition is the VPC's, not one address's: the other address its resolver answers at
    // answers every row the same way.
    probe_phase(&estate, 1, &rows, RANGE_RESOLVER).await;

    // Taking the firewall association away leaves its rules deciding nothing, which hands each of
    // their names to the layer below: the forwarding rule where one covers it, the zone otherwise.
    resolver_ok(
        &estate.router,
        "DisassociateFirewallRuleGroup",
        &json!({ "FirewallRuleGroupAssociationId": association }),
    )
    .await;
    probe_phase(&estate, 2, &rows, LINK_LOCAL_RESOLVER).await;

    // With the Resolver rule associations gone too, every name is answered out of the zone again.
    for rule in [&diverting, &processed] {
        resolver_ok(
            &estate.router,
            "DisassociateResolverRule",
            &json!({"ResolverRuleId": rule, "VPCId": estate.vpc}),
        )
        .await;
    }
    probe_phase(&estate, 3, &rows, LINK_LOCAL_RESOLVER).await;
}
