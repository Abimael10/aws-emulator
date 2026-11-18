// SPDX-License-Identifier: CC0-1.0

//! The BOOTSTRAP ADDRESS Amazon MSK publishes, dialled by the only client that can reach it: a
//! Lambda function attached to the cluster's own VPC.
//!
//! RULE: a data-plane address this emulator publishes is reachable by an in-process client of
//! this emulator at exactly the address the control plane reported for it, and what answers
//! there is that resource's own protocol handler over that resource's own state — so a
//! `python3.x` handler that reads `GetBootstrapBrokers`'s `BootstrapBrokerString` and opens a
//! socket to it speaks the Apache Kafka wire protocol with that cluster's brokers, and an
//! address this emulator opened no plane at is answered the way a host with nothing listening
//! there answers it. "Amazon MSK brokers are only accessible from within the cluster's VPC", so
//! this is the ONLY vantage point an MSK data plane is observable from: a cluster whose
//! published address no in-process client can open a connection to has no observable data
//! plane at all.
//!
//! The same function also proves it really is INSIDE that VPC before it measures anything, the
//! way a client carrying its measurement into a VPC does: a route lookup toward the VPC's own
//! DNS server — "the base of your VPC network range plus two" — leaves from the private address
//! of the interface Lambda stood up for the function in that VPC, and the resolver answers the
//! queries it sends there.
//!
//! Three crates have to agree for that to hold — EC2 owns the VPC, the subnets and the security
//! group, MSK opens the cluster's brokers and publishes their address, and Lambda runs the
//! handler that dials it — so the suite lives here.
//!
//! The rows differ along what the rule generalises over: which published member names the
//! address (the plaintext string, and the TLS one a client reaches over TLS), what the handler
//! asks of the broker (the APIs it serves, the brokers it reports), what it asks of the VPC
//! resolver (its own interface's hostname, a name of another zone), and the addresses that
//! answer nothing (a port of the same host nothing was opened at, an address outside the VPC).

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version an ec2Query form body names.
const EC2_API_VERSION: &str = "2016-11-15";

/// The network the cluster and the function both sit in.
const VPC_CIDR: &str = "10.44.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [("10.44.1.0/24", "us-west-2a"), ("10.44.2.0/24", "us-west-2b")];
const SECURITY_GROUP: &str = "xsvc-msk-invpc-group";

/// The address of the VPC's own DNS server: "the Amazon DNS server … is located at the base of
/// your VPC network range plus two" (Amazon VPC User Guide, "Amazon DNS server").
const VPC_RESOLVER: &str = "10.44.0.2";

/// The private DNS domain of [`REGION`]: `<region>.compute.internal` outside `us-east-1`
/// (Amazon VPC User Guide, "DHCP option sets").
const INTERNAL_DOMAIN: &str = "us-west-2.compute.internal";

/// The cluster every row dials, and the Apache Kafka version and broker type it runs.
const CLUSTER: &str = "xsvc-msk-invpc-cluster";
const KAFKA_VERSION: &str = "3.6.0";
const INSTANCE_TYPE: &str = "kafka.m5.large";

/// The stack, the execution role and the function that carry the probe.
const STACK: &str = "xsvc-msk-invpc-probe";
const EXEC_ROLE: &str = "xsvc-msk-invpc-exec";
const FUNCTION: &str = "xsvc-msk-invpc-probe-fn";

/// The `StackStatus` the probe stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what the handler can reach and not how fast
/// an interpreter gets through one.
const FUNCTION_TIMEOUT: u64 = 120;

/// The memory the probe function is configured for. No row here tests a memory limit, so the
/// limit is given generous room: an interpreter standing up on a loaded host must not be the
/// thing that decides the outcome.
const FUNCTION_MEMORY: u64 = 3_008;

/// How often to look while waiting for work a service's own clock owes — a cluster reaching
/// `ACTIVE`, a stack completing, a function reaching `Active` — and how many looks to take.
/// Nothing here sleeps for a fixed span: every wait is for the CONDITION, and the deadline is
/// generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const LOOKS: usize = 2_400;

/// A port of this machine no plane of this emulator answers at, which the row for an address
/// with nothing behind it dials. The core hands its listeners ephemeral ports, so this one is
/// outside that range.
const UNSERVED_PORT: u64 = 1;

/// The handler every row runs.
///
/// Pure standard library: `socket` opens the connection to the address the event names, and the
/// handler frames the Apache Kafka requests and the DNS queries itself — so what a row asserts
/// is what the broker and the resolver really answered on the wire.
const PROBE_HANDLER: &str = r#"
import socket

# The API keys the handler asks about, as the Apache Kafka protocol numbers them.
API_VERSIONS = 18
METADATA = 3

# The client this handler identifies itself to the broker as.
CLIENT_ID = b"in-vpc-probe"

# How long the handler waits on its own connection. Generous: what is under test is which
# address it reaches, never how fast a host answers, and the invocation's own Timeout bounds
# every wait besides.
WAIT = 60


def _request_header(api_key, api_version, correlation):
    return (
        api_key.to_bytes(2, "big")
        + api_version.to_bytes(2, "big")
        + correlation.to_bytes(4, "big")
        + len(CLIENT_ID).to_bytes(2, "big")
        + CLIENT_ID
    )


def _read_exact(stream, count):
    taken = b""
    while len(taken) < count:
        chunk = stream.recv(count - len(taken))
        if not chunk:
            raise OSError("the broker closed the connection")
        taken += chunk
    return taken


def _call(stream, api_key, api_version, correlation, body=b""):
    frame = _request_header(api_key, api_version, correlation) + body
    stream.sendall(len(frame).to_bytes(4, "big") + frame)
    length = int.from_bytes(_read_exact(stream, 4), "big")
    return _read_exact(stream, length)


def _string(data, at):
    length = int.from_bytes(data[at : at + 2], "big", signed=True)
    if length < 0:
        return (None, at + 2)
    return (data[at + 2 : at + 2 + length].decode("utf-8"), at + 2 + length)


def _api_versions(stream, correlation):
    reply = _call(stream, API_VERSIONS, 0, correlation)
    return {
        "correlation": int.from_bytes(reply[0:4], "big"),
        "error_code": int.from_bytes(reply[4:6], "big", signed=True),
        "api_key_count": int.from_bytes(reply[6:10], "big", signed=True),
        "serves_metadata": any(
            int.from_bytes(reply[10 + index * 6 : 12 + index * 6], "big", signed=True) == METADATA
            for index in range(int.from_bytes(reply[6:10], "big", signed=True))
        ),
    }


def _metadata(stream, correlation):
    # Every topic, which version 1 asks for with a null topic array.
    reply = _call(stream, METADATA, 1, correlation, (-1).to_bytes(4, "big", signed=True))
    at = 4
    count = int.from_bytes(reply[at : at + 4], "big", signed=True)
    at += 4
    brokers = []
    for _ in range(count):
        node = int.from_bytes(reply[at : at + 4], "big", signed=True)
        at += 4
        host, at = _string(reply, at)
        port = int.from_bytes(reply[at : at + 4], "big", signed=True)
        at += 4
        _rack, at = _string(reply, at)
        brokers.append({"node": node, "host": host, "port": port})
    return {"correlation": int.from_bytes(reply[0:4], "big"), "brokers": brokers}


def _broker_session(host, port, tls):
    opened = socket.create_connection((host, port), WAIT)
    try:
        opened.settimeout(WAIT)
        stream = opened
        version = None
        if tls:
            import ssl

            secure = ssl.create_default_context()
            stream = secure.wrap_socket(opened, server_hostname=host)
            version = stream.version()
        answered = {"tls_version": version}
        answered["api_versions"] = _api_versions(stream, 11)
        answered["metadata"] = _metadata(stream, 12)
        return answered
    finally:
        opened.close()


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


def _skip_name(data, at):
    """The offset just past the name at `at`: a run of labels, or the two bytes of a pointer
    to a name earlier in the message."""
    while True:
        length = data[at]
        if length == 0:
            return at + 1
        if length & 0xC0:
            return at + 2
        at += 1 + length


def _resolved(resolver, name):
    query = (
        (0x2A2A).to_bytes(2, "big")
        + b"\x01\x00"
        + b"\x00\x01"
        + b"\x00\x00" * 3
        + _name_bytes(name)
        + b"\x00\x01"
        + b"\x00\x01"
    )
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(WAIT)
    try:
        sock.sendto(query, (resolver, 53))
        reply, peer = sock.recvfrom(512)
    finally:
        sock.close()
    answers = []
    at = 12
    for _ in range(int.from_bytes(reply[4:6], "big")):
        at = _skip_name(reply, at) + 4
    for _ in range(int.from_bytes(reply[6:8], "big")):
        at = _skip_name(reply, at)
        kind = int.from_bytes(reply[at : at + 2], "big")
        length = int.from_bytes(reply[at + 8 : at + 10], "big")
        start = at + 10
        at = start + length
        if kind == 1 and length == 4:
            answers.append(".".join(str(octet) for octet in reply[start:at]))
    return {
        "rcode": reply[3] & 0x0F,
        "answers": answers,
        "resolver": peer[0],
    }


def handler(event, context):
    answered = {}
    if event.get("resolver"):
        try:
            answered["source"] = _source_toward(event["resolver"])
        except OSError as failed:
            answered["source_error"] = str(failed)
    for name in event.get("names") or []:
        try:
            answered.setdefault("resolved", {})[name] = _resolved(event["resolver"], name)
        except OSError as failed:
            answered.setdefault("resolved", {})[name] = {"error": str(failed)}
    if event.get("broker"):
        host, _, port = event["broker"].rpartition(":")
        try:
            answered["broker"] = _broker_session(host, int(port), bool(event.get("tls")))
        except OSError as failed:
            answered["broker_error"] = str(failed)
    return answered
"#;

/// The emulator as it is ASSEMBLED, on one engine: the cluster the handler dials is the one this
/// suite created, and the socket it opens is a connection to that cluster's own brokers.
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

/// One CloudFormation action.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    form_call(router, "cloudformation", "2010-05-15", action, members).await
}

/// One Amazon MSK `restJson1` call, as its status and its parsed body.
async fn kafka(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| value.to_string().into_bytes())
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("kafka.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("kafka"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
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

/// The network the cluster and the function share, minted by EC2 itself.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
}

/// Create that network through EC2's own operations.
async fn ec2_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (cidr, zone) in SUBNET_CIDRS {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("VpcId", vpc.as_str()),
                ("GroupName", SECURITY_GROUP),
                ("GroupDescription", "the group of the cluster and its clients"),
            ],
        )
        .await,
        "groupId",
    );
    Network {
        vpc,
        subnets,
        group,
    }
}

/// Create the cluster in that network, wait until MSK reports it `ACTIVE`, and answer every
/// bootstrap string it publishes, member by member.
async fn plant_the_cluster(
    router: &MultiServiceRouter,
    network: &Network,
) -> BTreeMap<String, String> {
    let (status, created) = kafka(
        router,
        "POST",
        "/v1/clusters",
        Some(json!({
            "clusterName": CLUSTER,
            "kafkaVersion": KAFKA_VERSION,
            "numberOfBrokerNodes": network.subnets.len(),
            "brokerNodeGroupInfo": {
                "instanceType": INSTANCE_TYPE,
                "clientSubnets": network.subnets,
                "securityGroups": [network.group],
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateCluster must succeed, got {status} with {created}"
    );
    let arn = created
        .get("clusterArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: the create must report a clusterArn, got {created}"))
        .to_string();
    let uri = format!("/v1/clusters/{}", form_encode(&arn));
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (status, described) = kafka(router, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "setup: DescribeCluster must succeed, got {status} with {described}"
        );
        last = described;
        if last.pointer("/clusterInfo/state").and_then(Value::as_str) == Some("ACTIVE") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        last.pointer("/clusterInfo/state").and_then(Value::as_str),
        Some("ACTIVE"),
        "setup: the cluster must reach ACTIVE, last reported {last}"
    );
    let (status, published) = kafka(
        router,
        "GET",
        &format!("/v1/clusters/{}/bootstrap-brokers", form_encode(&arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetBootstrapBrokers must succeed, got {status} with {published}"
    );
    published
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(member, value)| value.as_str().map(|address| (member, address.to_string())))
        .collect()
}

/// The template the suite deploys: the execution role, and the probe function ATTACHED to the
/// cluster's own VPC. The inline source is archived into the deployment package by
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
                        "SubnetIds": network.subnets,
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

#[tokio::test]
async fn an_in_vpc_function_speaks_kafka_with_the_address_msk_published() {
    let router = build_router();
    let network = ec2_network(&router).await;
    let published = plant_the_cluster(&router, &network).await;
    assert!(
        !published.is_empty(),
        "setup: a cluster with brokers must publish a bootstrap string, got {published:?}"
    );
    let physical = deploy(&router, &network).await;
    assert_eq!(
        physical.get("Probe").map(String::as_str),
        Some(FUNCTION),
        "setup: the stack must report the probe function as {FUNCTION:?}, got {physical:?}"
    );
    let configuration = wait_until_active(&router).await;
    // The function really is attached to the cluster's own VPC: the id reported for it is the
    // one EC2 holds for the subnets it was placed in, not a value the request carried.
    assert_eq!(
        configuration.pointer("/VpcConfig/VpcId").cloned(),
        Some(json!(network.vpc)),
        "setup: the probe must be attached to {:?}, got {configuration}",
        network.vpc
    );

    // FIRST the in-VPC proof, the way a client that carries its measurement into a VPC makes
    // it: the route toward the VPC's own resolver leaves from the private address of the
    // interface Lambda stood up in that VPC, and that resolver answers the queries sent to it.
    let own_hostname = {
        let report = probe(
            &router,
            "the function's own place in the VPC",
            &json!({ "resolver": VPC_RESOLVER }),
        )
        .await;
        let source = report
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("the route toward {VPC_RESOLVER} must report a source, got {report}"))
            .to_string();
        let prefix = VPC_CIDR.trim_end_matches("0.0/16");
        assert!(
            source.starts_with(prefix),
            "expected the source address to be inside the VPC range {VPC_CIDR}, got {source} \
             ({report})"
        );
        format!("ip-{}.{INTERNAL_DOMAIN}", source.replace('.', "-"))
    };
    // And the resolver answers for the VPC's own zone and refuses to make up an answer for
    // another: the hostname of the function's own address resolves to that address, and a name
    // of another zone — one the resolver would have to recurse for — is a server failure.
    let resolved = probe(
        &router,
        "the VPC resolver",
        &json!({
            "resolver": VPC_RESOLVER,
            "names": [own_hostname.clone(), "broker.example.com"],
        }),
    )
    .await;
    let own = resolved
        .pointer(&format!("/resolved/{own_hostname}"))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        own.get("rcode").and_then(Value::as_u64),
        Some(0),
        "expected the VPC resolver to answer {own_hostname} with NOERROR, got {resolved}"
    );
    assert_eq!(
        own.get("resolver").and_then(Value::as_str),
        Some(VPC_RESOLVER),
        "expected the reply to come from the VPC resolver itself, got {resolved}"
    );
    let own_address = own
        .get("answers")
        .and_then(Value::as_array)
        .and_then(|answers| answers.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        own_hostname,
        format!("ip-{}.{INTERNAL_DOMAIN}", own_address.replace('.', "-")),
        "expected the hostname to resolve to the address it names, got {resolved}"
    );
    assert_eq!(
        resolved
            .pointer("/resolved/broker.example.com")
            .and_then(|answer| answer.get("rcode"))
            .and_then(Value::as_u64),
        // SERVFAIL, as RFC 1035 numbers it: nothing of this emulator answers for a zone the
        // resolver would have to recurse for.
        Some(2),
        "expected a name of another zone to be a server failure, got {resolved}"
    );

    // THEN the data plane itself, at every address the control plane published.
    for (member, authority) in &published {
        let case = format!("the address published as {member}");
        let encrypted = member.contains("Tls") || member.contains("Sasl");
        let report = probe(
            &router,
            &case,
            &json!({ "broker": authority, "tls": encrypted }),
        )
        .await;
        let broker = report.get("broker").cloned().unwrap_or(Value::Null);
        assert!(
            report.get("broker_error").is_none(),
            "case {case:?}: the published address {authority} must be reachable from inside the \
             VPC, got {report}"
        );
        assert_eq!(
            broker.pointer("/api_versions/error_code").and_then(Value::as_i64),
            Some(0),
            "case {case:?}: expected the broker to answer ApiVersions, got {report}"
        );
        assert_eq!(
            broker.pointer("/api_versions/correlation").and_then(Value::as_i64),
            Some(11),
            "case {case:?}: expected the answer to carry the request's own correlation id, got \
             {report}"
        );
        assert_eq!(
            broker.pointer("/api_versions/serves_metadata").and_then(Value::as_bool),
            Some(true),
            "case {case:?}: expected the broker to report that it serves Metadata, got {report}"
        );
        // The broker it reports itself as is the address the control plane published, so a
        // client that re-resolves the cluster from its metadata keeps talking to this plane.
        let reported: Vec<String> = broker
            .pointer("/metadata/brokers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|broker| {
                format!(
                    "{}:{}",
                    broker.get("host").and_then(Value::as_str).unwrap_or_default(),
                    broker.get("port").and_then(Value::as_i64).unwrap_or_default()
                )
            })
            .collect();
        assert!(
            !reported.is_empty() && reported.iter().all(|broker| broker == authority),
            "case {case:?}: expected every broker reported to be the published address \
             {authority}, got {report}"
        );
        if encrypted {
            assert!(
                broker
                    .get("tls_version")
                    .and_then(Value::as_str)
                    .is_some_and(|version| version.starts_with("TLS")),
                "case {case:?}: expected the session to be the one the handshake negotiated, got \
                 {report}"
            );
        }
    }

    // And an address this emulator opened no plane at is answered the way a host answers one:
    // nothing is served at another port of the same host, and nothing routes to an address
    // outside the VPC.
    let authority = published
        .values()
        .next()
        .expect("a cluster with brokers publishes an address");
    let host = authority
        .rsplit_once(':')
        .map(|(host, _port)| host.to_string())
        .unwrap_or_default();
    let unreachable: [(&str, String); 2] = [
        (
            "a port of the same host nothing was opened at",
            format!("{host}:{UNSERVED_PORT}"),
        ),
        (
            "an address outside the VPC",
            "198.51.100.7:9092".to_string(),
        ),
    ];
    for (case, address) in unreachable {
        let report = probe(&router, case, &json!({ "broker": address })).await;
        assert!(
            report.get("broker_error").is_some() && report.get("broker").is_none(),
            "case {case:?}: expected {address} to answer nothing, got {report}"
        );
    }
}
