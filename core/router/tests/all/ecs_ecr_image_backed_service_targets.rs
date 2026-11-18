// SPDX-License-Identifier: CC0-1.0

//! An ECS service whose containers run an image out of this account's Amazon ECR registry
//! has its RUNNING tasks registered as Elastic Load Balancing v2 targets of exactly the
//! groups that service's own `loadBalancers` bindings name.
//!
//! RULE: Amazon ECS registers each running task of an `awsvpc` service as a target of every
//! target group the placement's `loadBalancers` bindings name — at the private IPv4 address
//! of the elastic network interface the placement gave it, on the `containerPort` the
//! binding names — and registers it NOWHERE otherwise, whatever registry the task's image
//! was fetched from: so a bound group reports one `TargetHealthDescription` per running task
//! of the service that binds it, and a group no service's `loadBalancers` names reports
//! none. Where the image came from decides whether the task runs at all, never which group
//! it is a target of.
//!
//! The rows differ along what the rule generalises over: how the image is addressed (a tag
//! and a digest of a repository of this account's private registry, and a public registry's
//! own reference), whether the service declares a binding at all, how many tasks it runs,
//! and which of the two ports its container listens on the binding names. The service with
//! no `loadBalancers` block is the negative row: its running task must be a target of
//! nothing, which is only observable beside services that do declare one, so every row runs
//! on the same cluster, the same network and the same execution role. The group nothing
//! references is read on every pass of the convergence wait, so its emptiness holds
//! throughout the window the other services' tasks come up in rather than once at the end.
//!
//! Five crates have to agree for the rule to hold, which is why this suite lives here: IAM
//! holds the execution role Amazon ECS pulls as, Amazon ECR holds the repository and the
//! image the reference resolves to, Amazon EC2 owns the network the tasks are placed in and
//! the path the pull takes, Amazon ECS owns the tasks and their interfaces' addresses, and
//! Elastic Load Balancing v2 holds the registrations and reports them. Everything is driven
//! through the real operations over ONE shared engine, so what is asserted is what each
//! service persisted.
//!
//! The sibling suite `ecs_targets_follow_the_services_own_bindings.rs` pins the same rule
//! for services whose images need no registry of this account at all; this one pins that an
//! ECR-sourced service — the shape a client deploying its own image behind an Application
//! Load Balancer runs — is registered by that same rule.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, items_of, members_of, opt_element};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query, an awsQuery and an IAM request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";
const IAM_VERSION: &str = "2010-05-08";

/// The ECS cluster and the container name every row runs.
const CLUSTER: &str = "ecrbacked";
const CONTAINER: &str = "app";

/// The two ports the container declares `portMappings` for, so the two a binding may name.
const CONTAINER_PORT: i64 = 80;
const SECOND_CONTAINER_PORT: i64 = 8081;

/// The repository of this account's registry the private rows pull from, and the tag the
/// image is pushed under.
const REPOSITORY: &str = "ecrbacked/app";
const TAG: &str = "release";

/// The reference the public row runs: a repository of the public gallery, which is not this
/// account's registry and is reached over the internet path the placement's subnets have.
const PUBLIC_IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// A Docker image manifest V2 schema 2 referencing no blob: what the pull depends on is
/// whether the registry HOLDS the image and may serve it to the execution role, not which
/// blobs it is built from — so the layer array its media type requires is there and empty.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json","#,
    r#""layers":[]}"#
);

/// The execution role Amazon ECS pulls as, and the managed policy AWS publishes for one
/// ("AmazonECSTaskExecutionRolePolicy", the task execution role policy of the Amazon ECS
/// Developer Guide) — attached rather than hand-written, so the pull is decided over the
/// document IAM really holds for that role.
const EXECUTION_ROLE: &str = "ecrbackedexecution";
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The network every row runs in.
const VPC_CIDR: &str = "10.71.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.71.1.0/24", "us-east-1a"),
    ("10.71.2.0/24", "us-east-1b"),
];

/// The destination of the route that carries a subnet's traffic off the VPC, which is the
/// path the image pull of a task placed in it takes.
const INTERNET_DESTINATION: &str = "0.0.0.0/0";

/// The target group NO service references.
const UNREFERENCED_GROUP: &str = "ecrbacked-unreferenced";

/// How long the convergence below is waited for, and how often it is looked at. Generous on
/// purpose: the deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// `Deployment$rolloutState` of a rollout that is over: "When the service reaches a steady
/// state, the deployment transitions to a `COMPLETED` state."
const ROLLOUT_COMPLETED: &str = "COMPLETED";

/// HOW one row's task definition addresses the image its container runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `<repositoryUri>:<tag>` of a repository of this account's private registry.
    PrivateTag,
    /// `<repositoryUri>@<digest>` of that same image, which is the other way a reference
    /// addresses it.
    PrivateDigest,
    /// A public gallery reference, which names no repository of this account.
    PublicRegistry,
}

/// The `loadBalancers` entry one row declares.
struct Binding {
    group_name: &'static str,
    /// The listener port that makes the group one the load balancer forwards to.
    listener_port: &'static str,
    container_port: i64,
}

/// One row: the task definition it registers, the service it creates, and the registrations
/// that service's running tasks must then be reported with.
struct Row {
    case: &'static str,
    family: &'static str,
    source: Source,
    service_name: &'static str,
    /// The group this service BINDS, or `None` for a service that declares no
    /// `loadBalancers` block at all.
    binding: Option<Binding>,
    /// How many tasks the service is asked for, which is how many registrations its bound
    /// group must hold.
    desired: i64,
}

const ROWS: [Row; 4] = [
    Row {
        case: "an ECR image addressed by tag, bound to its own group on the container port",
        family: "ecrbackedtag",
        source: Source::PrivateTag,
        service_name: "ecrtagged",
        binding: Some(Binding {
            group_name: "ecrbacked-tagged",
            listener_port: "80",
            container_port: CONTAINER_PORT,
        }),
        desired: 1,
    },
    Row {
        case: "the negative control: the same ECR image with no loadBalancers block",
        family: "ecrbackedtag",
        source: Source::PrivateTag,
        service_name: "ecrunwired",
        binding: None,
        desired: 1,
    },
    Row {
        case: "the same image addressed by digest, bound on the second container port",
        family: "ecrbackeddigest",
        source: Source::PrivateDigest,
        service_name: "ecrpinned",
        binding: Some(Binding {
            group_name: "ecrbacked-pinned",
            listener_port: "8080",
            container_port: SECOND_CONTAINER_PORT,
        }),
        desired: 1,
    },
    Row {
        case: "a public-registry image, two tasks on the one group the service binds",
        family: "ecrbackedpublic",
        source: Source::PublicRegistry,
        service_name: "ecrpublic",
        binding: Some(Binding {
            group_name: "ecrbacked-public",
            listener_port: "8082",
            container_port: CONTAINER_PORT,
        }),
        desired: 2,
    },
];

/// One emulator carrying IAM, Amazon ECR, Amazon EC2, Elastic Load Balancing v2 and Amazon
/// ECS over ONE shared engine, exactly as the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(engine),
    );
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. The key
/// id and signature are not credentials: the router above runs with request authentication
/// off, so nothing is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Send one form-encoded request to the service `signing_name` addresses, and answer its
/// status and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Drive one ec2Query action that must succeed, and answer its body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Drive one IAM action that must succeed, and answer its body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "iam", form(action, IAM_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Drive one awsQuery action against the ELBv2 model that must succeed, and answer its body.
async fn elbv2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancingv2",
        form(action, ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One `awsJson1_1` RPC call that must succeed, answered with its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    assert_eq!(
        status, 200,
        "setup: {signing_name} {operation} answered {status} with {parsed}"
    );
    parsed
}

/// One Amazon ECS call (`AmazonEC2ContainerServiceV20141113.<Operation>`).
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    rpc(
        router,
        "ecs",
        "AmazonEC2ContainerServiceV20141113",
        operation,
        body,
    )
    .await
}

/// One Amazon ECR call (`AmazonEC2ContainerRegistry_V20150921.<Operation>`).
async fn ecr(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    rpc(
        router,
        "ecr",
        "AmazonEC2ContainerRegistry_V20150921",
        operation,
        body,
    )
    .await
}

/// The network every row runs in: a VPC with an internet gateway, its own MAIN route table
/// sending [`INTERNET_DESTINATION`] to that gateway, two subnets in different zones, and one
/// self-referencing security group the tasks and the load balancer both sit in.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
}

/// Create that network through Amazon EC2 itself, so every id used below is one EC2 minted
/// and the path the image pull takes is one its own records hold.
async fn seed_network(router: &MultiServiceRouter) -> Network {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await,
        "vpcId",
    );
    let gateway = element(
        &ec2(router, "CreateInternetGateway", &[]).await,
        "internetGatewayId",
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
    )
    .await;
    // The default route goes into the table the VPC came with, which every subnet below is
    // implicitly associated with — "your VPC automatically comes with a main route table"
    // (Amazon VPC User Guide, "Route tables") — so no association of this suite's own
    // decides where a task's traffic goes.
    let tables = ec2(
        router,
        "DescribeRouteTables",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc.as_str()),
        ],
    )
    .await;
    let main = items_of(&tables)
        .into_iter()
        .find(|item| elements(item, "main").iter().any(|main| main == "true"))
        .and_then(|item| elements(item, "routeTableId").into_iter().next())
        .unwrap_or_else(|| panic!("the VPC must come with a main route table, got {tables}"));
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", main.as_str()),
            ("DestinationCidrBlock", INTERNET_DESTINATION),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
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
                ("GroupName", "ecrbacked-group"),
                ("GroupDescription", "the tasks and the load balancer"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // The one rule this suite authorizes: the group takes ITSELF as a source on every port,
    // which is what lets the load balancer's check of a target in it arrive.
    ec2(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", group.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", "0"),
            ("IpPermissions.1.ToPort", "65535"),
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

/// Create the execution role Amazon ECS pulls as — trusted by the Amazon ECS tasks principal
/// and carrying the published managed policy — and answer its ARN.
async fn seed_execution_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "ecs-tasks.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", trust.as_str()),
        ],
    )
    .await;
    iam(
        router,
        "AttachRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyArn", EXECUTION_ROLE_POLICY),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Push one image into one repository of this account's registry through Amazon ECR's own
/// API, and answer the two references that address it: `<uri>:<tag>` and `<uri>@<digest>`.
async fn seed_image(router: &MultiServiceRouter) -> (String, String) {
    let created = ecr(
        router,
        "CreateRepository",
        json!({"repositoryName": REPOSITORY}),
    )
    .await;
    let uri = created["repository"]["repositoryUri"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateRepository must report a repositoryUri, got {created}"))
        .to_string();
    let pushed = ecr(
        router,
        "PutImage",
        json!({
            "repositoryName": REPOSITORY,
            "imageManifest": MANIFEST,
            "imageTag": TAG,
        }),
    )
    .await;
    let digest = pushed["image"]["imageId"]["imageDigest"]
        .as_str()
        .unwrap_or_else(|| panic!("PutImage must report an imageDigest, got {pushed}"))
        .to_string();
    (format!("{uri}:{TAG}"), format!("{uri}@{digest}"))
}

/// The reference one row's container runs.
fn image_of(source: Source, tagged: &str, pinned: &str) -> String {
    match source {
        Source::PrivateTag => tagged.to_string(),
        Source::PrivateDigest => pinned.to_string(),
        Source::PublicRegistry => PUBLIC_IMAGE.to_string(),
    }
}

/// Register one row's revision: an `awsvpc`, Fargate-compatible revision pulling `image` as
/// the execution role, whose one container listens on both ports a binding may name.
async fn register_revision(
    router: &MultiServiceRouter,
    family: &str,
    image: &str,
    execution_role: &str,
) {
    ecs(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": execution_role,
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": image,
                "portMappings": [
                    { "containerPort": CONTAINER_PORT, "protocol": "tcp" },
                    { "containerPort": SECOND_CONTAINER_PORT, "protocol": "tcp" },
                ],
            }],
        }),
    )
    .await;
}

/// Create one `ip` target group on `port` of `network`'s VPC, and answer its ARN.
async fn target_group(
    router: &MultiServiceRouter,
    network: &Network,
    name: &str,
    port: i64,
) -> String {
    let created = elbv2(
        router,
        "CreateTargetGroup",
        &[
            ("Name", name),
            ("Protocol", "HTTP"),
            ("Port", &port.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", "/"),
        ],
    )
    .await;
    opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("no TargetGroupArn for {name} in {created}"))
}

/// Create the ECS service one row describes: its tasks across both subnets, in the one
/// security group, with the public address that makes the internet gateway its subnets route
/// through a path to the image's registry — bound to `group` where the row declares a
/// binding, and to nothing at all where it does not.
async fn create_service(
    router: &MultiServiceRouter,
    network: &Network,
    row: &Row,
    group: Option<&String>,
) {
    let mut request = json!({
        "cluster": CLUSTER,
        "serviceName": row.service_name,
        "taskDefinition": row.family,
        "desiredCount": row.desired,
        "launchType": "FARGATE",
        "networkConfiguration": {
            "awsvpcConfiguration": {
                "subnets": network.subnets,
                "securityGroups": [network.group],
                "assignPublicIp": "ENABLED",
            }
        },
    });
    if let (Some(binding), Some(group)) = (row.binding.as_ref(), group) {
        request["loadBalancers"] = json!([{
            "targetGroupArn": group,
            "containerName": CONTAINER,
            "containerPort": binding.container_port,
        }]);
    }
    let created = ecs(router, "CreateService", request).await;
    assert_eq!(
        created["service"]["desiredCount"].as_i64(),
        Some(row.desired),
        "case {:?}: the stored service must carry the count the request asked for, got \
         {created}",
        row.case
    );
}

/// Every `(address, port)` the group `arn` reports a target at.
async fn registered(router: &MultiServiceRouter, arn: &str) -> BTreeSet<(String, String)> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", arn)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|description| {
            let text = |tag: &str| {
                elements(description, tag)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            };
            let id = text("Id");
            if id.is_empty() {
                return None;
            }
            Some((id, text("Port")))
        })
        .collect()
}

/// The private IPv4 addresses of the RUNNING tasks of one row's service, read out of Amazon
/// ECS, beside the stopped reasons of any task of it that did not come up — so a row whose
/// image was not served says so instead of reporting an empty set.
async fn task_addresses(router: &MultiServiceRouter, row: &Row) -> (BTreeSet<String>, Vec<String>) {
    let listed = ecs(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name,
            "desiredStatus": "RUNNING",
        }),
    )
    .await;
    let arns: Vec<String> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect();
    if arns.is_empty() {
        return (BTreeSet::new(), stopped_reasons(router, row).await);
    }
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
    )
    .await;
    let addresses = described["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|task| task["lastStatus"].as_str() == Some("RUNNING"))
        .filter_map(|task| {
            task["attachments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|attachment| attachment["details"].as_array())
                .flatten()
                .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
                .and_then(|detail| detail["value"].as_str())
                .map(str::to_string)
        })
        .collect();
    (addresses, stopped_reasons(router, row).await)
}

/// `Task$stoppedReason` of every stopped task of one row's service, which is where a fetch a
/// registry withheld is reported.
async fn stopped_reasons(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let listed = ecs(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name,
            "desiredStatus": "STOPPED",
        }),
    )
    .await;
    let arns: Vec<String> = listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
    let described = ecs(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
    )
    .await;
    described["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|task| task["stoppedReason"].as_str().map(str::to_string))
        .collect()
}

/// What one row's service reports about the tasks it holds: its own counts, and the rollout
/// state of the deployment rolling them out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Converged {
    running_count: i64,
    pending_count: i64,
    rollout_state: String,
}

/// What `DescribeServices` reports for one row's service now.
async fn signals(router: &MultiServiceRouter, row: &Row) -> Converged {
    let described = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service_name] }),
    )
    .await;
    let service = &described["services"][0];
    // "PRIMARY": the deployment rolling the service's current revision out, which is the one
    // a client waits on.
    let primary = service["deployments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|deployment| deployment["status"].as_str() == Some("PRIMARY"))
        .cloned()
        .unwrap_or(Value::Null);
    Converged {
        running_count: service["runningCount"].as_i64().unwrap_or(-1),
        pending_count: service["pendingCount"].as_i64().unwrap_or(-1),
        rollout_state: primary["rolloutState"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    }
}

/// The terminal state every row's service must settle in: it runs the tasks it wants, none is
/// still coming up, and its `PRIMARY` deployment has reached the rollout state a service that
/// reached a steady state reports.
fn settled(row: &Row, reported: &Converged) -> bool {
    *reported
        == Converged {
            running_count: row.desired,
            pending_count: 0,
            rollout_state: ROLLOUT_COMPLETED.to_string(),
        }
}

/// The `(address, port)` set the group `arn` reports once it reports `expected`, polled
/// rather than waited for; answers what it last read at the deadline otherwise, for the
/// caller's own assertion to quote.
async fn reaching(
    router: &MultiServiceRouter,
    arn: &str,
    expected: &BTreeSet<(String, String)>,
) -> BTreeSet<(String, String)> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = registered(router, arn).await;
        if reported == *expected || Instant::now() >= deadline {
            return reported;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// RULE: an ECS service's running tasks are targets of exactly the groups its own
/// `loadBalancers` bindings name, at the address and port the placement and the binding
/// decide — whatever registry the image they run was fetched from.
#[tokio::test]
async fn an_ecr_image_backed_services_tasks_are_targets_of_the_groups_it_binds() {
    let router = emulator();
    let network = seed_network(&router).await;
    let execution_role = seed_execution_role(&router).await;
    let (tagged, pinned) = seed_image(&router).await;

    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    // One revision per family a row names; the two rows that share a family share its one
    // revision, which is what makes the negative row otherwise identical to the first.
    let mut families: Vec<&str> = Vec::new();
    for row in &ROWS {
        if families.contains(&row.family) {
            continue;
        }
        register_revision(
            &router,
            row.family,
            &image_of(row.source, &tagged, &pinned),
            &execution_role,
        )
        .await;
        families.push(row.family);
    }

    // The group NOTHING references: created first, forwarded to by no listener, bound by no
    // service.
    let unreferenced = target_group(&router, &network, UNREFERENCED_GROUP, CONTAINER_PORT).await;

    let load_balancer = element(
        &elbv2(
            &router,
            "CreateLoadBalancer",
            &[
                ("Name", "ecrbacked-alb"),
                ("Subnets.member.1", network.subnets[0].as_str()),
                ("Subnets.member.2", network.subnets[1].as_str()),
                ("SecurityGroups.member.1", network.group.as_str()),
                ("Scheme", "internet-facing"),
            ],
        )
        .await,
        "LoadBalancerArn",
    );

    // Every row's group, listener and service. The services are created back to back, so the
    // tasks of all of them come up together.
    let mut groups: Vec<Option<String>> = Vec::new();
    for row in &ROWS {
        let group = match row.binding.as_ref() {
            Some(binding) => {
                let group =
                    target_group(&router, &network, binding.group_name, binding.container_port)
                        .await;
                // A listener forwarding to the group is what makes it one the load balancer
                // forwards to — and what lets an ECS service bind it at all.
                elbv2(
                    &router,
                    "CreateListener",
                    &[
                        ("LoadBalancerArn", load_balancer.as_str()),
                        ("Protocol", "HTTP"),
                        ("Port", binding.listener_port),
                        ("DefaultActions.member.1.Type", "forward"),
                        ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
                    ],
                )
                .await;
                Some(group)
            }
            None => None,
        };
        create_service(&router, &network, row, group.as_ref()).await;
        groups.push(group);
    }

    // Wait for the CONDITION every row's service converges on, reading the unreferenced group
    // on every pass: its emptiness has to hold throughout the window the tasks are coming up
    // and being registered in, not only once afterwards.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let leaked = registered(&router, &unreferenced).await;
        assert!(
            leaked.is_empty(),
            "case {UNREFERENCED_GROUP:?}: a target group no service's loadBalancers names must \
             hold no registration, got {leaked:?}"
        );
        let mut reported = Vec::new();
        for row in &ROWS {
            reported.push(signals(&router, row).await);
        }
        if reported
            .iter()
            .zip(&ROWS)
            .all(|(reported, row)| settled(row, reported))
        {
            break;
        }
        if Instant::now() >= deadline {
            let mut reasons = Vec::new();
            for row in &ROWS {
                reasons.push((row.case, stopped_reasons(&router, row).await));
            }
            panic!(
                "every service must run the tasks it wants and its PRIMARY deployment reach a \
                 terminal rollout; got {reported:?} against the wanted counts {:?}, with the \
                 stopped tasks reporting {reasons:?}",
                ROWS.iter().map(|row| row.desired).collect::<Vec<_>>()
            );
        }
        ::tokio::time::sleep(POLL).await;
    }

    // Each row's own group holds exactly its own tasks' addresses on the bound port, and the
    // service that declares no binding is registered nowhere at all.
    let mut unwired_addresses: BTreeSet<String> = BTreeSet::new();
    let mut bound: Vec<(&Row, String, BTreeSet<String>)> = Vec::new();
    for (row, group) in ROWS.iter().zip(&groups) {
        let (addresses, reasons) = task_addresses(&router, row).await;
        assert_eq!(
            addresses.len(),
            usize::try_from(row.desired).unwrap_or_default(),
            "case {:?}: expected {} running task(s), each at its own interface's address, got \
             {addresses:?} with the stopped tasks reporting {reasons:?}",
            row.case,
            row.desired
        );
        match group {
            Some(group) => bound.push((row, group.clone(), addresses)),
            None => unwired_addresses.extend(addresses),
        }
    }

    for (row, group, addresses) in &bound {
        let binding = row
            .binding
            .as_ref()
            .expect("a bound row declares its binding");
        let expected: BTreeSet<(String, String)> = addresses
            .iter()
            .map(|address| (address.clone(), binding.container_port.to_string()))
            .collect();
        let reported = reaching(&router, group, &expected).await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected the bound group to hold exactly {expected:?}, got {reported:?}",
            row.case
        );
    }

    // Nothing holds a target of the service that declared no binding — neither the group
    // nothing references nor any group another service bound.
    let mut every_group: Vec<String> = vec![unreferenced.clone()];
    every_group.extend(bound.iter().map(|(_, group, _)| group.clone()));
    for group in &every_group {
        let reported = registered(&router, group).await;
        for address in &unwired_addresses {
            assert!(
                !reported.iter().any(|(id, _)| id == address),
                "case {:?}: the task of a service with no loadBalancers block is a target of no \
                 group, yet {group} reports {reported:?}",
                ROWS[1].case
            );
        }
    }
    let leaked = registered(&router, &unreferenced).await;
    assert!(
        leaked.is_empty(),
        "case {UNREFERENCED_GROUP:?}: a target group no service's loadBalancers names must hold \
         no registration, got {leaked:?}"
    );
}
