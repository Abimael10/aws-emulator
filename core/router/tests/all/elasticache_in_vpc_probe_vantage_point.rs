// SPDX-License-Identifier: CC0-1.0

//! The VANTAGE POINT an ElastiCache cache's data plane is observable from: a Lambda function
//! created with a `VpcConfig` naming subnets of the cache's own VPC.
//!
//! RULE: a function created with a `VpcConfig` runs INSIDE the VPC those subnets belong to, and
//! the managed `python3.x` runtime it runs gives its handler the whole standard library of the
//! release that runtime names — so, from one invocation,
//!
//! * traffic toward an address of that VPC (a cache node's own private address, any address of
//!   a subnet the function is attached to) leaves from the private address Amazon EC2 handed the
//!   interface Lambda created in the destination's subnet, which is an address inside that
//!   subnet's own block, while an address of another VPC's range has no route at all;
//! * that VPC's Amazon-provided resolver — at the base of the VPC's range plus two, and at the
//!   link-local address it also answers at — answers the queries the run sends it about the
//!   addresses of that VPC, and answers a name of its internal zone that names no address in it
//!   `NXDOMAIN`; and
//! * `import ssl` resolves, so the handler secures its connection to the in-transit-encrypted
//!   endpoint ElastiCache published for the cache in that VPC and speaks RESP with it.
//!
//! Each of those three is a signal a client uses to establish that it is measuring a PRIVATE
//! cache at all: an ElastiCache endpoint is reachable from inside its VPC and nowhere else, so a
//! probe that cannot show its own address is in that VPC, cannot reach that VPC's resolver, or
//! cannot import the module its TLS session needs, has measured nothing about the cache.
//!
//! Three crates have to agree for that to hold — Amazon EC2 owns the VPC, the subnets and every
//! interface in them, ElastiCache places the cache's node in one of those subnets and publishes
//! the address it serves RESP at, and Lambda runs the handler inside the attachment — so the
//! suite lives beside the router. Nothing reaches into any service's state: every value
//! asserted on was read back over the wire, and the addresses are the ones EC2's own inventory
//! holds rather than values this suite made up.
//!
//! The rows differ along what the rule generalises over: which subnet of the attachment the
//! destination sits in, whether the destination belongs to the VPC at all, which of the two
//! addresses the resolver is asked at, which record type is asked for, and whether the name asked
//! about names an address of the VPC.
//!
//! What this suite adds over the two beside it: the sibling router suite
//! (`elasticache_data_plane_reached_from_an_in_vpc_function.rs`) asserts what the cache ANSWERS at
//! the address it published, and the Lambda crate's own suite (`handler_in_vpc_network.rs`) asserts
//! the attachment against addresses of a VPC no cache is in. Neither asserts the three signals
//! TOGETHER against ONE cache, which is what a client establishes before it believes a measurement
//! of a private cache: that the run's own address is in the cache's VPC, that the destination it
//! asserts the route to is the cache node's own interface address as EC2's inventory holds it, that
//! the resolver of that VPC — and no route out of it — answers the run, and that the TLS session the
//! cache's endpoint is served on opens from there.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{
    block_contains, block_of, element, elements, form_encode, opt_element, physical_ids_of,
};

/// The Region every request here is signed for, and so the Region the resources belong to.
const REGION: &str = "us-west-2";

/// The EC2 API version an ec2Query form body names.
const EC2_API_VERSION: &str = "2016-11-15";

/// The ElastiCache API version an awsQuery form body names.
const ELASTICACHE_API_VERSION: &str = "2015-02-02";

/// The network the cache and the function share.
const VPC_CIDR: &str = "10.151.0.0/16";

/// The port the cache of this suite accepts connections on — the Valkey / Redis OSS client
/// port, which the create does not name, so it is the engine's own default and the port the
/// inbound rule admitting the cache's clients names.
const CACHE_PORT: &str = "6379";

/// The two subnets that network spans — one per Availability Zone, so the cache's node and the
/// function's interfaces are placed in subnets this suite knows the block of — each with its
/// zone and with ONE address of its own block that no resource here holds, which is what a route
/// row dials to assert the route rather than a resource's address. Every row checks that address
/// against the block beside it, so the pair cannot drift.
const SUBNETS: [Subnet; 2] = [
    Subnet {
        cidr: "10.151.0.0/24",
        zone: "us-west-2a",
        unheld: "10.151.0.200",
    },
    Subnet {
        cidr: "10.151.1.0/24",
        zone: "us-west-2b",
        unheld: "10.151.1.200",
    },
];

/// One subnet this suite creates; see [`SUBNETS`].
struct Subnet {
    cidr: &'static str,
    zone: &'static str,
    unheld: &'static str,
}

/// The address of that VPC's own resolver: "the reserved IP address at the base of the VPC
/// network range plus two" (`ModifyVpcAttributeRequest$EnableDnsSupport`).
const VPC_RESOLVER: &str = "10.151.0.2";

/// The LINK-LOCAL address the same resolver also answers at, named in that same sentence.
const LINK_LOCAL_RESOLVER: &str = "169.254.169.253";

/// An address of ANOTHER VPC's range, which the attachment has no route to — the counter-case
/// that makes the rows above an in-VPC vantage point rather than a host network.
const OUTSIDE_THE_VPC: &str = "10.152.0.5";

/// `ENETUNREACH`, the number a host with no route to an address answers with.
const ENETUNREACH: u64 = 101;

/// The private DNS domain of this Region: "AWS uses `ec2.internal` in `us-east-1` and
/// `<region>.compute.internal` in every other Region" (Amazon VPC User Guide, "DHCP option
/// sets").
const INTERNAL_DOMAIN: &str = "us-west-2.compute.internal";

/// The DNS record types a query probe asks for, as RFC 1035 numbers them.
const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;

/// The response codes the resolver answers with, as RFC 1035 §4.1.1 numbers them.
const RCODE_NOERROR: u64 = 0;
const RCODE_NXDOMAIN: u64 = 3;

/// `NetworkInterface$RequesterId` of an interface Amazon ElastiCache created and manages for a
/// cache node of the account, which is what a client filters its VPC's interfaces by to find
/// its caches.
const CACHE_REQUESTER: &str = "amazon-elasticache";

/// The `NetworkInterfaceType` a function's own interfaces are of, from the modelled EC2 enum's
/// `lambda` member.
const LAMBDA_INTERFACE_TYPE: &str = "lambda";

/// The cache resources this suite creates.
const SUBNET_GROUP: &str = "xsvc-ec-vantage-subnets";
const REPLICATION_GROUP: &str = "xsvc-ec-vantage-rg";

/// The token the replication group is created with. `AuthToken` must be at least 16 characters,
/// which is the only thing about this value that matters here.
const AUTH_TOKEN: &str = "a-token-of-at-least-sixteen-characters";

/// The stack, the execution role and the function that carry the probe.
const STACK: &str = "xsvc-ec-vantage-probe";
const EXEC_ROLE: &str = "xsvc-ec-vantage-exec";
const FUNCTION: &str = "xsvc-ec-vantage-probe-fn";

/// The `StackStatus` the probe stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status a cache that serves connections is in.
const AVAILABLE: &str = "available";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is what the handler can reach and not how fast a run
/// gets through one.
const FUNCTION_TIMEOUT: u64 = 120;

/// The memory the probe function is configured for, as its own `MemorySize`. No row here tests a
/// memory limit, so this limit is given generous room: an interpreter standing up on a loaded
/// host must not be the thing that decides the outcome. 3008 MiB is one of the sizes a function
/// is ordinarily configured with, well inside the member's documented 128..=10240 range.
const FUNCTION_MEMORY: u64 = 3_008;

/// How often to look while waiting for work a service's own clock owes — a cache reaching
/// `available`, a stack completing, a function reaching `Active` — and how many looks to take.
/// Nothing here sleeps for a fixed span: every wait is for the CONDITION, and the deadline is
/// generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const LOOKS: usize = 2_400;

/// The handler every row runs.
///
/// Pure standard library, with `socket` and `ssl` imported at MODULE scope: a runtime whose
/// library is missing either answers the invocation with an import failure instead of running
/// the handler at all. It answers one result per probe the event names — the address its traffic
/// toward an address leaves from, what the VPC's resolver replied to a query, or the RESP replies
/// a session with the cache endpoint read off the wire.
const PROBE_HANDLER: &str = r#"
import socket
import ssl

# How long the handler waits on its own socket. Generous: what is under test is which address it
# reaches, never how fast a host answers, and the invocation's own Timeout bounds every wait.
WAIT = 60


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
        if kind == 1 and length == 4:
            answered.append(".".join(str(octet) for octet in reply[start:pos]))
        elif kind == 12:
            answered.append(_read_name(reply, start))
    return answered


def _source(host, port):
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect((host, port))
        bound = sock.getsockname()
        return {"source": bound[0], "port": bound[1]}
    except OSError as failed:
        return {"source": None, "errno": failed.errno, "error": str(failed)}
    finally:
        sock.close()


def _resolved(resolver, name, kind, qid):
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
        "answers": _answers(reply),
        "peer": list(peer),
    }


def _encode(arguments):
    parts = ["*" + str(len(arguments)) + "\r\n"]
    for argument in arguments:
        parts.append("$" + str(len(argument)) + "\r\n" + argument + "\r\n")
    return "".join(parts).encode("utf-8")


def _reply(reader):
    line = reader.readline()
    if not line:
        return {"kind": "eof", "value": None}
    text = line.decode("utf-8")
    marker, body = text[0], text[1:].rstrip("\r\n")
    if marker == "+":
        return {"kind": "status", "value": body}
    if marker == "-":
        return {"kind": "error", "value": body}
    if marker == ":":
        return {"kind": "integer", "value": int(body)}
    if marker == "$":
        if int(body) < 0:
            return {"kind": "nil", "value": None}
        return {"kind": "bulk", "value": reader.readline().decode("utf-8").rstrip("\r\n")}
    return {"kind": "unknown", "value": body}


def _session(probe):
    try:
        opened = socket.create_connection((probe["host"], probe["port"]), WAIT)
    except OSError as refusal:
        return {"ok": False, "refused": str(refusal)}
    try:
        opened.settimeout(WAIT)
        secure = ssl.create_default_context()
        stream = secure.wrap_socket(opened, server_hostname=probe["host"])
        reader = stream.makefile("rb")
        replies = []
        for command in probe["commands"]:
            stream.sendall(_encode(list(command)))
            replies.append(_reply(reader))
        return {"ok": True, "version": stream.version(), "replies": replies}
    finally:
        opened.close()


def handler(event, context):
    results = []
    for index, probe in enumerate(event["probes"]):
        if probe["kind"] == "source":
            results.append(_source(probe["host"], int(probe["port"])))
        elif probe["kind"] == "query":
            results.append(
                _resolved(probe["resolver"], probe["name"], int(probe["type"]), 0x500 + index)
            )
        else:
            results.append(_session(probe))
    return {"results": results}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the cache the handler reaches is the one this
/// suite created, and the VPC it observes is the one EC2 minted for it.
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

/// One form-encoded call of `service`, answered with the HTTP status and the XML body.
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

/// One EC2 action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = form_call(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {body}"
    );
    body
}

/// One Amazon ElastiCache action that must succeed, answered with its XML body.
async fn elasticache(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) =
        form_call(router, "elasticache", ELASTICACHE_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "setup: ElastiCache {action} must succeed, got {status} with {body}"
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

/// The network the cache and the function share, minted by EC2 itself.
struct Network {
    vpc: String,
    /// Each subnet's id with the block it was created with, in the order they were created.
    subnets: Vec<(String, String)>,
    group: String,
}

/// Create that network through EC2's own operations.
async fn ec2_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for subnet in &SUBNETS {
        let created = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", subnet.cidr),
                ("AvailabilityZone", subnet.zone),
            ],
        )
        .await;
        subnets.push((element(&created, "subnetId"), subnet.cidr.to_string()));
    }
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("VpcId", vpc.as_str()),
                ("GroupName", "xsvc-ec-vantage-group"),
                ("GroupDescription", "the group of the cache and its clients"),
            ],
        )
        .await,
        "groupId",
    );
    // The inbound rule that admits the cache's clients: a cache placed in the account's VPC is
    // reached through an interface this group governs, and a connection no inbound rule of it
    // admits is dropped rather than served. The source is the group ITSELF, which the
    // function's own interface carries too — how a client admits its own workloads to its
    // cache — and the port is the one the cache's engine accepts connections on.
    ec2(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", group.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", CACHE_PORT),
            ("IpPermissions.1.ToPort", CACHE_PORT),
            ("IpPermissions.1.Groups.1.GroupId", group.as_str()),
        ],
    )
    .await;
    Network {
        vpc,
        subnets,
        group,
    }
}

/// One interface of the account's inventory, as a client reads one it found through a filter.
#[derive(Debug, Clone)]
struct Interface {
    subnet: String,
    address: String,
    description: String,
}

/// Every interface EC2 reports in `vpc` that matches one more filter, each read back through the
/// describe that reports ONE interface.
///
/// Read one at a time on purpose: an ec2Query answer nests `<item>` lists inside each interface
/// (its groups, its addresses), so a reader that split the whole answer would tear one interface
/// into several.
async fn interfaces_in(
    router: &MultiServiceRouter,
    vpc: &str,
    filter: (&str, &str),
) -> Vec<Interface> {
    let listed = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc),
            ("Filter.2.Name", filter.0),
            ("Filter.2.Value.1", filter.1),
        ],
    )
    .await;
    let mut described = Vec::new();
    for id in elements(&listed, "networkInterfaceId") {
        let one = ec2(
            router,
            "DescribeNetworkInterfaces",
            &[("NetworkInterfaceId.1", id.as_str())],
        )
        .await;
        described.push(Interface {
            subnet: opt_element(&one, "subnetId").unwrap_or_default(),
            address: opt_element(&one, "privateIpAddress").unwrap_or_default(),
            description: opt_element(&one, "description").unwrap_or_default(),
        });
    }
    described
}

/// Create the cache subnet group and the in-transit-encrypted replication group, and answer the
/// primary endpoint it publishes once it reports itself `available`.
async fn plant_the_cache(router: &MultiServiceRouter, network: &Network) -> (String, u64) {
    let mut members: Vec<(&str, &str)> = vec![
        ("CacheSubnetGroupName", SUBNET_GROUP),
        ("CacheSubnetGroupDescription", "the cache's own subnets"),
    ];
    let subnet_keys = ["SubnetIds.SubnetIdentifier.1", "SubnetIds.SubnetIdentifier.2"];
    for (key, (subnet, _)) in subnet_keys.iter().zip(network.subnets.iter()) {
        members.push((key, subnet.as_str()));
    }
    elasticache(router, "CreateCacheSubnetGroup", &members).await;
    elasticache(
        router,
        "CreateReplicationGroup",
        &[
            ("ReplicationGroupId", REPLICATION_GROUP),
            ("ReplicationGroupDescription", "a cache behind a token"),
            ("Engine", "redis"),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheClusters", "1"),
            ("TransitEncryptionEnabled", "true"),
            ("AuthToken", AUTH_TOKEN),
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("SecurityGroupIds.SecurityGroupId.1", network.group.as_str()),
        ],
    )
    .await;

    let mut last = String::new();
    for _ in 0..LOOKS {
        last = elasticache(
            router,
            "DescribeReplicationGroups",
            &[("ReplicationGroupId", REPLICATION_GROUP)],
        )
        .await;
        if opt_element(&last, "Status").as_deref() == Some(AVAILABLE)
            && block_of(&last, "PrimaryEndpoint").is_some()
        {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        opt_element(&last, "Status").as_deref(),
        Some(AVAILABLE),
        "setup: the replication group must reach {AVAILABLE}, last reported {last}"
    );
    let endpoint = block_of(&last, "PrimaryEndpoint")
        .unwrap_or_else(|| panic!("setup: the group must publish its primary, got {last}"));
    let port = element(endpoint, "Port").parse::<u64>().unwrap_or_else(|_| {
        panic!("setup: the primary must publish a numeric port, got {endpoint}")
    });
    (element(endpoint, "Address"), port)
}

/// The template the suite deploys: the execution role, and the probe function ATTACHED to both
/// subnets of the cache's own VPC. The inline source is archived into the deployment package by
/// CloudFormation, which is how a function whose handler is written here comes to exist.
fn stack_template(network: &Network) -> String {
    let subnets: Vec<&str> = network
        .subnets
        .iter()
        .map(|(subnet, _)| subnet.as_str())
        .collect();
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
                        "SubnetIds": subnets,
                        "SecurityGroupIds": [network.group],
                    },
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy the stack, wait for `CREATE_COMPLETE` and answer the physical id of each resource.
async fn deploy(router: &MultiServiceRouter, network: &Network) -> BTreeMap<String, String> {
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
    let mut reported = String::new();
    let mut described = String::new();
    for _ in 0..LOOKS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({STACK:?}) must succeed, got {status} with {body}"
        );
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        described = body;
        if !reported.ends_with("_IN_PROGRESS") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );
    let (status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", STACK)]).await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// Wait until Lambda reports the probe function `Active`, and answer its configuration.
async fn wait_until_active(router: &MultiServiceRouter) -> Value {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return configuration;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}")
}

/// Invoke the probe with these probes, and answer the one result it reported for each.
///
/// The handler imports `socket` and `ssl` at module scope, so an invocation that answers without
/// a function error is also a runtime whose standard library supplies both.
async fn probed(router: &MultiServiceRouter, case: &str, probes: &[Value]) -> Vec<Value> {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        json!({ "probes": probes }).to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    let raised = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        raised, None,
        "case {case:?}: the handler module must import and run to completion, got {raised:?} with \
         {payload}"
    );
    let results = payload
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        results.len(),
        probes.len(),
        "case {case:?}: expected one result per probe, got {payload}"
    );
    results
}

/// A probe asking which address a datagram toward `host` leaves from.
fn source_probe(host: &str) -> Value {
    json!({ "kind": "source", "host": host, "port": 53 })
}

/// A probe asking `resolver` about `name`, of record type `kind`.
fn query_probe(resolver: &str, name: &str, kind: u16) -> Value {
    json!({ "kind": "query", "resolver": resolver, "name": name, "type": kind })
}

/// The hostname form of an IPv4 address inside a VPC: the address with its dots replaced by
/// dashes, under this Region's internal domain.
fn hostname_of(address: &str) -> String {
    format!("ip-{}.{INTERNAL_DOMAIN}", address.replace('.', "-"))
}

/// The reverse name of an IPv4 address: its octets in reverse under `in-addr.arpa`.
fn reverse_of(address: &str) -> String {
    let mut octets: Vec<&str> = address.split('.').collect();
    octets.reverse();
    format!("{}.in-addr.arpa", octets.join("."))
}

/// The answers one query result carries, as the assertions name them.
fn answers_of(result: &Value) -> Vec<String> {
    result
        .get("answers")
        .and_then(Value::as_array)
        .map(|answers| {
            answers
                .iter()
                .map(|answer| answer.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One reply of a session result: the RESP kind and its value.
fn reply(result: &Value, at: usize) -> (String, Value) {
    let held = result
        .pointer(&format!("/replies/{at}"))
        .cloned()
        .unwrap_or(Value::Null);
    (
        held.get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        held.get("value").cloned().unwrap_or(Value::Null),
    )
}

/// RULE: see this suite's module documentation.
#[tokio::test]
async fn an_in_vpc_function_observes_the_vpc_its_cache_is_placed_in() {
    let router = build_router();
    let network = ec2_network(&router).await;
    let (cache_host, cache_port) = plant_the_cache(&router, &network).await;

    // The cache really is in the account's VPC: ElastiCache placed its node's interface in one of
    // the subnet group's subnets, and EC2's own inventory holds the address it carries.
    let cache_interfaces =
        interfaces_in(&router, &network.vpc, ("requester-id", CACHE_REQUESTER)).await;
    assert_eq!(
        cache_interfaces.len(),
        1,
        "setup: the one-node cache must hold one interface in the VPC, got {cache_interfaces:?}"
    );
    let cache_node = cache_interfaces[0].clone();
    let cache_subnet_cidr = network
        .subnets
        .iter()
        .find(|(subnet, _)| *subnet == cache_node.subnet)
        .map(|(_, cidr)| cidr.clone())
        .unwrap_or_else(|| {
            panic!("setup: the cache node must sit in a subnet of the group, got {cache_node:?}")
        });
    assert!(
        block_contains(&cache_subnet_cidr, &cache_node.address),
        "setup: the cache node's address {} must lie in {cache_subnet_cidr}, got {cache_node:?}",
        cache_node.address
    );

    let physical = deploy(&router, &network).await;
    assert_eq!(
        physical.get("Probe").map(String::as_str),
        Some(FUNCTION),
        "setup: the stack must report the probe function as {FUNCTION:?}, got {physical:?}"
    );
    let configuration = wait_until_active(&router).await;
    assert_eq!(
        configuration.pointer("/VpcConfig/VpcId").cloned(),
        Some(json!(network.vpc)),
        "setup: the probe must be attached to {:?}, got {configuration}",
        network.vpc
    );

    // The attachment as EC2 holds it: one interface of the function per attached subnet, each
    // with the address that subnet lent it.
    let own = interfaces_in(
        &router,
        &network.vpc,
        ("interface-type", LAMBDA_INTERFACE_TYPE),
    )
    .await;
    let description = format!("AWS Lambda VPC ENI-{FUNCTION}");
    let mine: Vec<Interface> = own
        .into_iter()
        .filter(|interface| interface.description == description)
        .collect();
    assert_eq!(
        mine.len(),
        network.subnets.len(),
        "setup: expected one interface per attached subnet, got {mine:?}"
    );
    let by_subnet: BTreeMap<String, String> = mine
        .iter()
        .map(|interface| (interface.subnet.clone(), interface.address.clone()))
        .collect();
    let in_cache_subnet = by_subnet
        .get(&cache_node.subnet)
        .cloned()
        .unwrap_or_else(|| panic!("setup: the probe must hold an interface beside the cache node"));
    let first_subnet = network.subnets[0].0.clone();
    let in_first_subnet = by_subnet
        .get(&first_subnet)
        .cloned()
        .unwrap_or_else(|| panic!("setup: the probe must hold an interface in its first subnet"));

    // ---- Which address the run's traffic LEAVES from. Each row: the destination, and the
    // interface the route must resolve through (`None` for a destination with no route at all).
    // One address of each attached subnet that no resource of this suite holds, so what such a
    // row asserts is the ROUTE and not a resource's own address ([`SUBNETS`]).
    let elsewhere: Vec<(String, &str)> = network
        .subnets
        .iter()
        .zip(SUBNETS.iter())
        .map(|((subnet, cidr), held)| {
            assert!(
                block_contains(cidr, held.unheld),
                "setup: {} must lie inside the subnet block {cidr} it is written beside",
                held.unheld
            );
            (subnet.clone(), held.unheld)
        })
        .collect();
    let mut rows: Vec<(String, String, Option<String>)> = vec![
        (
            "the cache node's own address in the VPC".to_string(),
            cache_node.address.clone(),
            Some(in_cache_subnet.clone()),
        ),
        (
            "the VPC's own resolver, which sits in the first attached subnet".to_string(),
            VPC_RESOLVER.to_string(),
            Some(in_first_subnet.clone()),
        ),
        (
            "the link-local address that resolver also answers at".to_string(),
            LINK_LOCAL_RESOLVER.to_string(),
            Some(in_first_subnet.clone()),
        ),
        (
            "an address of another VPC's range".to_string(),
            OUTSIDE_THE_VPC.to_string(),
            None,
        ),
    ];
    for (subnet, address) in &elsewhere {
        rows.push((
            format!("an address of the attached subnet {subnet}"),
            (*address).to_string(),
            by_subnet.get(subnet).cloned(),
        ));
    }
    let probes: Vec<Value> = rows
        .iter()
        .map(|(_case, destination, _expected)| source_probe(destination))
        .collect();
    let results = probed(&router, "the addresses the run's traffic leaves from", &probes).await;
    for ((case, destination, expected), result) in rows.iter().zip(results) {
        match expected {
            Some(address) => {
                assert_eq!(
                    result.get("source").and_then(Value::as_str),
                    Some(address.as_str()),
                    "case {case:?}: expected traffic toward {destination} to leave from \
                     {address}, got {result}"
                );
                assert!(
                    block_contains(VPC_CIDR, address),
                    "case {case:?}: expected {address} to lie inside the VPC's own range \
                     {VPC_CIDR}, got {result}"
                );
            }
            None => assert_eq!(
                result.get("errno").and_then(Value::as_u64),
                Some(ENETUNREACH),
                "case {case:?}: expected errno {ENETUNREACH} toward {destination}, got {result}"
            ),
        }
    }

    // ---- What the VPC's own resolver answers the run. Each row: the name, its record type, the
    // response code it must be answered with and the records it must carry — asked at BOTH
    // addresses that resolver answers at.
    let node_hostname = hostname_of(&cache_node.address);
    let query_rows: Vec<(&str, String, u16, u64, Vec<String>)> = vec![
        (
            "the hostname of the cache node's address",
            node_hostname.clone(),
            TYPE_A,
            RCODE_NOERROR,
            vec![cache_node.address.clone()],
        ),
        (
            "a reverse question about that address",
            reverse_of(&cache_node.address),
            TYPE_PTR,
            RCODE_NOERROR,
            vec![format!("{node_hostname}.")],
        ),
        (
            "the hostname of this run's own address",
            hostname_of(&in_first_subnet),
            TYPE_A,
            RCODE_NOERROR,
            vec![in_first_subnet.clone()],
        ),
        (
            "a name of the internal zone that names no address of the VPC",
            format!("cache.{INTERNAL_DOMAIN}"),
            TYPE_A,
            RCODE_NXDOMAIN,
            Vec::new(),
        ),
    ];
    for resolver in [VPC_RESOLVER, LINK_LOCAL_RESOLVER] {
        let probes: Vec<Value> = query_rows
            .iter()
            .map(|(_case, name, kind, _, _)| query_probe(resolver, name, *kind))
            .collect();
        let results = probed(&router, "the queries the VPC's resolver answers", &probes).await;
        for ((case, name, _kind, code, answers), result) in query_rows.iter().zip(results) {
            assert_eq!(
                result.get("rcode").and_then(Value::as_u64),
                Some(*code),
                "case {case:?} at {resolver}: expected response code {code} for {name}, got \
                 {result}"
            );
            assert_eq!(
                &answers_of(&result),
                answers,
                "case {case:?} at {resolver}: expected the answers {answers:?} for {name}, got \
                 {result}"
            );
            let peer = result
                .get("peer")
                .and_then(Value::as_array)
                .and_then(|peer| peer.first())
                .and_then(Value::as_str);
            assert_eq!(
                peer,
                Some(resolver),
                "case {case:?} at {resolver}: expected the reply to come from the address the \
                 query was sent to, got {result}"
            );
        }
    }

    // ---- And the cache itself, from that vantage point: the handler's own `ssl` secures the
    // connection to the address the control plane published for an in-transit-encrypted cache,
    // and the cache behind it answers RESP.
    let case = "the in-transit-encrypted endpoint the control plane published";
    let results = probed(
        &router,
        case,
        &[json!({
            "kind": "session",
            "host": cache_host,
            "port": cache_port,
            "commands": [
                ["AUTH", AUTH_TOKEN],
                ["SET", "vantage:point", "written from inside the vpc"],
                ["GET", "vantage:point"],
            ],
        })],
    )
    .await;
    let session = results[0].clone();
    assert_eq!(
        session["ok"],
        json!(true),
        "case {case:?}: expected the handler to reach {cache_host}:{cache_port}, got {session}"
    );
    let version = session["version"].as_str().unwrap_or_default();
    assert!(
        version.starts_with("TLSv1"),
        "case {case:?}: expected a negotiated TLS session, got {version:?} in {session}"
    );
    for (at, expected) in [
        (0, ("status", json!("OK"))),
        (1, ("status", json!("OK"))),
        (2, ("bulk", json!("written from inside the vpc"))),
    ] {
        let (kind, value) = reply(&session, at);
        assert_eq!(
            (kind.as_str(), value.clone()),
            expected,
            "case {case:?}: reply {at} must be {expected:?}, got {kind} {value} in {session}"
        );
    }
}
