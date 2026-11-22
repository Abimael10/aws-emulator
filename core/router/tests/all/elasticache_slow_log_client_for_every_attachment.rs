// SPDX-License-Identifier: CC0-1.0

//! WHO a cache's slow log names as the client of a command, for every way a function comes to
//! sit in the cache's VPC.
//!
//! RULE: a cache's engine states, for every command it records in its slow log, the address
//! the client that sent it reached the cache FROM — so a command issued by a Lambda function
//! attached to the cache's VPC is recorded, and delivered to the CloudWatch Logs group the
//! cache's slow-log `LogDeliveryConfiguration` names, carrying the private address of a
//! network interface Lambda stood up for that function in that VPC, which is the address
//! Amazon EC2 holds for it. Never a placeholder.
//!
//! The rows span what the rule generalises over: HOW the function came to be ATTACHED — a later
//! `UpdateFunctionConfiguration` that adds a `VpcConfig` to a function created with none — and
//! HOW MANY interfaces the attachment holds. "Lambda creates an elastic network interface for
//! each subnet in your function's VPC configuration", and the address the cache records is one
//! of those interfaces' however the attachment came about.
//!
//! The row the sibling suite `elasticache_slow_log_names_its_in_vpc_client` already drives —
//! one subnet, the `VpcConfig` carried by the template the function is created from — is left
//! to it rather than copied here. What a cache RECORDS and DELIVERS over the axes a parameter
//! group and a configuration span is `elasticache_slow_log_delivery_to_cloudwatch_logs`.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{
    block_contains, block_of, element, elements, form_encode, opt_element,
};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The API versions the form-encoded services below are addressed at.
const EC2_API_VERSION: &str = "2016-11-15";
/// See [`EC2_API_VERSION`].
const ELASTICACHE_API_VERSION: &str = "2015-02-02";
/// See [`EC2_API_VERSION`].
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";

/// The parameter-group family of the version line every cluster below runs — one of
/// `CacheParameterGroupFamily`'s own valid values.
const FAMILY: &str = "redis7";

/// The threshold every cluster's parameter group states for `slowlog-log-slower-than`: the
/// bottom of that parameter's published range, which records every command.
const EVERY_COMMAND: &str = "0";

/// The status a cache that serves connections is in.
const AVAILABLE: &str = "available";

/// The `StackStatus` a probe stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`, and the memory it is
/// configured with. Both generous: what is under test is what the handler's connection is
/// recorded as, never how fast an interpreter gets through one run.
const FUNCTION_TIMEOUT: u64 = 120;
/// See [`FUNCTION_TIMEOUT`].
const FUNCTION_MEMORY: u64 = 3_008;

/// How often to look while waiting for work a service's own clock owes, and how long to keep
/// looking. Nothing here sleeps for a fixed span: every wait is for the CONDITION, and the
/// deadline is generous — far past what any step of this suite takes — so a loaded host cannot
/// decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const DEADLINE: Duration = Duration::from_secs(180);

/// The value the probe writes under the key each row names.
const VALUE: &str = "written-inside-the-vpc";

/// The handler every row runs: pure standard library, speaking RESP itself over a socket it
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

/// One case: the network it builds, how the function comes to be attached to it, and the name
/// every resource of the row carries.
struct Case {
    /// What names this row in every assertion.
    name: &'static str,
    /// The VPC's own range, and the subnets cut out of it — one per interface the attachment is
    /// to hold.
    vpc_cidr: &'static str,
    subnet_cidrs: &'static [&'static str],
    /// Whether the `VpcConfig` travels in the template the function is created from, or is
    /// added to a function created with none by a later `UpdateFunctionConfiguration`.
    attach_at_create: bool,
    /// The prefix every resource of this row is named with.
    prefix: &'static str,
}

/// The zones the subnets of a row are placed in, in order.
const ZONES: [&str; 2] = ["us-west-2a", "us-west-2b"];

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
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(request.to_string()))
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

/// The network one row builds, minted by EC2 itself.
struct Network {
    subnets: Vec<String>,
    group: String,
}

/// Create that network through EC2's own operations: the VPC, one subnet per range the row
/// names, and the security group the cache and its client share.
async fn ec2_network(router: &MultiServiceRouter, case: &Case) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", case.vpc_cidr)]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (index, cidr) in case.subnet_cidrs.iter().enumerate() {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", ZONES[index % ZONES.len()]),
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
                ("GroupName", &format!("{}-group", case.prefix)),
                ("GroupDescription", "the group of the cache and its client"),
            ],
        )
        .await,
        "groupId",
    );
    Network { subnets, group }
}

/// The log group a row's cluster delivers its slow log into.
fn log_group(case: &Case) -> String {
    format!("/aws/elasticache/{}", case.prefix)
}

/// Create the log group, the parameter group that records every command, the cache subnet
/// group and the cluster that delivers its slow log — and answer the address the cluster
/// published for its node once the clock has made it `available`.
async fn planted_cache(
    router: &MultiServiceRouter,
    case: &Case,
    network: &Network,
) -> (String, u64) {
    logs(
        router,
        "CreateLogGroup",
        json!({ "logGroupName": log_group(case) }),
    )
    .await;
    let parameters = format!("{}-parameters", case.prefix);
    elasticache(
        router,
        "CreateCacheParameterGroup",
        &[
            ("CacheParameterGroupName", parameters.as_str()),
            ("CacheParameterGroupFamily", FAMILY),
            ("Description", "records every command"),
        ],
    )
    .await;
    elasticache(
        router,
        "ModifyCacheParameterGroup",
        &[
            ("CacheParameterGroupName", parameters.as_str()),
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
    let subnet_group = format!("{}-subnets", case.prefix);
    let named: Vec<(String, String)> = network
        .subnets
        .iter()
        .enumerate()
        .map(|(index, subnet)| {
            (
                format!("SubnetIds.SubnetIdentifier.{}", index + 1),
                subnet.clone(),
            )
        })
        .collect();
    let mut request: Vec<(&str, &str)> = vec![
        ("CacheSubnetGroupName", subnet_group.as_str()),
        ("CacheSubnetGroupDescription", "the cache's own subnets"),
    ];
    for (key, value) in &named {
        request.push((key.as_str(), value.as_str()));
    }
    elasticache(router, "CreateCacheSubnetGroup", &request).await;
    let cluster = format!("{}-cluster", case.prefix);
    let prefix = "LogDeliveryConfigurations.LogDeliveryConfigurationRequest.1";
    let group = log_group(case);
    elasticache(
        router,
        "CreateCacheCluster",
        &[
            ("CacheClusterId", cluster.as_str()),
            ("Engine", "redis"),
            ("CacheNodeType", "cache.t3.micro"),
            ("NumCacheNodes", "1"),
            ("CacheSubnetGroupName", subnet_group.as_str()),
            ("CacheParameterGroupName", parameters.as_str()),
            ("SecurityGroupIds.member.1", network.group.as_str()),
            (&format!("{prefix}.LogType"), "slow-log"),
            (&format!("{prefix}.DestinationType"), "cloudwatch-logs"),
            (
                &format!("{prefix}.DestinationDetails.CloudWatchLogsDetails.LogGroup"),
                group.as_str(),
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
            &[
                ("CacheClusterId", cluster.as_str()),
                ("ShowCacheNodeInfo", "true"),
            ],
        )
        .await;
        if opt_element(&body, "CacheClusterStatus").as_deref() == Some(AVAILABLE) {
            let nodes = block_of(&body, "CacheNodes").unwrap_or_else(|| {
                panic!(
                    "case {}: the cluster must report its node, got {body}",
                    case.name
                )
            });
            let endpoint = block_of(nodes, "Endpoint").unwrap_or_else(|| {
                panic!(
                    "case {}: the node must publish an endpoint, got {nodes}",
                    case.name
                )
            });
            let address = element(endpoint, "Address");
            let port = element(endpoint, "Port").parse::<u64>().unwrap_or_else(|error| {
                panic!(
                    "case {}: the node must publish a numeric port: {error}",
                    case.name
                )
            });
            return (address, port);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {}: the cluster must reach {AVAILABLE}, last reported {body}",
            case.name
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The `VpcConfig` a row's attachment is written with, in the spelling both the template and
/// `UpdateFunctionConfiguration` take.
fn vpc_config(network: &Network) -> Value {
    json!({
        "SubnetIds": network.subnets,
        "SecurityGroupIds": [network.group],
    })
}

/// The template a row deploys: the execution role, and the probe function — carrying the
/// `VpcConfig` when the row attaches it at the create. The inline source is archived into the
/// deployment package by CloudFormation, which is how a function whose handler is written here
/// comes to exist.
fn stack_template(case: &Case, network: &Network) -> String {
    let mut function = json!({
        "FunctionName": format!("{}-fn", case.prefix),
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Role": {"Fn::GetAtt": ["ExecRole", "Arn"]},
        "Timeout": FUNCTION_TIMEOUT,
        "MemorySize": FUNCTION_MEMORY,
        "Code": {"ZipFile": PROBE_HANDLER},
    });
    if case.attach_at_create {
        function["VpcConfig"] = vpc_config(network);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ExecRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{}-exec", case.prefix),
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
                "Properties": function,
            },
        },
    })
    .to_string()
}

/// Deploy the row's stack and wait for `CREATE_COMPLETE`.
async fn deploy(router: &MultiServiceRouter, case: &Case, network: &Network) {
    let stack = format!("{}-stack", case.prefix);
    let template = stack_template(case, network);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack.as_str()),
            ("TemplateBody", template.as_str()),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {}: CreateStack({stack:?}) must be accepted, got {status} with {answered}",
        case.name
    );
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {}: DescribeStacks({stack:?}) must succeed, got {status} with {body}",
            case.name
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE {
            return;
        }
        if !reported.ends_with("_IN_PROGRESS") {
            let (_status, events) = cloudformation(
                router,
                "DescribeStackEvents",
                &[("StackName", stack.as_str())],
            )
            .await;
            panic!(
                "case {}: the stack must reach {CREATE_COMPLETE}, got {reported} with resource \
                 failures {:?}",
                case.name,
                elements(&events, "ResourceStatusReason")
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {}: the stack must reach {CREATE_COMPLETE}, last reported {reported}",
            case.name
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The function's configuration once Lambda reports it settled: `Active`, with no update of its
/// own still in progress.
async fn settled_configuration(router: &MultiServiceRouter, case: &Case, name: &str) -> Value {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        let (_status, _headers, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        let settled = configuration["State"].as_str() == Some("Active")
            && configuration["LastUpdateStatus"].as_str() != Some("InProgress");
        if settled {
            return configuration;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {}: Lambda must report {name:?} settled, last reported {configuration}",
            case.name
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The row's probe function, attached to the cache's VPC: deployed by its stack, and — for the
/// row that attaches it afterwards — given its `VpcConfig` by `UpdateFunctionConfiguration`.
/// Answers the function's name once Lambda reports the attachment settled.
async fn probe_function(router: &MultiServiceRouter, case: &Case, network: &Network) -> String {
    deploy(router, case, network).await;
    let name = format!("{}-fn", case.prefix);
    // A configuration is changed only on a function Lambda reports settled — "the operation
    // cannot be performed at this time" is what an update of one that is not gets — so an
    // attachment added afterwards waits for the create to settle first.
    settled_configuration(router, case, &name).await;
    if !case.attach_at_create {
        let (status, _headers, updated) = lambda(
            router,
            "PUT",
            &format!("/2015-03-31/functions/{name}/configuration"),
            json!({ "VpcConfig": vpc_config(network) })
                .to_string()
                .as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {}: UpdateFunctionConfiguration must attach the function, got {status} with \
             {updated}",
            case.name
        );
    }
    let configuration = settled_configuration(router, case, &name).await;
    assert_eq!(
        configuration["VpcConfig"]["SubnetIds"],
        json!(network.subnets),
        "case {}: the function must report the subnets it is attached to, got {configuration}",
        case.name
    );
    name
}

/// The private addresses Amazon EC2 holds for the interfaces Lambda stood up for `function`:
/// the interfaces of the row's own subnets whose description names that function.
async fn function_interface_addresses(
    router: &MultiServiceRouter,
    case: &Case,
    network: &Network,
    function: &str,
) -> Vec<String> {
    let mut found = Vec::new();
    for subnet in &network.subnets {
        let listed = ec2(
            router,
            "DescribeNetworkInterfaces",
            &[("Filter.1.Name", "subnet-id"), ("Filter.1.Value.1", subnet)],
        )
        .await;
        for id in elements(&listed, "networkInterfaceId") {
            // One interface at a time: an ec2Query answer nests `<item>` lists inside each
            // interface, so a reader that split the whole answer would tear one into several.
            let described = ec2(
                router,
                "DescribeNetworkInterfaces",
                &[("NetworkInterfaceId.1", id.as_str())],
            )
            .await;
            if opt_element(&described, "description")
                .unwrap_or_default()
                .contains(function)
            {
                found.push(opt_element(&described, "privateIpAddress").unwrap_or_default());
            }
        }
    }
    assert_eq!(
        found.len(),
        network.subnets.len(),
        "case {}: EC2 must hold one interface of {function:?} per attached subnet, got {found:?}",
        case.name
    );
    found
}

/// The records the row's log group holds, parsed as the configuration's `json` `LogFormat`
/// renders them.
async fn delivered_records(router: &MultiServiceRouter, case: &Case) -> Vec<Value> {
    let answer = logs(
        router,
        "FilterLogEvents",
        json!({ "logGroupName": log_group(case) }),
    )
    .await;
    answer["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|event| {
            let message = event["message"].as_str()?;
            Some(serde_json::from_str(message).unwrap_or_else(|error| {
                panic!("a record delivered as JSON parses as one, got {error} in {message:?}")
            }))
        })
        .collect()
}

/// Drive ONE row of the table: plant the network and the cache, attach the function the way
/// the row says, issue the command from inside the VPC, and assert the delivered record names
/// that function's own interface.
///
/// One row per test rather than a loop over the three: each row stands up a VPC, a cache and a
/// function of its own, so a test per row is three independent pieces of work the runner can
/// take in parallel instead of one that carries all three.
async fn the_record_names_the_in_vpc_client(case: &Case) {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);

    let network = ec2_network(&router, case).await;
    let (host, port) = planted_cache(&router, case, &network).await;
    let function = probe_function(&router, case, &network).await;
    let key = format!("slowlog:{}", case.prefix);
    let written = format!("GETRANGE {key} 0 -1");

    // The command is issued from INSIDE the VPC, which is the only vantage point an
    // ElastiCache endpoint is reachable from.
    let (status, headers, report) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        json!({
            "host": host,
            "port": port,
            "commands": [["SET", &key, VALUE], ["GETRANGE", &key, "0", "-1"]],
        })
        .to_string()
        .as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {}: the invocation must be served, got {status} with {report}",
        case.name
    );
    assert_eq!(
        headers.get("x-amz-function-error").map(|_| "raised"),
        None,
        "case {}: the handler must run to completion, got {report}",
        case.name
    );
    assert_eq!(
        report.pointer("/replies/1/value"),
        Some(&json!(VALUE)),
        "case {}: the cache must answer what the handler wrote, got {report}",
        case.name
    );

    // WHO that client is, as Amazon EC2 holds it.
    let addresses = function_interface_addresses(&router, case, &network, &function).await;
    for address in &addresses {
        assert!(
            block_contains(case.vpc_cidr, address),
            "case {}: the function's interface must carry an address of {}, got {address:?}",
            case.name,
            case.vpc_cidr
        );
    }

    // And that is the client the delivered record names.
    let deadline = std::time::Instant::now() + DEADLINE;
    let record = loop {
        let found = delivered_records(&router, case)
            .await
            .into_iter()
            .find(|record| record["Command"].as_str() == Some(written.as_str()));
        if let Some(record) = found {
            break record;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {}: a record naming {written:?} must reach {}, got none",
            case.name,
            log_group(case)
        );
        tokio::time::sleep(LOOK).await;
    };
    let stated = record["ClientAddress"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (client, client_port) = stated.rsplit_once(':').unwrap_or_else(|| {
        panic!(
            "case {}: the record must name the client as address:port, got {record}",
            case.name
        )
    });
    assert!(
        addresses.iter().any(|address| address == client),
        "case {}: the record must name an address EC2 holds for the function ({addresses:?}), got \
         {record}",
        case.name
    );
    assert!(
        client_port.parse::<u16>().is_ok_and(|port| port > 0),
        "case {}: the record must name the port the client reached the cache from, got {record}",
        case.name
    );
}

/// The row where the function is created attached to nothing and a later
/// `UpdateFunctionConfiguration` stands the attachment up.
#[tokio::test]
async fn a_record_names_the_client_of_a_function_attached_after_its_create() {
    the_record_names_the_in_vpc_client(&Case {
        name: "attached after the create",
        vpc_cidr: "10.71.0.0/16",
        subnet_cidrs: &["10.71.1.0/24"],
        attach_at_create: false,
        prefix: "xsvc-ec-attach-later",
    })
    .await;
}

/// The row whose attachment holds an interface in each of two subnets, in two zones.
#[tokio::test]
async fn a_record_names_the_client_of_a_function_attached_to_two_subnets() {
    the_record_names_the_in_vpc_client(&Case {
        name: "attached to two subnets",
        vpc_cidr: "10.72.0.0/16",
        subnet_cidrs: &["10.72.1.0/24", "10.72.2.0/24"],
        attach_at_create: true,
        prefix: "xsvc-ec-attach-two",
    })
    .await;
}
