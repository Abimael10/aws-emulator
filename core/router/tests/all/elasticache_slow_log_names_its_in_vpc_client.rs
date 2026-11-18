// SPDX-License-Identifier: CC0-1.0

//! WHO a slow-log record names as the client that issued the command: the function inside the
//! cache's own VPC that sent it.
//!
//! RULE: a cache's engine states, for every command it records in its slow log, the address
//! the client that sent it reached the cache FROM — so a command issued by a Lambda function
//! attached to the cache's VPC is recorded, and delivered to the CloudWatch Logs group the
//! cache's slow-log `LogDeliveryConfiguration` names, carrying the private address of the
//! network interface Lambda stood up for that function in that VPC, which is the address
//! Amazon EC2 holds for it.
//!
//! An ElastiCache endpoint is VPC-only, so this is the only vantage point a client of one can
//! be in: a record naming anything but that client's own address could not have come from the
//! connection that issued the command. Four crates have to agree for the rule to hold — EC2
//! owns the VPC, the subnet and the interface's address, ElastiCache opens the cache's socket
//! and records what its engine executed, Lambda runs the handler that dials it, and CloudWatch
//! Logs holds the delivered records — so the suite lives here, over ONE engine, exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! What the sibling suite `elasticache_slow_log_delivery_to_cloudwatch_logs` pins is the
//! RECORDING and the DELIVERY over the axes a parameter group and a configuration span. What
//! this one adds, and all it is here for, is the one field only an in-VPC client can decide:
//! the client's own address, asserted against the interface EC2 reports for the function
//! rather than against any value this suite wrote.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{block_of, element, elements, form_encode, opt_element};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The API versions the form-encoded services below are addressed at.
const EC2_API_VERSION: &str = "2016-11-15";
/// See [`EC2_API_VERSION`].
const ELASTICACHE_API_VERSION: &str = "2015-02-02";
/// See [`EC2_API_VERSION`].
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";

/// The network the cache and the function both sit in.
const VPC_CIDR: &str = "10.60.0.0/16";
const SUBNET_CIDR: &str = "10.60.1.0/24";
const ZONE: &str = "us-west-2a";
const SECURITY_GROUP: &str = "xsvc-ec-slowlog-group";

/// The cache resources this suite creates.
const SUBNET_GROUP: &str = "xsvc-ec-slowlog-subnets";
const PARAMETER_GROUP: &str = "xsvc-ec-slowlog-parameters";
const CLUSTER: &str = "xsvc-ec-slowlog-cluster";

/// The parameter-group family of the version line the cluster runs — one of
/// `CacheParameterGroupFamily`'s own valid values.
const FAMILY: &str = "redis7";

/// The threshold the cluster's parameter group states for `slowlog-log-slower-than`: the
/// bottom of that parameter's published range, which records every command.
const EVERY_COMMAND: &str = "0";

/// The log group the cluster's slow-log configuration names, created through CloudWatch Logs
/// itself.
const LOG_GROUP: &str = "/aws/elasticache/xsvc-ec-slowlog";

/// The stack, the execution role and the function that carry the probe.
const STACK: &str = "xsvc-ec-slowlog-probe";
const EXEC_ROLE: &str = "xsvc-ec-slowlog-exec";
const FUNCTION: &str = "xsvc-ec-slowlog-probe-fn";

/// The `StackStatus` the probe stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status a cache that serves connections is in.
const AVAILABLE: &str = "available";

/// How long one invocation is given, as the function's own `Timeout`, and the memory it is
/// configured with. Both generous: what is under test is what the handler's connection is
/// recorded as, never how fast an interpreter gets through one run.
const FUNCTION_TIMEOUT: u64 = 120;
/// See [`FUNCTION_TIMEOUT`].
const FUNCTION_MEMORY: u64 = 3_008;

/// How often to look while waiting for work a service's own clock owes, and how long to keep
/// looking. Nothing here sleeps for a fixed span: every wait is for the CONDITION, and the
/// deadline is generous — far past what any step of this suite takes — so a loaded host
/// cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const DEADLINE: Duration = Duration::from_secs(180);

/// The key the probe writes, and the value it writes under it.
const KEY: &str = "slowlog:from-the-vpc";
/// See [`KEY`].
const VALUE: &str = "written-inside-the-vpc";

/// The handler the probe runs: pure standard library, speaking RESP itself over a socket it
/// opens to the address the control plane published for the cache.
const PROBE_HANDLER: &str = r#"
import socket

# How long the handler waits on its own connection. Generous: what is under test is what the
# cache records of the connection, never how fast a host answers.
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
    try:
        opened = socket.create_connection((event["host"], event["port"]), WAIT)
    except OSError as refusal:
        return {"ok": False, "refused": str(refusal)}
    try:
        opened.settimeout(WAIT)
        reader = opened.makefile("rb")
        replies = []
        for command in event["commands"]:
            opened.sendall(_encode([str(word) for word in command]))
            replies.append(_reply(reader))
        return {"ok": True, "replies": replies}
    finally:
        opened.close()
"#;

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature itself is never checked here.
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

/// One CloudFormation action, answered with its status and XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    form_call(
        router,
        "cloudformation",
        CLOUDFORMATION_API_VERSION,
        action,
        members,
    )
    .await
}

/// One CloudWatch Logs call that must succeed, answered with its parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, request: Value) -> Value {
    let body = request.to_string();
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let (parts, body) = response.into_parts();
    let text = String::from_utf8_lossy(&body).to_string();
    assert_eq!(
        parts.status.as_u16(),
        200,
        "setup: CloudWatch Logs {operation} must succeed, got {} with {text}",
        parts.status
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
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
                ("GroupDescription", "the group of the cache and its client"),
            ],
        )
        .await,
        "groupId",
    );
    Network { subnet, group }
}

/// Create the log group, the parameter group that records every command, the cache subnet
/// group and the cluster that delivers its slow log — and answer the address the cluster
/// published for its node once the clock has made it `available`.
async fn planted_cache(router: &MultiServiceRouter, network: &Network) -> (String, u64) {
    logs(router, "CreateLogGroup", json!({ "logGroupName": LOG_GROUP })).await;
    elasticache(
        router,
        "CreateCacheParameterGroup",
        &[
            ("CacheParameterGroupName", PARAMETER_GROUP),
            ("CacheParameterGroupFamily", FAMILY),
            ("Description", "records every command"),
        ],
    )
    .await;
    elasticache(
        router,
        "ModifyCacheParameterGroup",
        &[
            ("CacheParameterGroupName", PARAMETER_GROUP),
            (
                "ParameterNameValues.ParameterNameValue.1.ParameterName",
                "slowlog-log-slower-than",
            ),
            (
                "ParameterNameValues.ParameterNameValue.1.ParameterValue",
                EVERY_COMMAND,
            ),
        ],
    )
    .await;
    elasticache(
        router,
        "CreateCacheSubnetGroup",
        &[
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("CacheSubnetGroupDescription", "the cache's own subnets"),
            ("SubnetIds.SubnetIdentifier.1", network.subnet.as_str()),
        ],
    )
    .await;
    let prefix = "LogDeliveryConfigurations.LogDeliveryConfigurationRequest.1";
    elasticache(
        router,
        "CreateCacheCluster",
        &[
            ("CacheClusterId", CLUSTER),
            ("Engine", "redis"),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheNodes", "1"),
            ("CacheSubnetGroupName", SUBNET_GROUP),
            ("CacheParameterGroupName", PARAMETER_GROUP),
            ("SecurityGroupIds.member.1", network.group.as_str()),
            (&format!("{prefix}.LogType"), "slow-log"),
            (&format!("{prefix}.DestinationType"), "cloudwatch-logs"),
            (
                &format!("{prefix}.DestinationDetails.CloudWatchLogsDetails.LogGroup"),
                LOG_GROUP,
            ),
            (&format!("{prefix}.LogFormat"), "json"),
            (&format!("{prefix}.Enabled"), "true"),
        ],
    )
    .await;
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let body = elasticache(
            router,
            "DescribeCacheClusters",
            &[("CacheClusterId", CLUSTER), ("ShowCacheNodeInfo", "true")],
        )
        .await;
        if opt_element(&body, "CacheClusterStatus").as_deref() == Some(AVAILABLE) {
            let nodes = block_of(&body, "CacheNodes")
                .unwrap_or_else(|| panic!("setup: the cluster must report its node, got {body}"));
            let endpoint = block_of(nodes, "Endpoint").unwrap_or_else(|| {
                panic!("setup: the node must publish an endpoint, got {nodes}")
            });
            let address = element(endpoint, "Address");
            let port = element(endpoint, "Port").parse::<u64>().unwrap_or_else(|error| {
                panic!("setup: the node must publish a numeric port: {error}")
            });
            return (address, port);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "setup: the cluster must reach {AVAILABLE}, last reported {body}"
        );
        tokio::time::sleep(LOOK).await;
    }
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

/// Deploy the stack and wait for `CREATE_COMPLETE`.
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
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({STACK:?}) must succeed, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE {
            return;
        }
        if !reported.ends_with("_IN_PROGRESS") {
            let (_, events) =
                cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
            panic!(
                "setup: the stack must reach {CREATE_COMPLETE}, got {reported} with resource \
                 failures {:?}",
                elements(&events, "ResourceStatusReason")
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "setup: the stack must reach {CREATE_COMPLETE}, last reported {reported}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Wait until Lambda reports the probe function `Active`.
async fn wait_until_active(router: &MultiServiceRouter) {
    let deadline = std::time::Instant::now() + DEADLINE;
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
            std::time::Instant::now() < deadline,
            "setup: Lambda must report {FUNCTION:?} Active, last reported {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The private address Amazon EC2 holds for the interface Lambda stood up for the probe
/// function: the interfaces of the function's own subnet and security group, narrowed to the
/// one EC2 describes as the function's.
async fn function_interface_address(router: &MultiServiceRouter, network: &Network) -> String {
    let listed = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[
            ("Filter.1.Name", "subnet-id"),
            ("Filter.1.Value.1", network.subnet.as_str()),
            ("Filter.2.Name", "group-id"),
            ("Filter.2.Value.1", network.group.as_str()),
        ],
    )
    .await;
    let mut found = Vec::new();
    for id in elements(&listed, "networkInterfaceId") {
        // One interface at a time: an ec2Query answer nests `<item>` lists inside each
        // interface, so a reader that split the whole answer would tear one into several.
        let described = ec2(
            router,
            "DescribeNetworkInterfaces",
            &[("NetworkInterfaceId.1", id.as_str())],
        )
        .await;
        let description = opt_element(&described, "description").unwrap_or_default();
        if description.contains(FUNCTION) {
            found.push(opt_element(&described, "privateIpAddress").unwrap_or_default());
        }
    }
    assert_eq!(
        found.len(),
        1,
        "setup: EC2 must hold exactly one interface of {FUNCTION:?} in {}, got {found:?}",
        network.subnet
    );
    found.remove(0)
}

/// The records the log group holds, parsed as the configuration's `json` `LogFormat` renders
/// them, with the stream each was written to.
async fn delivered_records(router: &MultiServiceRouter) -> Vec<(String, Value)> {
    let answer = logs(
        router,
        "FilterLogEvents",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    answer["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|event| {
            let stream = event["logStreamName"].as_str()?.to_string();
            let message = event["message"].as_str()?;
            Some((
                stream,
                serde_json::from_str(message).unwrap_or_else(|error| {
                    panic!("a record delivered as JSON parses as one, got {error} in {message:?}")
                }),
            ))
        })
        .collect()
}

#[tokio::test]
async fn a_slow_log_record_names_the_in_vpc_function_that_issued_the_command() {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);

    let network = ec2_network(&router).await;
    let (host, port) = planted_cache(&router, &network).await;
    deploy(&router, &network).await;
    wait_until_active(&router).await;

    // The command is issued from INSIDE the VPC, which is the only vantage point an
    // ElastiCache endpoint is reachable from.
    let (status, headers, report) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        json!({
            "host": host,
            "port": port,
            "commands": [["SET", KEY, VALUE], ["GET", KEY]],
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "the invocation must be served, got {status} with {report}"
    );
    assert_eq!(
        headers.get("x-amz-function-error").map(|_| "raised"),
        None,
        "the handler must run to completion, got {report}"
    );
    assert_eq!(
        report["ok"], json!(true),
        "the handler must reach the address the cache published, got {report}"
    );
    assert_eq!(
        report.pointer("/replies/1/value"),
        Some(&json!(VALUE)),
        "the cache must answer what the handler wrote, got {report}"
    );

    // WHO that client is, as Amazon EC2 holds it: the private address of the interface
    // Lambda stood up for the function in the cache's own subnet.
    let client = function_interface_address(&router, &network).await;
    assert!(
        client.starts_with("10.60.1."),
        "the function's interface must carry an address of {SUBNET_CIDR}, got {client:?}"
    );

    // And that is the client the delivered record names.
    let deadline = std::time::Instant::now() + DEADLINE;
    let records = loop {
        let records = delivered_records(&router).await;
        if !records.is_empty() {
            break records;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the slow-log records must reach {LOG_GROUP}, got none"
        );
        tokio::time::sleep(LOOK).await;
    };
    let written = format!("SET {KEY} {VALUE}");
    let (stream, record) = records
        .iter()
        .find(|(_stream, record)| record["Command"].as_str() == Some(written.as_str()))
        .unwrap_or_else(|| {
            panic!("the group must carry a record naming {written:?}, got {records:?}")
        });
    assert_eq!(
        record["ClientAddress"]
            .as_str()
            .and_then(|stated| stated.rsplit_once(':').map(|(address, _port)| address)),
        Some(client.as_str()),
        "the record must name the address the command was issued from, got {record}"
    );
    assert!(
        record["ClientAddress"]
            .as_str()
            .and_then(|stated| stated.rsplit_once(':'))
            .is_some_and(|(_address, port)| port.parse::<u16>().is_ok_and(|port| port > 0)),
        "the record must name the port the client reached the cache from, got {record}"
    );
    assert_eq!(
        (
            record["CacheClusterId"].as_str(),
            record["CacheNodeId"].as_str(),
            stream.as_str()
        ),
        (Some(CLUSTER), Some("0001"), CLUSTER),
        "the record must name the node that executed the command, got {record} in {stream}"
    );
}
