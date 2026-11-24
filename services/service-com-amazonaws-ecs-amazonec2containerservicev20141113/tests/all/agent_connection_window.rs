// SPDX-License-Identifier: CC0-1.0

//! The AGENT CONNECTION a container instance's registration reports, driven end to end over
//! the shared wire harness in [`crate::ecs_wire`] with the Amazon EC2 service mounted beside
//! this one on the same engine — so the machine under the registration is stopped and
//! terminated through EC2's OWN API and every value asserted is read back through
//! `DescribeContainerInstances`.
//!
//! # The rule under test
//!
//! > `ContainerInstance$agentConnected` reports the connection the container AGENT holds, not
//! > the state of the machine under it: a registration whose machine has just stopped running
//! > still reports the agent connected, and only once that connection has timed out does it
//! > report `agentConnected` false — and the registration of a machine that is GONE is
//! > deregistered at that same point, which is the `INACTIVE` status a deregistration leaves.
//!
//! `ContainerInstance$agentConnected` is "This parameter returns `true` if the agent is
//! connected to Amazon ECS" and "only instances connected to an agent can accept task
//! placement requests"; `DeregisterContainerInstance` documents the status a deregistration
//! leaves ("the instance then transitions to an `INACTIVE` status"). The agent runs ON the
//! machine and reports in over a connection of its own, so the connection outlives the instant
//! the machine stopped — which is why a client can distinguish "host stopping" from "agent
//! disconnected" at all.
//!
//! # What the rows span
//!
//! The rows differ along what the rule generalises over: WHY the machine stopped running (a
//! stop, which leaves the registration in place, and a terminate, which takes it away) and so
//! what the registration settles as once the connection has timed out. Both rows read the
//! registration IMMEDIATELY after the call that stopped the machine — the reading the rule is
//! about — and then wait for the condition rather than for a span.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    create_cluster, ec2_element, ec2_form, emulator_with_ec2, list, ok, poll_until, string,
    SIGNED_REGION,
};

/// The cluster both rows launch their machine into.
const CLUSTER: &str = "agentwindowfleet";

/// The AMI every launch boots, and the instance type it is launched as.
const IMAGE_ID: &str = "ami-0123456789abcdef0";
const INSTANCE_TYPE: &str = "t3.micro";

/// `ContainerInstance$status` of a registered instance, and of a deregistered one.
const ACTIVE: &str = "ACTIVE";
const INACTIVE: &str = "INACTIVE";

/// The user data a launch into `cluster` carries: the documented shell line that appends the
/// container agent's `ECS_CLUSTER` assignment to its configuration file, base64 as a launch
/// carries it.
fn launched_into(cluster: &str) -> String {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD.encode(format!(
        "#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n"
    ))
}

/// Create one VPC and one subnet, through Amazon EC2's own API.
async fn network(router: &MultiServiceRouter, cidr: &str, subnet_cidr: &str, case: &str) -> String {
    let vpc = ec2_element(
        &ec2_form(router, "CreateVpc", &[("CidrBlock", cidr)], case).await,
        "vpcId",
        case,
    );
    let zone = format!("{SIGNED_REGION}a");
    let created = ec2_form(
        router,
        "CreateSubnet",
        &[
            ("VpcId", vpc.as_str()),
            ("CidrBlock", subnet_cidr),
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

/// The registration of the container instance whose machine is `instance_id`, as
/// `DescribeContainerInstances` reports it — reached by the ARN it was registered under, since
/// a deregistered instance is no longer listed.
async fn described(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeContainerInstances",
        json!({ "cluster": CLUSTER, "containerInstances": [arn] }),
        case,
    )
    .await;
    list(&body, "containerInstances", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the registration {arn} back, got {body}"))
}

/// The ARN the machine `instance_id` is registered under, once its agent has registered it:
/// the registration is the agent's own and owed by the clock, so the suite waits for the
/// CONDITION rather than for a span.
async fn registered_arn(router: &MultiServiceRouter, instance_id: &str, case: &str) -> String {
    let listed = poll_until(
        case,
        "the launched machine to register as a container instance",
        || async {
            let listing = ok(
                router,
                "ListContainerInstances",
                json!({ "cluster": CLUSTER }),
                case,
            )
            .await;
            let arns: Vec<String> = list(&listing, "containerInstanceArns", case)
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect();
            if arns.is_empty() {
                return json!({ "containerInstances": [] });
            }
            ok(
                router,
                "DescribeContainerInstances",
                json!({ "cluster": CLUSTER, "containerInstances": arns }),
                case,
            )
            .await
        },
        |body| {
            body["containerInstances"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|instance| {
                    instance["ec2InstanceId"].as_str() == Some(instance_id)
                        && instance["agentConnected"].as_bool() == Some(true)
                })
        },
    )
    .await;
    let instance = list(&listed, "containerInstances", case)
        .into_iter()
        .find(|instance| instance["ec2InstanceId"].as_str() == Some(instance_id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a registration for {instance_id}, got {listed}")
        });
    string(&instance, "containerInstanceArn", case)
}

/// One row: the Amazon EC2 action that takes the machine out of service, and the
/// `ContainerInstance$status` its registration settles in once the agent's connection has
/// timed out.
struct Row {
    case: &'static str,
    /// The Amazon EC2 action, and the address range of the network the row's machine is
    /// launched into (one per row, so neither row's machine is the other's).
    action: &'static str,
    vpc_cidr: &'static str,
    subnet_cidr: &'static str,
    /// The status the registration settles in.
    settles_as: &'static str,
}

const ROWS: [Row; 2] = [
    Row {
        case: "a stopped machine: the registration stays, its agent stops reporting in",
        action: "StopInstances",
        vpc_cidr: "10.92.0.0/16",
        subnet_cidr: "10.92.0.0/24",
        settles_as: ACTIVE,
    },
    Row {
        case: "a terminated machine: the registration is deregistered with it",
        action: "TerminateInstances",
        vpc_cidr: "10.93.0.0/16",
        subnet_cidr: "10.93.0.0/24",
        settles_as: INACTIVE,
    },
];

#[tokio::test]
async fn the_agent_connection_outlives_the_machine_until_it_times_out() {
    let router = emulator_with_ec2();
    create_cluster(&router, CLUSTER, "setup").await;

    for row in &ROWS {
        let case = row.case;
        let subnet = network(&router, row.vpc_cidr, row.subnet_cidr, case).await;
        let instance_id = launch(&router, &subnet, CLUSTER, case).await;
        let arn = registered_arn(&router, &instance_id, case).await;

        // The machine is taken out of service through Amazon EC2's own API, and the
        // registration is read straight afterwards: the agent's connection is what Amazon ECS
        // holds, and nothing has timed it out yet.
        ec2_form(
            &router,
            row.action,
            &[("InstanceId.1", instance_id.as_str())],
            case,
        )
        .await;
        let straight_after = described(&router, &arn, case).await;
        assert_eq!(
            straight_after["agentConnected"].as_bool(),
            Some(true),
            "case {case:?}: expected the agent's connection to outlive the call that stopped \
             the machine, got {straight_after}"
        );
        assert_eq!(
            straight_after["status"].as_str(),
            Some(ACTIVE),
            "case {case:?}: expected the registration to still be {ACTIVE} straight after the \
             call, got {straight_after}"
        );

        // And then the connection times out, which is what takes the agent off the
        // registration — and the registration of a machine that is gone with it. Waited for as
        // a condition: the timeout is owed by the clock, and a read past it reports it.
        let settled = poll_until(
            case,
            "the agent's connection to time out",
            || described(&router, &arn, case),
            |instance| instance["agentConnected"] == json!(false),
        )
        .await;
        assert_eq!(
            settled["agentConnected"].as_bool(),
            Some(false),
            "case {case:?}: expected the timed-out connection to report no agent, got {settled}"
        );
        let settled = poll_until(
            case,
            &format!("the registration to settle as {}", row.settles_as),
            || described(&router, &arn, case),
            |instance| instance["status"] == json!(row.settles_as),
        )
        .await;
        assert_eq!(
            settled["status"].as_str(),
            Some(row.settles_as),
            "case {case:?}: expected the registration to settle as {}, got {settled}",
            row.settles_as
        );
    }
}
