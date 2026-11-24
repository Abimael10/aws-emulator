// SPDX-License-Identifier: CC0-1.0

//! The capacity an EC2-launch-type placement needs, driven end to end over the shared wire
//! harness in [`crate::ecs_wire`] with the Amazon EC2 service mounted beside this one on the
//! same engine — so every host is an instance launched through EC2's OWN API, every status is
//! moved through this service's own `UpdateContainerInstancesState`, and every verdict is the
//! answer `RunTask` gives.
//!
//! # The rule under test
//!
//! > A placement whose infrastructure is the cluster's own registered capacity (the `EC2` and
//! > `EXTERNAL` launch types) needs a container instance that will ACCEPT a placement: "only
//! > instances connected to an agent can accept task placement requests", and a `DRAINING`
//! > instance is one Amazon ECS "prevents new tasks from being scheduled for placement on".
//! > A cluster holding none of those has nowhere to put the task, which is what
//! > `InvalidParameterException` / "No Container Instances were found in your cluster."
//! > reports — whether the cluster holds no registration at all or only registrations that
//! > are beyond placement.
//!
//! `RunTask$launchType`: "The EC2 launch type runs your tasks on Amazon EC2 instances
//! registered to your cluster"; `ContainerInstance$agentConnected` and
//! `UpdateContainerInstancesState` are the two sentences above.
//! `Cluster$registeredContainerInstancesCount` is a different question — "this includes
//! container instances in both ACTIVE and DRAINING status" — so a cluster can report
//! capacity it registered while having none a placement can use.
//!
//! # What the rows span
//!
//! The rows differ along what the rule generalises over: WHY the cluster has no placeable
//! capacity (no registration at all; its one registration `DRAINING`; its one registration's
//! machine stopped, so the agent's connection timed out), and the two contrasts that must be
//! ACCEPTED (one connected `ACTIVE` instance, and a second instance that is `ACTIVE` beside a
//! `DRAINING` one).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    create_cluster, ec2_element, ec2_form, emulator_with_ec2, list, ok, poll_until,
    refusal, register_definition, string, SIGNED_REGION,
};

/// The AMI every launch boots, and the instance type it is launched as.
const IMAGE_ID: &str = "ami-0123456789abcdef0";
const INSTANCE_TYPE: &str = "t3.micro";

/// The family every row places.
const FAMILY: &str = "placeablecapacityfamily";

/// `ContainerInstance$status` a row may move its host to.
const DRAINING: &str = "DRAINING";

/// The fixed sentence a placement into a cluster with no capacity earns.
const NO_CONTAINER_INSTANCES: &str = "No Container Instances were found in your cluster.";

/// The user data a launch into `cluster` carries: the documented shell line that appends the
/// container agent's `ECS_CLUSTER` assignment to its configuration file, base64 as a launch
/// carries it.
fn launched_into(cluster: &str) -> String {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD.encode(format!(
        "#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n"
    ))
}

/// Create one VPC and one subnet for a row, through Amazon EC2's own API.
async fn network(router: &MultiServiceRouter, index: usize, case: &str) -> String {
    let vpc = ec2_element(
        &ec2_form(
            router,
            "CreateVpc",
            &[("CidrBlock", format!("10.{}.0.0/16", 100 + index).as_str())],
            case,
        )
        .await,
        "vpcId",
        case,
    );
    let zone = format!("{SIGNED_REGION}a");
    let created = ec2_form(
        router,
        "CreateSubnet",
        &[
            ("VpcId", vpc.as_str()),
            ("CidrBlock", format!("10.{}.0.0/24", 100 + index).as_str()),
            ("AvailabilityZone", zone.as_str()),
        ],
        case,
    )
    .await;
    ec2_element(&created, "subnetId", case)
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

/// The container instances `cluster` holds, read through `ListContainerInstances` and then
/// `DescribeContainerInstances`.
async fn described_hosts(router: &MultiServiceRouter, cluster: &str, case: &str) -> Vec<Value> {
    let listing = ok(
        router,
        "ListContainerInstances",
        json!({ "cluster": cluster }),
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

/// The ARN of the host launched into `cluster`, once its agent has registered it: the
/// registration is the agent's own and owed by the clock, so the suite waits for the
/// CONDITION rather than for a span.
async fn registered_host(
    router: &MultiServiceRouter,
    cluster: &str,
    wanted: usize,
    case: &str,
) -> Vec<String> {
    let reading = poll_until(
        case,
        &format!("{wanted} connected container instance(s)"),
        || async {
            let hosts: Vec<String> = described_hosts(router, cluster, case)
                .await
                .iter()
                .filter(|host| host["agentConnected"].as_bool() == Some(true))
                .map(|host| string(host, "containerInstanceArn", case))
                .collect();
            json!(hosts)
        },
        |hosts| hosts.as_array().is_some_and(|hosts| hosts.len() == wanted),
    )
    .await;
    reading
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

/// What a row does to the capacity it launched before it places its task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capacity {
    /// Nothing is launched at all.
    None,
    /// One host, left connected and `ACTIVE`.
    OneConnected,
    /// One host, moved to `DRAINING`.
    OneDraining,
    /// One host whose Amazon EC2 machine is stopped, so the agent's connection times out.
    OneStopped,
    /// Two hosts, one of them moved to `DRAINING`.
    TwoOneDraining,
}

/// One row: what capacity the cluster holds and whether the placement is accepted.
struct Row {
    case: &'static str,
    capacity: Capacity,
    /// Whether `RunTask` must place the task, rather than refuse the call.
    placed: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a cluster with no registration at all",
        capacity: Capacity::None,
        placed: false,
    },
    Row {
        case: "one connected ACTIVE host: the placement is made",
        capacity: Capacity::OneConnected,
        placed: true,
    },
    Row {
        case: "its one host DRAINING: new tasks are not scheduled on it",
        capacity: Capacity::OneDraining,
        placed: false,
    },
    Row {
        case: "its one host's machine stopped, so no agent is connected",
        capacity: Capacity::OneStopped,
        placed: false,
    },
    Row {
        case: "two hosts, one DRAINING: the other still accepts the placement",
        capacity: Capacity::TwoOneDraining,
        placed: true,
    },
];

#[tokio::test]
async fn an_ec2_placement_needs_a_container_instance_that_accepts_one() {
    let router = emulator_with_ec2();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let cluster = format!("placeablecapacity{index}");
        create_cluster(&router, &cluster, case).await;
        register_definition(&router, FAMILY, case).await;

        let hosts = match row.capacity {
            Capacity::None => Vec::new(),
            Capacity::TwoOneDraining => {
                let subnet = network(&router, index, case).await;
                launch(&router, &subnet, &cluster, case).await;
                launch(&router, &subnet, &cluster, case).await;
                registered_host(&router, &cluster, 2, case).await
            }
            _ => {
                let subnet = network(&router, index, case).await;
                launch(&router, &subnet, &cluster, case).await;
                registered_host(&router, &cluster, 1, case).await
            }
        };

        match row.capacity {
            Capacity::OneDraining | Capacity::TwoOneDraining => {
                ok(
                    &router,
                    "UpdateContainerInstancesState",
                    json!({
                        "cluster": cluster,
                        "containerInstances": [hosts[0].clone()],
                        "status": DRAINING,
                    }),
                    case,
                )
                .await;
            }
            Capacity::OneStopped => {
                let machine = described_hosts(&router, &cluster, case)
                    .await
                    .into_iter()
                    .next()
                    .map(|host| string(&host, "ec2InstanceId", case))
                    .unwrap_or_else(|| panic!("case {case:?}: expected the host's machine id"));
                ec2_form(
                    &router,
                    "StopInstances",
                    &[("InstanceId.1", machine.as_str())],
                    case,
                )
                .await;
                // The agent's connection outlives the machine until it times out, so the
                // suite waits for the registration to report it gone rather than for a span.
                poll_until(
                    case,
                    "the agent's connection to time out",
                    || async {
                        json!(described_hosts(&router, &cluster, case)
                            .await
                            .iter()
                            .map(|host| host["agentConnected"].as_bool().unwrap_or(false))
                            .collect::<Vec<bool>>())
                    },
                    |connected| {
                        connected
                            .as_array()
                            .is_some_and(|flags| flags.iter().all(|flag| flag == &json!(false)))
                    },
                )
                .await;
            }
            _ => {}
        }

        let request = json!({
            "cluster": cluster.clone(),
            "taskDefinition": FAMILY,
            "launchType": "EC2",
            "count": 1,
        });
        if !row.placed {
            let (code, message) = refusal(&router, "RunTask", request, case).await;
            assert_eq!(
                code, "InvalidParameterException",
                "case {case:?}: expected the modelled refusal of a placement with no \
                 capacity, got {code:?} with {message:?}"
            );
            assert_eq!(
                message, NO_CONTAINER_INSTANCES,
                "case {case:?}: expected the fixed no-capacity sentence, got {message:?}"
            );
            // Nothing was placed: the cluster holds no task to read back.
            let listed = ok(&router, "ListTasks", json!({ "cluster": cluster }), case).await;
            assert!(
                list(&listed, "taskArns", case).is_empty(),
                "case {case:?}: expected the refused run to place nothing, got {listed}"
            );
            continue;
        }

        let body = ok(&router, "RunTask", request, case).await;
        let tasks = list(&body, "tasks", case);
        assert_eq!(
            tasks.len(),
            1,
            "case {case:?}: expected the run to place its task, got {body}"
        );
        // And it was placed on a host that ACCEPTS a placement, which the DRAINING one does
        // not.
        let host = string(&tasks[0], "containerInstanceArn", case);
        let accepting: Vec<String> = described_hosts(&router, &cluster, case)
            .await
            .iter()
            .filter(|instance| {
                instance["status"].as_str() == Some("ACTIVE")
                    && instance["agentConnected"].as_bool() == Some(true)
            })
            .map(|instance| string(instance, "containerInstanceArn", case))
            .collect();
        assert!(
            accepting.contains(&host),
            "case {case:?}: expected the task's host to be one that accepts a placement \
             {accepting:?}, got {host:?}"
        );
    }
}
