// SPDX-License-Identifier: CC0-1.0

//! Rolling a load-balanced Amazon ECS service onto a revision whose IMAGE is the only thing
//! changed replaces its tasks and re-registers the replacements in the SAME target group,
//! where the group's own checks report them healthy.
//!
//! RULE: a deployment the ECS scheduler rolls out — "whenever you create or deploy a service,
//! the ECS service scheduler creates a deployment", and `UpdateService` "modifies the
//! parameters of a service", for which `UpdateServiceRequest$taskDefinition` is "the family and
//! revision ... or full ARN of the task definition to run in your service" — ends with the
//! service running its new revision's tasks and NONE of the replaced revision's: each new task
//! is registered as a target of every group the service's own `loadBalancers` bindings name
//! (`LoadBalancer$targetGroupArn`, at the address its `awsvpc` interface was given and the
//! `containerPort` the binding states), reaches the state the group's own health checks decide
//! for it, and the replaced tasks are stopped and leave that group. The binding and the group
//! are untouched by the roll: what changes is which tasks are registered in it, never which
//! group is registered into.
//!
//! The rows differ along what the rule generalises over: how many tasks the service rolls (one
//! and two, so a row where the replacement overlaps the replaced), and WHAT the new revision's
//! reference addresses — another tag of the same pull through cache repository, a cache
//! repository of another upstream image, and the upstream reference itself, which names no
//! repository of this account at all. The recorded client's deploy — one task, an ECR-sourced
//! image behind an Application Load Balancer, rolled to a new image tag — is the first row.
//!
//! Five crates have to agree for the rule to hold, which is why this suite lives here: IAM holds
//! the execution role Amazon ECS pulls as, Amazon ECR holds the pull through cache rule and the
//! repositories the first pull of each reference creates, Amazon EC2 owns the network the tasks
//! are placed in and the path each pull takes, Amazon ECS owns the deployment, the tasks and
//! their interfaces' addresses, and Elastic Load Balancing v2 holds the registrations and the
//! checks they are reported from. Everything is driven through the real operations over ONE
//! shared engine, so what is asserted is what each service persisted.
//!
//! Every clock-owed move — a task's start, the registration it owes, the verdict the group's
//! cadence reaches, the stop the roll makes — is POLLED for until it holds, never slept for, and
//! each deadline is generous: a row asserts that the move HAPPENS, not how fast this host runs.

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
const CLUSTER: &str = "rolledimages";
const CONTAINER: &str = "app";

/// The one port every row's container declares a `portMapping` for, which is the port both web
/// server images' shipped configuration listens on, the port each binding names and each target
/// group's own.
const CONTAINER_PORT: i64 = 80;

/// The upstream registry the cache rule below caches from, which is the one public registry this
/// emulator serves.
const UPSTREAM_REGISTRY: &str = "public.ecr.aws";

/// The `ecrRepositoryPrefix` of the rule that caches that registry's whole namespace, under
/// which every cached reference below resolves.
const RULE_PREFIX: &str = "rolled";

/// The execution role Amazon ECS pulls as, and the managed policy AWS publishes for one
/// ("AmazonECSTaskExecutionRolePolicy", the task execution role policy of the Amazon ECS
/// Developer Guide) — attached rather than hand-written, so each pull is decided over the
/// document IAM really holds for that role.
const EXECUTION_ROLE: &str = "rolledimagesexecution";
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The network every row runs in.
const VPC_CIDR: &str = "10.93.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.93.1.0/24", "us-east-1a"),
    ("10.93.2.0/24", "us-east-1b"),
];

/// The health-check destination every group checks, which is the root both images' own
/// configuration serves.
const SERVED_PATH: &str = "/";

/// The `HealthCheckIntervalSeconds` and the threshold counts every target group is created with:
/// a verdict takes a threshold of consecutive checks, each one interval after the last, so each
/// row asks for the shortest interval the member admits ("the range is 5-300") and the fewest
/// checks a threshold may ask for ("the range is 2-10").
const INTERVAL_SECONDS: &str = "5";
const THRESHOLD: &str = "2";

/// The drain every group below is configured with, and the attribute that configures it: "the
/// amount of time, in seconds, for Elastic Load Balancing to wait before changing the state of a
/// deregistering target from `draining` to `unused`. The range is 0-3600 seconds. The default
/// value is 300 seconds." (`TargetGroupAttribute$Key`). A drain of no time is what makes the
/// DEPARTURE of a replaced task observable without waiting out that default — the departure
/// itself is the rule here, not how long a drain takes, which
/// `ecs_scale_in_drains_its_registered_target.rs` pins over the whole documented range.
const DELAY_ATTRIBUTE: &str = "deregistration_delay.timeout_seconds";
const NO_DRAIN: &str = "0";

/// The `TargetHealth$State` a target the load balancer sends requests to is reported in.
const HEALTHY: &str = "healthy";

/// `Task$lastStatus` of a task that is running, and the `desiredStatus` each listing asks for.
const RUNNING: &str = "RUNNING";
const STOPPED: &str = "STOPPED";

/// How long a clock-owed move is waited for before a row gives up, and how often it looks again.
/// Generous on purpose: the deadline is here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// One reference into a pull through cache repository under [`RULE_PREFIX`]: the upstream path
/// beneath that prefix, and the tag the reference asks for.
#[derive(Debug, Clone, Copy)]
struct Cached {
    upstream_path: &'static str,
    tag: &'static str,
}

/// What the revision a row rolls ONTO addresses.
#[derive(Debug, Clone, Copy)]
enum Reference {
    /// A reference into a cache repository under the rule's prefix — the same repository under
    /// another tag, or another upstream image's.
    Cached(Cached),
    /// The upstream reference itself, which names no repository of this account.
    Upstream(&'static str),
}

/// One row: the service it deploys, how many tasks it rolls, and the two revisions it rolls
/// between.
struct Row {
    case: &'static str,
    family: &'static str,
    service_name: &'static str,
    group_name: &'static str,
    load_balancer_name: &'static str,
    /// How many tasks the service runs, which is how many registrations its group holds both
    /// before and after the roll.
    desired: i64,
    /// What the revision the service is created on addresses.
    first: Cached,
    /// What the revision the roll moves it onto addresses instead.
    next: Reference,
}

const ROWS: [Row; 3] = [
    Row {
        case: "one task, rolled to another tag of the same cache repository",
        family: "rollednginxtag",
        service_name: "rollednginxtag",
        group_name: "rolled-nginx-tag",
        load_balancer_name: "rolled-nginx-tag-lb",
        desired: 1,
        first: Cached {
            upstream_path: "docker/library/nginx",
            tag: "alpine",
        },
        next: Reference::Cached(Cached {
            upstream_path: "docker/library/nginx",
            tag: "stable",
        }),
    },
    Row {
        case: "two tasks, rolled onto a cache repository of another upstream image",
        family: "rolledtwotasks",
        service_name: "rolledtwotasks",
        group_name: "rolled-two-tasks",
        load_balancer_name: "rolled-two-tasks-lb",
        desired: 2,
        first: Cached {
            upstream_path: "docker/library/nginx",
            tag: "alpine",
        },
        next: Reference::Cached(Cached {
            upstream_path: "docker/library/httpd",
            tag: "2.4",
        }),
    },
    Row {
        case: "one task, rolled from the cached reference to the upstream reference itself",
        family: "rolledupstream",
        service_name: "rolledupstream",
        group_name: "rolled-upstream",
        load_balancer_name: "rolled-upstream-lb",
        desired: 1,
        first: Cached {
            upstream_path: "docker/library/httpd",
            tag: "2.4",
        },
        next: Reference::Upstream("public.ecr.aws/docker/library/nginx:alpine"),
    },
];

/// One emulator carrying IAM, Amazon ECR, Amazon EC2, Elastic Load Balancing v2 and Amazon ECS
/// over ONE shared engine, exactly as the production router builds them.
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

/// A SigV4 credential scope naming `signing_name`, which is what routes the request. The key id
/// and signature are not credentials: the router above runs with request authentication off, so
/// nothing is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Send one form-encoded request to the service `signing_name` addresses, and answer its status
/// and body.
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
        "ELBv2 {action} must succeed, got {status} with body {body}"
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
        "{signing_name} {operation} answered {status} with {parsed}"
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
/// sending the default route to that gateway, two subnets in different zones, and one
/// self-referencing security group the tasks and the load balancers both sit in.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
}

/// Create that network through Amazon EC2 itself, so every id used below is one EC2 minted and
/// the path each image pull takes is one its own records hold.
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
    // (Amazon VPC User Guide, "Route tables") — so no association of this suite's own decides
    // where a task's traffic goes.
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
            ("DestinationCidrBlock", "0.0.0.0/0"),
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
                ("GroupName", "rolledimages-group"),
                ("GroupDescription", "the tasks and the load balancers"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // The one rule this suite authorizes: the group takes ITSELF as a source on every port,
    // which is what lets a load balancer's check of a target in it arrive.
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

/// Create the execution role Amazon ECS pulls as — trusted by the Amazon ECS tasks principal and
/// carrying the published managed policy — and answer its ARN.
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

/// Register the ONE pull through cache rule every cached reference below resolves through, and
/// answer the HOST of this account's registry — the host such a reference carries, read off what
/// Amazon ECR itself publishes for this account rather than built here.
///
/// No repository is created: a cache repository is created by the FIRST pull of a reference under
/// the rule's prefix, which is what each row's deployment exercises.
async fn seed_registry(router: &MultiServiceRouter) -> String {
    let created = ecr(
        router,
        "CreatePullThroughCacheRule",
        json!({
            "ecrRepositoryPrefix": RULE_PREFIX,
            "upstreamRegistryUrl": UPSTREAM_REGISTRY,
        }),
    )
    .await;
    assert_eq!(
        created["ecrRepositoryPrefix"].as_str(),
        Some(RULE_PREFIX),
        "setup: the stored rule must carry the prefix the request asked for, got {created}"
    );
    let described = ecr(router, "DescribeRegistry", json!({})).await;
    let registry = described["registryId"]
        .as_str()
        .unwrap_or_else(|| panic!("DescribeRegistry must report a registryId, got {described}"));
    format!("{registry}.dkr.ecr.{REGION}.amazonaws.com")
}

/// The reference one revision's container runs.
fn image_of(reference: Reference, registry_host: &str) -> String {
    match reference {
        Reference::Cached(cached) => cached_reference(cached, registry_host),
        Reference::Upstream(upstream) => upstream.to_string(),
    }
}

/// The reference into the cache repository `cached` names: `<registry>/<prefix>/<path>:<tag>`.
fn cached_reference(cached: Cached, registry_host: &str) -> String {
    format!(
        "{registry_host}/{RULE_PREFIX}/{}:{}",
        cached.upstream_path, cached.tag
    )
}

/// Register one revision of `family`: an `awsvpc`, Fargate-compatible revision pulling `image` as
/// the execution role, whose one container declares the port the binding names and NO command of
/// its own — so what it runs is the command its image declares. Answers the revision's own ARN,
/// which is what a task of it reports and what the roll below asks for by name.
async fn register_revision(
    router: &MultiServiceRouter,
    family: &str,
    image: &str,
    execution_role: &str,
) -> String {
    let registered = ecs(
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
                ],
            }],
        }),
    )
    .await;
    assert_eq!(
        registered["taskDefinition"]["containerDefinitions"][0]["image"].as_str(),
        Some(image),
        "setup: the stored revision of {family} must carry the image the request asked for, got \
         {registered}"
    );
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("RegisterTaskDefinition must report an ARN, got {registered}"))
        .to_string()
}

/// Create one row's `ip` target group, drained with no delay, its internet-facing load balancer
/// and the listener that forwards to that group; answer the group's ARN.
async fn front_door(router: &MultiServiceRouter, network: &Network, row: &Row) -> String {
    let created = elbv2(
        router,
        "CreateTargetGroup",
        &[
            ("Name", row.group_name),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", SERVED_PATH),
            ("HealthCheckIntervalSeconds", INTERVAL_SECONDS),
            ("HealthyThresholdCount", THRESHOLD),
            ("UnhealthyThresholdCount", THRESHOLD),
        ],
    )
    .await;
    let group = opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("case {:?}: no TargetGroupArn in {created}", row.case));
    elbv2(
        router,
        "ModifyTargetGroupAttributes",
        &[
            ("TargetGroupArn", group.as_str()),
            ("Attributes.member.1.Key", DELAY_ATTRIBUTE),
            ("Attributes.member.1.Value", NO_DRAIN),
        ],
    )
    .await;
    let balanced = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", row.load_balancer_name),
            ("Type", "application"),
            ("Scheme", "internet-facing"),
            ("Subnets.member.1", network.subnets[0].as_str()),
            ("Subnets.member.2", network.subnets[1].as_str()),
            ("SecurityGroups.member.1", network.group.as_str()),
        ],
    )
    .await;
    let load_balancer = element(&balanced, "LoadBalancerArn");
    elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer.as_str()),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", group.as_str()),
        ],
    )
    .await;
    group
}

/// Create the ECS service one row describes: its tasks across both subnets, in the one security
/// group, with the public address that makes the internet gateway its subnets route through a
/// path to the image's registry, bound to `group` on the container port.
async fn create_service(
    router: &MultiServiceRouter,
    network: &Network,
    row: &Row,
    revision: &str,
    group: &str,
) {
    let created = ecs(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name,
            "taskDefinition": revision,
            "desiredCount": row.desired,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": network.subnets,
                    "securityGroups": [network.group],
                    "assignPublicIp": "ENABLED",
                }
            },
            "loadBalancers": [{
                "targetGroupArn": group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
        }),
    )
    .await;
    assert_eq!(
        created["service"]["desiredCount"].as_i64(),
        Some(row.desired),
        "case {:?}: the stored service must carry the count the request asked for, got {created}",
        row.case
    );
}

/// The `(Id, Port, State)` of every registration `group` reports.
async fn health(router: &MultiServiceRouter, group: &str) -> Vec<(String, String, String)> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", group)]).await;
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
            Some((id, text("Port"), text("State")))
        })
        .collect()
}

/// The ARNs of the tasks of one service that `desired_status` names.
async fn task_arns(
    router: &MultiServiceRouter,
    service: &str,
    desired_status: &str,
) -> Vec<String> {
    let listed = ecs(
        router,
        "ListTasks",
        json!({
            "cluster": CLUSTER,
            "serviceName": service,
            "desiredStatus": desired_status,
        }),
    )
    .await;
    listed["taskArns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect()
}

/// Every RUNNING task of one service as `(the address its interface was given, the revision it
/// runs)`, read out of Amazon ECS itself.
async fn running_tasks(router: &MultiServiceRouter, service: &str) -> Vec<(String, String)> {
    let arns = task_arns(router, service, RUNNING).await;
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
        .filter(|task| task["lastStatus"].as_str() == Some(RUNNING))
        .filter_map(|task| {
            let address = task["attachments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|attachment| attachment["details"].as_array())
                .flatten()
                .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
                .and_then(|detail| detail["value"].as_str())?;
            let revision = task["taskDefinitionArn"].as_str()?;
            Some((address.to_string(), revision.to_string()))
        })
        .collect()
}

/// Every STOPPED task of one service as `(the revision it ran, the reason it was stopped)` — the
/// reason being where a fetch a registry withheld is reported.
async fn stopped_tasks(router: &MultiServiceRouter, service: &str) -> Vec<(String, String)> {
    let arns = task_arns(router, service, STOPPED).await;
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
        .map(|task| {
            (
                task["taskDefinitionArn"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                task["stoppedReason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Wait until one row's service runs exactly its wanted count of tasks of `revision` and its
/// group holds exactly those tasks' addresses, each reported [`HEALTHY`] on the bound container
/// port — and answer that address set.
///
/// Polls the CONDITION: every move it waits on (the task's start, the registration that start
/// owes, the verdict the group's cadence reaches, the departure of a task the roll replaced) is
/// one the clock owes, so nothing here sleeps for a span.
async fn converged(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    group: &str,
    revision: &str,
) -> BTreeSet<String> {
    let wanted = usize::try_from(row.desired).unwrap_or_default();
    let port = CONTAINER_PORT.to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let tasks = running_tasks(router, row.service_name).await;
        let on_revision: BTreeSet<String> = tasks
            .iter()
            .filter(|(_, ran)| ran == revision)
            .map(|(address, _)| address.clone())
            .collect();
        let reported = health(router, group).await;
        let held: BTreeSet<String> = reported
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect();
        let healthy: BTreeSet<String> = reported
            .iter()
            .filter(|(_, at, state)| at == &port && state == HEALTHY)
            .map(|(id, _, _)| id.clone())
            .collect();
        if tasks.len() == wanted && on_revision.len() == wanted && held == on_revision && healthy == on_revision {
            return on_revision;
        }
        if Instant::now() >= deadline {
            panic!(
                "case {case:?}: expected {wanted} task(s) of {revision} running and exactly their \
                 addresses reported {HEALTHY:?} on port {port} of {group}, got the running tasks \
                 {tasks:?} against the registrations {reported:?}, with the stopped tasks \
                 reporting {:?}",
                stopped_tasks(router, row.service_name).await
            );
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// The target group ARN one row's service is BOUND to, and the revision it reports running, as
/// `DescribeServices` answers them.
async fn bound(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let described = ecs(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service_name] }),
    )
    .await;
    let service = &described["services"][0];
    (
        service["loadBalancers"][0]["targetGroupArn"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        service["taskDefinition"].as_str().unwrap_or_default().to_string(),
    )
}

/// The ARN Elastic Load Balancing v2 answers for the group NAMED `name`, so the group the roll
/// left behind is addressed the way a client that only knows its name addresses it.
async fn group_named(router: &MultiServiceRouter, name: &str) -> String {
    let described = elbv2(router, "DescribeTargetGroups", &[("Names.member.1", name)]).await;
    opt_element(&described, "TargetGroupArn")
        .unwrap_or_else(|| panic!("no TargetGroupArn for {name} in {described}"))
}

/// RULE: rolling a load-balanced service onto a revision whose image is the only thing changed
/// replaces its tasks with tasks of that revision, registers THOSE in the same target group
/// until the group's checks report them healthy, and takes the replaced tasks out of it.
#[tokio::test]
async fn a_rolled_image_re_registers_the_services_tasks_on_the_same_group() {
    let router = emulator();
    let network = seed_network(&router).await;
    let execution_role = seed_execution_role(&router).await;
    let registry_host = seed_registry(&router).await;

    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    // Every row's first revision, front door and service. The services are created back to back,
    // so the tasks of all of them come up together.
    let mut deployed: Vec<(String, String)> = Vec::new();
    for row in &ROWS {
        let revision = register_revision(
            &router,
            row.family,
            &cached_reference(row.first, &registry_host),
            &execution_role,
        )
        .await;
        let group = front_door(&router, &network, row).await;
        create_service(&router, &network, row, &revision, &group).await;
        deployed.push((revision, group));
    }

    // The deploy: every row's service converges on the group it binds.
    let mut before: Vec<BTreeSet<String>> = Vec::new();
    for (row, (revision, group)) in ROWS.iter().zip(&deployed) {
        let case = format!("{}: the deploy", row.case);
        before.push(converged(&router, &case, row, group, revision).await);
    }

    // The roll: the same estate with nothing but the container's image reference changed. Every
    // row's update is made back to back too, so the rollouts overlap.
    let mut rolled: Vec<String> = Vec::new();
    for (row, (_, group)) in ROWS.iter().zip(&deployed) {
        let revision = register_revision(
            &router,
            row.family,
            &image_of(row.next, &registry_host),
            &execution_role,
        )
        .await;
        let updated = ecs(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": row.service_name,
                "taskDefinition": revision,
            }),
        )
        .await;
        assert_eq!(
            updated["service"]["taskDefinition"].as_str(),
            Some(revision.as_str()),
            "case {:?}: the update must move the service onto the revision it named, got {updated}",
            row.case
        );
        // The update changes which tasks run, not what routes to them.
        assert_eq!(
            updated["service"]["loadBalancers"][0]["targetGroupArn"].as_str(),
            Some(group.as_str()),
            "case {:?}: the update must keep the binding the create made, got {updated}",
            row.case
        );
        rolled.push(revision);
    }

    for ((row, (first, group)), next) in ROWS.iter().zip(&deployed).zip(&rolled) {
        let case = format!("{}: the roll", row.case);
        let after = converged(&router, &case, row, group, next).await;

        // The replaced tasks are gone from the group: an address that served the first revision
        // and is not one of the new tasks' is a target of nothing.
        let index = ROWS
            .iter()
            .position(|other| other.service_name == row.service_name)
            .expect("every row is in ROWS");
        let departed: Vec<&String> = before[index].difference(&after).collect();
        let held: Vec<String> = health(&router, group)
            .await
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        for address in &departed {
            assert!(
                !held.contains(address),
                "case {case:?}: the task the roll replaced must leave the group, yet {address} is \
                 still reported among {held:?}"
            );
        }

        // The roll really replaced the first revision's tasks rather than leaving them running:
        // ECS holds them stopped, on the revision they ran.
        let stopped = stopped_tasks(&router, row.service_name).await;
        assert!(
            stopped.iter().any(|(ran, _)| ran == first),
            "case {case:?}: expected the tasks of {first} to be stopped by the roll, got the \
             stopped tasks {stopped:?} against the running {:?}",
            running_tasks(&router, row.service_name).await
        );

        // The binding and the group are the ones the create made: the service still names that
        // group, the group still answers to its own name with the same ARN, and the service
        // reports the revision the roll moved it onto.
        let (bound_group, reported_revision) = bound(&router, row).await;
        assert_eq!(
            bound_group, *group,
            "case {case:?}: the service must still bind the group the create made, got \
             {bound_group}"
        );
        assert_eq!(
            reported_revision, *next,
            "case {case:?}: the service must report the revision the roll moved it onto, got \
             {reported_revision}"
        );
        assert_eq!(
            group_named(&router, row.group_name).await,
            *group,
            "case {case:?}: the group the roll left behind must be the one the create made"
        );
    }
}
