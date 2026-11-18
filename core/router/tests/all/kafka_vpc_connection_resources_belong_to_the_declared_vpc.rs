// SPDX-License-Identifier: CC0-1.0

//! An Amazon MSK VPC connection is placed only in the VPC its request DECLARES.
//!
//! RULE: a VPC connection is reached through elastic network interfaces, and an interface is
//! created inside one subnet and attached to security groups of that subnet's own VPC — so
//! every subnet `ClientSubnets` names and every group `SecurityGroups` names has to belong to
//! the VPC `VpcId` declares, and a create naming one the account's network puts in another VPC
//! is refused with the modelled `BadRequestException` (HTTP 400) naming the member at fault
//! and leaves no connection behind. `CreateVpcConnection` is the only operation of this API
//! that declares the VPC a placement is in, so it is the only one the rule reaches.
//!
//! A subnet and a security group are Amazon EC2's resources, so which VPC holds each is EC2's
//! to say: this suite lives beside the router because every VPC, subnet and group below is
//! minted by the real EC2 operations on the SAME engine that serves Amazon MSK, so every id in
//! a request is one EC2 handed out and every refusal is grounded in what EC2 actually
//! answered — which is also why an id EC2 resolves NOTHING for refuses nothing.
//!
//! The rows differ along what the rule generalises over: WHICH member names the foreign
//! resource (the client subnets, the security groups), how MANY of the named resources are
//! foreign, WHERE in the list the foreign one sits, which of the estate's two VPCs the request
//! declares, and whether an id resolves to a resource at all. The cardinality the service
//! already checks is held constant — every row names as many client subnets as the target
//! cluster is placed in — so what each refusal is about is the VPC rather than the count.
//!
//! Error source: `smithy_query(query="operation", operation="CreateVpcConnection")` on the
//! kafka model — `BadRequestException`, `errorKind` client, `httpError` 400.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The Apache Kafka version the cluster below asks for.
const KAFKA_VERSION: &str = "3.6.0";

/// The broker instance type the cluster below asks for.
const INSTANCE_TYPE: &str = "kafka.m5.large";

/// The client-authentication scheme every connection below asks for, which the target cluster
/// is created with turned on.
const AUTHENTICATION: &str = "SASL_IAM";

/// A well-formed security group id no `CreateSecurityGroup` below handed out, so EC2 resolves
/// nothing for it and it grounds no refusal.
const UNRESOLVABLE_GROUP: &str = "sg-0baadf00d00000001";

/// How long a poll waits for a transition the clock already owes a resource.
///
/// GENEROUS on purpose: what is waited for is a condition the records themselves owe, not a
/// span, so a loaded host only makes the loop go round more times.
const READY_DEADLINE: Duration = Duration::from_secs(120);

/// How long one turn of a poll loop pauses before looking again.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// One emulator carrying Amazon EC2 and Amazon MSK over ONE shared engine, as the production
/// router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_kafka_kafka::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One answer: its HTTP status and its body as JSON (`Value::Null` when it is not JSON).
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// The wire error code (`__type` of the restJson1 error envelope), or `""` on success.
    fn error_code(&self) -> &str {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The error envelope's message, or `""` when there is none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The member the refusal names at fault, or `""` when it names none.
    fn invalid_parameter(&self) -> &str {
        self.body
            .get("invalidParameter")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// Call one Amazon MSK operation over its restJson1 wire.
async fn kafka(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
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
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Reply { status, body }
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// One VPC of the estate: the ids EC2 minted for it, one subnet per Availability Zone, and a
/// security group of its own.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
}

/// The Availability Zones every VPC of the estate spreads its subnets over.
const ZONES: [&str; 3] = ["us-west-2a", "us-west-2b", "us-west-2c"];

/// Mint one VPC, one subnet per zone and one security group, through EC2's own operations.
async fn network(router: &MultiServiceRouter, cidr_prefix: &str, label: &str) -> Network {
    let vpc = element(
        &ec2(
            router,
            "CreateVpc",
            &[("CidrBlock", &format!("{cidr_prefix}.0.0/16"))],
        )
        .await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (index, zone) in ZONES.iter().enumerate() {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", &format!("{cidr_prefix}.{}.0/24", index + 1)),
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
                ("GroupName", &format!("msk-connection-{label}")),
                ("GroupDescription", "a group a VPC connection names"),
                ("VpcId", vpc.as_str()),
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

/// A `CreateCluster` body placing `brokers` brokers in `subnets`, with SASL/IAM client
/// authentication on — the scheme Amazon MSK requires the target cluster to turn on before it
/// serves a VPC connection asking for it.
fn cluster_body(name: &str, brokers: u32, subnets: &[String]) -> Value {
    json!({
        "clusterName": name,
        "kafkaVersion": KAFKA_VERSION,
        "numberOfBrokerNodes": brokers,
        "brokerNodeGroupInfo": {
            "instanceType": INSTANCE_TYPE,
            "clientSubnets": subnets,
        },
        "clientAuthentication": { "sasl": { "iam": { "enabled": true } } },
    })
}

/// A `CreateVpcConnection` body reaching `cluster` from `subnets` in the VPC `vpc` declares,
/// under `groups`.
fn connection_body(cluster: &str, vpc: &str, subnets: &[String], groups: &[String]) -> Value {
    json!({
        "targetClusterArn": cluster,
        "authentication": AUTHENTICATION,
        "vpcId": vpc,
        "clientSubnets": subnets,
        "securityGroups": groups,
    })
}

/// The cluster `cluster_arn` names, read again until Amazon MSK reports it `ACTIVE` — the
/// state the clock owes it after the create — and answered as the LAST read either way, so an
/// assertion reports what MSK really said.
///
/// Waits for the CONDITION rather than for a span: Amazon MSK serves a VPC connection only
/// from a cluster state in which the cluster HAS brokers, and when that holds is the cluster's
/// own business rather than this suite's.
async fn cluster_once_active(router: &MultiServiceRouter, cluster_arn: &str) -> Reply {
    let uri = format!("/v1/clusters/{}", form_encode(cluster_arn));
    let deadline = Instant::now() + READY_DEADLINE;
    let mut last = kafka(router, "GET", &uri, None).await;
    while Instant::now() < deadline {
        // A read that is not answered at all is not a transient condition — the ARN is the
        // one the create just minted — so it is answered straight back.
        if last.status != 200
            || last.body.pointer("/clusterInfo/state").and_then(Value::as_str) == Some("ACTIVE")
        {
            return last;
        }
        tokio::time::sleep(POLL_PAUSE).await;
        last = kafka(router, "GET", &uri, None).await;
    }
    last
}

/// The connection `arn` names, read again until Amazon MSK reports it `AVAILABLE`, under the
/// deadline and for the reason [`cluster_once_active`] states.
async fn connection_once_available(router: &MultiServiceRouter, arn: &str) -> Reply {
    let uri = format!("/v1/vpc-connection/{}", form_encode(arn));
    let deadline = Instant::now() + READY_DEADLINE;
    let mut last = kafka(router, "GET", &uri, None).await;
    while Instant::now() < deadline {
        if last.status != 200 || last.body.get("state").and_then(Value::as_str) == Some("AVAILABLE")
        {
            return last;
        }
        tokio::time::sleep(POLL_PAUSE).await;
        last = kafka(router, "GET", &uri, None).await;
    }
    last
}

/// Every VPC connection ARN `ListVpcConnections` reports, over every page.
async fn listed_connection_arns(router: &MultiServiceRouter) -> Vec<String> {
    let mut arns = Vec::new();
    let mut uri = "/v1/vpc-connections".to_string();
    loop {
        let reply = kafka(router, "GET", &uri, None).await;
        assert_eq!(
            reply.status, 200,
            "a VPC-connection listing must answer 200, got {} with {}",
            reply.status, reply.body
        );
        for connection in reply
            .body
            .get("vpcConnections")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Some(arn) = connection
                .get("vpcConnectionArn")
                .and_then(Value::as_str)
            {
                arns.push(arn.to_string());
            }
        }
        match reply.body.get("nextToken").and_then(Value::as_str) {
            Some(token) => uri = format!("/v1/vpc-connections?nextToken={}", form_encode(token)),
            None => break,
        }
    }
    arns
}

/// What a refusal must say about a client subnet the declared VPC does not hold, written out
/// here rather than read from the service's own constant so the wording is pinned by this
/// suite instead of by the code under test.
fn subnet_outside_vpc(subnet: &str, vpc: &str) -> String {
    format!("'{subnet}' is not a subnet of the VPC '{vpc}'.")
}

/// The same for a security group the declared VPC does not hold.
fn group_outside_vpc(group: &str, vpc: &str) -> String {
    format!("'{group}' is not a security group of the VPC '{vpc}'.")
}

/// One row of the refusal table: the case, the request to make, the member the refusal must
/// name at fault and the sentence it must carry.
struct RefusalRow {
    case: &'static str,
    body: Value,
    parameter: &'static str,
    message: String,
}

/// RULE (above): every client subnet and every security group a VPC connection names must
/// belong to the VPC its request declares, and one of another VPC is refused with a 400
/// `BadRequestException` naming that member, leaving no connection behind — while a
/// consistent request is served and the connection it mints settles `AVAILABLE`.
#[tokio::test]
async fn a_vpc_connection_is_refused_resources_of_another_vpc() {
    let router = emulator();
    // TWO VPCs of one account, each with its own subnets and its own security group: the
    // estate the rule is about. Every id below is one EC2 minted.
    let home = network(&router, "10.90", "home").await;
    let foreign = network(&router, "10.91", "foreign").await;

    let created = kafka(
        &router,
        "POST",
        "/v1/clusters",
        Some(cluster_body("vpc-connected", 3, &home.subnets)),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: the target cluster must be created, got {} with {}",
        created.status, created.body
    );
    let cluster = created
        .body
        .get("clusterArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: the create reported no clusterArn: {}", created.body))
        .to_string();
    let settled = cluster_once_active(&router, &cluster).await;
    assert_eq!(
        settled
            .body
            .pointer("/clusterInfo/state")
            .and_then(Value::as_str),
        Some("ACTIVE"),
        "setup: the target cluster must reach ACTIVE, last saw {}",
        settled.body
    );

    // The positive control FIRST, so every refusal below is shown to leave a working create
    // behind: subnets and a group of the VPC the request declares.
    let accepted = kafka(
        &router,
        "POST",
        "/v1/vpc-connection",
        Some(connection_body(
            &cluster,
            &home.vpc,
            &home.subnets,
            std::slice::from_ref(&home.group),
        )),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case \"a connection placed in the declared VPC's own subnets and group\": expected \
         the create to be accepted, got {} with {}",
        accepted.status, accepted.body
    );
    let connection_arn = accepted
        .body
        .get("vpcConnectionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case \"a connection placed in the declared VPC's own subnets and group\": the \
                 create reported no vpcConnectionArn: {}",
                accepted.body
            )
        })
        .to_string();
    let available = connection_once_available(&router, &connection_arn).await;
    assert_eq!(
        available.body.get("state").and_then(Value::as_str),
        Some("AVAILABLE"),
        "case \"a connection placed in the declared VPC's own subnets and group\": expected it \
         to settle AVAILABLE, last saw {}",
        available.body
    );
    assert_eq!(
        available.body.get("vpcId").and_then(Value::as_str),
        Some(home.vpc.as_str()),
        "case \"a connection placed in the declared VPC's own subnets and group\": expected the \
         declared VPC to be reported back, got {}",
        available.body
    );

    // What the account holds once the accepted create has run: every refusal below must leave
    // exactly this.
    let accepted_connections = listed_connection_arns(&router).await;

    // Each row names as many client subnets as the cluster is placed in, so the cardinality
    // the service already checks refuses none of them: what is wrong is the VPC.
    let rows = [
        RefusalRow {
            case: "every client subnet in another VPC",
            body: connection_body(
                &cluster,
                &home.vpc,
                &foreign.subnets,
                std::slice::from_ref(&home.group),
            ),
            parameter: "ClientSubnets",
            message: subnet_outside_vpc(&foreign.subnets[0], &home.vpc),
        },
        RefusalRow {
            case: "the SECOND of three client subnets in another VPC",
            body: connection_body(
                &cluster,
                &home.vpc,
                &[
                    home.subnets[0].clone(),
                    foreign.subnets[1].clone(),
                    home.subnets[2].clone(),
                ],
                std::slice::from_ref(&home.group),
            ),
            parameter: "ClientSubnets",
            message: subnet_outside_vpc(&foreign.subnets[1], &home.vpc),
        },
        RefusalRow {
            case: "the FIRST of three client subnets in another VPC",
            body: connection_body(
                &cluster,
                &home.vpc,
                &[
                    foreign.subnets[0].clone(),
                    home.subnets[1].clone(),
                    home.subnets[2].clone(),
                ],
                std::slice::from_ref(&home.group),
            ),
            parameter: "ClientSubnets",
            message: subnet_outside_vpc(&foreign.subnets[0], &home.vpc),
        },
        RefusalRow {
            case: "a declared VPC that holds none of the client subnets",
            body: connection_body(
                &cluster,
                &foreign.vpc,
                &home.subnets,
                std::slice::from_ref(&foreign.group),
            ),
            parameter: "ClientSubnets",
            message: subnet_outside_vpc(&home.subnets[0], &foreign.vpc),
        },
        RefusalRow {
            case: "a security group of another VPC",
            body: connection_body(
                &cluster,
                &home.vpc,
                &home.subnets,
                std::slice::from_ref(&foreign.group),
            ),
            parameter: "SecurityGroups",
            message: group_outside_vpc(&foreign.group, &home.vpc),
        },
        RefusalRow {
            case: "the declared VPC's own group and then one of another VPC",
            body: connection_body(
                &cluster,
                &home.vpc,
                &home.subnets,
                &[home.group.clone(), foreign.group.clone()],
            ),
            parameter: "SecurityGroups",
            message: group_outside_vpc(&foreign.group, &home.vpc),
        },
    ];

    for row in &rows {
        let case = row.case;
        let refused = kafka(&router, "POST", "/v1/vpc-connection", Some(row.body.clone())).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "BadRequestException"),
            "case {case:?}: expected a 400 BadRequestException, got {} with {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.invalid_parameter(),
            row.parameter,
            "case {case:?}: expected the refusal to name {} at fault, got {}",
            row.parameter,
            refused.body
        );
        assert_eq!(
            refused.message(),
            row.message,
            "case {case:?}: expected {:?}, got {}",
            row.message,
            refused.body
        );
        let connections = listed_connection_arns(&router).await;
        assert_eq!(
            connections, accepted_connections,
            "case {case:?}: a refused create must leave no connection behind, got \
             {connections:?}"
        );
    }

    // And the other half of the rule: a refusal is grounded in what EC2 ANSWERED, so an id it
    // resolves no group for says nothing about which VPC holds that group and refuses
    // nothing.
    let unresolvable = kafka(
        &router,
        "POST",
        "/v1/vpc-connection",
        Some(connection_body(
            &cluster,
            &home.vpc,
            &home.subnets,
            &[UNRESOLVABLE_GROUP.to_string()],
        )),
    )
    .await;
    assert_eq!(
        unresolvable.status, 200,
        "case \"a security group id EC2 holds no group for\": expected the create to be \
         accepted, got {} with {}",
        unresolvable.status, unresolvable.body
    );
    let connections = listed_connection_arns(&router).await;
    assert_eq!(
        connections.len(),
        accepted_connections.len() + 1,
        "case \"a security group id EC2 holds no group for\": expected the accepted create to \
         be registered, got {connections:?}"
    );
}
