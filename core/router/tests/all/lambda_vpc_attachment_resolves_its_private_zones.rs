// SPDX-License-Identifier: CC0-1.0

//! Where a VpcConfig-attached AWS Lambda function SITS, and what the VPC it sits in resolves for
//! it — end to end across Amazon EC2, CloudFormation, IAM, AWS Lambda and Route 53 on the
//! assembled emulator.
//!
//! RULE: a function configured with a VpcConfig naming a subnet is placed in that subnet's
//! network — its traffic toward an address of the VPC leaves from a private address inside the
//! attached subnet's own range — and the VPC's Amazon-provided DNS server, at the base of the
//! VPC's IPv4 range plus two, answers it out of exactly the Route 53 private hosted zones
//! ASSOCIATED with that VPC at the moment the query is asked: a name of such a zone resolves to
//! what the zone holds, and a name of a zone this VPC is not (or is no longer) associated with is
//! answered as a name that does not exist.
//!
//! Three services have to agree for that to hold, which is why this suite is here rather than in
//! one crate's own tests: Amazon EC2 owns the VPC, its subnet and the elastic network interface
//! Lambda attaches the function through, Route 53 owns the hosted zone, its record sets and its
//! VPC associations, and AWS Lambda runs the handler whose datagram reaches the resolver. The
//! handler frames and reads the DNS messages itself, so what is asserted is a real exchange on a
//! socket inside the VPC.
//!
//! The rows differ along what the rule generalises over: the SHAPE of the VPC's range (a `/16`
//! and a `/17` whose base is not the start of an octet, so the resolver address is derived rather
//! than assumed), the subnet range inside it, which vantage point asks (a function in each of the
//! two VPCs), which zone is asked about, a name a zone holds against one it does not, and whether
//! the asking VPC is associated with the zone at the time — before an association, after it, and
//! after it is withdrawn again.
//!
//! Both zones below sit under a top-level domain the root zone NEVER delegates (RFC 6761 §6
//! reserves `test` and `invalid`), which is what a private zone's name is in practice: a name
//! nothing outside the VPC can answer. That is also why a name of a zone the asking VPC is not
//! associated with comes back as a name that does not exist — a recursion for it ends at the root
//! with no delegation — rather than as a name whose existence is simply unknown here.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_contains, form_encode, opt_element};

/// The Region every request below is signed for, and the availability zones of that Region the
/// subnets are created in.
const REGION: &str = "us-west-2";

/// The XML namespace every Route 53 request body carries.
const XMLNS: &str = "https://route53.amazonaws.com/doc/2013-04-01/";

/// The DNS record type the rows ask for, as RFC 1035 §3.2.2 numbers it.
const TYPE_A: u64 = 1;

/// The response codes the resolver answers with, as RFC 1035 §4.1.1 numbers them.
const RCODE_NOERROR: u64 = 0;
/// See [`RCODE_NOERROR`].
const RCODE_NXDOMAIN: u64 = 3;

/// How far into a VPC's own IPv4 range its Amazon-provided DNS server sits: "the Amazon DNS
/// server … is located at the base of your VPC network range plus two" (Amazon VPC User Guide,
/// "Amazon DNS server").
const RESOLVER_OFFSET: u32 = 2;

/// One network this suite stands up: a VPC, one subnet of it, a private hosted zone associated
/// with that VPC at creation, and the function attached to the subnet.
struct Network {
    /// How a row naming this network reads in an assertion.
    case: &'static str,
    /// The range the VPC is created with, which decides where its resolver answers.
    vpc_cidr: &'static str,
    /// The range of the subnet the function is attached to, which is where its own address
    /// comes out of.
    subnet_cidr: &'static str,
    /// The availability zone that subnet sits in.
    availability_zone: &'static str,
    /// The private hosted zone associated with this VPC, the name it holds a record set at and
    /// the address that record set carries.
    zone: &'static str,
    /// See [`Network::zone`].
    hostname: &'static str,
    /// See [`Network::zone`].
    address: &'static str,
    /// The logical id the stack declares this network's function under.
    logical: &'static str,
}

/// The two networks every test of this suite stands up, differing in the shape of the VPC range,
/// the subnet inside it and the zone associated with it.
const NETWORKS: [Network; 2] = [
    Network {
        case: "a /16 VPC",
        vpc_cidr: "10.60.0.0/16",
        subnet_cidr: "10.60.3.0/24",
        availability_zone: "us-west-2a",
        zone: "payments.probe.test",
        hostname: "api.payments.probe.test",
        address: "10.60.3.200",
        logical: "PayingProbe",
    },
    Network {
        case: "a /17 VPC, whose range begins inside an octet",
        vpc_cidr: "192.168.128.0/17",
        subnet_cidr: "192.168.136.0/21",
        availability_zone: "us-west-2b",
        zone: "warehouse.probe.invalid",
        hostname: "db.warehouse.probe.invalid",
        address: "192.168.136.40",
        logical: "StoringProbe",
    },
];

/// A name under the first network's zone the zone holds NO record set at.
const ABSENT_NAME: &str = "ghost.payments.probe.test";

/// What the stack declares, and what the functions it deploys are configured with.
const ROLE: &str = "ProbeRole";
/// See [`ROLE`].
const RUNTIME: &str = "python3.12";
/// See [`ROLE`].
const HANDLER: &str = "index.handler";
/// See [`ROLE`]. Generous: no row here measures a span, and the invocation's own timeout is the
/// only bound on the handler's waits.
const FUNCTION_TIMEOUT: u64 = 120;
/// See [`ROLE`].
const FUNCTION_MEMORY_MB: u64 = 1_024;

/// The TTL the record sets below carry, inside `ResourceRecordSet$TTL`'s modelled 0..=172800.
const RECORD_TTL: u32 = 300;

/// The status a stack that was created reports, and the suffix of every status that is an
/// operation still in flight.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// How long this suite looks for work a service's own clock owes — a stack settling, a function
/// becoming active — and how often it looks again. Nothing here sleeps for a fixed span: every
/// wait is for the CONDITION, and the deadline is generous, far past what any step takes, so a
/// loaded host cannot decide an outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const DEADLINE: Duration = Duration::from_secs(180);

/// The handler both functions run: it reports the source address its traffic toward the address
/// the event names leaves from, and, for each probe the event lists, the reply the DNS server at
/// that address answered with.
///
/// The queries are framed and the replies read here, by the client, so the exchange asserted on
/// is a real one; an error the socket raised is reported instead of the reply, so an assertion
/// can quote the refusal the sandbox saw.
const PROBE_SOURCE: &str = r#"
import socket
import struct

# How long one exchange waits for its reply: generous, for an answer served in this same
# process. No probe measures a span.
WAIT = 30


def _source_toward(address):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect((address, 53))
        return sock.getsockname()[0]
    finally:
        sock.close()


def _name_bytes(name):
    labels = [label for label in name.rstrip(".").split(".") if label]
    return b"".join(bytes([len(label)]) + label.encode("ascii") for label in labels) + b"\x00"


def _query(name, kind, qid):
    return (
        struct.pack(">HHHHHH", qid, 0x0100, 1, 0, 0, 0)
        + _name_bytes(name)
        + struct.pack(">HH", kind, 1)
    )


def _skip_name(data, at):
    while True:
        length = data[at]
        if length == 0:
            return at + 1
        if length & 0xC0:
            return at + 2
        at += 1 + length


def _answers(reply):
    at = 12
    for _ in range(int.from_bytes(reply[4:6], "big")):
        at = _skip_name(reply, at) + 4
    answered = []
    for _ in range(int.from_bytes(reply[6:8], "big")):
        at = _skip_name(reply, at)
        kind = int.from_bytes(reply[at : at + 2], "big")
        length = int.from_bytes(reply[at + 8 : at + 10], "big")
        start = at + 10
        at = start + length
        if kind == 1 and length == 4:
            answered.append(".".join(str(octet) for octet in reply[start:at]))
    return answered


def _asked(server, name, kind, qid):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(WAIT)
    try:
        sock.sendto(_query(name, kind, qid), (server, 53))
        reply, peer = sock.recvfrom(512)
    except OSError as failed:
        return {"error": "%s: %s" % (type(failed).__name__, failed)}
    finally:
        sock.close()
    return {"rcode": reply[3] & 0x0F, "answers": _answers(reply), "from": peer[0]}


def handler(event, context):
    answered = {}
    toward = event.get("source_toward")
    if toward:
        try:
            answered["source"] = _source_toward(toward)
        except OSError as failed:
            answered["source_error"] = "%s: %s" % (type(failed).__name__, failed)
    answered["replies"] = [
        _asked(probe["server"], probe["name"], int(probe.get("type", 1)), 0x910 + index)
        for index, probe in enumerate(event.get("probes", []))
    ]
    return answered
"#;

/// The address of the Amazon-provided DNS server of the VPC created with `block`: the base of
/// that range plus [`RESOLVER_OFFSET`].
///
/// Derived from the block rather than written out beside it, so a row whose range begins
/// somewhere else asks a different address — which is the rule, and not one base's arithmetic.
fn vpc_resolver(block: &str) -> String {
    let (base, prefix) = block
        .split_once('/')
        .unwrap_or_else(|| panic!("expected a CIDR block of the form a.b.c.d/len, got {block:?}"));
    let base: Ipv4Addr = base
        .parse()
        .unwrap_or_else(|_| panic!("expected an IPv4 base address, got {block:?}"));
    let prefix: u32 = prefix
        .parse()
        .unwrap_or_else(|_| panic!("expected an IPv4 prefix length, got {block:?}"));
    assert!(
        (1..=30).contains(&prefix),
        "expected a VPC-sized prefix length, got {block:?}"
    );
    let network = u32::from(base) & (u32::MAX << (32 - prefix));
    Ipv4Addr::from(network + RESOLVER_OFFSET).to_string()
}

/// The assembled emulator, on ONE engine, exactly as the router mounts it: the services that own
/// the VPC, the zone and the function all read and write the same records.
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

/// The estate every test of this suite asks from: the two networks, each with its VPC, its zone
/// and the function attached to it.
struct Estate {
    router: MultiServiceRouter,
    /// The VPC of each network, in [`NETWORKS`] order.
    vpcs: Vec<String>,
    /// The hosted zone associated with each network's VPC at creation, in the same order.
    zones: Vec<String>,
    /// The name Lambda holds each network's deployed function under, in the same order.
    functions: Vec<String>,
}

/// Stand up both networks: the VPCs and their subnets, a security group each, a private hosted
/// zone associated with each VPC holding one `A` record set, and the two functions — one attached
/// to each subnet — that query from inside them.
///
/// `stack` names the stack this estate's functions are deployed by, so two tests of this suite
/// stand up estates of their own without sharing a name.
async fn estate(case: &str, stack: &str) -> Estate {
    let router = build_router();
    let mut vpcs = Vec::new();
    let mut subnets = Vec::new();
    let mut groups = Vec::new();
    let mut zones = Vec::new();

    for network in &NETWORKS {
        let created = ec2(&router, case, "CreateVpc", &[("CidrBlock", network.vpc_cidr)]).await;
        let vpc = opt_element(&created, "vpcId").unwrap_or_else(|| {
            panic!("case {case:?}: CreateVpc must mint a VPC for {:?}", network.case)
        });
        let answered = ec2(
            &router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", network.subnet_cidr),
                ("AvailabilityZone", network.availability_zone),
            ],
        )
        .await;
        let subnet = opt_element(&answered, "subnetId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet must mint a subnet"));
        let answered = ec2(
            &router,
            case,
            "CreateSecurityGroup",
            &[
                ("GroupName", &format!("{stack}-{}", network.logical)),
                ("GroupDescription", "the group the resolver client sits in"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await;
        let group = opt_element(&answered, "groupId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup must mint a group"));

        // The private hosted zone associated with THIS VPC, and the one record set the rows read
        // out of it.
        let created = route53(
            &router,
            "POST",
            "/2013-04-01/hostedzone",
            &format!(
                "<CreateHostedZoneRequest xmlns=\"{XMLNS}\"><Name>{}</Name>\
                 <CallerReference>{stack}-{}</CallerReference>\
                 <HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>\
                 <VPC><VPCRegion>{REGION}</VPCRegion><VPCId>{vpc}</VPCId></VPC>\
                 </CreateHostedZoneRequest>",
                network.zone, network.logical
            ),
        )
        .await;
        assert_eq!(
            created.0, 201,
            "case {case:?}: CreateHostedZone({:?}) must answer 201, got {} with {}",
            network.zone, created.0, created.1
        );
        let zone_id = opt_element(&created.1, "Id")
            .unwrap_or_else(|| panic!("case {case:?}: the create must answer the zone's Id"))
            .trim_start_matches("/hostedzone/")
            .to_string();
        let submitted = route53(
            &router,
            "POST",
            &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
            &format!(
                "<ChangeResourceRecordSetsRequest xmlns=\"{XMLNS}\"><ChangeBatch>\
                 <Comment>the zone's one record set</Comment><Changes><Change>\
                 <Action>UPSERT</Action><ResourceRecordSet><Name>{}</Name><Type>A</Type>\
                 <TTL>{RECORD_TTL}</TTL><ResourceRecords><ResourceRecord><Value>{}</Value>\
                 </ResourceRecord></ResourceRecords></ResourceRecordSet></Change></Changes>\
                 </ChangeBatch></ChangeResourceRecordSetsRequest>",
                network.hostname, network.address
            ),
        )
        .await;
        assert_eq!(
            submitted.0, 200,
            "case {case:?}: ChangeResourceRecordSets must be served, got {} with {}",
            submitted.0, submitted.1
        );

        vpcs.push(vpc);
        subnets.push(subnet);
        groups.push(group);
        zones.push(zone_id);
    }

    // The clients: one function per network, attached to that network's subnet. Deployed by
    // CloudFormation because that is what archives the handler written above into a deployment
    // package.
    let mut resources = serde_json::Map::new();
    resources.insert(
        ROLE.to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": format!("{stack}-role"),
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
        }),
    );
    let mut functions = Vec::new();
    for (index, network) in NETWORKS.iter().enumerate() {
        let name = format!("{stack}-{}", network.logical);
        resources.insert(
            network.logical.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "MemorySize": FUNCTION_MEMORY_MB,
                    "Code": {"ZipFile": PROBE_SOURCE},
                    "VpcConfig": {
                        "SubnetIds": [subnets[index].clone()],
                        "SecurityGroupIds": [groups[index].clone()],
                    },
                },
            }),
        );
        functions.push(name);
    }
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string();
    let (status, answered) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
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
            cloudformation(&router, "DescribeStacks", &[("StackName", stack)]).await;
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

    // Each function really is attached to its own network's VPC: the id Lambda reports for it is
    // the one EC2 holds for the subnet it was placed in, not a value the request carried.
    for (index, network) in NETWORKS.iter().enumerate() {
        let giving_up_at = Instant::now() + DEADLINE;
        loop {
            let (_, configuration) = lambda(
                &router,
                "GET",
                &format!("/2015-03-31/functions/{}/configuration", functions[index]),
                None,
            )
            .await;
            if configuration["State"].as_str() == Some("Active") {
                assert_eq!(
                    configuration["VpcConfig"]["VpcId"].as_str(),
                    Some(vpcs[index].as_str()),
                    "case {case:?}: the function of {:?} must be attached to that VPC, got \
                     {configuration}",
                    network.case
                );
                break;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: Lambda must report the function of {:?} Active, last read \
                 {configuration}",
                network.case
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    Estate {
        router,
        vpcs,
        zones,
        functions,
    }
}

/// Invoke the function of network `which` with `event` and answer what the handler reported.
async fn invoked(estate: &Estate, case: &str, which: usize, event: &Value) -> Value {
    let (status, answered) = lambda(
        &estate.router,
        "POST",
        &format!("/2015-03-31/functions/{}/invocations", estate.functions[which]),
        Some(event),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {answered}"
    );
    assert!(
        answered.get("errorMessage").is_none(),
        "case {case:?}: the handler must run to completion, got {answered}"
    );
    answered
}

/// What the handler reported for one query: the response code, the addresses of the answer
/// section, and the address the reply came from.
#[derive(Debug)]
struct Reply {
    rcode: Option<u64>,
    answers: Vec<String>,
    from: Option<String>,
}

/// The reply at `index` of what the handler reported, which must be a reply rather than the
/// error a socket raised.
fn reply_of(reported: &Value, case: &str, index: usize) -> Reply {
    let read = reported["replies"].get(index).unwrap_or_else(|| {
        panic!("case {case:?}: the handler must report one reply per probe, got {reported}")
    });
    assert!(
        read.get("error").is_none(),
        "case {case:?}: the query must be answered rather than refused, got {reported}"
    );
    Reply {
        rcode: read["rcode"].as_u64(),
        answers: read["answers"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .map(|value| value.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        from: read["from"].as_str().map(str::to_string),
    }
}

/// Ask the resolver of network `which`'s own VPC about `name` from inside that VPC, and assert
/// the response code and the addresses it answers with.
async fn asked(
    estate: &Estate,
    case: &str,
    which: usize,
    name: &str,
    rcode: u64,
    answers: &[&str],
) {
    let resolver = vpc_resolver(NETWORKS[which].vpc_cidr);
    let reported = invoked(
        estate,
        case,
        which,
        &json!({ "probes": [{ "server": resolver, "name": name, "type": TYPE_A }] }),
    )
    .await;
    let read = reply_of(&reported, case, 0);
    assert_eq!(
        read.rcode,
        Some(rcode),
        "case {case:?}: expected {name} to be answered with response code {rcode}, got \
         {read:?} ({reported})"
    );
    let expected: Vec<String> = answers.iter().map(|value| (*value).to_string()).collect();
    assert_eq!(
        read.answers, expected,
        "case {case:?}: expected {name} to answer {expected:?}, got {read:?} ({reported})"
    );
    assert_eq!(
        read.from.as_deref(),
        Some(resolver.as_str()),
        "case {case:?}: expected the reply to come from the VPC's own resolver {resolver}, got \
         {read:?} ({reported})"
    );
}

/// Associate the VPC of network `which` with `zone`, which must be served.
async fn associate(estate: &Estate, case: &str, zone: &str, which: usize) {
    let (status, text) = route53(
        &estate.router,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone}/associatevpc"),
        &format!(
            "<AssociateVPCWithHostedZoneRequest xmlns=\"{XMLNS}\"><VPC>\
             <VPCRegion>{REGION}</VPCRegion><VPCId>{}</VPCId></VPC>\
             </AssociateVPCWithHostedZoneRequest>",
            estate.vpcs[which]
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssociateVPCWithHostedZone must be served, got {status} with {text}"
    );
}

/// Withdraw the association of the VPC of network `which` from `zone`, which must be served.
async fn disassociate(estate: &Estate, case: &str, zone: &str, which: usize) {
    let (status, text) = route53(
        &estate.router,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone}/disassociatevpc"),
        &format!(
            "<DisassociateVPCFromHostedZoneRequest xmlns=\"{XMLNS}\"><VPC>\
             <VPCRegion>{REGION}</VPCRegion><VPCId>{}</VPCId></VPC>\
             </DisassociateVPCFromHostedZoneRequest>",
            estate.vpcs[which]
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DisassociateVPCFromHostedZone must be served, got {status} with {text}"
    );
}

/// RULE: a function attached to a subnet of a VPC is placed in that network — its traffic toward
/// an address of the VPC leaves from an address inside the attached subnet's own range — and the
/// VPC's Amazon-provided resolver, at the base of the VPC's range plus two, answers it out of the
/// private hosted zone associated with that VPC.
///
/// The rows differ along the shape of the range the rule is read off: a `/16` and a `/17` whose
/// base is not the start of an octet, so the resolver address and the subnet both move.
#[tokio::test]
async fn an_attached_function_sits_in_its_subnet_and_its_vpcs_resolver_answers_it() {
    let estate = estate("the attachment", "xsvc-pz-placement").await;

    for (which, network) in NETWORKS.iter().enumerate() {
        let case = network.case;
        let resolver = vpc_resolver(network.vpc_cidr);
        let reported = invoked(
            &estate,
            case,
            which,
            &json!({
                "source_toward": resolver,
                "probes": [{ "server": resolver, "name": network.hostname, "type": TYPE_A }],
            }),
        )
        .await;

        let source = reported["source"].as_str().unwrap_or_else(|| {
            panic!(
                "case {case:?}: the route toward the resolver {resolver} must report a source \
                 address, got {reported}"
            )
        });
        assert!(
            block_contains(network.subnet_cidr, source),
            "case {case:?}: expected a source address inside the attached subnet {}, got \
             {source:?} ({reported})",
            network.subnet_cidr
        );
        for other in NETWORKS.iter().filter(|other| other.case != network.case) {
            assert!(
                !block_contains(other.vpc_cidr, source),
                "case {case:?}: expected the source address {source:?} to be outside the other \
                 VPC's range {}, got {reported}",
                other.vpc_cidr
            );
        }

        let read = reply_of(&reported, case, 0);
        assert_eq!(
            read.rcode,
            Some(RCODE_NOERROR),
            "case {case:?}: expected the VPC's resolver at {resolver} to answer {} with response \
             code {RCODE_NOERROR}, got {read:?} ({reported})",
            network.hostname
        );
        assert_eq!(
            read.answers,
            vec![network.address.to_string()],
            "case {case:?}: expected {} to answer the address the zone holds, got {read:?} \
             ({reported})",
            network.hostname
        );
        assert_eq!(
            read.from.as_deref(),
            Some(resolver.as_str()),
            "case {case:?}: expected the reply to come from the resolver {resolver} itself, got \
             {read:?} ({reported})"
        );
    }
}

/// RULE: a private hosted zone's names resolve inside exactly the VPCs the zone is ASSOCIATED
/// with at the moment the query is asked — so a VPC it was never associated with, and one whose
/// association was withdrawn, are answered as names that do not exist, while the zone's own VPC
/// keeps resolving them.
///
/// The rows differ along which vantage point asks, which zone is asked about, whether the zone
/// holds the name at all, and the association's state: as created, after an association is added,
/// and after it is withdrawn again.
#[tokio::test]
async fn a_private_zone_answers_only_the_vpcs_it_is_associated_with() {
    let estate = estate("the association", "xsvc-pz-association").await;
    let [first, second] = &NETWORKS;

    // As created, each zone is associated with its own VPC and with no other.
    asked(
        &estate,
        "its own zone, from the VPC it was created with",
        0,
        first.hostname,
        RCODE_NOERROR,
        &[first.address],
    )
    .await;
    asked(
        &estate,
        "its own zone, from the other VPC it was created with",
        1,
        second.hostname,
        RCODE_NOERROR,
        &[second.address],
    )
    .await;
    asked(
        &estate,
        "a name the asking VPC's own zone does not hold",
        0,
        ABSENT_NAME,
        RCODE_NXDOMAIN,
        &[],
    )
    .await;
    asked(
        &estate,
        "the other VPC's zone, asked from a VPC it is not associated with",
        0,
        second.hostname,
        RCODE_NXDOMAIN,
        &[],
    )
    .await;
    asked(
        &estate,
        "the first VPC's zone, asked from a VPC it is not associated with",
        1,
        first.hostname,
        RCODE_NXDOMAIN,
        &[],
    )
    .await;

    // Associating the second zone with the first VPC is what makes its names resolve there, and
    // it changes nothing about the VPC the zone was created with.
    associate(&estate, "the added association", &estate.zones[1], 0).await;
    asked(
        &estate,
        "the newly associated zone, from the VPC just associated with it",
        0,
        second.hostname,
        RCODE_NOERROR,
        &[second.address],
    )
    .await;
    asked(
        &estate,
        "the newly associated zone, from the VPC it was created with",
        1,
        second.hostname,
        RCODE_NOERROR,
        &[second.address],
    )
    .await;

    // Withdrawing it again takes the name away from that VPC only.
    disassociate(&estate, "the withdrawn association", &estate.zones[1], 0).await;
    asked(
        &estate,
        "the disassociated zone, from the VPC withdrawn from it",
        0,
        second.hostname,
        RCODE_NXDOMAIN,
        &[],
    )
    .await;
    asked(
        &estate,
        "the disassociated zone, from the VPC it is still associated with",
        1,
        second.hostname,
        RCODE_NOERROR,
        &[second.address],
    )
    .await;
    asked(
        &estate,
        "the first zone, untouched by the other zone's associations",
        0,
        first.hostname,
        RCODE_NOERROR,
        &[first.address],
    )
    .await;
}
