// SPDX-License-Identifier: CC0-1.0

//! WHICH Elastic Load Balancing v2 target group an ECS service's running tasks are
//! registered into is decided by that service's OWN `loadBalancers`, and by nothing else.
//!
//! RULE: Amazon ECS registers each running task of a service as a target of exactly the
//! groups the service's own `loadBalancers` bindings name — at the private IPv4 address of
//! the elastic network interface the `awsvpc` placement gave it, on the `containerPort` the
//! binding names — so a group bound by one service holds one target per running task of THAT
//! service, and a group no service's `loadBalancers` names holds none, however many
//! load-balanced services run beside it.
//!
//! The rows differ along what the rule generalises over: whether the service declares a
//! binding at all, how many tasks it runs, and which of the two ports its container listens
//! on the binding names. A service with no `loadBalancers` block is the negative row: its
//! running task is registered NOWHERE, which is only observable beside a service that does
//! declare one, so both run on the same cluster, the same revision and the same subnets.
//!
//! The group nothing references is read on every poll of the convergence above, so its
//! emptiness is asserted while the other services' tasks come up and are registered rather
//! than once afterwards: a registration that leaked into it at any instant of that window
//! fails the suite.
//!
//! Three crates have to agree for the rule to hold, which is why this suite lives here:
//! Amazon EC2 owns the network the tasks are placed in, Amazon ECS owns the tasks and their
//! interfaces' addresses, and Elastic Load Balancing v2 holds the registrations and reports
//! them. Everything is driven through the real operations over ONE shared engine.

use std::collections::BTreeSet;
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

/// The ECS cluster, task-definition family and container every row runs.
const CLUSTER: &str = "bindings";
const FAMILY: &str = "bindingsfamily";
const CONTAINER: &str = "app";

/// The two ports the container declares `portMappings` for, so the two a binding may name.
const CONTAINER_PORT: i64 = 80;
const SECOND_CONTAINER_PORT: i64 = 8081;

/// The network every row runs in.
const VPC_CIDR: &str = "10.99.0.0/16";
const SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.99.1.0/24", "us-east-1a"),
    ("10.99.2.0/24", "us-east-1b"),
];

/// The target group NO service references, and the listener-free port it is created on.
const UNWIRED_GROUP: &str = "bindings-unwired";

/// How long the convergence below is waited for, and how often it is looked at. Generous on
/// purpose: the deadline is only here so a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(100);

/// One row: the service it creates, and the registrations that service's running tasks must
/// then be reported with.
struct Row {
    case: &'static str,
    service_name: &'static str,
    /// The target group this service BINDS, with the listener port that makes it one the
    /// load balancer forwards to, and the `containerPort` the binding names — or `None` for
    /// a service that declares no `loadBalancers` block at all.
    binding: Option<Binding>,
    /// How many tasks the service is asked for, which is how many registrations its bound
    /// group must hold.
    desired: i64,
}

/// The `loadBalancers` entry one row declares.
struct Binding {
    group_name: &'static str,
    listener_port: &'static str,
    container_port: i64,
}

const ROWS: [Row; 4] = [
    Row {
        case: "the recorded wiring: one task bound to its own group on the container port",
        service_name: "wired",
        binding: Some(Binding {
            group_name: "bindings-wired",
            listener_port: "80",
            container_port: CONTAINER_PORT,
        }),
        desired: 1,
    },
    Row {
        case: "the negative control: an identical service with no loadBalancers block",
        service_name: "unwired",
        binding: None,
        desired: 1,
    },
    Row {
        case: "two tasks, each a target of the one group the service binds",
        service_name: "wiredtwo",
        binding: Some(Binding {
            group_name: "bindings-wired-two",
            listener_port: "8080",
            container_port: CONTAINER_PORT,
        }),
        desired: 2,
    },
    Row {
        case: "a binding on the second port the container listens on",
        service_name: "wiredsecond",
        binding: Some(Binding {
            group_name: "bindings-wired-second",
            listener_port: "8082",
            container_port: SECOND_CONTAINER_PORT,
        }),
        desired: 1,
    },
];

/// One emulator carrying EC2, Elastic Load Balancing v2 and ECS over ONE shared engine,
/// exactly as the production router builds them.
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

/// Send one form-encoded request to the service `signing_name` addresses, and answer its
/// status and body.
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
        status, 200,
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
        status, 200,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One awsJson1_1 call to ECS that must succeed, answered with its parsed body.
async fn ecs_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
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
    assert_eq!(
        status, 200,
        "setup: ECS {operation} answered {status} with {parsed}"
    );
    parsed
}

/// The network every row runs in: a VPC with an internet gateway and a route table that
/// sends `0.0.0.0/0` to it associated with both of its public subnets, and one
/// self-referencing security group the tasks and the load balancer both sit in.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    group: String,
}

/// Create that network through Amazon EC2 itself, so every id used below is one EC2 minted
/// and every route the image fetch is decided over is one its own records hold.
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
        let subnet = element(
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
        );
        subnets.push(subnet);
    }
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
    let group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "bindings-group"),
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

/// Create the cluster and the revision every row's service runs: an `awsvpc`,
/// Fargate-compatible revision whose one container listens on both ports a binding may name.
async fn seed_ecs(router: &MultiServiceRouter) {
    ecs_ok(router, "CreateCluster", json!({ "clusterName": CLUSTER })).await;
    ecs_ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": CONTAINER,
                "image": "public.ecr.aws/docker/library/busybox:latest",
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
/// security group, bound to `group` where the row declares a binding and to nothing at all
/// where it does not.
async fn create_service(
    router: &MultiServiceRouter,
    network: &Network,
    row: &Row,
    group: Option<&String>,
) {
    let mut request = json!({
        "cluster": CLUSTER,
        "serviceName": row.service_name,
        "taskDefinition": FAMILY,
        "desiredCount": row.desired,
        "launchType": "FARGATE",
        "networkConfiguration": {
            "awsvpcConfiguration": {
                "subnets": network.subnets,
                "securityGroups": [network.group],
                // The task's own public address, which is what makes the internet gateway
                // its subnet routes through a path to the image's registry.
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
    let created = ecs_ok(router, "CreateService", request).await;
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

/// The private IPv4 addresses of the RUNNING tasks of one row's service, read out of ECS.
async fn task_addresses(router: &MultiServiceRouter, row: &Row) -> BTreeSet<String> {
    let listed = ecs_ok(
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
        return BTreeSet::new();
    }
    let described = ecs_ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": arns }),
    )
    .await;
    described["tasks"]
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
        .collect()
}

/// What one row's service reports about the tasks it holds: its own counts, and the counts
/// and `rolloutState` of the deployment that is rolling them out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Converged {
    running_count: i64,
    pending_count: i64,
    /// `Deployment$rolloutState` of the `PRIMARY` deployment, and the tasks it reports.
    rollout_state: String,
    deployment_running_count: i64,
    deployment_pending_count: i64,
}

/// What `DescribeServices` reports for one row's service now.
async fn signals(router: &MultiServiceRouter, row: &Row) -> Converged {
    let described = ecs_ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [row.service_name] }),
    )
    .await;
    let service = &described["services"][0];
    // "PRIMARY": the deployment that is rolling the service's current revision out, which is
    // the one whose rolloutState a client waits on.
    let primary = service["deployments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|deployment| deployment["status"].as_str() == Some("PRIMARY"))
        .cloned()
        .unwrap_or(Value::Null);
    let count = |value: &Value, member: &str| value[member].as_i64().unwrap_or(-1);
    Converged {
        running_count: count(service, "runningCount"),
        pending_count: count(service, "pendingCount"),
        rollout_state: primary["rolloutState"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        deployment_running_count: count(&primary, "runningCount"),
        deployment_pending_count: count(&primary, "pendingCount"),
    }
}

/// The terminal state every row's service must settle in: it holds the tasks it wants, none
/// is still coming up, and its `PRIMARY` deployment reports the same and has reached the
/// `COMPLETED` rollout `Deployment$rolloutState` documents for a service that reached a
/// steady state.
fn settled(row: &Row, reported: &Converged) -> bool {
    *reported
        == Converged {
            running_count: row.desired,
            pending_count: 0,
            rollout_state: ROLLOUT_COMPLETED.to_string(),
            deployment_running_count: row.desired,
            deployment_pending_count: 0,
        }
}

/// `Deployment$rolloutState` of a rollout that is over: "When the service reaches a steady
/// state, the deployment transitions to a `COMPLETED` state."
const ROLLOUT_COMPLETED: &str = "COMPLETED";

#[tokio::test]
async fn an_ecs_services_tasks_are_targets_of_the_groups_its_own_bindings_name() {
    let router = emulator();
    let network = seed_network(&router).await;
    seed_ecs(&router).await;

    // The group NOTHING references: created first, forwarded to by no listener, bound by no
    // service.
    let unwired = target_group(&router, &network, UNWIRED_GROUP, CONTAINER_PORT).await;

    let load_balancer = element(
        &elbv2(
            &router,
            "CreateLoadBalancer",
            &[
                ("Name", "bindings-alb"),
                ("Subnets.member.1", network.subnets[0].as_str()),
                ("Subnets.member.2", network.subnets[1].as_str()),
                ("SecurityGroups.member.1", network.group.as_str()),
                ("Scheme", "internet-facing"),
            ],
        )
        .await,
        "LoadBalancerArn",
    );

    // Every row's group, listener and service. The services are created back to back, so
    // the tasks of all of them come up together.
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

    // Wait for the CONDITION every row's service converges on, reading the unreferenced
    // group on every pass: its emptiness has to hold throughout the window the other
    // services' tasks are coming up and being registered in, not only once at the end.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let leaked = registered(&router, &unwired).await;
        assert!(
            leaked.is_empty(),
            "case {UNWIRED_GROUP:?}: a target group no service's loadBalancers names must \
             hold no registration, got {leaked:?}"
        );
        let mut reported = Vec::new();
        for row in &ROWS {
            reported.push(signals(&router, row).await);
        }
        let converged = reported
            .iter()
            .zip(&ROWS)
            .all(|(reported, row)| settled(row, reported));
        if converged {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "every service must run the tasks it wants and its PRIMARY deployment reach a \
             terminal rollout; got {reported:?} against the wanted counts {:?}",
            ROWS.iter().map(|row| row.desired).collect::<Vec<_>>()
        );
        ::tokio::time::sleep(POLL).await;
    }

    // Each row's own group holds exactly its own tasks' addresses on the bound port, and
    // the service that declares no binding is registered nowhere at all.
    let mut unwired_addresses: BTreeSet<String> = BTreeSet::new();
    let mut bound: Vec<(&Row, String, BTreeSet<String>)> = Vec::new();
    for (row, group) in ROWS.iter().zip(&groups) {
        let addresses = task_addresses(&router, row).await;
        assert_eq!(
            addresses.len(),
            usize::try_from(row.desired).unwrap_or_default(),
            "case {:?}: expected {} running task(s), each at its own interface's address, \
             got {addresses:?}",
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
        let reported = reaching(&router, row.case, group, &expected).await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected the bound group to hold exactly {expected:?}, got {reported:?}",
            row.case
        );
    }

    // Nothing holds a target of the service that declared no binding — neither the group
    // nothing references nor any group another service bound.
    let mut every_group: Vec<String> = vec![unwired.clone()];
    every_group.extend(bound.iter().map(|(_, group, _)| group.clone()));
    for group in &every_group {
        let reported = registered(&router, group).await;
        for address in &unwired_addresses {
            assert!(
                !reported.iter().any(|(id, _)| id == address),
                "case {:?}: the task of a service with no loadBalancers block is a target of \
                 no group, yet {group} reports {reported:?}",
                ROWS[1].case
            );
        }
    }
    let leaked = registered(&router, &unwired).await;
    assert!(
        leaked.is_empty(),
        "case {UNWIRED_GROUP:?}: a target group no service's loadBalancers names must hold \
         no registration, got {leaked:?}"
    );

    // Every address a registration carries is an address OF one of the subnets the
    // placement named: the interface an `awsvpc` task is allocated is created in one of
    // them, so its address comes out of that subnet's own range rather than from anywhere
    // else.
    for (row, group, addresses) in &bound {
        for address in addresses {
            assert!(
                is_a_placement_subnet_address(address),
                "case {:?}: the address {address} the group {group} holds must be an \
                 assignable address of one of the placement's subnets {SUBNET_CIDRS:?}",
                row.case
            );
        }
    }
}

/// Whether `address` is an assignable address of one of the subnets every placement above
/// names.
///
/// The subnets are `/24` blocks, so an address of one has that block's three network octets;
/// AWS reserves the first four addresses of every subnet's block and its last one (Amazon VPC
/// User Guide, "Subnet CIDR blocks"), so neither is an address an interface may be given.
fn is_a_placement_subnet_address(address: &str) -> bool {
    let Ok(parsed) = address.parse::<::std::net::Ipv4Addr>() else {
        return false;
    };
    let octets = parsed.octets();
    SUBNET_CIDRS.iter().any(|(cidr, _)| {
        cidr.strip_suffix("/24")
            .and_then(|network| network.parse::<::std::net::Ipv4Addr>().ok())
            .is_some_and(|network| {
                network.octets()[..3] == octets[..3] && (4..=254).contains(&octets[3])
            })
    })
}

/// The `(address, port)` set the group `arn` reports once it reports `expected`, polled
/// rather than waited for; answers what it reports at the deadline otherwise, for the
/// caller's own assertion to quote.
async fn reaching(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    expected: &BTreeSet<(String, String)>,
) -> BTreeSet<(String, String)> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = registered(router, arn).await;
        if &reported == expected || Instant::now() >= deadline {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected:?} in {arn}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}
