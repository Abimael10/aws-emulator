// SPDX-License-Identifier: CC0-1.0

//! What an associated FORWARD Resolver rule does to the answers a VPC's own Resolver serves —
//! end to end across Amazon EC2, Route 53, Route 53 Resolver, IAM, CloudFormation and AWS Lambda
//! on the assembled emulator.
//!
//! RULE under test: a query asked inside a VPC is decided by the Resolver rules whose association
//! with that VPC has settled, BEFORE any zone of the VPC is read for it — the rule whose
//! `DomainName` is the queried name's longest whole-label suffix decides it ("if a query matches
//! multiple Resolver rules … outbound DNS queries are routed using the Resolver rule that
//! contains the most specific domain name"), a `FORWARD` rule sends the query to the resolvers
//! its `TargetIps` name instead of answering it locally, and every other type leaves the query to
//! the VPC's own resolver, which is what `SYSTEM` is for. A name no rule's domain covers resolves
//! exactly as before, and a rule whose association the account removed decides nothing any more.
//!
//! Only an assembled emulator can pin it: the two VPCs, their subnets and the security groups are
//! Amazon EC2's records, the private hosted zones and their record sets are Route 53's, the
//! endpoints, the rules and their associations are Route 53 Resolver's, and the client that reads
//! the answers is a program AWS Lambda runs inside the first VPC — which sends each query ITSELF,
//! framing and reading real DNS messages on a datagram socket.
//!
//! The rows differ along what the rule generalises over: the rule's domain asked for exactly and
//! asked for one and several labels under it, a sibling name of the same zone no rule covers, a
//! name under a more specific rule of the other type, a name under a rule whose target is an
//! address NOTHING answers DNS at, a name under a rule whose target IS a resolver of the account
//! (an inbound endpoint of the second VPC, which answers out of ITS own zone), and a name that
//! target holds nothing at.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The VPC the querying client sits in, with the two subnets a Resolver endpoint needs: "you must
/// specify at least two IP addresses, in two different Availability Zones"
/// (`CreateResolverEndpointRequest$IpAddresses`).
const VPC_CIDR: &str = "10.95.0.0/16";
/// See [`VPC_CIDR`].
const SUBNET_CIDRS: [&str; 2] = ["10.95.1.0/24", "10.95.2.0/24"];
/// The VPC's own resolver, at the base of its range plus two.
const VPC_RESOLVER: &str = "10.95.0.2";
/// The addresses the OUTBOUND endpoint of that VPC is created on, which are the addresses a
/// forwarded query leaves through.
const OUTBOUND_IPS: [&str; 2] = ["10.95.1.10", "10.95.2.10"];
/// An address of that VPC NOTHING answers DNS at, which is where the first forwarding rule sends
/// its queries.
const DEAD_TARGET: &str = "10.95.9.250";

/// The second VPC, which stands in for the network a forwarding rule points at: its INBOUND
/// endpoint is a resolver of the account, so a rule whose target is one of its addresses reaches
/// something that answers.
const OTHER_VPC_CIDR: &str = "10.96.0.0/16";
/// See [`OTHER_VPC_CIDR`].
const OTHER_SUBNET_CIDRS: [&str; 2] = ["10.96.1.0/24", "10.96.2.0/24"];
/// The addresses the inbound endpoint of that VPC answers at.
const INBOUND_IPS: [&str; 2] = ["10.96.1.10", "10.96.2.10"];

/// The private hosted zone associated with the querying VPC, and the names it holds.
const ZONE: &str = "svc.internal";
/// The name the first forwarding rule's domain is, which the zone also holds a record at.
const SUBJECT: &str = "subject.svc.internal";
/// See [`SUBJECT`].
const SUBJECT_ADDRESS: &str = "192.0.2.10";
/// A name one label under [`SUBJECT`], which the same rule covers.
const UNDER_SUBJECT: &str = "host.subject.svc.internal";
/// See [`UNDER_SUBJECT`].
const UNDER_SUBJECT_ADDRESS: &str = "192.0.2.11";
/// A name under [`SUBJECT`] that a more specific rule of the other type covers.
const PROCESSED: &str = "inner.subject.svc.internal";
/// See [`PROCESSED`].
const PROCESSED_ADDRESS: &str = "192.0.2.12";
/// A name of the same zone no rule covers at all.
const CONTROL: &str = "control.svc.internal";
/// See [`CONTROL`].
const CONTROL_ADDRESS: &str = "192.0.2.20";

/// The private hosted zone associated with the SECOND VPC, which only its own resolver answers
/// out of, and the name it holds.
const OTHER_ZONE: &str = "corp.example";
/// See [`OTHER_ZONE`].
const OTHER_NAME: &str = "desk.corp.example";
/// See [`OTHER_ZONE`].
const OTHER_ADDRESS: &str = "192.0.2.30";
/// A name of that zone it holds no record at.
const OTHER_ABSENT: &str = "absent.corp.example";

/// The record type every probe asks for, as RFC 1035 numbers it, and the response codes the
/// resolver answers with.
const TYPE_A: u64 = 1;
/// See [`TYPE_A`].
const RCODE_NOERROR: u64 = 0;
/// See [`TYPE_A`].
const RCODE_SERVFAIL: u64 = 2;
/// See [`TYPE_A`].
const RCODE_NXDOMAIN: u64 = 3;

/// The stack, role and function this suite deploys, and what the function declares.
const STACK: &str = "r53r-forward-rule-probe";
/// See [`STACK`].
const ROLE: &str = "ProbeRole";
/// See [`STACK`].
const FUNCTION: &str = "Probe";
/// See [`STACK`].
const RUNTIME: &str = "python3.12";
/// See [`STACK`].
const HANDLER: &str = "index.handler";

/// How long one invocation of the probe is given, and how much memory it is configured with.
/// Both generous: what is under test is what the resolver answers, never how fast an interpreter
/// gets through one run.
const FUNCTION_TIMEOUT: u64 = 120;
/// See [`FUNCTION_TIMEOUT`].
const FUNCTION_MEMORY_MB: u64 = 1_024;

/// The status a stack that was created reports, and the suffix of every status that is an
/// operation still in flight.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// How long this suite looks for work a service's own clock owes — a stack settling, an endpoint
/// becoming operational, an association completing — and how often it looks again. Nothing here
/// sleeps for a fixed span: every wait is for the CONDITION, and the deadline is generous, far
/// past what any step takes, so a loaded host cannot decide an outcome.
const LOOK: Duration = Duration::from_millis(100);
/// See [`LOOK`].
const DEADLINE: Duration = Duration::from_secs(180);

/// The handler the function runs: for each probe the event names it sends ONE DNS query on a
/// datagram socket to the address the probe gives, and reports what came back — the response
/// code, the records of the answer section, or the error the socket raised when nothing came back
/// at all.
///
/// The messages are framed and read here, by the client, so the exchange asserted on is a real
/// one; nothing about it is this emulator's own shape.
const PROBE_SOURCE: &str = r#"
import socket


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


def _answers(reply):
    pos = 12
    for _ in range(int.from_bytes(reply[4:6], "big")):
        pos = _skip_name(reply, pos) + 4
    answered = []
    for _ in range(int.from_bytes(reply[6:8], "big")):
        pos = _skip_name(reply, pos)
        kind = int.from_bytes(reply[pos : pos + 2], "big")
        length = int.from_bytes(reply[pos + 8 : pos + 10], "big")
        start = pos + 10
        pos = start + length
        data = reply[start:pos]
        if kind == 1 and length == 4:
            answered.append(".".join(str(octet) for octet in data))
        elif kind == 5:
            answered.append(_read_name(reply, start))
    return answered


def _resolved(server, name, kind, qid):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(30)
    try:
        sock.sendto(_query(name, kind, qid), (server, 53))
        reply, _peer = sock.recvfrom(512)
    except OSError as failed:
        return {"rcode": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()
    return {"rcode": reply[3] & 0x0F, "answers": _answers(reply)}


def handler(event, context):
    results = []
    for index, probe in enumerate(event["probes"]):
        results.append(
            _resolved(probe["server"], probe["name"], int(probe["type"]), 0x600 + index)
        )
    return {"results": results}
"#;

/// The assembled emulator, on ONE engine, exactly as the router mounts it: the services that own
/// the VPCs, the zones, the rules and the function all read and write the same records.
fn build_router() -> MultiServiceRouter {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One form-encoded call over an `awsQuery`/`ec2Query` wire, as its status and its XML body.
async fn form_call(
    router: &MultiServiceRouter,
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
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization(signing_name))
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).into_owned())
}

/// One Amazon EC2 call that must be served, as its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, text) = form_call(router, "ec2", "2016-11-15", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Amazon EC2 {action} must be served, got {status} with {text}"
    );
    text
}

/// One CloudFormation call, as its status and its XML body.
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
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).into_owned())
}

/// One Route 53 Resolver call over its `awsJson1_1` wire, as its status and its parsed body.
async fn resolver(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("route53resolver.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("route53resolver"))
        .header(http::header::CONTENT_TYPE, "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Route53Resolver.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Route 53 Resolver call that must be served, as its parsed body.
async fn resolver_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, answered) = resolver(router, operation, body).await;
    assert_eq!(
        status, 200,
        "Route 53 Resolver {operation} must be served, got {status} with {answered}"
    );
    answered
}

/// One AWS Lambda `restJson1` call, as its status and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
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
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(response.body()).into_owned())
        })
    };
    (status, parsed)
}

/// The estate every phase of this suite queries from.
struct Estate {
    router: MultiServiceRouter,
    /// The VPC the client sits in, which the rules are associated with.
    vpc: String,
    /// The OUTBOUND endpoint of that VPC, which every forwarding rule sends through.
    outbound_endpoint: String,
    /// The security group the SECOND VPC's inbound endpoint sits behind, whose DNS rule one
    /// phase revokes.
    inbound_group: String,
    /// The name Lambda holds the deployed probe function under.
    function: String,
}

/// Create a security group in `vpc` and answer its id.
async fn security_group(
    router: &MultiServiceRouter,
    case: &str,
    vpc: &str,
    name: &str,
) -> String {
    let answered = ec2(
        router,
        case,
        "CreateSecurityGroup",
        &[
            ("GroupName", name),
            ("GroupDescription", "DNS inside the VPC"),
            ("VpcId", vpc),
        ],
    )
    .await;
    opt_element(&answered, "groupId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup must mint a group"))
}

/// Authorize DNS on `protocol` into `group` from `source`, or revoke that same rule.
async fn dns_rule(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    group: &str,
    source: &str,
) {
    for protocol in ["udp", "tcp"] {
        ec2(
            router,
            case,
            action,
            &[
                ("GroupId", group),
                ("IpPermissions.1.IpProtocol", protocol),
                ("IpPermissions.1.FromPort", "53"),
                ("IpPermissions.1.ToPort", "53"),
                ("IpPermissions.1.IpRanges.1.CidrIp", source),
            ],
        )
        .await;
    }
}

/// Create a VPC of `cidr` with a subnet per entry of `subnet_cidrs`, and answer the VPC's id
/// beside its subnets'.
async fn network(
    router: &MultiServiceRouter,
    case: &str,
    cidr: &str,
    subnet_cidrs: &[&str; 2],
) -> (String, Vec<String>) {
    let created = ec2(router, case, "CreateVpc", &[("CidrBlock", cidr)]).await;
    let vpc = opt_element(&created, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must mint a VPC: {created}"));
    let mut subnets = Vec::new();
    for (index, subnet_cidr) in subnet_cidrs.iter().enumerate() {
        let zone = format!("{REGION}{}", ["a", "b"][index]);
        let answered = ec2(
            router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", subnet_cidr),
                ("AvailabilityZone", zone.as_str()),
            ],
        )
        .await;
        subnets.push(
            opt_element(&answered, "subnetId")
                .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet must mint a subnet")),
        );
    }
    (vpc, subnets)
}

/// Create a private hosted zone associated with `vpc` and write one `A` record set per entry of
/// `records`.
async fn private_zone(
    router: &MultiServiceRouter,
    case: &str,
    vpc: &str,
    zone: &str,
    records: &[(&str, &str)],
) {
    let created = route53(
        router,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <Name>{zone}</Name><CallerReference>{STACK}-{zone}</CallerReference>\
             <HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>\
             <VPC><VPCRegion>{REGION}</VPCRegion><VPCId>{vpc}</VPCId></VPC>\
             </CreateHostedZoneRequest>"
        ),
    )
    .await;
    assert_eq!(
        created.0, 201,
        "case {case:?}: CreateHostedZone must answer 201, got {} with {}",
        created.0, created.1
    );
    let zone_id = opt_element(&created.1, "Id")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer the zone's Id"))
        .trim_start_matches("/hostedzone/")
        .to_string();
    let changes: String = records
        .iter()
        .map(|(name, address)| {
            format!(
                "<Change><Action>UPSERT</Action><ResourceRecordSet><Name>{name}</Name>\
                 <Type>A</Type><TTL>300</TTL><ResourceRecords><ResourceRecord>\
                 <Value>{address}</Value></ResourceRecord></ResourceRecords>\
                 </ResourceRecordSet></Change>"
            )
        })
        .collect();
    let submitted = route53(
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
        submitted.0, 200,
        "case {case:?}: ChangeResourceRecordSets must be served, got {} with {}",
        submitted.0, submitted.1
    );
}

/// Create a Resolver endpoint of `direction` on `addresses`, wait for the status that makes it
/// serve ("`OPERATIONAL` … the Resolver endpoint is ready to be used") and answer its id.
async fn resolver_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    direction: &str,
    group: &str,
    subnets: &[String],
    addresses: &[&str; 2],
) -> String {
    let created = resolver_ok(
        router,
        "CreateResolverEndpoint",
        &json!({
            "CreatorRequestId": format!("{STACK}-{direction}"),
            "Name": direction,
            "Direction": direction,
            "SecurityGroupIds": [group],
            "IpAddresses": [
                {"SubnetId": subnets[0], "Ip": addresses[0]},
                {"SubnetId": subnets[1], "Ip": addresses[1]},
            ],
        }),
    )
    .await;
    let endpoint = created["ResolverEndpoint"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint an endpoint: {created}"))
        .to_string();
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let read = resolver_ok(
            router,
            "GetResolverEndpoint",
            &json!({"ResolverEndpointId": endpoint}),
        )
        .await;
        if read["ResolverEndpoint"]["Status"].as_str() == Some("OPERATIONAL") {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the {direction} endpoint must reach OPERATIONAL, last read {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
    endpoint
}

/// Build the estate: both VPCs with their subnets and groups, both private hosted zones, the
/// outbound endpoint of the querying VPC, the inbound endpoint of the other one, and the function
/// that queries the resolver from inside the querying VPC.
async fn estate(case: &str) -> Estate {
    let router = build_router();

    let (vpc, subnets) = network(&router, case, VPC_CIDR, &SUBNET_CIDRS).await;
    let (other_vpc, other_subnets) =
        network(&router, case, OTHER_VPC_CIDR, &OTHER_SUBNET_CIDRS).await;
    let client_group = security_group(&router, case, &vpc, "resolver-client").await;
    let outbound_group = security_group(&router, case, &vpc, "resolver-outbound").await;
    let inbound_group = security_group(&router, case, &other_vpc, "resolver-inbound").await;
    // The inbound endpoint's own groups "must include one or more inbound rules … For inbound
    // access, open port 53" (`CreateResolverEndpointRequest$SecurityGroupIds`), and what reaches
    // it here is a query forwarded out of the first VPC — so the rule admits that VPC's range.
    dns_rule(
        &router,
        case,
        "AuthorizeSecurityGroupIngress",
        &inbound_group,
        VPC_CIDR,
    )
    .await;

    private_zone(
        &router,
        case,
        &vpc,
        ZONE,
        &[
            (SUBJECT, SUBJECT_ADDRESS),
            (UNDER_SUBJECT, UNDER_SUBJECT_ADDRESS),
            (PROCESSED, PROCESSED_ADDRESS),
            (CONTROL, CONTROL_ADDRESS),
        ],
    )
    .await;
    private_zone(
        &router,
        case,
        &other_vpc,
        OTHER_ZONE,
        &[(OTHER_NAME, OTHER_ADDRESS)],
    )
    .await;

    let outbound_endpoint = resolver_endpoint(
        &router,
        case,
        "OUTBOUND",
        &outbound_group,
        &subnets,
        &OUTBOUND_IPS,
    )
    .await;
    resolver_endpoint(
        &router,
        case,
        "INBOUND",
        &inbound_group,
        &other_subnets,
        &INBOUND_IPS,
    )
    .await;

    // The client: a function attached to the querying VPC, deployed by CloudFormation because
    // that is what archives the handler written above into a deployment package.
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{STACK}-role"),
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
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{STACK}-probe"),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "MemorySize": FUNCTION_MEMORY_MB,
                    "Code": {"ZipFile": PROBE_SOURCE},
                    "VpcConfig": {
                        "SubnetIds": [subnets[0], subnets[1]],
                        "SecurityGroupIds": [client_group],
                    },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = cloudformation(
        &router,
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
        "case {case:?}: CreateStack must be accepted, got {status} with {answered}"
    );
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (_, described) =
            cloudformation(&router, "DescribeStacks", &[("StackName", STACK)]).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE {
            break;
        }
        assert!(
            reported.ends_with(IN_PROGRESS) && Instant::now() < giving_up_at,
            "case {case:?}: the stack must reach {CREATE_COMPLETE}, last reported {reported:?} \
             with {described}"
        );
        tokio::time::sleep(LOOK).await;
    }

    let function = format!("{STACK}-probe");
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (_, configuration) = lambda(
            &router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            assert_eq!(
                configuration["VpcConfig"]["VpcId"].as_str(),
                Some(vpc.as_str()),
                "case {case:?}: the function must be attached to the VPC, got {configuration}"
            );
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: Lambda must report the probe Active, last read {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }

    Estate {
        router,
        vpc,
        outbound_endpoint,
        inbound_group,
        function,
    }
}

/// Create a Resolver rule of `rule_type` on `domain`, forwarding to `targets` through the
/// estate's outbound endpoint where it names any, and answer its id.
async fn resolver_rule(
    estate: &Estate,
    case: &str,
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
        created["ResolverRule"]["DomainName"].as_str(),
        Some(format!("{domain}.").as_str()),
        "case {case:?}: the rule must hold the domain it was created for, got {created}"
    );
    created["ResolverRule"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint a rule: {created}"))
        .to_string()
}

/// Associate `rule` with the estate's VPC and wait for the association to SETTLE: "`COMPLETE`
/// … the Resolver rule was successfully associated with the VPC".
async fn associate(estate: &Estate, case: &str, rule: &str) {
    let associated = resolver_ok(
        &estate.router,
        "AssociateResolverRule",
        &json!({"ResolverRuleId": rule, "VPCId": estate.vpc, "Name": "probe"}),
    )
    .await;
    let association = associated["ResolverRuleAssociation"]["Id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the associate must mint an association: {associated}")
        })
        .to_string();
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let read = resolver_ok(
            &estate.router,
            "GetResolverRuleAssociation",
            &json!({"ResolverRuleAssociationId": association}),
        )
        .await;
        if read["ResolverRuleAssociation"]["Status"].as_str() == Some("COMPLETE") {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the association of {rule} must reach COMPLETE, last read {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// What the probe reported for one query: `None` for a query nothing answered.
#[derive(Debug)]
struct Reported {
    rcode: Option<u64>,
    answers: Vec<String>,
}

/// Send one query per entry of `names` to the VPC's own resolver from inside the VPC, and answer
/// what the function reported for each.
async fn probed(estate: &Estate, case: &str, names: &[&str]) -> Vec<Reported> {
    let payload = json!({
        "probes": names
            .iter()
            .map(|name| json!({"server": VPC_RESOLVER, "name": name, "type": TYPE_A}))
            .collect::<Vec<Value>>(),
    });
    let (status, answered) = lambda(
        &estate.router,
        "POST",
        &format!("/2015-03-31/functions/{}/invocations", estate.function),
        Some(&payload),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {answered}"
    );
    let results = answered["results"].as_array().unwrap_or_else(|| {
        panic!("case {case:?}: the handler must report one result per probe: {answered}")
    });
    assert_eq!(
        results.len(),
        names.len(),
        "case {case:?}: expected {} results, got {answered}",
        names.len()
    );
    results
        .iter()
        .map(|result| Reported {
            rcode: result["rcode"].as_u64(),
            answers: result["answers"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// One expectation of a phase: the name asked for, the response code and the answer section.
struct Expected {
    case: &'static str,
    name: &'static str,
    code: u64,
    answers: Vec<String>,
}

/// An expectation that `name` is answered with `code` and `answers`.
fn expect(case: &'static str, name: &'static str, code: u64, answers: &[&str]) -> Expected {
    Expected {
        case,
        name,
        code,
        answers: answers.iter().map(|value| (*value).to_string()).collect(),
    }
}

/// Ask every expectation of one phase in one invocation and check each answer.
async fn check(estate: &Estate, phase: &str, rows: &[Expected]) {
    let names: Vec<&str> = rows.iter().map(|row| row.name).collect();
    let reported = probed(estate, phase, &names).await;
    for (row, answered) in rows.iter().zip(&reported) {
        assert_eq!(
            answered.rcode,
            Some(row.code),
            "case {:?} ({phase}): expected response code {} for {}, got {answered:?}",
            row.case,
            row.code,
            row.name
        );
        assert_eq!(
            &answered.answers, &row.answers,
            "case {:?} ({phase}): expected the answer section {:?} for {}, got {answered:?}",
            row.case, row.answers, row.name
        );
    }
}

/// RULE: the Resolver rules in force in a VPC decide a query asked inside it before any zone of
/// the VPC is read — the most specific rule enclosing the name decides it, a `FORWARD` rule sends
/// the query to the resolvers its `TargetIps` name instead of answering it locally, a `SYSTEM`
/// rule under one leaves the query to the VPC's own resolver, a name no rule covers is unaffected,
/// and a rule whose association is removed stops deciding anything.
#[tokio::test]
async fn an_associated_forward_rule_diverts_only_the_names_its_domain_covers() {
    let case = "the in-VPC socket client";
    let estate = estate(case).await;

    // Before any rule is associated, every name is answered out of the zone of the VPC, and the
    // name only the OTHER VPC's zone holds is one the public namespace establishes does not
    // exist.
    check(
        &estate,
        "before any rule is associated",
        &[
            expect("the rule's own domain", SUBJECT, RCODE_NOERROR, &[SUBJECT_ADDRESS]),
            expect(
                "a name under it",
                UNDER_SUBJECT,
                RCODE_NOERROR,
                &[UNDER_SUBJECT_ADDRESS],
            ),
            expect(
                "a name a more specific rule will cover",
                PROCESSED,
                RCODE_NOERROR,
                &[PROCESSED_ADDRESS],
            ),
            expect("a name no rule covers", CONTROL, RCODE_NOERROR, &[CONTROL_ADDRESS]),
            expect(
                "a name only the other VPC's zone holds",
                OTHER_NAME,
                RCODE_NXDOMAIN,
                &[],
            ),
        ],
    )
    .await;

    // The three rules: one forwarding the subject domain to an address NOTHING answers DNS at,
    // one leaving a subdomain of it to the VPC's own Resolver, and one forwarding another domain
    // to the inbound endpoint of the second VPC, which does answer.
    let diverting = resolver_rule(
        &estate,
        case,
        "diverting",
        "FORWARD",
        SUBJECT,
        &[DEAD_TARGET],
    )
    .await;
    let processed = resolver_rule(&estate, case, "processed", "SYSTEM", PROCESSED, &[]).await;
    let reaching = resolver_rule(
        &estate,
        case,
        "reaching",
        "FORWARD",
        OTHER_ZONE,
        &[INBOUND_IPS[0]],
    )
    .await;
    for rule in [&diverting, &processed, &reaching] {
        associate(&estate, case, rule).await;
    }

    check(
        &estate,
        "once the rules are associated",
        &[
            expect(
                "the forwarding rule's own domain, sent to an address nothing answers at",
                SUBJECT,
                RCODE_SERVFAIL,
                &[],
            ),
            expect(
                "a name one label under that domain",
                UNDER_SUBJECT,
                RCODE_SERVFAIL,
                &[],
            ),
            expect(
                "a name the more specific rule leaves to the VPC's own resolver",
                PROCESSED,
                RCODE_NOERROR,
                &[PROCESSED_ADDRESS],
            ),
            expect(
                "a name under that more specific rule the zone holds nothing at",
                "absent.inner.subject.svc.internal",
                RCODE_NXDOMAIN,
                &[],
            ),
            expect(
                "a name of the same zone no rule covers",
                CONTROL,
                RCODE_NOERROR,
                &[CONTROL_ADDRESS],
            ),
            expect(
                "a name forwarded to a resolver that answers",
                OTHER_NAME,
                RCODE_NOERROR,
                &[OTHER_ADDRESS],
            ),
            expect(
                "a name that resolver's own zone holds nothing at",
                OTHER_ABSENT,
                RCODE_NXDOMAIN,
                &[],
            ),
            expect(
                "a name whose labels only look like the forwarded domain's",
                "host.other-corp.example",
                RCODE_NXDOMAIN,
                &[],
            ),
        ],
    )
    .await;

    // A forwarded query arrives at the inbound endpoint's INTERFACE, so revoking the DNS rule of
    // the group that interface sits behind drops it: the query is still sent away, and now
    // nothing answers it.
    dns_rule(
        &estate.router,
        case,
        "RevokeSecurityGroupIngress",
        &estate.inbound_group,
        VPC_CIDR,
    )
    .await;
    check(
        &estate,
        "with the target endpoint's DNS rule revoked",
        &[
            expect(
                "the forwarded name once the target admits no query",
                OTHER_NAME,
                RCODE_SERVFAIL,
                &[],
            ),
            expect(
                "a name no rule covers, which the revoke does not touch",
                CONTROL,
                RCODE_NOERROR,
                &[CONTROL_ADDRESS],
            ),
        ],
    )
    .await;
    dns_rule(
        &estate.router,
        case,
        "AuthorizeSecurityGroupIngress",
        &estate.inbound_group,
        VPC_CIDR,
    )
    .await;

    // Removing the association stops that rule applying, and leaves the others in force.
    resolver_ok(
        &estate.router,
        "DisassociateResolverRule",
        &json!({"ResolverRuleId": diverting, "VPCId": estate.vpc}),
    )
    .await;
    check(
        &estate,
        "once the forwarding rule is disassociated",
        &[
            expect(
                "the domain the removed rule covered",
                SUBJECT,
                RCODE_NOERROR,
                &[SUBJECT_ADDRESS],
            ),
            expect(
                "a name under it",
                UNDER_SUBJECT,
                RCODE_NOERROR,
                &[UNDER_SUBJECT_ADDRESS],
            ),
            expect(
                "the other forwarding rule, still associated",
                OTHER_NAME,
                RCODE_NOERROR,
                &[OTHER_ADDRESS],
            ),
        ],
    )
    .await;
}
