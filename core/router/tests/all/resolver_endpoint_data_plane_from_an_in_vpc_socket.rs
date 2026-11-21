// SPDX-License-Identifier: CC0-1.0

//! The Route 53 Resolver DATA PLANE of a VPC, as a program running inside that VPC observes it
//! over a SOCKET — end to end across Amazon EC2, Route 53, Route 53 Resolver, IAM,
//! CloudFormation and AWS Lambda on the assembled emulator.
//!
//! RULE under test: a DNS query sent from inside a VPC is answered out of the PRIVATE HOSTED
//! ZONES associated with that VPC — at the VPC's own resolver address and at the addresses of an
//! inbound Resolver endpoint in it — and a query addressed to an endpoint's address is delivered
//! only while a security group on that endpoint admits the query's protocol and port from the
//! querying address; a query no rule admits is DROPPED, so nothing at all comes back to the
//! client rather than a refusal. Revoking that rule therefore denies the query and authorizing
//! it again restores the answer, and the VPC's own resolver — which sits behind no interface of
//! the account — keeps answering throughout.
//!
//! Only an assembled emulator can pin it: the addresses that answer are Route 53 Resolver's
//! records, what admits a query to one of them is Amazon EC2's security-group rules, what the
//! answer holds is Route 53's record sets, and the client that observes all three is a program
//! AWS Lambda runs inside the VPC — which sends the query ITSELF, framing and reading real DNS
//! messages on a datagram socket, so what is asserted here is an exchange on the wire and not a
//! shape this emulator agreed with itself.
//!
//! The rows differ along what the rule generalises over: WHICH address the query is sent to (each
//! of the endpoint's two addresses, the VPC's own range plus two, the link-local resolver, and an
//! address of the VPC nothing answers DNS at), WHICH name and record type is asked for (a name
//! the zone holds, an alias chased to its address, an `AAAA`, a `TXT`, a name the zone holds
//! nothing at, a name under no zone of the VPC, and a name of the VPC's own internal zone), and
//! WHAT the endpoint's security group admits as its DNS rule is revoked and authorized again.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every request below is signed for, whose private DNS domain is therefore
/// `ec2.internal` ("AWS uses `ec2.internal` in `us-east-1`", Amazon VPC User Guide, "DHCP option
/// sets").
const REGION: &str = "us-east-1";

/// The VPC the whole suite runs in, and the two subnets an inbound Resolver endpoint needs: "you
/// must specify at least two IP addresses, in two different Availability Zones"
/// (`CreateResolverEndpointRequest$IpAddresses`).
const VPC_CIDR: &str = "10.93.0.0/16";
const SUBNET_CIDRS: [&str; 2] = ["10.93.1.0/24", "10.93.2.0/24"];

/// The addresses the endpoint is created on, one per subnet.
const ENDPOINT_IPS: [&str; 2] = ["10.93.1.10", "10.93.2.10"];

/// The VPC's own resolver, at the base of its range plus two and at the link-local address every
/// VPC's resolver also answers at.
const VPC_RANGE_RESOLVER: &str = "10.93.0.2";
const LINK_LOCAL_RESOLVER: &str = "169.254.169.253";

/// An address of the VPC nothing answers DNS at.
const SILENT_ADDRESS: &str = "10.93.9.9";

/// The private hosted zone the records live in.
const ZONE: &str = "svc.internal";

/// The addresses and text the zone's record sets hold, from the ranges RFC 5737 and RFC 3849
/// reserve for documentation and a text value of the form a zone holds one in.
const RECORD_ADDRESS: &str = "192.0.2.10";
const RECORD_ADDRESS_V6: &str = "2001:db8::10";
/// The same address as the PROBE writes it: it reads the sixteen bytes of the record as eight
/// groups and writes each one, without the `::` shorthand for the run of zero groups.
const RECORD_ADDRESS_V6_AS_READ: &str = "2001:db8:0:0:0:0:0:10";
const RECORD_TEXT: &str = "owner=platform";

/// A name of the VPC's OWN internal zone: the Amazon-provided hostname of an address in it.
const INTERNAL_NAME: &str = "ip-10-93-1-5.ec2.internal";
const INTERNAL_ADDRESS: &str = "10.93.1.5";

/// The DNS record types a probe asks for, as RFC 1035 numbers them, and the response codes the
/// resolver answers with.
const TYPE_A: u64 = 1;
const TYPE_AAAA: u64 = 28;
const TYPE_TXT: u64 = 16;
const RCODE_NOERROR: u64 = 0;
const RCODE_SERVFAIL: u64 = 2;
const RCODE_NXDOMAIN: u64 = 3;

/// The stack, role and function this suite deploys, and what the function declares.
const STACK: &str = "resolver-endpoint-socket-probe";
const ROLE: &str = "ProbeRole";
const FUNCTION: &str = "Probe";
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation of the probe is given, well above the one query it sends.
const FUNCTION_TIMEOUT: u64 = 30;

/// How much memory the probe is configured with: `MemorySize` is what the run's own ceiling is
/// read against, and the probe's one socket query needs nothing like this much — it is held well
/// above what the run takes so that the ceiling is never what decides this suite.
const FUNCTION_MEMORY_MB: u64 = 512;

/// The status a stack that was created reports, and the suffix of every status that is an
/// operation still in flight.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// How long this suite looks for a clock-owed move — a stack settling, an endpoint becoming
/// operational, a function becoming active — and how often it looks again. Generous, so a loaded
/// host does not decide any outcome.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(100);

/// The handler the function runs: for each probe the event names it sends ONE DNS query on a
/// datagram socket to the address and port the probe gives, and reports what came back — the
/// response code, the records of the answer section, or the error the socket raised when nothing
/// came back at all.
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
        elif kind == 28 and length == 16:
            groups = [data[index : index + 2].hex() for index in range(0, 16, 2)]
            answered.append(":".join(group.lstrip("0") or "0" for group in groups))
        elif kind == 5:
            answered.append(_read_name(reply, start))
        elif kind == 16:
            answered.append(data[1 : 1 + data[0]].decode("ascii"))
    return answered


def _resolved(server, name, kind, qid):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(5)
    try:
        sock.sendto(_query(name, kind, qid), (server, 53))
        reply, _peer = sock.recvfrom(512)
    except OSError as failed:
        return {"rcode": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()
    return {
        "rcode": reply[3] & 0x0F,
        "authoritative": bool(reply[2] & 0x04),
        "answers": _answers(reply),
    }


def handler(event, context):
    results = []
    for index, probe in enumerate(event["probes"]):
        results.append(
            _resolved(probe["server"], probe["name"], int(probe["type"]), 0x500 + index)
        )
    return {"results": results}
"#;

/// The assembled emulator, on ONE engine, exactly as the router mounts it: the services that own
/// the VPC, the zone, the endpoint and the function all read and write the same records.
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
        .header(
            http::header::AUTHORIZATION,
            authorization("route53resolver"),
        )
        .header(http::header::CONTENT_TYPE, "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Route53Resolver.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
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
    /// The security group the endpoint's interfaces are behind, whose DNS rule the phases
    /// revoke and authorize again.
    endpoint_group: String,
    /// The name Lambda holds the deployed probe function under.
    function: String,
}

/// Build it: the VPC and its two subnets, the two security groups, the private hosted zone and
/// its record sets, the inbound Resolver endpoint on [`ENDPOINT_IPS`], and the function that
/// queries them from inside the VPC.
async fn estate(case: &str) -> Estate {
    let router = build_router();

    let created = ec2(&router, case, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await;
    let vpc = opt_element(&created, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must mint a VPC: {created}"));
    let mut subnets = Vec::new();
    for (index, cidr) in SUBNET_CIDRS.iter().enumerate() {
        let zone = format!("{REGION}{}", ["a", "b"][index]);
        let answered = ec2(
            &router,
            case,
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
                .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet must mint a subnet")),
        );
    }
    let endpoint_group = security_group(&router, case, &vpc, "resolver-endpoint").await;
    let client_group = security_group(&router, case, &vpc, "resolver-client").await;
    // The endpoint's own groups "must include one or more inbound rules … For inbound access,
    // open port 53" (`CreateResolverEndpointRequest$SecurityGroupIds`), on both protocols DNS is
    // carried over.
    for protocol in ["udp", "tcp"] {
        authorize(&router, case, &endpoint_group, protocol).await;
    }

    // The private hosted zone associated with the VPC, and the record sets the queries below are
    // answered out of.
    let created = route53(
        &router,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <Name>{ZONE}</Name><CallerReference>{STACK}</CallerReference>\
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
    let changes = [
        format!("<Name>app.{ZONE}</Name><Type>A</Type><TTL>300</TTL><ResourceRecords>\
                 <ResourceRecord><Value>{RECORD_ADDRESS}</Value></ResourceRecord>\
                 </ResourceRecords>"),
        format!("<Name>www.{ZONE}</Name><Type>CNAME</Type><TTL>300</TTL><ResourceRecords>\
                 <ResourceRecord><Value>app.{ZONE}</Value></ResourceRecord></ResourceRecords>"),
        format!("<Name>v6.{ZONE}</Name><Type>AAAA</Type><TTL>300</TTL><ResourceRecords>\
                 <ResourceRecord><Value>{RECORD_ADDRESS_V6}</Value></ResourceRecord>\
                 </ResourceRecords>"),
        format!("<Name>txt.{ZONE}</Name><Type>TXT</Type><TTL>300</TTL><ResourceRecords>\
                 <ResourceRecord><Value>\"{RECORD_TEXT}\"</Value></ResourceRecord>\
                 </ResourceRecords>"),
    ]
    .map(|record| format!("<Change><Action>UPSERT</Action><ResourceRecordSet>{record}</ResourceRecordSet></Change>"))
    .join("");
    let submitted = route53(
        &router,
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

    // The inbound endpoint, on an address in each subnet, behind the group authorized above.
    let (status, answered) = resolver(
        &router,
        "CreateResolverEndpoint",
        &json!({
            "CreatorRequestId": STACK,
            "Name": "inbound",
            "Direction": "INBOUND",
            "SecurityGroupIds": [endpoint_group],
            "IpAddresses": [
                {"SubnetId": subnets[0], "Ip": ENDPOINT_IPS[0]},
                {"SubnetId": subnets[1], "Ip": ENDPOINT_IPS[1]},
            ],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateResolverEndpoint must be served, got {status} with {answered}"
    );
    let endpoint_id = answered["ResolverEndpoint"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint an endpoint: {answered}"))
        .to_string();
    // An endpoint answers nothing until it is OPERATIONAL: this waits for that STATUS rather
    // than for a span of the clock.
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (_, read) = resolver(
            &router,
            "GetResolverEndpoint",
            &json!({"ResolverEndpointId": endpoint_id}),
        )
        .await;
        if read["ResolverEndpoint"]["Status"].as_str() == Some("OPERATIONAL") {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the endpoint must reach OPERATIONAL, last read {read}"
        );
        tokio::time::sleep(LOOK).await;
    }

    // The client: a function attached to the VPC, deployed by CloudFormation because that is
    // what archives the handler written above into a deployment package.
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
                    // The interface calls attaching a function to a VPC takes, which is what
                    // the managed `AWSLambdaVPCAccessExecutionRole` policy grants.
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
        let (_, described) = cloudformation(&router, "DescribeStacks", &[("StackName", STACK)]).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE {
            break;
        }
        assert!(
            reported.ends_with(IN_PROGRESS) && Instant::now() < giving_up_at,
            "case {case:?}: the stack must reach {CREATE_COMPLETE}, last reported \
             {reported:?} with {described}"
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
        endpoint_group,
        function,
    }
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

/// Authorize DNS on `protocol` from the whole VPC into `group`.
async fn authorize(router: &MultiServiceRouter, case: &str, group: &str, protocol: &str) {
    ec2(
        router,
        case,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", group),
            ("IpPermissions.1.IpProtocol", protocol),
            ("IpPermissions.1.FromPort", "53"),
            ("IpPermissions.1.ToPort", "53"),
            ("IpPermissions.1.IpRanges.1.CidrIp", VPC_CIDR),
        ],
    )
    .await;
}

/// Revoke that same rule, which is the one change the denial phase makes.
async fn revoke(router: &MultiServiceRouter, case: &str, group: &str, protocol: &str) {
    ec2(
        router,
        case,
        "RevokeSecurityGroupIngress",
        &[
            ("GroupId", group),
            ("IpPermissions.1.IpProtocol", protocol),
            ("IpPermissions.1.FromPort", "53"),
            ("IpPermissions.1.ToPort", "53"),
            ("IpPermissions.1.IpRanges.1.CidrIp", VPC_CIDR),
        ],
    )
    .await;
}

/// One probe: the address the query is sent to, and the name and type it asks for.
struct Probe {
    case: &'static str,
    server: String,
    name: String,
    record_type: u64,
}

/// What the probe reported for one query: `None` for a query nothing answered.
#[derive(Debug)]
struct Reported {
    rcode: Option<u64>,
    answers: Vec<String>,
}

/// Send `probes` from inside the VPC and answer what the function reported for each.
async fn probed(estate: &Estate, case: &str, probes: &[Probe]) -> Vec<Reported> {
    let payload = json!({
        "probes": probes
            .iter()
            .map(|probe| json!({
                "server": probe.server,
                "name": probe.name,
                "type": probe.record_type,
            }))
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
        probes.len(),
        "case {case:?}: expected {} results, got {answered}",
        probes.len()
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

/// A probe for `name` of `record_type` sent to `server`.
fn probe(case: &'static str, server: &str, name: &str, record_type: u64) -> Probe {
    Probe {
        case,
        server: server.to_string(),
        name: name.to_string(),
        record_type,
    }
}

/// RULE: every address DNS is answered at inside the VPC answers out of the private hosted zones
/// associated with it — each of the endpoint's addresses and the VPC's own resolver — with the
/// records the name and type asked for, while an address nothing answers DNS at answers nothing
/// at all; and revoking the endpoint group's DNS rule denies the query to the endpoint's
/// addresses without touching the VPC's own resolver, until the rule is authorized again.
#[tokio::test]
async fn an_in_vpc_socket_client_reads_the_resolver_endpoints_gated_answers() {
    let case = "the in-VPC socket client";
    let estate = estate(case).await;

    // (the probe, the response code it must answer, the values the answer must carry)
    let served: Vec<(Probe, u64, Vec<String>)> = vec![
        (
            probe("the endpoint's address in the first subnet", ENDPOINT_IPS[0], &format!("app.{ZONE}"), TYPE_A),
            RCODE_NOERROR,
            vec![RECORD_ADDRESS.to_string()],
        ),
        (
            probe("the endpoint's address in the second subnet", ENDPOINT_IPS[1], &format!("app.{ZONE}"), TYPE_A),
            RCODE_NOERROR,
            vec![RECORD_ADDRESS.to_string()],
        ),
        (
            probe("the VPC's own range plus two", VPC_RANGE_RESOLVER, &format!("app.{ZONE}"), TYPE_A),
            RCODE_NOERROR,
            vec![RECORD_ADDRESS.to_string()],
        ),
        (
            probe("the link-local resolver of the VPC", LINK_LOCAL_RESOLVER, &format!("app.{ZONE}"), TYPE_A),
            RCODE_NOERROR,
            vec![RECORD_ADDRESS.to_string()],
        ),
        (
            probe("an alias chased to the address it names", ENDPOINT_IPS[0], &format!("www.{ZONE}"), TYPE_A),
            RCODE_NOERROR,
            vec![format!("app.{ZONE}."), RECORD_ADDRESS.to_string()],
        ),
        (
            probe("the zone's own AAAA record", ENDPOINT_IPS[0], &format!("v6.{ZONE}"), TYPE_AAAA),
            RCODE_NOERROR,
            vec![RECORD_ADDRESS_V6_AS_READ.to_string()],
        ),
        (
            probe("the zone's own TXT record", ENDPOINT_IPS[0], &format!("txt.{ZONE}"), TYPE_TXT),
            RCODE_NOERROR,
            vec![RECORD_TEXT.to_string()],
        ),
        (
            probe("a name the zone holds nothing at", ENDPOINT_IPS[0], &format!("absent.{ZONE}"), TYPE_A),
            RCODE_NXDOMAIN,
            Vec::new(),
        ),
        (
            probe("a name under no zone of the VPC", ENDPOINT_IPS[0], "example.com", TYPE_A),
            RCODE_SERVFAIL,
            Vec::new(),
        ),
        (
            probe("a name of the VPC's own internal zone", ENDPOINT_IPS[0], INTERNAL_NAME, TYPE_A),
            RCODE_NOERROR,
            vec![INTERNAL_ADDRESS.to_string()],
        ),
    ];
    let probes: Vec<Probe> = served
        .iter()
        .map(|(probe, _, _)| Probe {
            case: probe.case,
            server: probe.server.clone(),
            name: probe.name.clone(),
            record_type: probe.record_type,
        })
        .collect();
    let reported = probed(&estate, case, &probes).await;
    for ((probe, code, values), answered) in served.iter().zip(&reported) {
        assert_eq!(
            answered.rcode,
            Some(*code),
            "case {:?}: expected response code {code}, got {answered:?}",
            probe.case
        );
        assert_eq!(
            &answered.answers, values,
            "case {:?}: expected the answer section {values:?}, got {answered:?}",
            probe.case
        );
    }

    // An address of the VPC nothing answers DNS at takes the datagram and answers nothing, which
    // is what the client reads as a query that was never answered.
    let silent = probed(
        &estate,
        case,
        &[probe(
            "an address of the VPC nothing answers DNS at",
            SILENT_ADDRESS,
            &format!("app.{ZONE}"),
            TYPE_A,
        )],
    )
    .await;
    assert_eq!(
        silent[0].rcode, None,
        "case \"an address of the VPC nothing answers DNS at\": expected no answer at all, got \
         {:?}",
        silent[0]
    );

    // Revoking the endpoint group's datagram rule denies the query to EVERY address of the
    // endpoint, and leaves the VPC's own resolver — which no security group gates — answering.
    revoke(&estate.router, case, &estate.endpoint_group, "udp").await;
    let denied = probed(
        &estate,
        case,
        &[
            probe("the first endpoint address once the rule is revoked", ENDPOINT_IPS[0], &format!("app.{ZONE}"), TYPE_A),
            probe("the second endpoint address once the rule is revoked", ENDPOINT_IPS[1], &format!("app.{ZONE}"), TYPE_A),
            probe("the VPC's own resolver, which no security group gates", VPC_RANGE_RESOLVER, &format!("app.{ZONE}"), TYPE_A),
        ],
    )
    .await;
    for (answered, row) in denied.iter().zip([
        "the first endpoint address once the rule is revoked",
        "the second endpoint address once the rule is revoked",
    ]) {
        assert_eq!(
            answered.rcode, None,
            "case {row:?}: a denied query is answered with nothing at all, got {answered:?}"
        );
        assert!(
            answered.answers.is_empty(),
            "case {row:?}: a denied query carries no answer, got {answered:?}"
        );
    }
    assert_eq!(
        denied[2].answers,
        vec![RECORD_ADDRESS.to_string()],
        "case \"the VPC's own resolver, which no security group gates\": expected the record, got \
         {:?}",
        denied[2]
    );

    // Authorizing the rule again admits the query again.
    authorize(&estate.router, case, &estate.endpoint_group, "udp").await;
    let restored = probed(
        &estate,
        case,
        &[probe(
            "the rule authorized again",
            ENDPOINT_IPS[0],
            &format!("app.{ZONE}"),
            TYPE_A,
        )],
    )
    .await;
    assert_eq!(
        restored[0].rcode,
        Some(RCODE_NOERROR),
        "case \"the rule authorized again\": expected the query to be answered again, got {:?}",
        restored[0]
    );
    assert_eq!(
        restored[0].answers,
        vec![RECORD_ADDRESS.to_string()],
        "case \"the rule authorized again\": expected the zone's own record, got {:?}",
        restored[0]
    );
}
