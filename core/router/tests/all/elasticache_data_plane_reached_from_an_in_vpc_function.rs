// SPDX-License-Identifier: CC0-1.0

//! The CACHE ENDPOINT Amazon ElastiCache publishes, dialled by the only client that can
//! reach it: a Lambda function attached to the cache's own VPC.
//!
//! RULE: a data-plane address this emulator publishes is reachable by an in-process client of
//! this emulator at exactly the address the control plane reported for it, and what answers
//! there is that resource's own protocol handler over that resource's own state — so a
//! `python3.x` handler that reads `CacheCluster$CacheNodes[].Endpoint` or
//! `NodeGroup$PrimaryEndpoint` / `NodeGroup$ReaderEndpoint` out of ElastiCache and opens a
//! socket to it speaks RESP with that cache, in the clear or inside the TLS session a cache
//! with in-transit encryption is served on, and an address no plane of this emulator answers
//! at is answered the way a host with nothing listening there answers it. An ElastiCache
//! endpoint is VPC-only, so this is the ONLY vantage point its data plane is observable from:
//! a cache whose published address no in-process client can open a connection to has no
//! observable data plane at all.
//!
//! Three crates have to agree for that to hold — EC2 owns the VPC, the subnet and the
//! security group, ElastiCache opens the cache's socket and publishes its address, and Lambda
//! runs the handler that dials it — so the suite lives here.
//!
//! The rows differ along what the rule generalises over: WHICH kind of cache published the
//! address (a cache cluster's node endpoint, a replication group's primary endpoint, its
//! reader endpoint), whether the wire is encrypted, who the session is (unauthenticated,
//! holding the cache's auth token), which role the address carries (a writer, a read-only
//! replica), the ORDER the handler issues its writes in (chosen by the standard library's own
//! `random`, so the invariant is order-independent), and an address this emulator opened no
//! plane at.
//!
//! The handler module is pure standard library — it imports `random`, `socket`, `ssl` and
//! `struct` at module scope — so an invocation that answers at all is also a runtime whose
//! standard library those modules import from.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version an ec2Query form body names.
const EC2_API_VERSION: &str = "2016-11-15";

/// The ElastiCache API version an awsQuery form body names.
const ELASTICACHE_API_VERSION: &str = "2015-02-02";

/// The network the cache and the function both sit in.
const VPC_CIDR: &str = "10.42.0.0/16";
const SUBNET_CIDR: &str = "10.42.1.0/24";
const ZONE: &str = "us-west-2a";
const SECURITY_GROUP: &str = "xsvc-elasticache-invpc-group";

/// The port the caches of this suite accept connections on — the Valkey / Redis OSS client
/// port, which neither create names, so it is the engine's own default and the port the
/// inbound rule admitting the cache's clients names.
const CACHE_PORT: &str = "6379";

/// The cache resources every row addresses.
const SUBNET_GROUP: &str = "xsvc-ec-invpc-subnets";
const CLUSTER: &str = "xsvc-ec-invpc-cluster";
const REPLICATION_GROUP: &str = "xsvc-ec-invpc-rg";

/// The token the replication group is created with. `AuthToken` must be at least 16
/// characters, which is the only thing about this value that matters here.
const AUTH_TOKEN: &str = "a-token-of-at-least-sixteen-characters";

/// The stack, the execution role and the function that carry the probe.
const STACK: &str = "xsvc-ec-invpc-probe";
const EXEC_ROLE: &str = "xsvc-ec-invpc-exec";
const FUNCTION: &str = "xsvc-ec-invpc-probe-fn";

/// The `StackStatus` the probe stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status a cache that serves connections is in.
const AVAILABLE: &str = "available";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what the handler can reach and not how
/// fast an interpreter gets through one.
const FUNCTION_TIMEOUT: u64 = 120;

/// The memory the probe function is configured for, as its own `MemorySize`. No row here
/// tests a memory limit, so this limit is given generous room: the bound a run is held to is
/// the function's configured memory, and an interpreter standing up on a loaded host must not
/// be the thing that decides the outcome. 3008 MiB is one of the sizes a function is
/// ordinarily configured with, well inside the member's documented 128..=10240 range.
const FUNCTION_MEMORY: u64 = 3_008;

/// How often to look while waiting for work a service's own clock owes — a cache reaching
/// `available`, a stack completing, a function reaching `Active` — and how many looks to
/// take. Nothing here sleeps for a fixed span: every wait is for the CONDITION, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const LOOKS: usize = 2_400;

/// A port of this machine no cache of this emulator answers at, which is what the row for an
/// address with no plane behind it dials. The core hands its listeners ephemeral ports, so
/// this one is outside that range.
const UNSERVED_PORT: u64 = 1;

/// The handler every row runs.
///
/// Pure standard library: `random` chooses the order one row issues its writes in, `struct`
/// computes the value another row stores, `socket` opens the connection to the address the
/// event names and `ssl` secures it where the event asks for it. It speaks RESP itself — the
/// commands the event carries, in the order it chose — and reports every reply as it read it
/// off the wire.
const PROBE_HANDLER: &str = r#"
import random
import socket
import ssl
import struct

# The placeholder an argument carries when its value is the one the handler COMPUTES, so a row
# can assert on a value no part of its request wrote.
PACKED = "@packed"

# How long the handler waits on its own connection. Generous: what is under test is which
# address it reaches, never how fast a host answers, and the invocation's own Timeout bounds
# every wait besides.
WAIT = 60


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
    if marker == "*":
        count = int(body)
        if count < 0:
            return {"kind": "nil", "value": None}
        return {"kind": "array", "value": [_reply(reader) for _ in range(count)]}
    return {"kind": "unknown", "value": body}


def handler(event, context):
    packed = None
    if event.get("number") is not None:
        packed = struct.pack(">I", event["number"]).hex()
    try:
        opened = socket.create_connection((event["host"], event["port"]), WAIT)
    except OSError as refusal:
        return {"ok": False, "refused": str(refusal)}
    try:
        opened.settimeout(WAIT)
        version = None
        if event.get("tls"):
            secure = ssl.create_default_context()
            stream = secure.wrap_socket(opened, server_hostname=event["host"])
            version = stream.version()
        else:
            stream = opened
        reader = stream.makefile("rb")
        commands = [list(command) for command in event["commands"]]
        order = list(range(len(commands)))
        if event.get("shuffle"):
            random.Random(event["seed"]).shuffle(order)
        replies = [None] * len(commands)
        for at in order:
            arguments = [packed if word == PACKED else word for word in commands[at]]
            stream.sendall(_encode(arguments))
            replies[at] = _reply(reader)
        return {
            "ok": True,
            "version": version,
            "order": order,
            "packed": packed,
            "replies": replies,
        }
    finally:
        opened.close()
"#;

/// The emulator as it is ASSEMBLED, on one engine: the cache the handler dials is the one this
/// suite created, and the socket it opens is a connection to that cache's own RESP server.
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

/// One Amazon ElastiCache action.
async fn elasticache(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "elasticache", ELASTICACHE_API_VERSION, action, members).await
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
    subnet: String,
    group: String,
}

/// Create that network through EC2's own operations.
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
                ("GroupName", SECURITY_GROUP),
                ("GroupDescription", "the group of the cache and its clients"),
            ],
        )
        .await,
        "groupId",
    );
    // The inbound rule that admits the cache's clients: a cache placed in the account's VPC is
    // reached through an interface this group governs, and a connection no inbound rule of it
    // admits is dropped rather than served. The source is the group ITSELF — the group the
    // function's own interface carries too — which is how a client admits its own workloads to
    // its cache, and the port is the one the cache's engine accepts connections on.
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
    Network { vpc, subnet, group }
}
/// An `(address, port)` pair read out of the block `scope` of an answer.
fn endpoint_of(body: &str, scope: &str, what: &str) -> (String, u64) {
    let block = block_of(body, scope)
        .unwrap_or_else(|| panic!("setup: {what} must publish {scope}, got {body}"));
    let address = element(block, "Address");
    let port = element(block, "Port")
        .parse::<u64>()
        .unwrap_or_else(|_| panic!("setup: {what} must publish a numeric port, got {block}"));
    (address, port)
}

/// Create the cache subnet group, the cache cluster and the replication group, and answer the
/// three endpoints the rows dial once each cache reports itself `available`.
async fn plant_the_caches(
    router: &MultiServiceRouter,
    network: &Network,
) -> BTreeMap<String, (String, u64)> {
    let (status, body) = elasticache(
        router,
        "CreateCacheSubnetGroup",
        &[
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("CacheSubnetGroupDescription", "the cache's own subnets"),
            ("SubnetIds.SubnetIdentifier.1", network.subnet.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateCacheSubnetGroup must succeed, got {status} with {body}"
    );
    let (status, body) = elasticache(
        router,
        "CreateCacheCluster",
        &[
            ("CacheClusterId", CLUSTER),
            ("Engine", "redis"),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheNodes", "1"),
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("SecurityGroupIds.SecurityGroupId.1", network.group.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateCacheCluster must succeed, got {status} with {body}"
    );
    let (status, body) = elasticache(
        router,
        "CreateReplicationGroup",
        &[
            ("ReplicationGroupId", REPLICATION_GROUP),
            ("ReplicationGroupDescription", "a group behind a token"),
            ("Engine", "redis"),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheClusters", "2"),
            ("TransitEncryptionEnabled", "true"),
            ("AuthToken", AUTH_TOKEN),
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("SecurityGroupIds.SecurityGroupId.1", network.group.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateReplicationGroup must succeed, got {status} with {body}"
    );

    let mut published = BTreeMap::new();
    let mut last = String::new();
    for _ in 0..LOOKS {
        let (status, body) = elasticache(
            router,
            "DescribeCacheClusters",
            &[("CacheClusterId", CLUSTER), ("ShowCacheNodeInfo", "true")],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeCacheClusters must succeed, got {status} with {body}"
        );
        last = body;
        if opt_element(&last, "CacheClusterStatus").as_deref() == Some(AVAILABLE)
            && block_of(&last, "CacheNodes").is_some()
        {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        opt_element(&last, "CacheClusterStatus").as_deref(),
        Some(AVAILABLE),
        "setup: the cache cluster must reach {AVAILABLE}, last reported {last}"
    );
    let nodes = block_of(&last, "CacheNodes")
        .unwrap_or_else(|| panic!("setup: the cluster must report its node, got {last}"));
    published.insert(
        "cluster".to_string(),
        endpoint_of(nodes, "Endpoint", "the cache cluster's node"),
    );

    let mut last = String::new();
    for _ in 0..LOOKS {
        let (status, body) = elasticache(
            router,
            "DescribeReplicationGroups",
            &[("ReplicationGroupId", REPLICATION_GROUP)],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeReplicationGroups must succeed, got {status} with {body}"
        );
        last = body;
        if opt_element(&last, "Status").as_deref() == Some(AVAILABLE)
            && block_of(&last, "ReaderEndpoint").is_some()
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
    published.insert(
        "primary".to_string(),
        endpoint_of(&last, "PrimaryEndpoint", "the replication group's primary"),
    );
    published.insert(
        "reader".to_string(),
        endpoint_of(&last, "ReaderEndpoint", "the replication group's reader"),
    );
    published
}

/// The template the suite deploys: the execution role, and the probe function ATTACHED to the
/// cache's own VPC. The inline source is archived into the deployment package by
/// CloudFormation, which is how a function whose handler is written here comes to exist.
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

/// Invoke the probe with one row's event, and answer the report the handler returned.
async fn probe(router: &MultiServiceRouter, case: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        event.to_string().as_bytes(),
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
        "case {case:?}: the handler module must import and run to completion, got {raised:?} \
         with {payload}"
    );
    payload
}

/// One reply of a row, as the assertions name it: the RESP kind and its value.
fn reply(report: &Value, at: usize) -> (String, Value) {
    let held = report
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

/// A RESP reply's expected shape, as a row writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// A simple status (`+OK`).
    Status(&'static str),
    /// A bulk string carrying this text.
    Bulk(&'static str),
    /// A bulk string carrying the value the HANDLER computed, which no part of the request
    /// wrote.
    Packed,
    /// The nil reply a key the cache does not hold is answered with.
    Nil,
    /// An error whose CODE — the first word of the message, which is what a client branches on
    /// — is this.
    ErrorCode(&'static str),
}

/// Assert one reply of `report` against what the row expects of it.
fn assert_reply(case: &str, report: &Value, at: usize, expected: Expected) {
    let (kind, value) = reply(report, at);
    match expected {
        Expected::Status(text) => assert_eq!(
            (kind.as_str(), value.clone()),
            ("status", json!(text)),
            "case {case:?}: reply {at} must be the status {text:?}, got {kind} {value} in {report}"
        ),
        Expected::Bulk(text) => assert_eq!(
            (kind.as_str(), value.clone()),
            ("bulk", json!(text)),
            "case {case:?}: reply {at} must be the bulk string {text:?}, got {kind} {value} in \
             {report}"
        ),
        Expected::Packed => {
            let packed = report.get("packed").cloned().unwrap_or(Value::Null);
            assert_eq!(
                (kind.as_str(), value.clone()),
                ("bulk", packed.clone()),
                "case {case:?}: reply {at} must answer the value the handler packed ({packed}), \
                 got {kind} {value} in {report}"
            );
        }
        Expected::Nil => assert_eq!(
            kind.as_str(),
            "nil",
            "case {case:?}: reply {at} must be nil, got {kind} {value} in {report}"
        ),
        Expected::ErrorCode(code) => {
            let first = value
                .as_str()
                .unwrap_or_default()
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                (kind.as_str(), first.as_str()),
                ("error", code),
                "case {case:?}: reply {at} must be the error {code}, got {kind} {value} in \
                 {report}"
            );
        }
    }
}

/// The four keys the shuffled row writes, with the value each must read back.
const SHUFFLED: [(&str, &str); 4] = [
    ("shard:alpha", "first"),
    ("shard:beta", "second"),
    ("shard:gamma", "third"),
    ("shard:delta", "fourth"),
];

#[tokio::test]
async fn an_in_vpc_function_speaks_resp_with_the_cache_endpoint_elasticache_published() {
    let router = build_router();
    let network = ec2_network(&router).await;
    let published = plant_the_caches(&router, &network).await;
    let physical = deploy(&router, &network).await;
    assert_eq!(
        physical.get("Probe").map(String::as_str),
        Some(FUNCTION),
        "setup: the stack must report the probe function as {FUNCTION:?}, got {physical:?}"
    );
    let configuration = wait_until_active(&router).await;
    // The function really is attached to the cache's own VPC: the id reported for it is the
    // one EC2 holds for the subnet it was placed in, not a value the request carried.
    assert_eq!(
        configuration.pointer("/VpcConfig/VpcId").cloned(),
        Some(json!(network.vpc)),
        "setup: the probe must be attached to {:?}, got {configuration}",
        network.vpc
    );

    let (cluster_host, cluster_port) = published["cluster"].clone();
    let (primary_host, primary_port) = published["primary"].clone();
    let (reader_host, reader_port) = published["reader"].clone();

    // ---- A cache cluster's own node endpoint, in the clear: the handler reaches the RESP
    // server this emulator opened for that cluster, and the keyspace it writes is the one it
    // reads.
    let case = "a cache cluster's node endpoint, reached in the clear";
    let report = probe(
        &router,
        case,
        &json!({
            "host": cluster_host,
            "port": cluster_port,
            "tls": false,
            "commands": [
                ["PING"],
                ["GET", "never:written"],
                ["SET", "greeting", "hello from the vpc"],
                ["GET", "greeting"],
                ["EXISTS", "greeting"],
            ],
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the handler to reach the published endpoint, got {report}"
    );
    assert_eq!(
        report["version"], Value::Null,
        "case {case:?}: a cluster without in-transit encryption is served in the clear, got \
         {report}"
    );
    for (at, expected) in [
        (0, Expected::Status("PONG")),
        (1, Expected::Nil),
        (2, Expected::Status("OK")),
        (3, Expected::Bulk("hello from the vpc")),
    ] {
        assert_reply(case, &report, at, expected);
    }
    let (kind, value) = reply(&report, 4);
    assert_eq!(
        (kind.as_str(), value.clone()),
        ("integer", json!(1)),
        "case {case:?}: EXISTS must count the key the handler wrote, got {kind} {value}"
    );

    // ---- A value the HANDLER computed, stored and read back: nothing in the request carries
    // it, so what comes back is the cache's own answer and not an echo.
    let case = "a value the handler's own standard library computed";
    let report = probe(
        &router,
        case,
        &json!({
            "host": cluster_host,
            "port": cluster_port,
            "tls": false,
            "number": 1_234_567,
            "commands": [["SET", "packed", "@packed"], ["GET", "packed"]],
        }),
    )
    .await;
    assert_eq!(
        report["packed"], json!(format!("{:08x}", 1_234_567u32)),
        "case {case:?}: expected the handler to pack the number big-endian, got {report}"
    );
    assert_reply(case, &report, 0, Expected::Status("OK"));
    assert_reply(case, &report, 1, Expected::Packed);

    // ---- Writes issued in an order the handler CHOSE: whatever the order, every key reads
    // back its own value, which is the invariant a keyspace holds.
    let case = "four keys written in an order the handler chose";
    let writes: Vec<Value> = SHUFFLED
        .iter()
        .map(|(key, value)| json!(["SET", key, value]))
        .collect();
    let report = probe(
        &router,
        case,
        &json!({
            "host": cluster_host,
            "port": cluster_port,
            "tls": false,
            "shuffle": true,
            "seed": 20_261_105,
            "commands": writes,
        }),
    )
    .await;
    let mut order: Vec<u64> = report["order"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    order.sort_unstable();
    assert_eq!(
        order,
        (0..SHUFFLED.len() as u64).collect::<Vec<u64>>(),
        "case {case:?}: the handler must issue each write exactly once, got {report}"
    );
    for at in 0..SHUFFLED.len() {
        assert_reply(case, &report, at, Expected::Status("OK"));
    }
    let reads: Vec<Value> = SHUFFLED
        .iter()
        .map(|(key, _)| json!(["GET", key]))
        .collect();
    let report = probe(
        &router,
        case,
        &json!({
            "host": cluster_host,
            "port": cluster_port,
            "tls": false,
            "commands": reads,
        }),
    )
    .await;
    for (at, (_, value)) in SHUFFLED.iter().enumerate() {
        assert_reply(case, &report, at, Expected::Bulk(value));
    }

    // ---- A replication group's PRIMARY endpoint, over the TLS session a cache with
    // in-transit encryption is served on, behind the cache's own auth token.
    let case = "a replication group's primary endpoint, encrypted and behind its auth token";
    let report = probe(
        &router,
        case,
        &json!({
            "host": primary_host,
            "port": primary_port,
            "tls": true,
            "commands": [
                ["GET", "replicated"],
                ["AUTH", "not-the-token-at-all"],
                ["AUTH", AUTH_TOKEN],
                ["SET", "replicated", "written on the primary"],
                ["GET", "replicated"],
            ],
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the handler to reach the primary endpoint, got {report}"
    );
    let version = report["version"].as_str().unwrap_or_default();
    assert!(
        version.starts_with("TLSv1"),
        "case {case:?}: expected a negotiated TLS session, got {version:?} in {report}"
    );
    for (at, expected) in [
        (0, Expected::ErrorCode("NOAUTH")),
        (1, Expected::ErrorCode("WRONGPASS")),
        (2, Expected::Status("OK")),
        (3, Expected::Status("OK")),
        (4, Expected::Bulk("written on the primary")),
    ] {
        assert_reply(case, &report, at, expected);
    }

    // ---- The READER endpoint of the same group: one keyspace behind two addresses, and the
    // replica refuses a write because of the role the address it answers at carries.
    let case = "the same group's reader endpoint, which reads the primary's write and takes none";
    let report = probe(
        &router,
        case,
        &json!({
            "host": reader_host,
            "port": reader_port,
            "tls": true,
            "commands": [
                ["AUTH", AUTH_TOKEN],
                ["GET", "replicated"],
                ["SET", "replicated", "written on the replica"],
                ["GET", "replicated"],
            ],
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the handler to reach the reader endpoint, got {report}"
    );
    for (at, expected) in [
        (0, Expected::Status("OK")),
        (1, Expected::Bulk("written on the primary")),
        (2, Expected::ErrorCode("READONLY")),
        (3, Expected::Bulk("written on the primary")),
    ] {
        assert_reply(case, &report, at, expected);
    }

    // ---- And the counter-case: a port of this machine no cache answers at is answered the
    // way a host with nothing listening there answers it, so nothing above is a connection
    // that would have been opened to any address at all.
    let case = "a port of this machine no cache of this emulator answers at";
    let report = probe(
        &router,
        case,
        &json!({
            "host": cluster_host,
            "port": UNSERVED_PORT,
            "tls": false,
            "commands": [["PING"]],
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(false),
        "case {case:?}: expected no plane to be reached, got {report}"
    );
    let refused = report["refused"].as_str().unwrap_or_default();
    assert!(
        refused.contains("Connection refused"),
        "case {case:?}: expected the connection to be refused, got {refused:?} in {report}"
    );
}
