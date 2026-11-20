// SPDX-License-Identifier: CC0-1.0

//! ONE Application Load Balancer listener fanning requests out to TWO Amazon ECS services by
//! the path a client asked for.
//!
//! RULE: **an Application Load Balancer listener answers each request with the actions of the
//! FIRST rule, in priority order, whose conditions the request meets, and with its own
//! `DefaultActions` when no rule's conditions are met — so a `forward` rule sends the request
//! to ITS OWN target group, and the answer is the one the workload registered in that group
//! gives for the path as the client wrote it; a request no rule matches never reaches any
//! group.**
//!
//! The grounds. "Rules are evaluated in priority order, from the lowest value to the highest
//! value ... When the conditions for a rule are met, its actions are performed. If the
//! conditions for no rules are met, the actions for the default rule are performed"
//! (`smithy_query(query="doc",shape="CreateRule")`), a `path-pattern` condition is "the path
//! patterns to compare against the request URL" whose comparison "is case sensitive"
//! (`smithy_query(query="doc",shape="PathPatternConditionConfig",member="Values")`), and
//! `LoadBalancer$DNSName` is "the public DNS name of the load balancer" — the one address the
//! client dials for all of it. Nothing in a `forward` rewrites the request path, so the path
//! the rule matched is the path the target is asked for.
//!
//! Three crates have to agree for that to hold, which is why this suite lives here: Amazon ECS
//! runs each service's container and records the server it listens with at its task's own
//! address, Amazon EC2 owns the network and the security-group rule that lets the checks and
//! the traffic arrive, and Elastic Load Balancing v2 holds the rules, the registrations and the
//! answer. Everything is driven through the real operations on ONE shared engine, so every
//! assertion is about state those services persisted.
//!
//! The rows differ along what the rule generalises over: which rule matches (each of the two
//! `forward` rules, and the default action), where the wildcard tail of the pattern matches (a
//! directory and a document several segments deeper), a path that lies under NEITHER pattern,
//! the listener's root, and a path that differs from a pattern only in case — which, the
//! comparison being case sensitive, matches no rule either.
//!
//! Every clock-owed move — each task's start, the registration it owes and the health its
//! group's checks determine — is POLLED for until it holds, never slept for, and the deadline
//! is generous: the suite asserts that the moves HAPPEN, not how fast this host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, members_of, opt_element};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request carry.
const EC2_API_VERSION: &str = "2016-11-15";
const ELB_VERSION: &str = "2015-12-01";

/// The status every control-plane call of the setup must answer.
const OK: u16 = 200;

/// The cluster both services run in, and the container name every binding names.
const CLUSTER: &str = "fanout";
const CONTAINER: &str = "app";

/// The network this suite runs in.
const VPC_CIDR: &str = "10.70.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.70.1.0/24", "us-east-1a"),
    ("10.70.2.0/24", "us-east-1b"),
];

/// The port each container's server listens on, which is the `containerPort` its revision
/// declares, the port its binding names and its target group's own.
const CONTAINER_PORT: i64 = 80;

/// The `HealthCheckIntervalSeconds` both target groups are created with: the shortest the
/// member admits ("the range is 5-300"), so a wait for a verdict waits the least the
/// configuration allows.
const HEALTH_CHECK_INTERVAL_SECONDS: &str = "5";

/// The `TargetHealth$State` a target the load balancer's checks reached carries.
const HEALTHY: &str = "healthy";

/// The status and the body the listener's DEFAULT action answers with, which is what a request
/// no rule matches gets.
const DEFAULT_STATUS: u16 = 404;
const DEFAULT_BODY: &str = "no-match";

/// The document each service's container writes, so a body the other service (or anything but
/// those programs) produced fails its row.
const BODY_A: &str = "FANOUT-SERVICE-A";
const BODY_A_DEEP: &str = "FANOUT-SERVICE-A-DEEP";
const BODY_B: &str = "FANOUT-SERVICE-B";
const BODY_B_DEEP: &str = "FANOUT-SERVICE-B-DEEP";

/// How long a clock-owed move is waited for before the suite gives up, and how often it looks
/// again. Generous on purpose: the deadline is here so a failure is a failure rather than a
/// hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// One deployed service of the fan-out: the names its resources take, the prefix the listener
/// rule routes to it, and what its container serves.
struct Deployed {
    /// The names this service's own resources take.
    family: &'static str,
    group_name: &'static str,
    service_name: &'static str,
    /// The path prefix the rule forwarding to this service's group matches, and the priority
    /// that rule is created at.
    prefix: &'static str,
    priority: &'static str,
    /// The two documents the container's program writes under that prefix.
    body: &'static str,
    deep_body: &'static str,
}

/// The two services behind the one listener.
const DEPLOYED: [Deployed; 2] = [
    Deployed {
        family: "fanouta",
        group_name: "fanout-a",
        service_name: "fanouta",
        prefix: "/svc-a",
        priority: "10",
        body: BODY_A,
        deep_body: BODY_A_DEEP,
    },
    Deployed {
        family: "fanoutb",
        group_name: "fanout-b",
        service_name: "fanoutb",
        prefix: "/svc-b",
        priority: "20",
        body: BODY_B,
        deep_body: BODY_B_DEEP,
    },
];

/// One request table row: what the client asks the one published address for, and what it must
/// be answered with.
struct Row {
    case: &'static str,
    path: &'static str,
    status: u16,
    /// The body the answer must carry, trimmed — empty where the row expects none.
    body: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the first rule's prefix is answered by the service bound to its own group",
        path: "/svc-a/",
        status: OK,
        body: BODY_A,
    },
    Row {
        case: "the first rule's wildcard tail reaches a document several segments deeper",
        path: "/svc-a/deep/page.html",
        status: OK,
        body: BODY_A_DEEP,
    },
    Row {
        case: "the second rule's prefix is answered by the other service, with no leak",
        path: "/svc-b/",
        status: OK,
        body: BODY_B,
    },
    Row {
        case: "the second rule's wildcard tail reaches that service's own deeper document",
        path: "/svc-b/deep/page.html",
        status: OK,
        body: BODY_B_DEEP,
    },
    Row {
        case: "a path under neither pattern is the listener's default action",
        path: "/no-such-path",
        status: DEFAULT_STATUS,
        body: DEFAULT_BODY,
    },
    Row {
        case: "the listener's root is under neither pattern either",
        path: "/",
        status: DEFAULT_STATUS,
        body: DEFAULT_BODY,
    },
    Row {
        // "The comparison is case sensitive" — so this path meets no rule's conditions.
        case: "a path differing from a pattern only in case matches no rule",
        path: "/SVC-A/",
        status: DEFAULT_STATUS,
        body: DEFAULT_BODY,
    },
];

/// One emulator carrying EC2, Elastic Load Balancing v2 and ECS over ONE shared engine, exactly
/// as the production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
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

/// Send one form-encoded request to the service `signing_name` addresses, and answer its status
/// and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
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
        status, OK,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
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
        status, OK,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One awsJson1_1 call to ECS, answered with its status and parsed body.
async fn ecs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerServiceV20141113.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDECSTEST/20260101/{REGION}\
                 /ecs/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
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
    (status, parsed)
}

/// One ECS call that must succeed, answered with its parsed body.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = ecs(router, operation, body).await;
    assert_eq!(
        status, OK,
        "setup: ECS {operation} answered {status} with {parsed}"
    );
    parsed
}

/// The network both services run in: a VPC with two subnets and an internet gateway, the load
/// balancer's own security group, and the group the tasks are placed in — which admits the
/// load balancer's checks and traffic on the port the containers serve on.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    load_balancer_group: String,
    task_group: String,
}

/// Create that network through EC2 itself, so every id asserted against is one EC2 minted.
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
    // The path off the VPC a task needs to fetch the image its container runs: Amazon ECS reads
    // the route table that applies to the placement's subnet before it starts a task, and a
    // placement with no route off the VPC cannot reach a public registry.
    let route_table = element(
        &ec2(router, "CreateRouteTable", &[("VpcId", vpc.as_str())]).await,
        "routeTableId",
    );
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", route_table.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", gateway.as_str()),
        ],
    )
    .await;
    for subnet in &subnets {
        ec2(
            router,
            "AssociateRouteTable",
            &[
                ("RouteTableId", route_table.as_str()),
                ("SubnetId", subnet.as_str()),
            ],
        )
        .await;
    }
    let group = |name: &'static str, description: &'static str| {
        let vpc = vpc.clone();
        async move {
            element(
                &ec2(
                    router,
                    "CreateSecurityGroup",
                    &[
                        ("GroupName", name),
                        ("GroupDescription", description),
                        ("VpcId", vpc.as_str()),
                    ],
                )
                .await,
                "groupId",
            )
        }
    };
    let load_balancer_group = group("fanout-alb", "the load balancers own group").await;
    let task_group = group("fanout-tasks", "the group both services tasks are placed in").await;
    // The ONE rule this suite authorizes: the tasks' group takes the load balancer's own group
    // as a source on the port the containers serve on, which is what lets a check of a target
    // in it arrive.
    let port = CONTAINER_PORT.to_string();
    ec2(
        router,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", task_group.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", port.as_str()),
            ("IpPermissions.1.ToPort", port.as_str()),
            (
                "IpPermissions.1.Groups.1.GroupId",
                load_balancer_group.as_str(),
            ),
        ],
    )
    .await;
    Network {
        vpc,
        subnets,
        load_balancer_group,
        task_group,
    }
}

/// The container command that serves `service`'s own documents under its own path prefix: the
/// index its prefix names and one document several segments deeper, written into the image's
/// document root by the container's own program, which then runs the image's own server in the
/// foreground.
fn serving_command(service: &Deployed) -> Vec<String> {
    let root = format!("/usr/share/nginx/html{}", service.prefix);
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!(
            "mkdir -p {root}/deep && echo {} > {root}/index.html && echo {} > \
             {root}/deep/page.html && nginx -g 'daemon off;'",
            service.body, service.deep_body
        ),
    ]
}

/// Register one `awsvpc`, Fargate-compatible revision of `family` whose single container
/// listens on the suite's port and runs `command`, and answer its ARN.
async fn register_revision(
    router: &MultiServiceRouter,
    family: &str,
    command: &[&str],
) -> String {
    let registered = ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": "public.ecr.aws/docker/library/nginx:alpine",
                "memory": 128,
                "portMappings": [{ "containerPort": CONTAINER_PORT }],
                "command": command,
            }],
        }),
    )
    .await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the registration must answer an ARN, got {registered}"))
        .to_string()
}

/// Create the `ip` target group one service's tasks register into, health-checked on the path
/// that service serves its index at, and answer its ARN.
async fn create_target_group(
    router: &MultiServiceRouter,
    network: &Network,
    service: &Deployed,
) -> String {
    let health_check_path = format!("{}/", service.prefix);
    let created = elbv2(
        router,
        "CreateTargetGroup",
        &[
            ("Name", service.group_name),
            ("Protocol", "HTTP"),
            ("Port", &CONTAINER_PORT.to_string()),
            ("TargetType", "ip"),
            ("VpcId", network.vpc.as_str()),
            ("HealthCheckProtocol", "HTTP"),
            ("HealthCheckPath", health_check_path.as_str()),
            (
                "HealthCheckIntervalSeconds",
                HEALTH_CHECK_INTERVAL_SECONDS,
            ),
        ],
    )
    .await;
    opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("no TargetGroupArn for {} in {created}", service.group_name))
}

/// Create the one internet-facing load balancer every row dials, and answer its ARN with the
/// address it published as its `DNSName`.
async fn create_load_balancer(router: &MultiServiceRouter, network: &Network) -> (String, String) {
    let balanced = elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", "fanout-alb"),
            ("Type", "application"),
            ("Scheme", "internet-facing"),
            ("Subnets.member.1", network.subnets[0].as_str()),
            ("Subnets.member.2", network.subnets[1].as_str()),
            (
                "SecurityGroups.member.1",
                network.load_balancer_group.as_str(),
            ),
        ],
    )
    .await;
    (
        element(&balanced, "LoadBalancerArn"),
        element(&balanced, "DNSName"),
    )
}

/// Create the one HTTP:80 listener whose default action is the fixed response above, and answer
/// its ARN.
async fn create_listener(router: &MultiServiceRouter, load_balancer: &str) -> String {
    let created = elbv2(
        router,
        "CreateListener",
        &[
            ("LoadBalancerArn", load_balancer),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("DefaultActions.member.1.Type", "fixed-response"),
            (
                "DefaultActions.member.1.FixedResponseConfig.StatusCode",
                &DEFAULT_STATUS.to_string(),
            ),
            (
                "DefaultActions.member.1.FixedResponseConfig.ContentType",
                "text/plain",
            ),
            (
                "DefaultActions.member.1.FixedResponseConfig.MessageBody",
                DEFAULT_BODY,
            ),
        ],
    )
    .await;
    element(&created, "ListenerArn")
}

/// Add the rule that forwards one service's own path pattern to that service's own group.
async fn create_rule(
    router: &MultiServiceRouter,
    listener: &str,
    service: &Deployed,
    group: &str,
) {
    let pattern = format!("{}*", service.prefix);
    elbv2(
        router,
        "CreateRule",
        &[
            ("ListenerArn", listener),
            ("Priority", service.priority),
            ("Conditions.member.1.Field", "path-pattern"),
            ("Conditions.member.1.Values.member.1", pattern.as_str()),
            ("Actions.member.1.Type", "forward"),
            ("Actions.member.1.TargetGroupArn", group),
        ],
    )
    .await;
}

/// Create one ECS service of `revision`, bound to `group`, with one task.
async fn create_service(
    router: &MultiServiceRouter,
    network: &Network,
    service_name: &str,
    revision: &str,
    group: &str,
) {
    ecs_ok(
        router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": service_name,
            "taskDefinition": revision,
            "desiredCount": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnets[0]],
                    "securityGroups": [network.task_group],
                    "assignPublicIp": "ENABLED",
                }
            },
            "loadBalancers": [{
                "targetGroupArn": group,
                "containerName": CONTAINER,
                "containerPort": CONTAINER_PORT,
            }],
            "healthCheckGracePeriodSeconds": 120,
        }),
    )
    .await;
}

/// The `(Id, State)` of every registration `group` reports.
async fn health(router: &MultiServiceRouter, group: &str) -> Vec<(String, String)> {
    let body = elbv2(router, "DescribeTargetHealth", &[("TargetGroupArn", group)]).await;
    members_of(&body)
        .into_iter()
        .map(|description| {
            let text = |tag: &str| {
                elements(description, tag)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            };
            (text("Id"), text("State"))
        })
        .collect()
}

/// Wait until `group` reports exactly one registration and that registration is `healthy`,
/// answering the address it is registered at.
///
/// Polls the CONDITION — the task's start, the registration that start owes and the health its
/// checks determine are all clock-owed — rather than waiting a fixed time.
async fn healthy_target(router: &MultiServiceRouter, service: &Deployed, group: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = health(router, group).await;
        if reported.len() == 1 && reported[0].1 == HEALTHY {
            return reported[0].0.clone();
        }
        if Instant::now() >= deadline {
            // What the service itself says about the tasks it placed, so a service that never
            // converges names the reason rather than only the empty read.
            let described = ecs_ok(
                router,
                "DescribeServices",
                json!({ "cluster": CLUSTER, "services": [service.service_name] }),
            )
            .await;
            panic!(
                "case {:?}: expected one registration reported {HEALTHY} in {}, got \
                 {reported:?}; the service reports {described}",
                service.service_name, service.group_name
            );
        }
        ::tokio::time::sleep(POLL).await;
    }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_listeners_path_rules_answer_from_each_services_own_target_group() {
    let router = emulator();
    let network = seed_network(&router).await;
    ecs_ok(&router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    let (load_balancer, dns_name) = create_load_balancer(&router, &network).await;
    let listener = create_listener(&router, &load_balancer).await;

    // Both services are created BEFORE either is waited for, so the two tasks start together
    // rather than one after the other.
    let mut groups = Vec::new();
    for service in &DEPLOYED {
        let group = create_target_group(&router, &network, service).await;
        create_rule(&router, &listener, service, &group).await;
        let command = serving_command(service);
        let borrowed: Vec<&str> = command.iter().map(String::as_str).collect();
        let revision = register_revision(&router, service.family, &borrowed).await;
        create_service(&router, &network, service.service_name, &revision, &group).await;
        groups.push(group);
    }

    // Each group holds exactly ONE healthy registration, and the two name different addresses:
    // a service registers its own tasks into its own group and nothing else's.
    let mut registered = Vec::new();
    for (service, group) in DEPLOYED.iter().zip(&groups) {
        let address = healthy_target(&router, service, group).await;
        assert!(
            address.starts_with("10.70."),
            "case {:?}: the registration must name the task's own address in the VPC, got \
             {address:?}",
            service.service_name
        );
        registered.push(address);
    }
    assert_ne!(
        registered[0], registered[1],
        "the two services must register their own tasks' addresses, got {registered:?}"
    );

    for row in ROWS {
        let answered = get(&router, &dns_name, row.path).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {} for {}, got {} with body {:?}",
            row.case, row.status, row.path, answered.status, answered.body
        );
        assert_eq!(
            answered.body.trim(),
            row.body,
            "case {:?}: expected the body {:?} for {}, got {:?}",
            row.case,
            row.body,
            row.path,
            answered.body
        );
    }
}
