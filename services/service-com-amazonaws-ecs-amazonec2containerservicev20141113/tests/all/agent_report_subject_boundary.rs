// SPDX-License-Identifier: CC0-1.0

//! The agent-facing `Submit*StateChange` validation boundary, driven end to end over the
//! shared wire harness in [`crate::ecs_wire`] with the Amazon EC2 service mounted beside
//! this one on the same engine — so the capacity the EC2-backed rows report about is
//! LAUNCHED through EC2's own API and every value asserted is read back through
//! `DescribeTasks` and `DescribeContainerInstances`.
//!
//! # The rules under test
//!
//! * A report naming a container the task it resolved does NOT hold is refused
//!   `InvalidParameterException`, and the sentence names the container the REQUEST referred
//!   to and the id of the task it was weighed against — the same refusal whichever
//!   container-scoped member of the agent plane carried the name
//!   (`SubmitTaskStateChange$containers`, `SubmitTaskStateChange$managedAgents`,
//!   `SubmitContainerStateChange$containerName`) — and the task's container list is left as
//!   it was, with no record minted under the name nothing held.
//! * The boundary is reachable on BOTH capacities a cluster has: a task on the
//!   infrastructure Amazon Web Services manages is allocated its interface inline by the
//!   call that runs it, its state report is refused HTTP 403 `AccessDeniedException`, and a
//!   state the agent plane does not accept for that interface is refused
//!   `InvalidParameterException`; an Amazon EC2 instance launched into the cluster becomes
//!   the capacity a `StartTask` places a task on, that task reaches `RUNNING` on its own
//!   clock, and the agent's RETRY of the stage it is already at is acknowledged and moves
//!   nothing.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through the
//! service's own reads, so a response that is not computed from persisted state cannot pass.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    answered, cluster_with_capacity, create_cluster, ec2, ec2_element, ec2_form, emulator_with_ec2,
    list, ok, poll_until, refusal, service, string,
};
use crate::task_lifecycle::container;

/// The cluster every row acts in.
const CLUSTER: &str = "agentsubjectplane";

/// The two containers the families below declare, so a report naming one names one of
/// several and the container nobody reported on can be asserted untouched.
const FIRST_CONTAINER: &str = "app";
const SECOND_CONTAINER: &str = "sidecar";

/// The image the containers start from; irrelevant to these rules, so one value.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The long-running program the EC2-backed row's containers perform: a container given
/// nothing to do but WAIT is there for that wait, which is what keeps the task it belongs to
/// `RUNNING` for a client to report about.
const WAITING_PROGRAM: [&str; 3] = ["/bin/sh", "-c", "sleep 3600"];

/// One container definition of the EC2-backed family: the image and the program above.
fn waiting_container(name: &str) -> Value {
    let mut definition = container(name, IMAGE);
    definition["command"] = json!(WAITING_PROGRAM);
    definition
}

/// How long the EC2-backed task is watched once it is `RUNNING`, re-read inside the loop that
/// spends it: its container's program waits far longer than this, so a task whose container
/// is still there is still `RUNNING` at the end of the window however slowly the host got
/// there. Wider than the span a standalone task's work would otherwise be finished in
/// (`state::CONTAINER_WORK_SPAN`), which is what makes the window about the RULE.
const STAYS_RUNNING_WINDOW: std::time::Duration = std::time::Duration::from_millis(
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::CONTAINER_WORK_SPAN
        .as_millis() as u64
        * 2,
);

/// How often that window re-reads the task.
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// What the agent plane answers a report it has recorded with.
const ACK: &str = "ACK";

/// The one managed agent `ManagedAgentName` names (`smithy_query(query="shape",
/// shape="ManagedAgentName")` -> `ExecuteCommandAgent`).
const MANAGED_AGENT: &str = "ExecuteCommandAgent";

/// The sentence a task state report Amazon ECS cannot attribute to a verified container
/// agent is refused with — fixed text, pinned here as a literal so a wording this service
/// drifts to is a failure rather than a value the assertion follows.
const UNVERIFIED_TASK_REPORT: &str =
    "Fargate task state changes from the agent must have the instance verified";

/// The sentence a state the agent plane does not accept for an attachment is refused with:
/// the member as the sentence names it, and the one state a report may carry.
const ATTACHMENT_STATUS_NOT_ACCEPTED: &str = "attachmentStatus should be one of [ATTACHED]";

/// The refusal a report naming a container the task does not hold earns, as Amazon ECS words
/// it: fixed text around the container name the request carried and the id of the task.
fn unable_to_find_container(container_name: &str, task_id: &str) -> String {
    format!("Unable to find container {container_name} for task {task_id}")
}

/// The id segment of a task ARN — the last segment of `task/<cluster>/<id>`, which is what
/// the agent plane's sentence names a task by.
fn task_id_of(task_arn: &str) -> &str {
    task_arn.rsplit('/').next().unwrap_or(task_arn)
}

// ---------------------------------------------------------------------------
// The container a report names
// ---------------------------------------------------------------------------

/// The family the container-name rows run: the default network mode, so the task is placed
/// on the capacity the cluster registered.
const BRIDGE_FAMILY: &str = "agentsubjectbridge";

/// WHICH container-scoped member of the agent plane carries the name one row reports under.
#[derive(Debug, Clone, Copy)]
enum Carrier {
    /// `SubmitTaskStateChange$containers`: "any containers that's associated with the state
    /// change request".
    TaskReportContainers,
    /// `SubmitTaskStateChange$managedAgents`: "the details for the managed agent that's
    /// associated with the task", whose `containerName` is required.
    TaskReportManagedAgents,
    /// `SubmitContainerStateChange$containerName`: "the name of the container".
    ContainerReport,
}

impl Carrier {
    /// The operation a report under this member is sent to.
    fn operation(self) -> &'static str {
        match self {
            Self::TaskReportContainers | Self::TaskReportManagedAgents => {
                "SubmitTaskStateChange"
            }
            Self::ContainerReport => "SubmitContainerStateChange",
        }
    }

    /// The report itself, naming `container_name` against `task_reference`.
    fn request(self, task_reference: &str, container_name: &str) -> Value {
        match self {
            Self::TaskReportContainers => json!({
                "cluster": CLUSTER,
                "task": task_reference,
                "containers": [{ "containerName": container_name, "status": "RUNNING" }],
            }),
            Self::TaskReportManagedAgents => json!({
                "cluster": CLUSTER,
                "task": task_reference,
                "managedAgents": [{
                    "containerName": container_name,
                    "managedAgentName": MANAGED_AGENT,
                    "status": "RUNNING",
                }],
            }),
            Self::ContainerReport => json!({
                "cluster": CLUSTER,
                "task": task_reference,
                "containerName": container_name,
                "status": "RUNNING",
            }),
        }
    }
}

/// Every container of `task`, by name, with what a report could have moved about it — so the
/// list a refused report left can be compared whole against the list it was weighed against.
fn container_footprint(task: &Value, case: &str) -> BTreeMap<String, Value> {
    list(task, "containers", case)
        .into_iter()
        .map(|container| {
            (
                container["name"].as_str().unwrap_or_default().to_string(),
                json!({
                    "lastStatus": container["lastStatus"],
                    "runtimeId": container["runtimeId"],
                    "exitCode": container["exitCode"],
                    "reason": container["reason"],
                    "managedAgents": container["managedAgents"],
                }),
            )
        })
        .collect()
}

/// One task, as `DescribeTasks` reports it.
async fn describe_task(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

#[tokio::test]
async fn a_report_naming_a_container_the_task_does_not_hold_names_it_and_the_task() {
    // The rule generalises over WHICH member of the agent plane carried the name, over the
    // NAME itself and over the FORM the report refers to the task in — so the table spans
    // all three. The names span what a `ContainerDefinition$name` may be and is not held by
    // this task: a plain unheld name, one differing from a held name only in case (the
    // lookup is exact), one at the published 255-character ceiling, and one spelled with the
    // hyphens, underscores and digits the member admits.
    let long_name = "c".repeat(255);
    let names: [(&str, &str); 4] = [
        ("a name nothing on the task holds", "not-a-real-container-on-this-task"),
        ("a held name in another case", "App"),
        ("a 255-character name", long_name.as_str()),
        ("hyphens, underscores and digits", "side_car-2"),
    ];
    let carriers = [
        Carrier::TaskReportContainers,
        Carrier::TaskReportManagedAgents,
        Carrier::ContainerReport,
    ];

    let case = "an unheld container name";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": BRIDGE_FAMILY,
            "containerDefinitions": [
                container(FIRST_CONTAINER, IMAGE),
                container(SECOND_CONTAINER, IMAGE),
            ],
        }),
        case,
    )
    .await;
    let placed = ok(
        &router,
        "RunTask",
        json!({ "cluster": CLUSTER, "taskDefinition": BRIDGE_FAMILY, "count": 1 }),
        case,
    )
    .await;
    let task_arn = string(&list(&placed, "tasks", case)[0], "taskArn", case);
    let task_id = task_id_of(&task_arn).to_string();

    for carrier in carriers {
        for (name_case, container_name) in names {
            for reference_form in ["the full ARN", "the bare task id"] {
                let row = &format!(
                    "{} under {carrier:?} by {reference_form}",
                    name_case
                );
                let reference = match reference_form {
                    "the full ARN" => task_arn.clone(),
                    _ => task_id.clone(),
                };
                let before = container_footprint(&describe_task(&router, &task_arn, row).await, row);
                let (code, message) = refusal(
                    &router,
                    carrier.operation(),
                    carrier.request(&reference, container_name),
                    row,
                )
                .await;
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    (
                        "InvalidParameterException",
                        unable_to_find_container(container_name, &task_id).as_str()
                    ),
                    "case {row:?}: expected the refusal to name the container and the task, \
                     got {code} with {message:?}"
                );
                let after = container_footprint(&describe_task(&router, &task_arn, row).await, row);
                assert_eq!(
                    after, before,
                    "case {row:?}: a refused report must leave the container list as it was, \
                     expected {before:?}, got {after:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The boundary on both capacities
// ---------------------------------------------------------------------------

/// The Fargate-capable family: `awsvpc` with task-level cpu and memory.
const FARGATE_FAMILY: &str = "agentsubjectfargate";

/// The family the EC2-backed row starts: the default network mode, so it is placed on the
/// instance the launch registered.
const EC2_FAMILY: &str = "agentsubjectec2";

/// An AMI id for the launches below. Well-formed (`ami-` and 17 hexadecimal digits), which
/// is all a launch needs: which AMI carries the container agent is not a fact this emulator
/// publishes.
const IMAGE_ID: &str = "ami-0123456789abcdef0";

/// The instance type the launch below asks for.
const INSTANCE_TYPE: &str = "t3.micro";

/// The network every placement below names: a VPC, one subnet of it with a path to the
/// internet (which is what a task given a public address fetches its images over) and one
/// security group.
struct Network {
    subnet_id: String,
    security_group_id: String,
}

/// Create that network through Amazon EC2's own API.
async fn seed_network(router: &MultiServiceRouter, case: &str) -> Network {
    let vpc_id = ec2_element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")], case).await,
        "vpcId",
        case,
    );
    let subnet_id = ec2_element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", &vpc_id),
                ("CidrBlock", "10.90.0.0/20"),
                ("AvailabilityZone", &format!("{}a", crate::ecs_wire::SIGNED_REGION)),
            ],
            case,
        )
        .await,
        "subnetId",
        case,
    );
    let gateway_id = ec2_element(
        &ec2(router, "CreateInternetGateway", &[], case).await,
        "internetGatewayId",
        case,
    );
    ec2(
        router,
        "AttachInternetGateway",
        &[("InternetGatewayId", &gateway_id), ("VpcId", &vpc_id)],
        case,
    )
    .await;
    let route_table_id = ec2_element(
        &ec2(router, "CreateRouteTable", &[("VpcId", &vpc_id)], case).await,
        "routeTableId",
        case,
    );
    ec2(
        router,
        "AssociateRouteTable",
        &[("RouteTableId", &route_table_id), ("SubnetId", &subnet_id)],
        case,
    )
    .await;
    ec2(
        router,
        "CreateRoute",
        &[
            ("RouteTableId", &route_table_id),
            ("DestinationCidrBlock", "0.0.0.0/0"),
            ("GatewayId", &gateway_id),
        ],
        case,
    )
    .await;
    let security_group_id = ec2_element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "agentsubject-group"),
                ("GroupDescription", "the group every placement below names"),
                ("VpcId", &vpc_id),
            ],
            case,
        )
        .await,
        "groupId",
        case,
    );
    Network {
        subnet_id,
        security_group_id,
    }
}

/// The user data a launch into `cluster` carries: the documented line that appends the
/// agent's `ECS_CLUSTER` assignment to its configuration file, base64 as a launch carries it
/// ("user data must be base64-encoded").
fn agent_configuration(cluster: &str) -> String {
    use ::base64::Engine as _;

    ::base64::engine::general_purpose::STANDARD.encode(format!(
        "#!/bin/bash\necho ECS_CLUSTER={cluster} >> /etc/ecs/ecs.config\n"
    ))
}

#[tokio::test]
async fn the_agent_plane_boundary_is_reachable_on_both_capacities_of_a_cluster() {
    let case = "the agent plane boundary";
    let router = emulator_with_ec2();
    create_cluster(&router, CLUSTER, case).await;
    let network = seed_network(&router, case).await;

    // ---- the capacity Amazon Web Services manages ----
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FARGATE_FAMILY,
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [container(FIRST_CONTAINER, IMAGE)],
        }),
        case,
    )
    .await;
    let placed = ok(
        &router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": FARGATE_FAMILY,
            "count": 1,
            "launchType": "FARGATE",
            "networkConfiguration": {
                "awsvpcConfiguration": {
                    "subnets": [network.subnet_id],
                    "securityGroups": [network.security_group_id],
                    "assignPublicIp": "ENABLED",
                }
            },
        }),
        case,
    )
    .await;
    let fargate_task = list(&placed, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected one task, got {placed}"));
    let fargate_arn = string(&fargate_task, "taskArn", case);

    // "If the network mode is `awsvpc`, the task is allocated an elastic network interface":
    // the interface is the task's, so the call that ran it reports the attachment INLINE.
    let attachments = list(&fargate_task, "attachments", case);
    let interface = attachments
        .iter()
        .find(|attachment| attachment["type"].as_str() == Some("ElasticNetworkInterface"))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the run to report the task's elastic network \
                 interface, got {fargate_task}"
            )
        });
    let attachment_id = string(interface, "id", case);

    // The task Amazon ECS placed on that capacity is one Amazon ECS runs itself, so the
    // server takes it to `RUNNING` on its own clock — the positive control every refusal
    // below is weighed against — and the interface it was allocated is `ATTACHED` by then.
    let running = poll_until(
        case,
        "the Fargate task to reach RUNNING",
        || describe_task(&router, &fargate_arn, case),
        |task| task["lastStatus"] == json!("RUNNING"),
    )
    .await;
    let attached = list(&running, "attachments", case)
        .into_iter()
        .find(|attachment| attachment["type"].as_str() == Some("ElasticNetworkInterface"))
        .unwrap_or_else(|| panic!("case {case:?}: expected the interface, got {running}"));
    assert_eq!(
        attached["status"].as_str(),
        Some("ATTACHED"),
        "case {case:?}: expected a running task's interface to be in place, got {attached}"
    );

    // A task on that capacity runs on no container instance of the account, so no caller can
    // be verified as its agent: its state report is refused HTTP 403.
    let (status, body) = answered(
        &router,
        "SubmitTaskStateChange",
        json!({ "cluster": CLUSTER, "task": fargate_arn, "status": "RUNNING" }),
    )
    .await;
    assert_eq!(
        (
            status,
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .contains("AccessDeniedException"),
            body["message"].as_str().unwrap_or_default(),
        ),
        (403, true, UNVERIFIED_TASK_REPORT),
        "case {case:?}: expected the unverified-agent refusal, got {status} with {body}"
    );

    // The one state the agent plane accepts for that interface is the attach it completed, so
    // a state outside the set is refused — whatever resource the reference resolves to.
    let (code, message) = refusal(
        &router,
        "SubmitAttachmentStateChanges",
        json!({
            "cluster": CLUSTER,
            "attachments": [{ "attachmentArn": attachment_id, "status": "NOT_A_REAL_STATUS" }],
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("InvalidParameterException", ATTACHMENT_STATUS_NOT_ACCEPTED),
        "case {case:?}: expected the out-of-set refusal, got {code} with {message:?}"
    );

    // ---- the capacity the account launched ----
    // The launch goes through the form that percent-encodes its values: the user data is
    // base64, so it may carry characters an unencoded form body would not survive.
    let launched = ec2_form(
        &router,
        "RunInstances",
        &[
            ("ImageId", IMAGE_ID),
            ("InstanceType", INSTANCE_TYPE),
            ("SubnetId", &network.subnet_id),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("UserData", &agent_configuration(CLUSTER)),
        ],
        case,
    )
    .await;
    let instance_id = ec2_element(&launched, "instanceId", case);

    // The registration is the agent's own and falls due on the CLOCK, so the capacity is
    // polled for rather than waited out.
    let registered = poll_until(
        case,
        &format!("the instance {instance_id} to register as this cluster's capacity"),
        || async {
            let listed = ok(
                &router,
                "ListContainerInstances",
                json!({ "cluster": CLUSTER }),
                case,
            )
            .await;
            let arns = list(&listed, "containerInstanceArns", case);
            if arns.is_empty() {
                return json!({ "containerInstances": [] });
            }
            ok(
                &router,
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
                    instance["ec2InstanceId"].as_str() == Some(instance_id.as_str())
                        && instance["status"].as_str() == Some("ACTIVE")
                        && instance["agentConnected"] == json!(true)
                })
        },
    )
    .await;
    let container_instance = list(&registered, "containerInstances", case)
        .into_iter()
        .find(|instance| instance["ec2InstanceId"].as_str() == Some(instance_id.as_str()))
        .unwrap_or_else(|| panic!("case {case:?}: expected the registration, got {registered}"));
    let container_instance_arn = string(&container_instance, "containerInstanceArn", case);

    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": EC2_FAMILY,
            "containerDefinitions": [
                waiting_container(FIRST_CONTAINER),
                waiting_container(SECOND_CONTAINER),
            ],
        }),
        case,
    )
    .await;
    let started = ok(
        &router,
        "StartTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": EC2_FAMILY,
            "containerInstances": [container_instance_arn],
        }),
        case,
    )
    .await;
    let ec2_arn = string(&list(&started, "tasks", case)[0], "taskArn", case);

    // The start is owed by the clock, not by the call: the task is polled until it runs.
    let running = poll_until(
        case,
        "the started task to reach RUNNING",
        || describe_task(&router, &ec2_arn, case),
        |task| task["lastStatus"] == json!("RUNNING"),
    )
    .await;
    assert_eq!(
        running["lastStatus"],
        json!("RUNNING"),
        "case {case:?}: expected the started task to run, got {running}"
    );

    // And it STAYS there: its containers were given nothing to do but wait, so the task goes
    // on running while they do — which is what makes an idempotent report of the stage it is
    // already at something a client can make at all. Re-read inside the window, so what the
    // assertion rests on is the condition holding read after read.
    let watching_until = std::time::Instant::now() + STAYS_RUNNING_WINDOW;
    let mut settled = running;
    while std::time::Instant::now() < watching_until {
        tokio::time::sleep(WATCH_INTERVAL).await;
        settled = describe_task(&router, &ec2_arn, case).await;
        assert_eq!(
            settled["lastStatus"],
            json!("RUNNING"),
            "case {case:?}: expected the task to go on running while its containers wait, got \
             {settled}"
        );
    }
    // The counter the idempotent reports below must leave alone, read once the task has come
    // to rest: "every time a task experiences a change ... the version counter is
    // incremented", and the start itself is such a change.
    let version_before = settled["version"].clone();

    // The agent RETRIES, so a report of the stage the task is already at is acknowledged and
    // moves nothing — the task's own report and its container's alike.
    for (report_case, operation, request) in [
        (
            "the task's own retry",
            "SubmitTaskStateChange",
            json!({ "cluster": CLUSTER, "task": ec2_arn, "status": "RUNNING" }),
        ),
        (
            "its container's retry",
            "SubmitContainerStateChange",
            json!({
                "cluster": CLUSTER,
                "task": ec2_arn,
                "containerName": FIRST_CONTAINER,
                "status": "RUNNING",
            }),
        ),
    ] {
        let acknowledged = ok(&router, operation, request, report_case).await;
        assert_eq!(
            acknowledged["acknowledgment"].as_str(),
            Some(ACK),
            "case {report_case:?}: expected the report to be acknowledged, got {acknowledged}"
        );
        let read = describe_task(&router, &ec2_arn, report_case).await;
        assert_eq!(
            (read["lastStatus"].clone(), read["version"].clone()),
            (json!("RUNNING"), version_before.clone()),
            "case {report_case:?}: expected an idempotent report to move nothing, got {read}"
        );
    }

    // And the container-name boundary, on the task that has a verifiable agent: the refusal
    // names what the request named and the task it was weighed against, and no record is
    // minted under the name nothing held.
    let before = container_footprint(&describe_task(&router, &ec2_arn, case).await, case);
    let unheld = "not-a-real-container-on-this-task";
    let (code, message) = refusal(
        &router,
        "SubmitTaskStateChange",
        json!({
            "cluster": CLUSTER,
            "task": ec2_arn,
            "containers": [{ "containerName": unheld, "status": "RUNNING" }],
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "InvalidParameterException",
            unable_to_find_container(unheld, task_id_of(&ec2_arn)).as_str()
        ),
        "case {case:?}: expected the refusal to name the container and the task, got {code} \
         with {message:?}"
    );
    let after = container_footprint(&describe_task(&router, &ec2_arn, case).await, case);
    assert_eq!(
        after, before,
        "case {case:?}: expected the container list left as it was, got {after:?}"
    );
    assert!(
        !after.contains_key(unheld),
        "case {case:?}: expected no container minted under {unheld:?}, got {after:?}"
    );
}
