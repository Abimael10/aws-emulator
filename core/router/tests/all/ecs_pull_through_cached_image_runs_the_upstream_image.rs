// SPDX-License-Identifier: CC0-1.0

//! What a container of an image held in an Amazon ECR PULL THROUGH CACHE repository runs, as the
//! load balancer in front of the Amazon ECS task running it sees it.
//!
//! RULE: **a pull through cache repository holds the UPSTREAM image the rule covering it caches
//! ("a way to cache images from an upstream registry source in your Amazon ECR private
//! registry", `CreatePullThroughCacheRule`), so a reference into such a repository names that
//! upstream image: everything a container runtime reads OFF the image — the command the image
//! declares for a container whose definition states none, and the files the image ships — is the
//! upstream image's, exactly as it is for a container addressing the upstream reference itself.
//! A container of a web server image therefore answers the root of the directory its shipped
//! configuration serves with the page that image ships, whichever of the two references its task
//! definition carries; a repository that holds an image of its OWN is no claim about any upstream
//! image, so nothing is read off it and such a container serves nothing.**
//!
//! The grounds. `ContainerOverride$command` is "the command to send to the container that
//! overrides the default command from the Docker image or the task definition", so an image HAS a
//! command of its own and a container whose revision states none runs it; a web server image's
//! documented command is an invocation of its own server over its own shipped configuration, and
//! such an image provides the directory that configuration serves with a page inside it. WHICH
//! image a cached reference names is the registry's answer: the import stores the upstream image
//! under the reference the rule resolves it to, so the two references name one image.
//!
//! The rows differ along what the rule generalises over: WHICH upstream repository the cached
//! reference resolves to (the gallery's mirror of a Docker official image, the publisher's own
//! repository of the same server, and another server's image), which TAG it asks for (a variant,
//! the moving `latest`, and a release family), whether the covering rule prepends an
//! `upstreamRepositoryPrefix` of its own, and whether the repository caches an upstream image at
//! all. The fourth row is the control: the upstream reference itself, with no cache repository in
//! between. The fifth is the negative one: a repository of this registry nothing caches into.
//!
//! Five crates have to agree for the rule to hold, which is why this suite lives here: IAM holds
//! the execution role Amazon ECS pulls as, Amazon ECR holds the cache rules and the repositories
//! the first pull of each reference creates, Amazon EC2 owns the network the tasks are placed in
//! and the path each pull takes, Amazon ECS runs the container and records the server its program
//! started at its task's own address, and Elastic Load Balancing v2 registers the task, checks it
//! and answers the client. Everything is driven through the real operations over ONE shared
//! engine, so what is asserted is what each service persisted.
//!
//! Every clock-owed move — the task's start, the registration it owes, the verdict the group's
//! cadence reaches — is POLLED for until it holds, never slept for, and each deadline is
//! generous: a row asserts that the move HAPPENS, not how fast this host runs.

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
const CLUSTER: &str = "cachedimages";
const CONTAINER: &str = "app";

/// The one port every row's container declares a `portMapping` for, which is the port both web
/// server images' shipped configuration listens on, the port each binding names and each target
/// group's own.
const CONTAINER_PORT: i64 = 80;

/// The upstream registry both cache rules below cache from, which is the one public registry this
/// emulator serves.
const UPSTREAM_REGISTRY: &str = "public.ecr.aws";

/// The `ecrRepositoryPrefix` of the rule that caches the upstream registry's whole namespace, and
/// of the one that prepends an `upstreamRepositoryPrefix` of its own instead.
const ROOT_RULE_PREFIX: &str = "mirror";
const PREFIXED_RULE_PREFIX: &str = "cached/docker";

/// The `upstreamRepositoryPrefix` the second rule prepends: "the repository name prefix to use
/// when caching images from the source registry" on the UPSTREAM side, so a reference under the
/// rule's own prefix resolves beneath it.
const UPSTREAM_PREFIX: &str = "docker/library";

/// The repository of this account's registry the negative row pulls from, and the tag its image is
/// pushed under: a repository that holds an image of its own, which no rule caches into.
const OWN_REPOSITORY: &str = "plain/app";
const OWN_TAG: &str = "release";

/// A Docker image manifest V2 schema 2 referencing no blob, pushed into that repository: what the
/// row depends on is that the registry HOLDS an image of its own there, not which blobs it is
/// built from — so the layer array its media type requires is there and empty.
const MANIFEST: &str = concat!(
    r#"{"schemaVersion":2,"#,
    r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json","#,
    r#""layers":[]}"#
);

/// The execution role Amazon ECS pulls as, and the managed policy AWS publishes for one
/// ("AmazonECSTaskExecutionRolePolicy", the task execution role policy of the Amazon ECS Developer
/// Guide) — attached rather than hand-written, so each pull is decided over the document IAM
/// really holds for that role.
const EXECUTION_ROLE: &str = "cachedimagesexecution";
const EXECUTION_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy";

/// The network every row runs in.
const VPC_CIDR: &str = "10.84.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.84.1.0/24", "us-east-1a"),
    ("10.84.2.0/24", "us-east-1b"),
];

/// The health-check destination every group below checks, which is the root the images' own
/// configuration serves, and the path the client asks for.
const SERVED_PATH: &str = "/";

/// The `HealthCheckIntervalSeconds` and the threshold counts every target group is created with: a
/// verdict takes a threshold of consecutive checks, each one interval after the last, so each row
/// asks for the shortest interval the member admits ("the range is 5-300") and the fewest checks a
/// threshold may ask for ("the range is 2-10").
const INTERVAL_SECONDS: &str = "5";
const THRESHOLD: &str = "2";

/// The `TargetHealth$State` every row's registration settles at.
const HEALTHY: &str = "healthy";

/// How long a clock-owed move is waited for before a row gives up, and how often it looks again.
/// Generous on purpose: the deadline is here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// The pages the two web server images ship at the root of the directory each serves, as the
/// distribution of each ships it
/// (`service_common::static_web_servers::WEB_SERVER_PACKAGES`, whose `default_document` carries
/// the whole page — a fragment of each is asserted here).
const NGINX_PAGE: &str = "Welcome to nginx!";
const HTTPD_PAGE: &str = "It works!";

/// HOW one row's task definition addresses the image its container runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `<registry>/<ecrRepositoryPrefix>/<upstream path>:<tag>`, a reference into the cache
    /// repository of the rule whose prefix is given.
    Cached {
        rule_prefix: &'static str,
        upstream_path: &'static str,
        tag: &'static str,
    },
    /// The upstream reference itself, which names no repository of this account.
    Upstream(&'static str),
    /// `<repositoryUri>:<tag>` of the repository of this account's registry that holds an image of
    /// its own.
    OwnRepository,
}

/// One row: the revision it registers, the names its own resources take, and the page the client
/// asking the address its load balancer published must then be answered with.
struct Row {
    case: &'static str,
    family: &'static str,
    source: Source,
    service_name: &'static str,
    group_name: &'static str,
    load_balancer_name: &'static str,
    /// A fragment the body answered at the served root must carry, and the empty string for the
    /// row whose image this emulator holds none of — which must carry NEITHER page.
    carries: &'static str,
}

const ROWS: [Row; 5] = [
    Row {
        case: "a cache repository of the gallery's nginx mirror, on a variant tag",
        family: "cachedmirrornginx",
        source: Source::Cached {
            rule_prefix: ROOT_RULE_PREFIX,
            upstream_path: "docker/library/nginx",
            tag: "alpine",
        },
        service_name: "cachedmirrornginx",
        group_name: "cached-mirror-nginx",
        load_balancer_name: "cached-mirror-nginx-lb",
        carries: NGINX_PAGE,
    },
    Row {
        case: "the publisher's own nginx repository through the same rule, on the moving tag",
        family: "cachedpublishernginx",
        source: Source::Cached {
            rule_prefix: ROOT_RULE_PREFIX,
            upstream_path: "nginx/nginx",
            tag: "latest",
        },
        service_name: "cachedpublishernginx",
        group_name: "cached-publisher-nginx",
        load_balancer_name: "cached-publisher-nginx-lb",
        carries: NGINX_PAGE,
    },
    Row {
        case: "a rule that prepends an upstream repository prefix, caching the httpd image",
        family: "cachedprefixedhttpd",
        source: Source::Cached {
            rule_prefix: PREFIXED_RULE_PREFIX,
            upstream_path: "httpd",
            tag: "2.4",
        },
        service_name: "cachedprefixedhttpd",
        group_name: "cached-prefixed-httpd",
        load_balancer_name: "cached-prefixed-httpd-lb",
        carries: HTTPD_PAGE,
    },
    Row {
        case: "the control: the upstream reference itself, with no cache repository in between",
        family: "upstreamnginx",
        source: Source::Upstream("public.ecr.aws/docker/library/nginx:alpine"),
        service_name: "upstreamnginx",
        group_name: "upstream-nginx",
        load_balancer_name: "upstream-nginx-lb",
        carries: NGINX_PAGE,
    },
    Row {
        case: "the negative control: a repository of this registry no cache rule covers",
        family: "ownrepository",
        source: Source::OwnRepository,
        service_name: "ownrepository",
        group_name: "own-repository",
        load_balancer_name: "own-repository-lb",
        carries: "",
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

/// The network every row runs in: a VPC with an internet gateway, its own MAIN route table sending
/// the default route to that gateway, two subnets in different zones, and one self-referencing
/// security group the tasks and the load balancers both sit in.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
}

/// Create that network through Amazon EC2 itself, so every id used below is one EC2 minted and the
/// path each image pull takes is one its own records hold.
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
    // implicitly associated with — "your VPC automatically comes with a main route table" (Amazon
    // VPC User Guide, "Route tables") — so no association of this suite's own decides where a
    // task's traffic goes.
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
                ("GroupName", "cachedimages-group"),
                ("GroupDescription", "the tasks and the load balancers"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // The one rule this suite authorizes: the group takes ITSELF as a source on every port, which
    // is what lets a load balancer's check of a target in it arrive.
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

/// Register the two pull through cache rules the cached rows pull through, and push one image into
/// the repository of this account's registry the negative row pulls from — answering the HOST of
/// this account's registry and the reference that addresses that own image.
///
/// Only the own repository is created: a cache repository is created by the FIRST pull of a
/// reference under its rule's prefix, which is exactly what the cached rows exercise.
async fn seed_registry(router: &MultiServiceRouter) -> (String, String) {
    for (prefix, upstream_prefix) in [
        (ROOT_RULE_PREFIX, None),
        (PREFIXED_RULE_PREFIX, Some(UPSTREAM_PREFIX)),
    ] {
        let mut request = json!({
            "ecrRepositoryPrefix": prefix,
            "upstreamRegistryUrl": UPSTREAM_REGISTRY,
        });
        if let Some(upstream_prefix) = upstream_prefix {
            request["upstreamRepositoryPrefix"] = json!(upstream_prefix);
        }
        let created = ecr(router, "CreatePullThroughCacheRule", request).await;
        assert_eq!(
            created["ecrRepositoryPrefix"].as_str(),
            Some(prefix),
            "setup: the stored rule must carry the prefix the request asked for, got {created}"
        );
    }
    let created = ecr(
        router,
        "CreateRepository",
        json!({"repositoryName": OWN_REPOSITORY}),
    )
    .await;
    let uri = created["repository"]["repositoryUri"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateRepository must report a repositoryUri, got {created}"))
        .to_string();
    ecr(
        router,
        "PutImage",
        json!({
            "repositoryName": OWN_REPOSITORY,
            "imageManifest": MANIFEST,
            "imageTag": OWN_TAG,
        }),
    )
    .await;
    let host = uri
        .split_once('/')
        .map(|(host, _)| host.to_string())
        .unwrap_or_else(|| panic!("a repositoryUri must carry a registry host, got {uri}"));
    (host, format!("{uri}:{OWN_TAG}"))
}

/// The reference one row's container runs.
fn image_of(source: Source, registry_host: &str, own_image: &str) -> String {
    match source {
        Source::Cached {
            rule_prefix,
            upstream_path,
            tag,
        } => format!("{registry_host}/{rule_prefix}/{upstream_path}:{tag}"),
        Source::Upstream(reference) => reference.to_string(),
        Source::OwnRepository => own_image.to_string(),
    }
}

/// Register one row's revision: an `awsvpc`, Fargate-compatible revision pulling `image` as the
/// execution role, whose one container declares the port the binding names and NO command of its
/// own — so what it runs is the command its image declares.
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
                ],
            }],
        }),
    )
    .await;
}

/// Create one row's `ip` target group, its internet-facing load balancer and the listener that
/// forwards to that group, and answer the group's ARN with the address the load balancer
/// published.
async fn front_door(router: &MultiServiceRouter, network: &Network, row: &Row) -> (String, String) {
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
    let dns_name = element(&balanced, "DNSName");
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
    (group, dns_name)
}

/// Create the ECS service one row describes: one task across both subnets, in the one security
/// group, with the public address that makes the internet gateway its subnets route through a path
/// to the image's registry, bound to `group` on the container port.
async fn create_service(
    router: &MultiServiceRouter,
    network: &Network,
    row: &Row,
    group: &str,
) {
    let created = ecs(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": row.service_name,
            "taskDefinition": row.family,
            "desiredCount": 1,
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
        Some(1),
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

/// Wait until `group` reports exactly one registration, in `HEALTHY`, and answer the address it is
/// registered at.
///
/// Polls the CONDITION — the task's start, the registration that start owes and the verdict its
/// group's cadence reaches are all clock-owed — rather than waiting a fixed time.
async fn settled(router: &MultiServiceRouter, row: &Row, group: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = health(router, group).await;
        if reported.len() == 1 && reported[0].2 == HEALTHY {
            assert_eq!(
                reported[0].1,
                CONTAINER_PORT.to_string(),
                "case {:?}: the registration must carry the bound container port, got {reported:?}",
                row.case
            );
            return reported[0].0.clone();
        }
        if Instant::now() >= deadline {
            // What the service itself says about the tasks it placed, so a row that never
            // converges names the reason rather than only the reads that did not settle.
            let described = ecs(
                router,
                "DescribeServices",
                json!({ "cluster": CLUSTER, "services": [row.service_name] }),
            )
            .await;
            panic!(
                "case {:?}: expected one registration reported {HEALTHY:?}, got {reported:?}; \
                 the service reports {described}, with the stopped tasks reporting {:?}",
                row.case,
                stopped_reasons(router, row).await
            );
        }
        ::tokio::time::sleep(POLL).await;
    }
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

/// One answered data-plane request: the status and the body, as the wire carries them.
struct Answered {
    status: u16,
    body: String,
}

/// Send one request to the address the load balancer published, carrying no AWS credential —
/// exactly as a client handed that name sends it.
async fn get(router: &MultiServiceRouter, dns_name: &str, path: &str) -> Answered {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{dns_name}{path}"))
        .header("host", dns_name)
        .body(Bytes::new())
        .expect("a well-formed data-plane request");
    let response = router.dispatch(request).await;
    Answered {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// RULE: a reference into an Amazon ECR pull through cache repository names the UPSTREAM image
/// that rule caches, so the program the container runs and the files it serves are that image's —
/// and a repository holding an image of its own states nothing about any upstream image.
#[tokio::test]
async fn a_cached_images_container_runs_the_upstream_image() {
    let router = emulator();
    let network = seed_network(&router).await;
    let execution_role = seed_execution_role(&router).await;
    let (registry_host, own_image) = seed_registry(&router).await;

    ecs(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;

    // Every row's revision, front door and service. The services are created back to back, so the
    // tasks of all of them come up together.
    let mut doors: Vec<(String, String)> = Vec::new();
    for row in &ROWS {
        register_revision(
            &router,
            row.family,
            &image_of(row.source, &registry_host, &own_image),
            &execution_role,
        )
        .await;
        let (group, dns_name) = front_door(&router, &network, row).await;
        create_service(&router, &network, row, &group).await;
        doors.push((group, dns_name));
    }

    for (row, (group, dns_name)) in ROWS.iter().zip(&doors) {
        let address = settled(&router, row, group).await;
        let answered = get(&router, dns_name, SERVED_PATH).await;
        if row.carries.is_empty() {
            // The negative row: nothing is read off an image this emulator holds none of, so the
            // container serves neither image's page — whatever status the load balancer answers
            // for a target nothing stands behind.
            for page in [NGINX_PAGE, HTTPD_PAGE] {
                assert!(
                    !answered.body.contains(page),
                    "case {:?}: a repository holding an image of its own states nothing about any \
                     upstream image, yet the target at {address} answered {} with {:?}",
                    row.case,
                    answered.status,
                    answered.body
                );
            }
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {:?}: expected the task at {address} to serve the page its image ships, got \
             {} with {:?}",
            row.case, answered.status, answered.body
        );
        assert!(
            answered.body.contains(row.carries),
            "case {:?}: expected the body served at {SERVED_PATH} to carry {:?}, got {:?}",
            row.case,
            row.carries,
            answered.body
        );
    }

    // The cache repository each cached row pulled through was CREATED by that pull and holds the
    // tag the reference asked for: the registry really cached the upstream image rather than the
    // pull being waved through.
    for row in &ROWS {
        let Source::Cached {
            rule_prefix,
            upstream_path,
            tag,
        } = row.source
        else {
            continue;
        };
        let repository_name = format!("{rule_prefix}/{upstream_path}");
        let listed = ecr(
            &router,
            "ListImages",
            json!({ "repositoryName": repository_name }),
        )
        .await;
        let tags: Vec<String> = listed["imageIds"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|id| id["imageTag"].as_str().map(str::to_string))
            .collect();
        assert!(
            tags.iter().any(|held| held == tag),
            "case {:?}: the cache repository {repository_name} must hold the tag the pull asked \
             for, got {tags:?}",
            row.case
        );
    }
}
