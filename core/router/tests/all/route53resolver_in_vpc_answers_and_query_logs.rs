// SPDX-License-Identifier: CC0-1.0

//! What the Route 53 Resolver of a VPC answers a query asked INSIDE that VPC, and what it
//! RECORDS about it — end to end across Amazon EC2, Route 53, Route 53 Resolver, CloudWatch
//! Logs, IAM, CloudFormation and AWS Lambda on the assembled emulator.
//!
//! Four rules are under test, each over the one vantage point a client of a VPC's resolver can
//! be in — a program AWS Lambda runs inside that VPC, which frames and reads real DNS messages
//! on a datagram socket:
//!
//! 1. a public name whose existence and addresses a DNS STANDARD fixes is answered out of that
//!    standard, and every other name under no zone of the VPC is one the resolver has no answer
//!    for;
//! 2. a WILDCARD record set of a private hosted zone associated with the VPC answers every name
//!    under its parent that no closer name of the zone answers, under the name that was asked
//!    for;
//! 3. the AUTODEFINED rules for reverse DNS lookups are what answer a reverse question about an
//!    address of the VPC, so a VPC whose Resolver configuration has them disabled stops
//!    answering one;
//! 4. a query the Resolver answered is RECORDED in each query logging configuration associated
//!    with the VPC, carrying the question, the response code, the answer section and the address
//!    inside the VPC the query came from — and, for a query a DNS Firewall rule matched, that
//!    rule's action, rule group and domain list, which is the only evidence an `ALERT` rule
//!    leaves because it changes no answer.
//!
//! Only an assembled emulator can pin any of them: the VPC, its subnet and the interface the
//! function's queries leave from are Amazon EC2's records, the zone and its record sets are
//! Route 53's, the resolver configuration, the firewall rules and the query logging
//! configurations are Route 53 Resolver's, the delivered records are CloudWatch Logs', and the
//! client is a function AWS Lambda runs in the VPC.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every request below is signed for, whose private DNS domain is therefore
/// `ec2.internal` ("AWS uses `ec2.internal` in `us-east-1`", Amazon VPC User Guide, "DHCP option
/// sets").
const REGION: &str = "us-east-1";

/// The network the function and the zone sit in.
const VPC_CIDR: &str = "10.94.0.0/16";
/// See [`VPC_CIDR`].
const SUBNET_CIDR: &str = "10.94.1.0/24";
/// See [`VPC_CIDR`].
const AVAILABILITY_ZONE: &str = "us-east-1a";

/// The VPC's own resolver, at the base of its range plus two.
const VPC_RESOLVER: &str = "10.94.0.2";

/// The private hosted zone associated with the VPC.
const ZONE: &str = "svc.internal";

/// The parent the zone's WILDCARD record set stands under, the name it answers with, and the one
/// name under that parent the zone holds a record set of its own at.
const WILDCARD_PARENT: &str = "wild.svc.internal";
/// See [`WILDCARD_PARENT`].
const WILDCARD_ADDRESS: &str = "192.0.2.40";
/// See [`WILDCARD_PARENT`].
const EXACT_NAME: &str = "exact.wild.svc.internal";
/// See [`WILDCARD_PARENT`].
const EXACT_ADDRESS: &str = "192.0.2.41";

/// An address of the VPC, and the Amazon-provided hostname and reverse name it has: the reverse
/// question the autodefined rules for reverse lookups answer.
const IN_VPC_ADDRESS_NAME: &str = "ip-10-94-1-5.ec2.internal";
/// See [`IN_VPC_ADDRESS_NAME`].
const IN_VPC_REVERSE_NAME: &str = "5.1.94.10.in-addr.arpa";

/// The addresses a name a DNS standard fixes resolves to: the first address of the blocks
/// RFC 5737 §3 and RFC 3849 reserve for documentation, which is what the registry publishes for
/// the second-level domains RFC 2606 §3 reserves.
const DOCUMENTATION_ADDRESS: &str = "192.0.2.1";
/// The same IPv6 address as the probe writes it: it reads the sixteen bytes of the record as
/// eight groups and writes each one, without the `::` shorthand for the run of zero groups.
const DOCUMENTATION_ADDRESS_V6_AS_READ: &str = "2001:db8:0:0:0:0:0:1";

/// The DNS record types a probe asks for, as RFC 1035 numbers them, and the response codes the
/// resolver answers with.
const TYPE_A: u64 = 1;
/// See [`TYPE_A`].
const TYPE_AAAA: u64 = 28;
/// See [`TYPE_A`].
const TYPE_PTR: u64 = 12;
/// See [`TYPE_A`].
const RCODE_NOERROR: u64 = 0;
/// See [`TYPE_A`].
const RCODE_SERVFAIL: u64 = 2;
/// See [`TYPE_A`].
const RCODE_NXDOMAIN: u64 = 3;

/// The log group the query logging configuration of this suite delivers into.
const LOG_GROUP: &str = "/aws/route53resolver/xsvc-in-vpc-queries";

/// The stack, role and function this suite deploys, and what the function declares.
const ROLE: &str = "ProbeRole";
/// See [`ROLE`].
const FUNCTION: &str = "Probe";
/// See [`ROLE`].
const RUNTIME: &str = "python3.12";
/// See [`ROLE`].
const HANDLER: &str = "index.handler";

/// How long one invocation of the probe is given, and how much memory it is configured with.
/// Both generous: what is under test is what the resolver answers and records, never how fast an
/// interpreter gets through one run.
const FUNCTION_TIMEOUT: u64 = 120;
/// See [`FUNCTION_TIMEOUT`].
const FUNCTION_MEMORY_MB: u64 = 1_024;

/// The status a stack that was created reports, and the suffix of every status that is an
/// operation still in flight.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const IN_PROGRESS: &str = "_IN_PROGRESS";

/// How long this suite looks for work a service's own clock owes — a stack settling, a function
/// becoming active, a configuration settling, a record arriving — and how often it looks again.
/// Nothing here sleeps for a fixed span: every wait is for the CONDITION, and the deadline is
/// generous, far past what any step takes, so a loaded host cannot decide an outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const DEADLINE: Duration = Duration::from_secs(180);

/// The handler the function runs: for each probe the event names it sends ONE DNS query on a
/// datagram socket to the address the probe gives, and reports what came back — the response
/// code, the records of the answer section, or the error the socket raised when nothing came
/// back at all.
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
        elif kind in (5, 12):
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
            _resolved(probe["server"], probe["name"], int(probe["type"]), 0x700 + index)
        )
    return {"results": results}
"#;

/// The assembled emulator, on ONE engine, exactly as the router mounts it: the services that own
/// the VPC, the zone, the configurations and the function all read and write the same records.
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

/// One CloudWatch Logs call that must be served, as its parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let host = format!("logs.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("logs"))
        .header(http::header::CONTENT_TYPE, "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "CloudWatch Logs {operation} must be served, got {status} with {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
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
    /// The VPC the function and the zone sit in.
    vpc: String,
    /// The name Lambda holds the deployed probe function under.
    function: String,
}

/// Build it: the VPC and its subnet, a security group, the private hosted zone with the record
/// sets the rows ask for, and the function that queries the resolver from inside the VPC.
///
/// `stack` names the stack this estate's function is deployed by, so two tests of this suite
/// stand up estates of their own without sharing a name.
async fn estate(case: &str, stack: &str) -> Estate {
    let router = build_router();

    let created = ec2(&router, case, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await;
    let vpc = opt_element(&created, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must mint a VPC: {created}"));
    let answered = ec2(
        &router,
        case,
        "CreateSubnet",
        &[
            ("VpcId", vpc.as_str()),
            ("CidrBlock", SUBNET_CIDR),
            ("AvailabilityZone", AVAILABILITY_ZONE),
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
            ("GroupName", &format!("{stack}-group")),
            ("GroupDescription", "the group the resolver client sits in"),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    let group = opt_element(&answered, "groupId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSecurityGroup must mint a group"));

    // The private hosted zone associated with the VPC, and the record sets the queries below are
    // answered out of: a WILDCARD record set, and one name under its parent the zone holds a
    // record set of its own at.
    let created = route53(
        &router,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <Name>{ZONE}</Name><CallerReference>{stack}</CallerReference>\
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
        format!(
            "<Name>*.{WILDCARD_PARENT}</Name><Type>A</Type><TTL>120</TTL><ResourceRecords>\
             <ResourceRecord><Value>{WILDCARD_ADDRESS}</Value></ResourceRecord></ResourceRecords>"
        ),
        format!(
            "<Name>{EXACT_NAME}</Name><Type>A</Type><TTL>120</TTL><ResourceRecords>\
             <ResourceRecord><Value>{EXACT_ADDRESS}</Value></ResourceRecord></ResourceRecords>"
        ),
    ]
    .map(|record| {
        format!(
            "<Change><Action>UPSERT</Action><ResourceRecordSet>{record}</ResourceRecordSet>\
             </Change>"
        )
    })
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

    // The client: a function attached to the VPC, deployed by CloudFormation because that is
    // what archives the handler written above into a deployment package.
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: {
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
            },
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{stack}-probe"),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "MemorySize": FUNCTION_MEMORY_MB,
                    "Code": {"ZipFile": PROBE_SOURCE},
                    "VpcConfig": {
                        "SubnetIds": [subnet],
                        "SecurityGroupIds": [group],
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

    let function = format!("{stack}-probe");
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
        function,
    }
}

/// What the probe reported for one query: `None` for a query nothing answered.
#[derive(Debug)]
struct Reported {
    rcode: Option<u64>,
    answers: Vec<String>,
}

/// Send one query for `name` of `record_type` to the VPC's own resolver from inside the VPC, and
/// answer what the function reported.
async fn probed(estate: &Estate, case: &str, name: &str, record_type: u64) -> Reported {
    let payload = json!({
        "probes": [{ "server": VPC_RESOLVER, "name": name, "type": record_type }],
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
    let result = answered["results"].get(0).unwrap_or_else(|| {
        panic!("case {case:?}: the handler must report one result per probe: {answered}")
    });
    Reported {
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
    }
}

/// RULE: the VPC's own resolver answers a public name whose addresses a DNS standard fixes out
/// of that standard, a name under a WILDCARD record set of a private zone of the VPC out of that
/// record set, and has no answer for any other name under no zone of the VPC.
///
/// The rows differ along what the two rules generalise over: WHICH reserved domain and host
/// label is asked for and in WHICH address family, a type such a name holds no record of, a name
/// one label under the wildcard's parent and one several labels under it, the parent itself, a
/// name the zone holds its own record set at, a type the wildcard holds no record of, and the
/// public names outside the standard's class.
#[tokio::test]
async fn the_vpc_resolver_answers_standard_public_names_and_the_zones_wildcard() {
    let case = "the in-VPC socket client";
    let estate = estate(case, "xsvc-r53r-answers").await;

    // (case, the name asked for, the type, the response code, the values the answer carries)
    let rows: &[(&str, String, u64, u64, Vec<String>)] = &[
        (
            "a reserved documentation domain",
            "example.com".to_string(),
            TYPE_A,
            RCODE_NOERROR,
            vec![DOCUMENTATION_ADDRESS.to_string()],
        ),
        (
            "another reserved documentation domain",
            "example.net".to_string(),
            TYPE_A,
            RCODE_NOERROR,
            vec![DOCUMENTATION_ADDRESS.to_string()],
        ),
        (
            "the host label published beside the third",
            "www.example.org".to_string(),
            TYPE_A,
            RCODE_NOERROR,
            vec![DOCUMENTATION_ADDRESS.to_string()],
        ),
        (
            "the IPv6 address of such a name",
            "example.com".to_string(),
            TYPE_AAAA,
            RCODE_NOERROR,
            vec![DOCUMENTATION_ADDRESS_V6_AS_READ.to_string()],
        ),
        (
            "a label under the same domain no standard fixes",
            "broker.example.com".to_string(),
            TYPE_A,
            RCODE_SERVFAIL,
            Vec::new(),
        ),
        (
            "an ordinary public name",
            "aws.amazon.com".to_string(),
            TYPE_A,
            RCODE_SERVFAIL,
            Vec::new(),
        ),
        (
            "a name one label under the wildcard's parent",
            format!("app.{WILDCARD_PARENT}"),
            TYPE_A,
            RCODE_NOERROR,
            vec![WILDCARD_ADDRESS.to_string()],
        ),
        (
            "a name several labels under it",
            format!("db.inner.{WILDCARD_PARENT}"),
            TYPE_A,
            RCODE_NOERROR,
            vec![WILDCARD_ADDRESS.to_string()],
        ),
        (
            "the wildcard's parent itself, which a wildcard does not cover",
            WILDCARD_PARENT.to_string(),
            TYPE_A,
            RCODE_NXDOMAIN,
            Vec::new(),
        ),
        (
            "a name the zone holds its own record set at",
            EXACT_NAME.to_string(),
            TYPE_A,
            RCODE_NOERROR,
            vec![EXACT_ADDRESS.to_string()],
        ),
        (
            "a type the wildcard holds no record of",
            format!("app.{WILDCARD_PARENT}"),
            TYPE_AAAA,
            RCODE_NOERROR,
            Vec::new(),
        ),
    ];
    for (row, name, record_type, code, values) in rows {
        let answered = probed(&estate, row, name, *record_type).await;
        assert_eq!(
            answered.rcode,
            Some(*code),
            "case {row:?}: expected response code {code} for {name}, got {answered:?}"
        );
        assert_eq!(
            &answered.answers, values,
            "case {row:?}: expected the answer section {values:?} for {name}, got {answered:?}"
        );
    }
}

/// RULE: the autodefined rules for reverse DNS lookups answer a reverse question about an
/// address of the VPC, and a VPC whose Resolver configuration has them DISABLED stops answering
/// one — while the forward names of the VPC keep being answered, because the setting is about
/// the reverse rules alone.
#[tokio::test]
async fn disabling_the_autodefined_reverse_rules_stops_the_reverse_answer() {
    let case = "the autodefined reverse rules";
    let estate = estate(case, "xsvc-r53r-reverse").await;

    let before = probed(
        &estate,
        "the reverse name while the rules are on",
        IN_VPC_REVERSE_NAME,
        TYPE_PTR,
    )
    .await;
    assert_eq!(
        before.answers,
        vec![format!("{IN_VPC_ADDRESS_NAME}.")],
        "case {case:?}: expected the Amazon-provided hostname of the address, got {before:?}"
    );

    // The one change this phase makes: the VPC's Resolver configuration is asked to disable them.
    let updated = resolver_ok(
        &estate.router,
        "UpdateResolverConfig",
        &json!({ "ResourceId": estate.vpc, "AutodefinedReverseFlag": "DISABLE" }),
    )
    .await;
    assert_eq!(
        updated["ResolverConfig"]["ResourceId"].as_str(),
        Some(estate.vpc.as_str()),
        "case {case:?}: the update must answer the VPC's own configuration, got {updated}"
    );

    // The move the clock owes, waited out by looking for the STATUS rather than for a span.
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let read = resolver_ok(
            &estate.router,
            "GetResolverConfig",
            &json!({ "ResourceId": estate.vpc }),
        )
        .await;
        if read["ResolverConfig"]["AutodefinedReverse"].as_str() == Some("DISABLED") {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the configuration must settle on DISABLED, last read {read}"
        );
        tokio::time::sleep(LOOK).await;
    }

    let after = probed(
        &estate,
        "the reverse name once the rules are off",
        IN_VPC_REVERSE_NAME,
        TYPE_PTR,
    )
    .await;
    assert!(
        after.answers.is_empty(),
        "case {case:?}: expected the reverse answer to stop, got {after:?}"
    );
    assert_eq!(
        after.rcode,
        Some(RCODE_NXDOMAIN),
        "case {case:?}: expected the reverse name to be answered as one that does not exist, \
         got {after:?}"
    );

    // The setting is about the REVERSE rules alone: the forward names of the VPC's zones keep
    // being answered.
    let forward = probed(
        &estate,
        "a forward name once the reverse rules are off",
        &format!("app.{WILDCARD_PARENT}"),
        TYPE_A,
    )
    .await;
    assert_eq!(
        forward.answers,
        vec![WILDCARD_ADDRESS.to_string()],
        "case {case:?}: expected the forward answer to be unchanged, got {forward:?}"
    );
}

/// RULE: a query the Resolver answered inside a VPC is recorded in each query logging
/// configuration associated with that VPC — carrying the question, the response code, the answer
/// section and the address inside the VPC the query came from — and a query a DNS Firewall rule
/// matched also carries that rule's action, rule group and domain list, which is the only
/// evidence an `ALERT` rule leaves because it changes no answer.
#[tokio::test]
async fn an_answered_query_is_recorded_in_the_vpcs_query_logging_configuration() {
    let case = "the query logging configuration";
    let estate = estate(case, "xsvc-r53r-querylog").await;

    // The destination: a log group of the account's own, created through CloudWatch Logs itself.
    logs(
        &estate.router,
        "CreateLogGroup",
        &json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    let destination = format!(
        "arn:aws:logs:{REGION}:{}:log-group:{LOG_GROUP}:*",
        engine::emulator_account_id()
    );
    let created = resolver_ok(
        &estate.router,
        "CreateResolverQueryLogConfig",
        &json!({
            "CreatorRequestId": "xsvc-r53r-querylog",
            "Name": "in-vpc-queries",
            "DestinationArn": destination,
        }),
    )
    .await;
    let config_id = created["ResolverQueryLogConfig"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint a configuration: {created}"))
        .to_string();
    resolver_ok(
        &estate.router,
        "AssociateResolverQueryLogConfig",
        &json!({ "ResolverQueryLogConfigId": config_id, "ResourceId": estate.vpc }),
    )
    .await;
    // An association logs nothing until it is ACTIVE: this waits for that STATUS.
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let listed = resolver_ok(
            &estate.router,
            "ListResolverQueryLogConfigAssociations",
            &json!({}),
        )
        .await;
        let active = listed["ResolverQueryLogConfigAssociations"]
            .as_array()
            .map(|associations| {
                associations
                    .iter()
                    .any(|association| association["Status"].as_str() == Some("ACTIVE"))
            })
            .unwrap_or_default();
        if active {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the association must reach ACTIVE, last read {listed}"
        );
        tokio::time::sleep(LOOK).await;
    }

    let asked = format!("app.{WILDCARD_PARENT}");
    let answered = probed(&estate, "a query recorded by the configuration", &asked, TYPE_A).await;
    assert_eq!(
        answered.answers,
        vec![WILDCARD_ADDRESS.to_string()],
        "case {case:?}: expected the zone's own answer, got {answered:?}"
    );
    let record = recorded(&estate, case, &asked, None).await;
    assert_eq!(
        record["vpc_id"].as_str(),
        Some(estate.vpc.as_str()),
        "case {case:?}: expected the record to name the VPC the query was asked in, got {record}"
    );
    assert_eq!(
        record["query_type"].as_str(),
        Some("A"),
        "case {case:?}: expected the type that was asked about, got {record}"
    );
    assert_eq!(
        record["query_class"].as_str(),
        Some("IN"),
        "case {case:?}: expected the class that was asked about, got {record}"
    );
    assert_eq!(
        record["rcode"].as_str(),
        Some("NOERROR"),
        "case {case:?}: expected the response code the client was answered with, got {record}"
    );
    assert_eq!(
        record["region"].as_str(),
        Some(REGION),
        "case {case:?}: expected the Region of the destination, got {record}"
    );
    assert_eq!(
        record["account_id"].as_str(),
        Some(engine::emulator_account_id()),
        "case {case:?}: expected the account the configuration belongs to, got {record}"
    );
    let answers: Vec<String> = record["answers"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["Rdata"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        answers,
        vec![WILDCARD_ADDRESS.to_string()],
        "case {case:?}: expected the answer section the client read, got {record}"
    );
    // The address the query came FROM is the private address of the interface Lambda stood up
    // for the function in the VPC, which is an address of the VPC's own range.
    let source = record["srcaddr"].as_str().unwrap_or_default().to_string();
    assert!(
        source.starts_with("10.94."),
        "case {case:?}: expected the private address of the function's interface, got {record}"
    );
    assert_eq!(
        record["transport"].as_str(),
        Some("UDP"),
        "case {case:?}: expected the transport the query was carried over, got {record}"
    );
    // The RESOURCE the query came from, as a record attributes one: the function is no instance,
    // so it is the interface of the VPC the query left through — the one Amazon EC2 holds the
    // source address on.
    let attributed = record["srcids"]["instance"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let interfaces = ec2(
        &estate.router,
        case,
        "DescribeNetworkInterfaces",
        &[
            ("Filter.1.Name", "private-ip-address"),
            ("Filter.1.Value.1", source.as_str()),
        ],
    )
    .await;
    assert!(
        !attributed.is_empty() && interfaces.contains(&attributed),
        "case {case:?}: expected the record to attribute the query to the interface Amazon EC2 \
         holds {source:?} on, got {attributed:?} against {interfaces}"
    );
    assert!(
        record.get("firewall_rule_action").is_none(),
        "case {case:?}: a query no rule matched names no rule, got {record}"
    );

    // An ALERT rule covering the same name changes no answer, and the record of the match is the
    // only evidence it left.
    let list = resolver_ok(
        &estate.router,
        "CreateFirewallDomainList",
        &json!({ "CreatorRequestId": "xsvc-r53r-alert-list", "Name": "alerted" }),
    )
    .await;
    let list_id = list["FirewallDomainList"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint a domain list: {list}"))
        .to_string();
    resolver_ok(
        &estate.router,
        "UpdateFirewallDomains",
        &json!({
            "FirewallDomainListId": list_id,
            "Operation": "ADD",
            "Domains": [asked.clone()],
        }),
    )
    .await;
    let group = resolver_ok(
        &estate.router,
        "CreateFirewallRuleGroup",
        &json!({ "CreatorRequestId": "xsvc-r53r-alert-group", "Name": "alerting" }),
    )
    .await;
    let group_id = group["FirewallRuleGroup"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint a rule group: {group}"))
        .to_string();
    resolver_ok(
        &estate.router,
        "CreateFirewallRule",
        &json!({
            "CreatorRequestId": "xsvc-r53r-alert-rule",
            "FirewallRuleGroupId": group_id,
            "FirewallDomainListId": list_id,
            "Name": "alert-on-the-name",
            "Priority": 100,
            "Action": "ALERT",
        }),
    )
    .await;
    resolver_ok(
        &estate.router,
        "AssociateFirewallRuleGroup",
        &json!({
            "CreatorRequestId": "xsvc-r53r-alert-association",
            "FirewallRuleGroupId": group_id,
            "VpcId": estate.vpc,
            "Priority": 101,
            "Name": "alerting-association",
        }),
    )
    .await;
    // An association filters nothing until it is COMPLETE: this waits for that STATUS.
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let listed = resolver_ok(
            &estate.router,
            "ListFirewallRuleGroupAssociations",
            &json!({ "VpcId": estate.vpc }),
        )
        .await;
        let complete = listed["FirewallRuleGroupAssociations"]
            .as_array()
            .map(|associations| {
                associations
                    .iter()
                    .any(|association| association["Status"].as_str() == Some("COMPLETE"))
            })
            .unwrap_or_default();
        if complete {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: the rule group association must reach COMPLETE, last read {listed}"
        );
        tokio::time::sleep(LOOK).await;
    }

    let alerted = probed(&estate, "a query a matched ALERT rule permits", &asked, TYPE_A).await;
    assert_eq!(
        alerted.answers,
        vec![WILDCARD_ADDRESS.to_string()],
        "case {case:?}: an ALERT rule blocks nothing, so the zone's own answer stands, got \
         {alerted:?}"
    );
    let record = recorded(&estate, case, &asked, Some("ALERT")).await;
    assert_eq!(
        record["firewall_rule_group_id"].as_str(),
        Some(group_id.as_str()),
        "case {case:?}: expected the rule group of the rule that matched, got {record}"
    );
    assert_eq!(
        record["firewall_domain_list_id"].as_str(),
        Some(list_id.as_str()),
        "case {case:?}: expected the domain list the rule matched against, got {record}"
    );
    assert_eq!(
        record["rcode"].as_str(),
        Some("NOERROR"),
        "case {case:?}: an ALERT rule changes no answer, got {record}"
    );
}

/// The record the query logging configuration delivered for `name`, read out of its destination
/// log group — waiting for the RECORD to be there rather than for a span of the clock.
///
/// `firewall_rule_action` selects between the records of the same name where one is wanted: a
/// query no rule matched names no rule at all, and one a rule matched names its action.
async fn recorded(
    estate: &Estate,
    case: &str,
    name: &str,
    firewall_rule_action: Option<&str>,
) -> Value {
    let wanted = format!("{name}.");
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let filtered = logs(
            &estate.router,
            "FilterLogEvents",
            &json!({ "logGroupName": LOG_GROUP }),
        )
        .await;
        let events = filtered["events"].as_array().cloned().unwrap_or_default();
        let found = events.iter().rev().find_map(|event| {
            let message = event["message"].as_str()?;
            let record: Value = serde_json::from_str(message).ok()?;
            (record["query_name"].as_str() == Some(wanted.as_str())
                && record["firewall_rule_action"].as_str() == firewall_rule_action)
                .then_some(record)
        });
        if let Some(record) = found {
            return record;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected a record of the query for {wanted:?} \
             (firewall action {firewall_rule_action:?}) in {LOG_GROUP}, last read {filtered}"
        );
        tokio::time::sleep(LOOK).await;
    }
}
