// SPDX-License-Identifier: CC0-1.0

//! WHICH host a task placed through a CAPACITY PROVIDER runs on, driven end to end over the
//! shared wire harness in [`crate::ecs_wire`] with Amazon EC2 and Amazon EC2 Auto Scaling
//! mounted beside this service on one engine — so every host is an instance launched through
//! EC2's OWN API, every group and provider is created through its own API, and every value
//! asserted is read back through `ListContainerInstances`, `DescribeContainerInstances` and
//! `DescribeTasks`.
//!
//! # The rule under test
//!
//! > The infrastructure a placement runs on is the launch type it NAMED, else the class of
//! > capacity the capacity provider that placed the task binds onto — so a task placed
//! > through a provider over a caller's Auto Scaling group runs on one of its cluster's
//! > registered container instances and reports that instance in `Task$containerInstanceArn`,
//! > while a task placed through a Fargate provider runs on capacity AWS owns and reports
//! > none.
//!
//! `Task$launchType` is "The infrastructure where your task runs on" and
//! `Task$containerInstanceArn` "The ARN of the container instance that hosts the task"; "the
//! EC2 launch type runs your tasks on Amazon EC2 instances registered to your cluster", which
//! is the capacity an Auto Scaling group capacity provider brings ("When you use Fargate, the
//! capacity providers are FARGATE or FARGATE_SPOT"). A placement says where it runs with
//! EITHER member — "A task can use either a launch type or a capacity provider strategy" — so
//! the host is decided on the resolved infrastructure rather than on the member that was sent.
//!
//! # What the rows span
//!
//! The table differs along what the rule generalises over: the CLASS of capacity the strategy
//! routes through (a provider over an Auto Scaling group, the two predefined Fargate
//! providers), how the provider is configured (managed scaling on, and no managed-scaling
//! block at all), how many tasks one call places (one, and two spread over the cluster's
//! hosts), how many entries the strategy carries (one, and two providers over two groups),
//! and the launch type named OUTRIGHT as the contrast the rule is read against.
//!
//! The second rule, over the same estate
//! ([`a_service_placed_through_a_capacity_provider_runs_its_tasks_on_the_clusters_hosts`]):
//! the SERVICE scheduler's own placement resolves the same way, so a service created with a
//! `capacityProviderStrategy` over such a provider runs its tasks on the cluster's container
//! instances too.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    auto_scaling, count, create_cluster, ec2_element, ec2_form, emulator_with_ec2, list, ok,
    poll_until, string, SIGNED_REGION,
};

/// The cluster every row below places into.
const CLUSTER: &str = "providerhostfleet";

/// The AMI every launch boots. Well-formed (`ami-<17 hexadecimal digits>`), which is all a
/// launch needs; which AMI carries the container agent is not a fact this emulator publishes.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The instance type every host is launched as.
const INSTANCE_TYPE: &str = "t3.micro";

/// `ContainerInstance$status` of a registered host.
const ACTIVE: &str = "ACTIVE";

/// `Task$lastStatus` of a task that is running.
const RUNNING: &str = "RUNNING";

/// The two capacity providers Amazon ECS manages for every account, which place tasks on
/// Fargate ("When you use Fargate, the capacity providers are FARGATE or FARGATE_SPOT").
const FARGATE: &str = "FARGATE";
const FARGATE_SPOT: &str = "FARGATE_SPOT";

/// The launch types the two classes of capacity report.
const LAUNCH_TYPE_EC2: &str = "EC2";
const LAUNCH_TYPE_FARGATE: &str = "FARGATE";

/// The providers over an Auto Scaling group of the account, and the groups they bind onto:
/// one whose managed scaling Amazon ECS runs, one that states no managed-scaling block at all.
const SCALED_PROVIDER: &str = "providerhostscaled";
const SCALED_GROUP: &str = "providerhost-scaled-asg";
const PLAIN_PROVIDER: &str = "providerhostplain";
const PLAIN_GROUP: &str = "providerhost-plain-asg";

/// The user data a host is launched with: the documented shell line that appends the container
/// agent's `ECS_CLUSTER` assignment to its configuration file, base64 as a launch carries it.
fn launched_into(cluster: &str) -> String {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD.encode(format!(
        "#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n"
    ))
}

/// Create one VPC and one subnet per named zone, through Amazon EC2's own API, with a route
/// to an internet gateway — the path the image fetch of a task placed in one of them takes.
async fn network(router: &MultiServiceRouter, zones: &[String], case: &str) -> Vec<String> {
    let vpc = ec2_element(
        &ec2_form(router, "CreateVpc", &[("CidrBlock", "10.91.0.0/16")], case).await,
        "vpcId",
        case,
    );
    let mut subnets = Vec::new();
    for (index, zone) in zones.iter().enumerate() {
        let cidr = format!("10.91.{index}.0/24");
        let created = ec2_form(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", cidr.as_str()),
                ("AvailabilityZone", zone.as_str()),
            ],
            case,
        )
        .await;
        subnets.push(ec2_element(&created, "subnetId", case));
    }
    // "An internet gateway ... to enable internet access": the registry a task's image is
    // fetched from is reached over this route, so a placement in these subnets can be
    // launched at all.
    let gateway = ec2_element(
        &ec2_form(router, "CreateInternetGateway", &[], case).await,
        "internetGatewayId",
        case,
    );
    ec2_form(
        router,
        "AttachInternetGateway",
        &[
            ("InternetGatewayId", gateway.as_str()),
            ("VpcId", vpc.as_str()),
        ],
        case,
    )
    .await;
    let table = ec2_element(
        &ec2_form(
            router,
            "DescribeRouteTables",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc.as_str())],
            case,
        )
        .await,
        "routeTableId",
        case,
    );
    ec2_form(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", table.as_str()),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", gateway.as_str()),
        ],
        case,
    )
    .await;
    subnets
}

/// Launch ONE instance into `subnet`, configured for `cluster`, and answer its instance id.
async fn launch(router: &MultiServiceRouter, subnet: &str, cluster: &str, case: &str) -> String {
    let user_data = launched_into(cluster);
    let launched = ec2_form(
        router,
        "RunInstances",
        &[
            ("ImageId", IMAGE_ID),
            ("InstanceType", INSTANCE_TYPE),
            ("SubnetId", subnet),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("UserData", user_data.as_str()),
        ],
        case,
    )
    .await;
    ec2_element(&launched, "instanceId", case)
}

/// The container instances `cluster` reports, read through `ListContainerInstances` and then
/// `DescribeContainerInstances` — never out of a store.
async fn described_hosts(router: &MultiServiceRouter, cluster: &str, case: &str) -> Vec<Value> {
    let listing = ok(
        router,
        "ListContainerInstances",
        json!({ "cluster": cluster, "status": ACTIVE }),
        case,
    )
    .await;
    let arns: Vec<String> = list(&listing, "containerInstanceArns", case)
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();
    if arns.is_empty() {
        return Vec::new();
    }
    let body = ok(
        router,
        "DescribeContainerInstances",
        json!({ "cluster": cluster, "containerInstances": arns }),
        case,
    )
    .await;
    list(&body, "containerInstances", case)
}

/// The ARNs of the hosts of `cluster` that are `ACTIVE` with their agent connected — the
/// capacity a placement can be made on.
async fn connected_hosts(
    router: &MultiServiceRouter,
    cluster: &str,
    case: &str,
) -> BTreeSet<String> {
    described_hosts(router, cluster, case)
        .await
        .iter()
        .filter(|host| {
            host["status"].as_str() == Some(ACTIVE) && host["agentConnected"].as_bool() == Some(true)
        })
        .map(|host| string(host, "containerInstanceArn", case))
        .collect()
}

/// Those hosts once `wanted` of them have registered: the registration is the agent's own and
/// owed by the clock, so a suite waits for the CONDITION rather than for a span.
async fn settled_hosts(
    router: &MultiServiceRouter,
    cluster: &str,
    wanted: usize,
    case: &str,
) -> BTreeSet<String> {
    let reading = poll_until(
        case,
        &format!("{wanted} connected container instance(s)"),
        || async {
            let hosts = connected_hosts(router, cluster, case).await;
            json!(hosts.into_iter().collect::<Vec<String>>())
        },
        |hosts| hosts.as_array().is_some_and(|hosts| hosts.len() >= wanted),
    )
    .await;
    reading
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

/// Create one Auto Scaling group of the account, with the launch template it launches from,
/// through each service's own API.
async fn create_group(router: &MultiServiceRouter, group: &str, cluster: &str, case: &str) {
    let template = format!("{group}-lt");
    let user_data = launched_into(cluster);
    ec2_form(
        router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", template.as_str()),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
            ("LaunchTemplateData.UserData", user_data.as_str()),
        ],
        case,
    )
    .await;
    let zone = format!("{SIGNED_REGION}a");
    auto_scaling(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("LaunchTemplate.LaunchTemplateName", template.as_str()),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("AvailabilityZones.member.1", zone.as_str()),
        ],
        case,
    )
    .await;
}

/// Register one revision whose containers share the host's networking (`bridge`), which is
/// what a placement onto the cluster's own capacity runs.
async fn register_bridge_definition(
    router: &MultiServiceRouter,
    family: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "bridge",
            "requiresCompatibilities": ["EC2"],
            "containerDefinitions": [{
                "name": "app",
                "image": "public.ecr.aws/docker/library/busybox:1.36",
                "essential": true,
                "cpu": 64,
                "memory": 64,
                "command": ["sh", "-c", "sleep 900"],
            }],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// Register one revision the FARGATE launch type can run: its own networking (`awsvpc`) and
/// the task-level size that infrastructure requires.
async fn register_fargate_definition(
    router: &MultiServiceRouter,
    family: &str,
    case: &str,
) -> String {
    let body = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "app",
                "image": "public.ecr.aws/docker/library/busybox:1.36",
                "essential": true,
                "command": ["sh", "-c", "sleep 900"],
            }],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}

/// One task of `cluster`, as `DescribeTasks` reports it.
async fn described_task(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": [arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task {arn} back, got {body}"))
}

/// That task once it is `RUNNING`: the start is owed by the clock, so the suite waits for the
/// status rather than for a span.
async fn running_task(router: &MultiServiceRouter, cluster: &str, arn: &str, case: &str) -> Value {
    poll_until(
        case,
        "the placed task to reach RUNNING",
        || described_task(router, cluster, arn, case),
        |task| task["lastStatus"] == json!(RUNNING),
    )
    .await
}

/// One row of the table: the strategy a run places through, how many tasks it asks for, and
/// what the tasks it places must report.
struct Row {
    case: &'static str,
    /// The `capacityProviderStrategy` the run carries, or `None` for the row that names a
    /// launch type outright.
    strategy: Option<Value>,
    /// The `launchType` the run names, where it names one.
    launch_type: Option<&'static str>,
    /// Whether the row runs the `awsvpc` revision Fargate requires rather than the `bridge`
    /// one the cluster's own capacity runs.
    fargate_revision: bool,
    /// How many tasks the run asks for.
    count: i64,
    /// The `launchType` every task it places must report.
    reported_launch_type: &'static str,
    /// Whether each task must name one of the cluster's `ACTIVE` container instances as its
    /// host, or no host at all.
    hosted: bool,
}

#[tokio::test]
async fn a_task_placed_through_a_capacity_provider_runs_on_the_capacity_that_provider_binds_onto() {
    let case = "the capacity-provider host table";
    let router = emulator_with_ec2();
    create_cluster(&router, CLUSTER, case).await;
    let zones = vec![format!("{SIGNED_REGION}a"), format!("{SIGNED_REGION}b")];
    let subnets = network(&router, &zones, case).await;
    for subnet in &subnets {
        launch(&router, subnet, CLUSTER, case).await;
    }
    let hosts = settled_hosts(&router, CLUSTER, subnets.len(), case).await;
    assert_eq!(
        hosts.len(),
        subnets.len(),
        "case {case:?}: expected the cluster to hold {} connected host(s) before the rows run, \
         got {hosts:?}",
        subnets.len()
    );

    // The providers the rows place through: two over Auto Scaling groups of the account, and
    // the two predefined Fargate providers, all associated with the cluster — "a capacity
    // provider must be associated with the cluster to be used in a capacity provider
    // strategy".
    create_group(&router, SCALED_GROUP, CLUSTER, case).await;
    create_group(&router, PLAIN_GROUP, CLUSTER, case).await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": SCALED_PROVIDER,
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": SCALED_GROUP,
                "managedScaling": { "status": "ENABLED" },
            },
        }),
        case,
    )
    .await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": PLAIN_PROVIDER,
            "autoScalingGroupProvider": { "autoScalingGroupArn": PLAIN_GROUP },
        }),
        case,
    )
    .await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": CLUSTER,
            "capacityProviders": [SCALED_PROVIDER, PLAIN_PROVIDER, FARGATE, FARGATE_SPOT],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;

    let bridge = register_bridge_definition(&router, "providerhostbridge", case).await;
    let fargate = register_fargate_definition(&router, "providerhostfargate", case).await;

    let rows: [Row; 6] = [
        Row {
            case: "two tasks through a provider over an Auto Scaling group: one host each",
            strategy: Some(json!([{ "capacityProvider": SCALED_PROVIDER, "weight": 1 }])),
            launch_type: None,
            fargate_revision: false,
            count: 2,
            reported_launch_type: LAUNCH_TYPE_EC2,
            hosted: true,
        },
        Row {
            case: "one task through that provider",
            strategy: Some(json!([{ "capacityProvider": SCALED_PROVIDER, "weight": 1 }])),
            launch_type: None,
            fargate_revision: false,
            count: 1,
            reported_launch_type: LAUNCH_TYPE_EC2,
            hosted: true,
        },
        Row {
            case: "a provider that states no managed-scaling block at all",
            strategy: Some(json!([{ "capacityProvider": PLAIN_PROVIDER, "weight": 1 }])),
            launch_type: None,
            fargate_revision: false,
            count: 1,
            reported_launch_type: LAUNCH_TYPE_EC2,
            hosted: true,
        },
        Row {
            case: "a strategy of two providers, each over its own group",
            strategy: Some(json!([
                { "capacityProvider": SCALED_PROVIDER, "weight": 1 },
                { "capacityProvider": PLAIN_PROVIDER, "weight": 1 },
            ])),
            launch_type: None,
            fargate_revision: false,
            count: 2,
            reported_launch_type: LAUNCH_TYPE_EC2,
            hosted: true,
        },
        Row {
            case: "the predefined FARGATE provider: capacity AWS owns, so no host",
            strategy: Some(json!([{ "capacityProvider": FARGATE, "weight": 1 }])),
            launch_type: None,
            fargate_revision: true,
            count: 1,
            reported_launch_type: LAUNCH_TYPE_FARGATE,
            hosted: false,
        },
        Row {
            case: "FARGATE_SPOT, the other Fargate provider: no host either",
            strategy: Some(json!([{ "capacityProvider": FARGATE_SPOT, "weight": 1 }])),
            launch_type: None,
            fargate_revision: true,
            count: 1,
            reported_launch_type: LAUNCH_TYPE_FARGATE,
            hosted: false,
        },
    ];

    // The contrast the rule is read against: the SAME placement with the launch type named
    // outright, which has always taken one of the cluster's hosts.
    let named = Row {
        case: "the EC2 launch type named outright",
        strategy: None,
        launch_type: Some(LAUNCH_TYPE_EC2),
        fargate_revision: false,
        count: 1,
        reported_launch_type: LAUNCH_TYPE_EC2,
        hosted: true,
    };

    for row in rows.iter().chain([&named]) {
        let case = row.case;
        let mut request = json!({
            "cluster": CLUSTER,
            "taskDefinition": if row.fargate_revision { fargate.clone() } else { bridge.clone() },
            "count": row.count,
            "startedBy": "providerhostrow",
        });
        if let Some(strategy) = row.strategy.clone() {
            request["capacityProviderStrategy"] = strategy;
        }
        if let Some(launch_type) = row.launch_type {
            request["launchType"] = json!(launch_type);
        }
        if row.fargate_revision {
            request["networkConfiguration"] =
                json!({ "awsvpcConfiguration": { "subnets": [subnets[0].clone()] } });
        }
        let body = ok(&router, "RunTask", request, case).await;
        let placed = list(&body, "tasks", case);
        assert_eq!(
            placed.len() as i64,
            row.count,
            "case {case:?}: expected the run to place {} task(s), got {body}",
            row.count
        );
        let arns: Vec<String> = placed
            .iter()
            .map(|task| string(task, "taskArn", case))
            .collect();

        let mut taken: BTreeSet<String> = BTreeSet::new();
        for arn in &arns {
            // A task on the cluster's own capacity is read once it RUNS there, which is the
            // reading the rule is about; a task on capacity AWS owns is read as it was placed,
            // since the host it names is decided by the placement and this suite mounts no
            // registry for its image to be fetched from.
            let task = if row.hosted {
                running_task(&router, CLUSTER, arn, case).await
            } else {
                described_task(&router, CLUSTER, arn, case).await
            };
            assert_eq!(
                string(&task, "launchType", case),
                row.reported_launch_type,
                "case {case:?}: expected the task to report the infrastructure it runs on, got \
                 {task}"
            );
            let host = task["containerInstanceArn"].as_str().map(str::to_string);
            if !row.hosted {
                assert_eq!(
                    host, None,
                    "case {case:?}: expected a task on capacity AWS owns to name no container \
                     instance, got {host:?}"
                );
                continue;
            }
            let host = host.unwrap_or_else(|| {
                panic!("case {case:?}: expected the task to name the host it runs on, got {task}")
            });
            assert!(
                hosts.contains(&host),
                "case {case:?}: expected the task's host to be one of the cluster's container \
                 instances {hosts:?}, got {host:?}"
            );
            // The host says the same thing back: it counts the task that runs on it.
            let counted = described_hosts(&router, CLUSTER, case)
                .await
                .into_iter()
                .find(|instance| instance["containerInstanceArn"] == json!(host))
                .map(|instance| {
                    count(&instance, "runningTasksCount", case)
                        + count(&instance, "pendingTasksCount", case)
                })
                .unwrap_or_default();
            assert!(
                counted > 0,
                "case {case:?}: expected the host {host} to count the task placed on it, got \
                 {counted}"
            );
            taken.insert(host);
        }
        if row.hosted && row.count > 1 {
            assert_eq!(
                taken.len() as i64,
                row.count,
                "case {case:?}: expected the scheduler to spread the run's {} tasks over that \
                 many of the cluster's hosts, got {taken:?}",
                row.count
            );
        }

        // Each row leaves the cluster as it found it, so the host the next row's spread picks
        // is not decided by the tasks this one left behind.
        for arn in &arns {
            ok(
                &router,
                "StopTask",
                json!({ "cluster": CLUSTER, "task": arn, "reason": "next row" }),
                case,
            )
            .await;
        }
    }
}

/// The cluster the service below runs in, and the service and family it runs.
const SERVICE_CLUSTER: &str = "providerhostservice";
const SERVICE: &str = "providerhostsvc";
const SERVICE_FAMILY: &str = "providerhostsvcfamily";

/// The group and provider the service places through.
const SERVICE_GROUP: &str = "providerhost-service-asg";
const SERVICE_PROVIDER: &str = "providerhostserviceprovider";

/// The same rule for the placement the SERVICE scheduler makes: a service that says where its
/// tasks run with a `capacityProviderStrategy` over an Auto Scaling group provider runs them
/// on the container instances of its cluster, and every task names the one that hosts it.
#[tokio::test]
async fn a_service_placed_through_a_capacity_provider_runs_its_tasks_on_the_clusters_hosts() {
    let case = "a service placed through a capacity provider";
    let router = emulator_with_ec2();
    create_cluster(&router, SERVICE_CLUSTER, case).await;
    let zones = vec![format!("{SIGNED_REGION}a"), format!("{SIGNED_REGION}b")];
    let subnets = network(&router, &zones, case).await;
    for subnet in &subnets {
        launch(&router, subnet, SERVICE_CLUSTER, case).await;
    }
    let hosts = settled_hosts(&router, SERVICE_CLUSTER, subnets.len(), case).await;
    assert_eq!(
        hosts.len(),
        subnets.len(),
        "case {case:?}: expected the cluster to hold {} connected host(s), got {hosts:?}",
        subnets.len()
    );

    create_group(&router, SERVICE_GROUP, SERVICE_CLUSTER, case).await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": SERVICE_PROVIDER,
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": SERVICE_GROUP,
                "managedScaling": { "status": "ENABLED" },
            },
        }),
        case,
    )
    .await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": SERVICE_CLUSTER,
            "capacityProviders": [SERVICE_PROVIDER],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    register_bridge_definition(&router, SERVICE_FAMILY, case).await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": SERVICE_CLUSTER,
            "serviceName": SERVICE,
            "taskDefinition": SERVICE_FAMILY,
            "desiredCount": 2,
            "capacityProviderStrategy": [
                { "capacityProvider": SERVICE_PROVIDER, "weight": 1 },
            ],
        }),
        case,
    )
    .await;

    // The scheduler's tasks, read back once the service runs the count it wants.
    let listed = poll_until(
        case,
        "the service to run the two tasks it wants",
        || async {
            ok(
                &router,
                "ListTasks",
                json!({
                    "cluster": SERVICE_CLUSTER,
                    "serviceName": SERVICE,
                    "desiredStatus": RUNNING,
                }),
                case,
            )
            .await
        },
        |body| {
            body["taskArns"]
                .as_array()
                .is_some_and(|arns| arns.len() == 2)
        },
    )
    .await;
    let arns: Vec<String> = list(&listed, "taskArns", case)
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect();

    let mut taken: BTreeSet<String> = BTreeSet::new();
    for arn in &arns {
        let task = running_task(&router, SERVICE_CLUSTER, arn, case).await;
        assert_eq!(
            string(&task, "launchType", case),
            LAUNCH_TYPE_EC2,
            "case {case:?}: expected the service's task to report the infrastructure its \
             provider binds onto, got {task}"
        );
        assert_eq!(
            string(&task, "capacityProviderName", case),
            SERVICE_PROVIDER,
            "case {case:?}: expected the task to name the provider that placed it, got {task}"
        );
        let host = task["containerInstanceArn"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the task to name the host it runs on, got {task}")
            });
        assert!(
            hosts.contains(&host),
            "case {case:?}: expected the host to be one of the cluster's container instances \
             {hosts:?}, got {host:?}"
        );
        taken.insert(host);
    }
    assert_eq!(
        taken.len(),
        2,
        "case {case:?}: expected the scheduler to spread the service's two tasks over two of \
         the cluster's hosts, got {taken:?}"
    );
}

/// The cluster, group, provider and family of the PARKED placement below.
const PARKED_CLUSTER: &str = "providerhostparked";
const PARKED_GROUP: &str = "providerhost-parked-asg";
const PARKED_PROVIDER: &str = "providerhostparkedprovider";
const PARKED_FAMILY: &str = "providerhostparkedfamily";

/// The same rule for the placement that has to WAIT for its capacity: a task routed through a
/// provider whose Auto Scaling group Amazon ECS scales is accepted before any container
/// instance exists ("Amazon ECS manages the scale-in and scale-out actions of the Auto Scaling
/// group"), so it is answered with no host at all — there is none — and is placed on the
/// container instance that capacity brings, which is the host it then reports.
#[tokio::test]
async fn a_task_parked_until_managed_scaling_brings_capacity_runs_on_the_host_it_brought() {
    let case = "a placement parked until its capacity arrives";
    let router = emulator_with_ec2();
    create_cluster(&router, PARKED_CLUSTER, case).await;
    // The group holds no capacity, and no instance is launched by hand: the only capacity this
    // cluster ever gets is the capacity Amazon ECS asks the group for.
    create_group(&router, PARKED_GROUP, PARKED_CLUSTER, case).await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": PARKED_PROVIDER,
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": PARKED_GROUP,
                "managedScaling": { "status": "ENABLED" },
            },
        }),
        case,
    )
    .await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": PARKED_CLUSTER,
            "capacityProviders": [PARKED_PROVIDER],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    let definition = register_bridge_definition(&router, PARKED_FAMILY, case).await;
    assert!(
        described_hosts(&router, PARKED_CLUSTER, case).await.is_empty(),
        "case {case:?}: expected the cluster to hold no capacity before the run"
    );

    let body = ok(
        &router,
        "RunTask",
        json!({
            "cluster": PARKED_CLUSTER,
            "taskDefinition": definition,
            "count": 1,
            "capacityProviderStrategy": [{ "capacityProvider": PARKED_PROVIDER, "weight": 1 }],
        }),
        case,
    )
    .await;
    let placed = list(&body, "tasks", case);
    assert_eq!(
        placed.len(),
        1,
        "case {case:?}: expected the run to be accepted while its capacity is acquired, got \
         {body}"
    );
    let arn = string(&placed[0], "taskArn", case);
    assert_eq!(
        placed[0]["containerInstanceArn"].as_str(),
        None,
        "case {case:?}: expected a task whose capacity does not exist yet to name no host, got \
         {}",
        placed[0]
    );

    // The capacity arrives on its own — Amazon ECS raises the group, the instance registers —
    // and the task is placed on it: waited for as a condition, since every step is clock-owed.
    let task = running_task(&router, PARKED_CLUSTER, &arn, case).await;
    let hosts = connected_hosts(&router, PARKED_CLUSTER, case).await;
    assert!(
        !hosts.is_empty(),
        "case {case:?}: expected the capacity Amazon ECS acquired to be a container instance \
         of the cluster, got {hosts:?}"
    );
    assert_eq!(
        string(&task, "launchType", case),
        LAUNCH_TYPE_EC2,
        "case {case:?}: expected the parked task to report the infrastructure its provider \
         binds onto, got {task}"
    );
    let host = task["containerInstanceArn"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the task placed by the capacity that arrived to name \
                 the host it runs on, got {task}"
            )
        });
    assert!(
        hosts.contains(&host),
        "case {case:?}: expected the host to be one of the cluster's container instances \
         {hosts:?}, got {host:?}"
    );
    // And the host counts it, which is what makes the binding the cluster's own rather than a
    // value on the task alone.
    let counted = described_hosts(&router, PARKED_CLUSTER, case)
        .await
        .into_iter()
        .find(|instance| instance["containerInstanceArn"] == json!(host))
        .map(|instance| {
            count(&instance, "runningTasksCount", case) + count(&instance, "pendingTasksCount", case)
        })
        .unwrap_or_default();
    assert!(
        counted > 0,
        "case {case:?}: expected the host {host} to count the task placed on it, got {counted}"
    );
}
