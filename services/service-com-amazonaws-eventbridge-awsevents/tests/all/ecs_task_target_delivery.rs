// SPDX-License-Identifier: CC0-1.0

//! An AMAZON ECS TASK as the target of a rule: what a delivery to it runs, end to end over
//! the wire through the real `MultiServiceRouter` with Amazon ECS and Amazon EC2 mounted
//! beside EventBridge — so the task a delivery started is read back where a client reads
//! it, through `ecs:ListTasks`/`ecs:DescribeTasks`.
//!
//! The RULE asserted here: a rule target whose `Arn` names an Amazon ECS CLUSTER is
//! delivered to by making `ecs:RunTask` on that cluster with the target's own
//! `EcsParameters` as the members of the run — "contains the Amazon ECS task definition and
//! task count to be used, if the event target is an Amazon ECS task"
//! (`Target$EcsParameters`), whose `TaskCount` is "the number of tasks to create based on
//! `TaskDefinition`. The default is 1". So Amazon ECS is what resolves the reference the
//! block pins and what refuses it: a block pinning `family:revision` keeps launching THAT
//! revision however many newer ones are registered, a block naming the family alone
//! launches the latest `ACTIVE` revision, and a block pinning a revision that has been
//! deregistered launches nothing and fails the delivery ("you can't use an `INACTIVE` task
//! definition to run new tasks") until it is re-pinned to an active one.
//!
//! The rows differ along what the rule generalises over: whether the pinned reference names
//! a revision or a family, which revision it names of a family that has several, the status
//! of the revision it names, how many tasks the block asks for, and the networking the
//! block hands the run.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{InvocationOutcome, SERVICE_PRINCIPAL};

use crate::target_dispatch::{
    ec2, ecs, emulator, given_role, given_rule, invocations, publish, xml_text, Emulator,
};

/// The action the role a task target names has to carry: EventBridge starts the task by
/// making this call on the cluster.
const RUN_TASK_ACTION: &str = "ecs:RunTask";

/// The container image the registered revisions run. Nothing is pulled here — the revision
/// only has to be one Fargate accepts.
const IMAGE: &str = "public.ecr.aws/docker/library/busybox:1.36";

/// The CIDR block the test VPC and its subnet are given, well inside the private ranges
/// RFC 1918 reserves.
const VPC_CIDR: &str = "10.42.0.0/16";
const SUBNET_CIDR: &str = "10.42.1.0/24";

/// How long a task a delivery ran is given to appear, and how long a read waits before
/// looking again. The budget is generous: what the assertion depends on is the run having
/// been made, not on how fast the host runs.
const LAUNCH_BUDGET: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One ECS call that must succeed.
async fn ecs_ok(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = ecs(emulator, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ecs {operation} answered {status} {answer}"
    );
    answer
}

/// A VPC with one subnet in it, which is where an `awsvpc` task's interface is created;
/// answers the subnet id.
async fn given_subnet(emulator: &Emulator, case: &str) -> String {
    given_network(emulator, case, false).await
}

/// The same, with the subnet ROUTED to the internet through an internet gateway — the
/// placement a Fargate task that pulls a public image has to be made into: the VPC's own
/// (main) route table carries the default route, which is the table a subnet with no
/// association of its own uses.
async fn given_routed_subnet(emulator: &Emulator, case: &str) -> String {
    given_network(emulator, case, true).await
}

/// A VPC, a subnet in it, and — when `routed` — an internet gateway with the default route
/// to it in the VPC's main route table. Answers the subnet id.
async fn given_network(emulator: &Emulator, case: &str, routed: bool) -> String {
    let (status, created) = ec2(emulator, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVpc answered {status}: {created}"
    );
    let vpc = xml_text(&created, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: no vpcId in {created}"));
    let (status, created) = ec2(
        emulator,
        "CreateSubnet",
        &[("VpcId", &vpc), ("CidrBlock", SUBNET_CIDR)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSubnet answered {status}: {created}"
    );
    let subnet = xml_text(&created, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: no subnetId in {created}"));
    if routed {
        let (status, created) = ec2(emulator, "CreateInternetGateway", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateInternetGateway answered {status}: {created}"
        );
        let gateway = xml_text(&created, "internetGatewayId")
            .unwrap_or_else(|| panic!("case {case:?}: no internetGatewayId in {created}"));
        let (status, attached) = ec2(
            emulator,
            "AttachInternetGateway",
            &[("VpcId", &vpc), ("InternetGatewayId", &gateway)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: AttachInternetGateway answered {status}: {attached}"
        );
        let (status, tables) = ec2(
            emulator,
            "DescribeRouteTables",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeRouteTables answered {status}: {tables}"
        );
        let table = xml_text(&tables, "routeTableId")
            .unwrap_or_else(|| panic!("case {case:?}: no routeTableId in {tables}"));
        let (status, routed) = ec2(
            emulator,
            "CreateRoute",
            &[
                ("RouteTableId", &table),
                ("DestinationCidrBlock", "0.0.0.0/0"),
                ("GatewayId", &gateway),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateRoute answered {status}: {routed}"
        );
    }
    subnet
}

/// A cluster called `name`; answers the ARN a rule's target names it by.
async fn given_cluster(emulator: &Emulator, case: &str, name: &str) -> String {
    let created = ecs_ok(emulator, case, "CreateCluster", &json!({ "clusterName": name })).await;
    created["cluster"]["clusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no clusterArn in {created}"))
        .to_string()
}

/// One `awsvpc`/Fargate revision of `family`; answers the ARN it was registered under,
/// which carries its revision number.
async fn given_revision(emulator: &Emulator, case: &str, family: &str) -> String {
    let registered = ecs_ok(
        emulator,
        case,
        "RegisterTaskDefinition",
        &json!({
            "family": family,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [{
                "name": "app",
                "image": IMAGE,
                "essential": true,
            }],
        }),
    )
    .await;
    registered["taskDefinition"]["taskDefinitionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no taskDefinitionArn in {registered}"))
        .to_string()
}

/// The task ARNs the cluster holds, read as a client reads them.
async fn tasks_of(emulator: &Emulator, case: &str, cluster: &str) -> Vec<String> {
    let listed = ecs_ok(emulator, case, "ListTasks", &json!({ "cluster": cluster })).await;
    listed["taskArns"]
        .as_array()
        .map(|arns| {
            arns.iter()
                .filter_map(|arn| arn.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The cluster's tasks, read again until there are `wanted` of them or the budget runs out:
/// a delivery is made inside `PutEvents`, so the first read usually answers, and the loop is
/// what keeps the assertion about the run rather than about the host.
async fn tasks_until(emulator: &Emulator, case: &str, cluster: &str, wanted: usize) -> Vec<String> {
    let deadline = Instant::now() + LAUNCH_BUDGET;
    let mut seen = tasks_of(emulator, case, cluster).await;
    while seen.len() < wanted && Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        seen = tasks_of(emulator, case, cluster).await;
    }
    seen
}

/// What `DescribeTasks` reports for `arns`.
async fn described(emulator: &Emulator, case: &str, cluster: &str, arns: &[String]) -> Vec<Value> {
    let described = ecs_ok(
        emulator,
        case,
        "DescribeTasks",
        &json!({ "cluster": cluster, "tasks": arns }),
    )
    .await;
    described["tasks"].as_array().cloned().unwrap_or_default()
}

/// The target one of these rules carries: the cluster, the role, and the `EcsParameters`
/// block the run is made from.
fn ecs_target(cluster: &str, role: &str, parameters: Value) -> Value {
    json!([{
        "Id": "the-task",
        "Arn": cluster,
        "RoleArn": role,
        "EcsParameters": parameters,
    }])
}

/// The `EcsParameters` a Fargate task of `task_definition` is run from, in the subnet
/// `subnet`.
fn fargate_parameters(task_definition: &str, subnet: &str) -> Value {
    json!({
        "TaskDefinitionArn": task_definition,
        "LaunchType": "FARGATE",
        "NetworkConfiguration": {
            "awsvpcConfiguration": { "Subnets": [subnet], "AssignPublicIp": "DISABLED" },
        },
    })
}

/// The task a rule's firing ran, as a client FINDS it: `ListTasks` narrowed to the
/// `startedBy` the delivery stamped, and the lifecycle that task then runs through.
///
/// `RunTask$startedBy` is "an optional tag specified when a task is started", and
/// EventBridge stamps the rule its delivery ran the task for — so a client that knows the
/// rule can list exactly the tasks that rule launched, however many other tasks the cluster
/// holds. The task then reaches `RUNNING` like any other Fargate task placed into a routed
/// subnet with a public address ("PROVISIONING ... PENDING ... RUNNING", the documented task
/// lifecycle), which is what makes the launch observable rather than merely accepted.
#[tokio::test]
async fn a_launched_task_is_found_by_the_rule_that_started_it_and_reaches_running() {
    let case = "a routed public subnet, a task that pulls its image";
    let emulator = emulator();
    let subnet = given_routed_subnet(&emulator, case).await;
    let cluster = given_cluster(&emulator, case, "found-by-rule").await;
    let revision = given_revision(&emulator, case, "ebecs-found").await;
    let role = given_role(
        &emulator,
        "events-run-task",
        SERVICE_PRINCIPAL,
        Some(RUN_TASK_ACTION),
    )
    .await;
    // A rule name of the length a client really uses, so what the delivery stamps is read
    // back whole rather than only for a short name.
    let rule = "to-the-task-launched-by-a-named-rule";
    let mut parameters = fargate_parameters(&revision, &subnet);
    parameters["NetworkConfiguration"]["awsvpcConfiguration"]["AssignPublicIp"] = json!("ENABLED");
    given_rule(&emulator, rule, ecs_target(&cluster, &role, parameters)).await;

    publish(&emulator).await;

    // Found by the rule that ran it, with no other filter.
    let deadline = Instant::now() + LAUNCH_BUDGET;
    let mut arns = started_by(&emulator, case, &cluster, rule).await;
    while arns.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
        arns = started_by(&emulator, case, &cluster, rule).await;
    }
    assert_eq!(
        arns.len(),
        1,
        "case {case:?}: expected the rule's own task, got {arns:?} ({:?})",
        invocations(&emulator)
    );

    // And it runs: the launch was made into a subnet its image can be fetched from, so the
    // task goes through its lifecycle to RUNNING rather than stopping.
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let tasks = described(&emulator, case, &cluster, &arns).await;
        last = tasks.first().cloned().unwrap_or(Value::Null);
        if last["lastStatus"] == json!("RUNNING") {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: expected the launched task to reach RUNNING, got {last} ({:?})",
        invocations(&emulator)
    );
}

/// The tasks of `cluster` that `rule`'s firing ran, as `ListTasks` narrowed by `startedBy`
/// reports them.
async fn started_by(emulator: &Emulator, case: &str, cluster: &str, rule: &str) -> Vec<String> {
    let listed = ecs_ok(
        emulator,
        case,
        "ListTasks",
        &json!({ "cluster": cluster, "startedBy": format!("events-rule/{rule}") }),
    )
    .await;
    listed["taskArns"]
        .as_array()
        .map(|arns| {
            arns.iter()
                .filter_map(|arn| arn.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// WHICH revision a delivery runs is the one the target's `EcsParameters` pin: a reference
/// naming a revision keeps launching that revision after newer ones are registered, and one
/// naming the family alone launches the latest `ACTIVE` revision — over a table whose rows
/// differ along the reference the block carries.
#[tokio::test]
async fn a_task_target_runs_the_revision_its_parameters_pin() {
    // (case, which of the three registered revisions the block pins — `None` names the
    //  family alone, the revision the launched task is expected to run)
    let rows: [(&str, Option<i32>, i32); 5] = [
        ("the first revision, with two newer ones registered", Some(1), 1),
        ("the middle revision", Some(2), 2),
        ("the latest revision, named explicitly", Some(3), 3),
        ("the family alone, which is the latest ACTIVE revision", None, 3),
        // The same pin as the first row, stated again to show it is the PIN and not the
        // order of registration that decides it: the family has three revisions either way.
        ("the first revision again", Some(1), 1),
    ];

    for (case, pinned, expected) in rows {
        let emulator = emulator();
        let subnet = given_subnet(&emulator, case).await;
        let cluster = given_cluster(&emulator, case, "pinned-revisions").await;
        let family = "ebecs-pinned";
        let mut registered = Vec::new();
        for _ in 0..3 {
            registered.push(given_revision(&emulator, case, family).await);
        }
        let reference = match pinned {
            Some(revision) => family_arn(&registered[0], &format!("{family}:{revision}")),
            None => family_arn(&registered[0], family),
        };
        let role = given_role(&emulator, "events-run-task", SERVICE_PRINCIPAL, Some(RUN_TASK_ACTION))
            .await;
        given_rule(
            &emulator,
            "to-the-task",
            ecs_target(&cluster, &role, fargate_parameters(&reference, &subnet)),
        )
        .await;

        publish(&emulator).await;

        let arns = tasks_until(&emulator, case, &cluster, 1).await;
        assert_eq!(
            arns.len(),
            1,
            "case {case:?}: one delivery runs one task, got {arns:?} ({:?})",
            invocations(&emulator)
        );
        let tasks = described(&emulator, case, &cluster, &arns).await;
        let wanted = format!("{family}:{expected}");
        assert!(
            tasks[0]["taskDefinitionArn"]
                .as_str()
                .is_some_and(|arn| arn.ends_with(&wanted)),
            "case {case:?}: expected the task to run {wanted}, got {}",
            tasks[0]["taskDefinitionArn"]
        );
        // The task carries the rule that ran it, which is what tells a client's own tasks
        // from the ones a rule's firing launched.
        assert_eq!(
            tasks[0]["startedBy"].as_str(),
            Some("events-rule/to-the-task"),
            "case {case:?}: expected the task to be started by the rule, got {}",
            tasks[0]["startedBy"]
        );
    }
}

/// The ARN of `reference` in the same account and Region as `registered`, which is the ARN
/// a target's `TaskDefinitionArn` names a revision (or a family) by.
fn family_arn(registered: &str, reference: &str) -> String {
    let (prefix, _) = registered
        .split_once("task-definition/")
        .unwrap_or_else(|| panic!("a registered revision's ARN names a task definition: {registered}"));
    format!("{prefix}task-definition/{reference}")
}

/// A revision the block pins that has been DEREGISTERED launches nothing — "you can't use
/// an INACTIVE task definition to run new tasks" — and the delivery fails, so the failure
/// is what a client sees rather than a silent nothing; re-pinning the target to an ACTIVE
/// revision launches again, from the same rule and the same event.
#[tokio::test]
async fn a_task_target_pinned_to_a_deregistered_revision_launches_nothing_until_it_is_repinned() {
    let case = "a pin that is deregistered, then repinned";
    let emulator = emulator();
    let subnet = given_subnet(&emulator, case).await;
    let cluster = given_cluster(&emulator, case, "repinned").await;
    let family = "ebecs-repinned";
    let first = given_revision(&emulator, case, family).await;
    let role =
        given_role(&emulator, "events-run-task", SERVICE_PRINCIPAL, Some(RUN_TASK_ACTION)).await;
    given_rule(
        &emulator,
        "to-the-task",
        ecs_target(&cluster, &role, fargate_parameters(&first, &subnet)),
    )
    .await;

    // The pinned revision is deregistered, so the launch it describes is one Amazon ECS
    // will not make.
    ecs_ok(
        &emulator,
        case,
        "DeregisterTaskDefinition",
        &json!({ "taskDefinition": first }),
    )
    .await;

    publish(&emulator).await;
    let blocked = tasks_of(&emulator, case, &cluster).await;
    assert!(
        blocked.is_empty(),
        "case {case:?}: a pin to a deregistered revision runs no task, got {blocked:?}"
    );
    let recorded = invocations(&emulator);
    assert_eq!(
        recorded.len(),
        1,
        "case {case:?}: expected one handover, got {recorded:?}"
    );
    assert!(
        matches!(recorded[0].2, InvocationOutcome::Failed { .. }),
        "case {case:?}: the blocked launch is a FAILED delivery, got {recorded:?}"
    );

    // Re-pinned to a revision that IS active, the same rule and the same event launch.
    let second = given_revision(&emulator, case, family).await;
    let (status, body) = crate::target_dispatch::events(
        &emulator,
        "PutTargets",
        &json!({
            "Rule": "to-the-task",
            "Targets": ecs_target(&cluster, &role, fargate_parameters(&second, &subnet)),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: re-pinning the target answered {status} {body}"
    );

    publish(&emulator).await;
    let arns = tasks_until(&emulator, case, &cluster, 1).await;
    assert_eq!(
        arns.len(),
        1,
        "case {case:?}: the re-pinned target launches, got {arns:?} ({:?})",
        invocations(&emulator)
    );
    let tasks = described(&emulator, case, &cluster, &arns).await;
    assert_eq!(
        tasks[0]["taskDefinitionArn"].as_str(),
        Some(second.as_str()),
        "case {case:?}: expected the task to run the revision it was re-pinned to, got {}",
        tasks[0]["taskDefinitionArn"]
    );
}

/// The `AssignPublicIp` of a task target's network configuration governs the REAL public
/// address of the interface the launched task runs on: "whether the task's elastic network
/// interface receives a public IP address" (`AwsVpcConfiguration$assignPublicIp`), which is
/// a property of the interface Amazon VPC created — so Amazon EC2's own
/// `DescribeNetworkInterfaces` answers for the id the task's attachment reports, and that
/// interface holds a public address exactly when the target asked for one.
///
/// The two rows are the two values of the member, over the same rule, the same subnet and
/// the same revision. Both are decided on ONE read of the interface, which that read
/// asserts answered 200 for: absence of the association is then the interface holding no
/// public address, and never an interface that is gone.
#[tokio::test]
async fn assign_public_ip_governs_the_launched_tasks_interface() {
    // (case, the `AssignPublicIp` the block states, whether the interface holds a public
    //  address)
    let rows: [(&str, &str, bool); 2] = [
        ("the member asking for a public address", "ENABLED", true),
        ("the member asking for none", "DISABLED", false),
    ];

    for (case, assign_public_ip, expect_public_ip) in rows {
        let emulator = emulator();
        let subnet = default_subnet(&emulator, case).await;
        let cluster = given_cluster(&emulator, case, "public-ip").await;
        let revision = given_revision(&emulator, case, "ebecs-public-ip").await;
        let role = given_role(
            &emulator,
            "events-run-task",
            SERVICE_PRINCIPAL,
            Some(RUN_TASK_ACTION),
        )
        .await;
        let mut parameters = fargate_parameters(&revision, &subnet);
        parameters["NetworkConfiguration"]["awsvpcConfiguration"]["AssignPublicIp"] =
            json!(assign_public_ip);
        given_rule(
            &emulator,
            "to-the-task",
            ecs_target(&cluster, &role, parameters),
        )
        .await;

        publish(&emulator).await;

        // The interface the launched task was allocated, as the task's own attachment names
        // it — the join key between the Amazon ECS view of the task and the Amazon EC2 view
        // of its network.
        let interface_id = interface_of_launched_task(&emulator, case, &cluster).await;
        let interface = ec2_interface(&emulator, case, &interface_id).await;
        assert!(
            interface.contains("<requesterManaged>true</requesterManaged>"),
            "case {case:?}: the interface Amazon ECS created for the task is managed by it, \
             got {interface}"
        );
        assert_eq!(
            xml_text(&interface, "subnetId").as_deref(),
            Some(subnet.as_str()),
            "case {case:?}: the interface sits in the subnet the target named, got \
             {interface}"
        );
        // The public address is the INTERFACE's own, assigned when Amazon ECS created it,
        // so the one read above — which answered 200 for an interface that EXISTS — decides
        // the row: an interface holding no association holds no public address.
        let public_ip = xml_text(&interface, "publicIp");
        assert_eq!(
            public_ip.is_some(),
            expect_public_ip,
            "case {case:?}: expected the interface to hold a public address: \
             {expect_public_ip}, got {public_ip:?} in {interface} ({:?})",
            invocations(&emulator)
        );
    }
}

/// The first subnet of the account, as a client reads them — the networking a target that
/// names no VPC of its own places its task in.
async fn default_subnet(emulator: &Emulator, case: &str) -> String {
    let (status, listed) = ec2(emulator, "DescribeSubnets", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSubnets answered {status}: {listed}"
    );
    xml_text(&listed, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: the account holds no subnet: {listed}"))
}

/// The `networkInterfaceId` the launched task's elastic-network-interface attachment
/// reports, read again until it is there: Amazon ECS creates and attaches that interface
/// while the task provisions, so the detail appears once the interface is in place.
async fn interface_of_launched_task(emulator: &Emulator, case: &str, cluster: &str) -> String {
    let deadline = Instant::now() + LAUNCH_BUDGET;
    loop {
        for task in tasks_in_any_state(emulator, case, cluster).await {
            if let Some(id) = task["attachments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|attachment| attachment["type"] == json!("ElasticNetworkInterface"))
                .flat_map(|attachment| {
                    attachment["details"].as_array().cloned().unwrap_or_default()
                })
                .find(|detail| detail["name"] == json!("networkInterfaceId"))
                .and_then(|detail| detail["value"].as_str().map(str::to_string))
            {
                return id;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the launched task never reported the interface it runs on"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every task of `cluster`, whichever status it is wanted in: a task on its way out is one
/// the launch still created.
async fn tasks_in_any_state(emulator: &Emulator, case: &str, cluster: &str) -> Vec<Value> {
    let mut arns = tasks_of(emulator, case, cluster).await;
    let stopped = ecs_ok(
        emulator,
        case,
        "ListTasks",
        &json!({ "cluster": cluster, "desiredStatus": "STOPPED" }),
    )
    .await;
    arns.extend(
        stopped["taskArns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|arn| arn.as_str().map(str::to_string)),
    );
    if arns.is_empty() {
        return Vec::new();
    }
    described(emulator, case, cluster, &arns).await
}

/// The interface Amazon EC2 holds under `interface_id`, as a client reads it — the join the
/// attachment's detail is for.
async fn ec2_interface(emulator: &Emulator, case: &str, interface_id: &str) -> String {
    let (status, body) = ec2(
        emulator,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", interface_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the id {interface_id} the task's attachment reports is an interface \
         Amazon EC2 holds; DescribeNetworkInterfaces answered {status}: {body}"
    );
    body
}

/// How many tasks one delivery runs is the block's own `TaskCount` — "the number of tasks to
/// create based on `TaskDefinition`. The default is 1" — over a table whose rows differ
/// along the count the block states.
#[tokio::test]
async fn a_task_target_runs_as_many_tasks_as_its_parameters_ask_for() {
    // (case, the `TaskCount` the block states, how many tasks the delivery runs)
    let rows: [(&str, Option<i32>, usize); 3] = [
        ("no count at all, which is the documented default", None, 1),
        ("the lower bound of the member's own range", Some(1), 1),
        ("a count above one", Some(3), 3),
    ];

    for (case, count, expected) in rows {
        let emulator = emulator();
        let subnet = given_subnet(&emulator, case).await;
        let cluster = given_cluster(&emulator, case, "counted").await;
        let revision = given_revision(&emulator, case, "ebecs-counted").await;
        let role = given_role(
            &emulator,
            "events-run-task",
            SERVICE_PRINCIPAL,
            Some(RUN_TASK_ACTION),
        )
        .await;
        let mut parameters = fargate_parameters(&revision, &subnet);
        if let Some(count) = count {
            parameters["TaskCount"] = json!(count);
        }
        given_rule(
            &emulator,
            "to-the-task",
            ecs_target(&cluster, &role, parameters),
        )
        .await;

        publish(&emulator).await;

        let arns = tasks_until(&emulator, case, &cluster, expected).await;
        assert_eq!(
            arns.len(),
            expected,
            "case {case:?}: expected {expected} task(s), got {arns:?} ({:?})",
            invocations(&emulator)
        );
        for task in described(&emulator, case, &cluster, &arns).await {
            assert_eq!(
                task["taskDefinitionArn"].as_str(),
                Some(revision.as_str()),
                "case {case:?}: every task runs the pinned revision, got {}",
                task["taskDefinitionArn"]
            );
        }
    }
}
