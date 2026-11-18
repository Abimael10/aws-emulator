// SPDX-License-Identifier: CC0-1.0

//! Amazon ECS resolves the ONE network an `awsvpc` placement configures against Amazon EC2,
//! which owns it.
//!
//! RULE: a placement in the `awsvpc` network mode configures ONE elastic network interface,
//! created in one subnet of one VPC and governed by the security groups of that same VPC —
//! `AwsVpcConfiguration$subnets` states "All specified subnets must be from the same VPC",
//! `$securityGroups` states "All specified security groups must be from the same VPC", and a
//! security group belongs to exactly one VPC (`SecurityGroup$vpcId`). So a placement whose named
//! subnets and security groups are not all in one VPC describes an interface that could never be
//! created: it is refused with the modelled `InvalidParameterException` (HTTP 400) and nothing
//! of it is stored, while a placement whose resources are all in one network is accepted and
//! reads back through the service's own describe.
//!
//! This suite lives beside the router because the network is Amazon EC2's: every VPC, subnet and
//! security group below is minted by the real EC2 operations on the SAME engine that serves
//! Amazon ECS, so every id in an accepted request is one EC2 handed out and every refusal is
//! grounded in what EC2 actually holds.
//!
//! The rows differ along what the rule generalises over: how many subnets and how many security
//! groups the placement names, which VPC each of them comes from, the POSITION of the one that
//! is elsewhere, and whether the placement names a security group at all. The same rule is
//! asserted on `RunTask` and on `UpdateService`, the other calls that name a configuration for
//! the same interface.
//!
//! Sources: `smithy_query(query="doc", shape="AwsVpcConfiguration", member="subnets")` and
//! `member="securityGroups"` on the ecs model for the two sentences above;
//! `smithy_query(query="operation", operation="CreateService,RunTask,UpdateService")` for the
//! modelled `InvalidParameterException` each of them answers.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix of Amazon ECS's `awsJson1_1` operations.
const TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The cluster every placement below is made in.
const CLUSTER: &str = "awsvpcnetworkmembership";

/// The `awsvpc`, Fargate-capable family every placement below places.
const FAMILY: &str = "awsvpcmembershipfamily";

/// The image the one container starts from; irrelevant to this rule, so one value.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The error code every refusal of this rule carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence a placement spread across two networks earns.
const DIFFERENT_NETWORKS: &str = "You have specified two resources that belong to different \
                                  networks.";

/// One emulator carrying Amazon EC2 and Amazon ECS over ONE shared engine, as the production
/// router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
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
    /// The bare shape name of the awsJson error code, or `""` on success.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The error envelope's message, or `""` when there is none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// Call one Amazon ECS operation over its `awsJson1_1` wire.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Reply {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ecs"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("serialize the request body"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Reply { status, body }
}

/// One Amazon ECS call that must be served, answered with its body.
async fn served(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let reply = ecs(router, operation, body).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected ECS {operation} to answer 200, got {} with {}",
        reply.status, reply.body
    );
    reply.body
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

/// One network of the account as Amazon EC2 holds it: a VPC with two subnets carved out of its
/// block and two security groups of it, every one minted by EC2's own operations.
struct Network {
    subnets: [String; 2],
    groups: [String; 2],
}

/// Build one network through Amazon EC2's API. `block` is the second octet of its `/16`, so two
/// calls of this build two VPCs that share no address.
async fn network_of(router: &MultiServiceRouter, label: &str, block: u8) -> Network {
    let vpc = element(
        &ec2(
            router,
            "CreateVpc",
            &[("CidrBlock", &format!("10.{block}.0.0/16"))],
        )
        .await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (index, zone) in ["a", "b"].into_iter().enumerate() {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", &format!("10.{block}.{index}.0/24")),
                    ("AvailabilityZone", &format!("{REGION}{zone}")),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    let mut groups = Vec::new();
    for index in 0..2 {
        groups.push(element(
            &ec2(
                router,
                "CreateSecurityGroup",
                &[
                    ("GroupName", &format!("{label}-group-{index}")),
                    ("GroupDescription", "a group of this network"),
                    ("VpcId", vpc.as_str()),
                ],
            )
            .await,
            "groupId",
        ));
    }
    Network {
        subnets: [subnets[0].clone(), subnets[1].clone()],
        groups: [groups[0].clone(), groups[1].clone()],
    }
}

/// An emulator with the cluster and the `awsvpc` revision every placement below names, and the
/// two networks the rows draw their subnets and groups from.
async fn fixture() -> (MultiServiceRouter, Network, Network) {
    let router = emulator();
    served(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        "fixture",
    )
    .await;
    served(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "containerDefinitions": [{
                "name": "web",
                "image": IMAGE,
                "memory": 128,
            }],
        }),
        "fixture",
    )
    .await;
    let here = network_of(&router, "here", 60).await;
    let elsewhere = network_of(&router, "elsewhere", 61).await;
    (router, here, elsewhere)
}

/// A `networkConfiguration` naming exactly `subnets` and `groups`, with `securityGroups` left
/// out of the request entirely when the row names none.
fn configuration(subnets: &[&String], groups: &[&String]) -> Value {
    let mut awsvpc = json!({ "subnets": subnets });
    if !groups.is_empty() {
        awsvpc["securityGroups"] = json!(groups);
    }
    json!({ "awsvpcConfiguration": awsvpc })
}

/// The list member of a body, absent reading as empty.
fn list(body: &Value, member: &str) -> Vec<Value> {
    body.get(member)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The ids a configuration's `subnets` or `securityGroups` reports.
fn ids(configured: &Value, member: &str) -> Vec<String> {
    list(configured, member)
        .iter()
        .map(|id| id.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The `awsvpcConfiguration` the stored service reports, or `None` when the cluster holds no
/// service under `name`.
async fn read_back(router: &MultiServiceRouter, name: &str, case: &str) -> Option<Value> {
    let described = served(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
        case,
    )
    .await;
    let services = list(&described, "services");
    let service = services.first()?.clone();
    Some(service["networkConfiguration"]["awsvpcConfiguration"].clone())
}

#[tokio::test]
async fn a_service_is_refused_unless_its_subnets_and_security_groups_are_in_one_vpc() {
    let (router, here, elsewhere) = fixture().await;

    // (case, the subnets named, the groups named, whether ONE VPC holds all of them)
    let rows: [(&str, Vec<&String>, Vec<&String>, bool); 8] = [
        (
            "one subnet and one group of its own VPC",
            vec![&here.subnets[0]],
            vec![&here.groups[0]],
            true,
        ),
        (
            "two subnets and two groups of one VPC",
            vec![&here.subnets[0], &here.subnets[1]],
            vec![&here.groups[0], &here.groups[1]],
            true,
        ),
        (
            "a subnet of one VPC and no group at all",
            vec![&here.subnets[1]],
            vec![],
            true,
        ),
        (
            "a group of another VPC",
            vec![&here.subnets[0]],
            vec![&elsewhere.groups[0]],
            false,
        ),
        (
            "a group of this VPC followed by one of another",
            vec![&here.subnets[0]],
            vec![&here.groups[0], &elsewhere.groups[1]],
            false,
        ),
        (
            "a group of another VPC followed by one of this",
            vec![&here.subnets[1]],
            vec![&elsewhere.groups[1], &here.groups[1]],
            false,
        ),
        (
            "subnets of two VPCs",
            vec![&here.subnets[0], &elsewhere.subnets[0]],
            vec![&here.groups[0]],
            false,
        ),
        (
            "a second subnet and a group both of another VPC",
            vec![&here.subnets[0], &elsewhere.subnets[1]],
            vec![&elsewhere.groups[0]],
            false,
        ),
    ];

    for (index, (case, subnets, groups, one_network)) in rows.iter().enumerate() {
        let name = format!("membershipsvc{index}");
        let reply = ecs(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": name,
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "launchType": "FARGATE",
                "networkConfiguration": configuration(subnets, groups),
            }),
        )
        .await;
        if *one_network {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected the service to be created, got {} with {}",
                reply.status, reply.body
            );
            // The accepted configuration reads back through the service's own describe: the
            // network it names is what the stored service places into.
            let configured = read_back(&router, &name, case)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: expected {name} to be stored"));
            let expected_subnets: Vec<String> =
                subnets.iter().map(|subnet| (*subnet).clone()).collect();
            let reported_subnets = ids(&configured, "subnets");
            assert_eq!(
                reported_subnets, expected_subnets,
                "case {case:?}: expected the subnets {expected_subnets:?} to read back, got \
                 {configured}"
            );
            let expected_groups: Vec<String> =
                groups.iter().map(|group| (*group).clone()).collect();
            let reported_groups = ids(&configured, "securityGroups");
            assert_eq!(
                reported_groups, expected_groups,
                "case {case:?}: expected the security groups {expected_groups:?} to read back, \
                 got {configured}"
            );
            continue;
        }
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected a placement across two networks to be refused, got {} \
             with {}",
            reply.status, reply.body
        );
        assert_eq!(
            (reply.error_code().as_str(), reply.message()),
            (INVALID_PARAMETER, DIFFERENT_NETWORKS),
            "case {case:?}: expected the cross-network refusal, got {}",
            reply.body
        );
        let stored = read_back(&router, &name, case).await;
        assert!(
            stored.is_none(),
            "case {case:?}: expected no service stored under {name}, got {stored:?}"
        );
    }
}

#[tokio::test]
async fn a_run_task_is_held_to_the_same_one_network_rule() {
    // The rule is about the interface the placement configures, not about which call made the
    // placement: `RunTask` names the same configuration and earns the same verdict.
    let (router, here, elsewhere) = fixture().await;
    let case = "a run naming a group of another VPC";
    let started_by = "membershiprun";
    let reply = ecs(
        &router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": FAMILY,
            "count": 1,
            "launchType": "FARGATE",
            "networkConfiguration": configuration(&[&here.subnets[0]], &[&elsewhere.groups[0]]),
            "startedBy": started_by,
        }),
    )
    .await;
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected the run to be refused, got {} with {}",
        reply.status, reply.body
    );
    assert_eq!(
        (reply.error_code().as_str(), reply.message()),
        (INVALID_PARAMETER, DIFFERENT_NETWORKS),
        "case {case:?}: expected the cross-network refusal, got {}",
        reply.body
    );
    let listed = served(
        &router,
        "ListTasks",
        json!({ "cluster": CLUSTER, "startedBy": started_by }),
        case,
    )
    .await;
    let arns = list(&listed, "taskArns");
    assert!(
        arns.is_empty(),
        "case {case:?}: expected the refused run to have started no task, got {arns:?}"
    );

    // The same run inside ONE network is made, so what the refusal above reports is the second
    // network rather than the shape of the request.
    let inside = "a run inside one VPC";
    let run = served(
        &router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": FAMILY,
            "count": 1,
            "launchType": "FARGATE",
            "networkConfiguration": configuration(&[&here.subnets[0]], &[&here.groups[0]]),
            "startedBy": "membershiprunserved",
        }),
        inside,
    )
    .await;
    assert_eq!(
        list(&run, "tasks").len(),
        1,
        "case {inside:?}: expected one task to be started, got {run}"
    );
    assert!(
        list(&run, "failures").is_empty(),
        "case {inside:?}: expected no failure, got {run}"
    );
}

#[tokio::test]
async fn an_update_cannot_move_a_service_onto_a_group_of_another_vpc() {
    // `UpdateService$networkConfiguration` names the network the service's next deployment
    // places into, so it is held to the same rule — and a refused update changes nothing.
    let (router, here, elsewhere) = fixture().await;
    let case = "an update naming a group of another VPC";
    let name = "membershipupdated";
    served(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "FARGATE",
            "networkConfiguration": configuration(&[&here.subnets[0]], &[&here.groups[0]]),
        }),
        case,
    )
    .await;
    let reply = ecs(
        &router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": name,
            "networkConfiguration": configuration(&[&here.subnets[0]], &[&elsewhere.groups[0]]),
            "forceNewDeployment": true,
        }),
    )
    .await;
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected the update to be refused, got {} with {}",
        reply.status, reply.body
    );
    assert_eq!(
        (reply.error_code().as_str(), reply.message()),
        (INVALID_PARAMETER, DIFFERENT_NETWORKS),
        "case {case:?}: expected the cross-network refusal, got {}",
        reply.body
    );
    let configured = read_back(&router, name, case)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected {name} to be stored"));
    assert_eq!(
        ids(&configured, "securityGroups"),
        vec![here.groups[0].clone()],
        "case {case:?}: expected the refused update to leave the service's groups as they \
         were, got {configured}"
    );

    // An update that moves the service inside its OWN network is accepted, so what the refusal
    // above reports is the second network rather than an unchangeable member.
    let inside = "an update inside one VPC";
    served(
        &router,
        "UpdateService",
        json!({
            "cluster": CLUSTER,
            "service": name,
            "networkConfiguration": configuration(&[&here.subnets[1]], &[&here.groups[1]]),
            "forceNewDeployment": true,
        }),
        inside,
    )
    .await;
    let moved = read_back(&router, name, inside)
        .await
        .unwrap_or_else(|| panic!("case {inside:?}: expected {name} to be stored"));
    assert_eq!(
        ids(&moved, "securityGroups"),
        vec![here.groups[1].clone()],
        "case {inside:?}: expected the accepted update to move the service's groups, got {moved}"
    );
    assert_eq!(
        ids(&moved, "subnets"),
        vec![here.subnets[1].clone()],
        "case {inside:?}: expected the accepted update to move the service's subnets, got {moved}"
    );
}
