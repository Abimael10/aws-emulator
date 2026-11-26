// SPDX-License-Identifier: CC0-1.0

//! The launch types a revision that publishes CONTAINER PORTS reports, and the placements
//! that list then admits, driven end to end over the shared wire harness in
//! [`crate::ecs_wire`].
//!
//! The rule under test: `TaskDefinition$compatibilities` ("Amazon ECS validates the task
//! definition parameters with those supported by the launch type") is DERIVED from the stored
//! revision's own parameters, and a launch type is reported only when those parameters meet
//! that launch type's documented requirements — EC2 supports every network mode ("For Amazon
//! ECS tasks on Amazon EC2 Linux instances, any network mode can be used"); the external
//! (on-premises) launch type has no `awsvpc` networking; Fargate requires the `awsvpc` network
//! mode plus TASK-level `cpu` and `memory` ("This field is required for Fargate"); and a
//! revision whose networking is not `awsvpc` reports neither Fargate nor `MANAGED_INSTANCES`,
//! the remaining member of the `Compatibility` enum (`smithy_query(query="shape",
//! shape="com.amazonaws.ecs#Compatibility")` -> `EC2`, `FARGATE`, `EXTERNAL`,
//! `MANAGED_INSTANCES`) — managed-instances capacity is reached through a capacity provider
//! rather than by naming a launch type (`RunTask$launchType`: "If you want to use Amazon ECS
//! Managed Instances, you must use the `capacityProviderStrategy` request parameter and omit
//! the `launchType` request parameter").
//!
//! A container's own members are not launch-type requirements, so the rows here span the
//! shapes a PORT-PUBLISHING revision takes — a dynamic host port, a non-reserved static one,
//! an omitted one, a mapping under `host` that maps onto the same host port, several mapped
//! containers — and the container-level `cpu`/`memory` RESERVATION. The derivation moves only
//! along the network mode and, under `awsvpc`, the task size: a bridge-networked revision
//! reports the same two launch types however its ports and reservations are shaped and
//! whatever it asked to be validated against.
//!
//! The second suite drives the gate the same derived list feeds: a placement may name a
//! launch type only when the revision reports it and the launch type is one a placement names
//! outright (`CreateService$launchType` names `EC2`, `FARGATE` and `EXTERNAL`), so each
//! revision's admitted set is its own derived list intersected with those three — never the
//! whole enum — and the same sentence comes back whichever operation did the placing.
//!
//! Every case goes through the HTTP front door, so a list that is not derived from the stored
//! revision cannot pass.

use serde_json::{json, Value};

use crate::ecs_wire::{
    awsvpc_network_configuration, cluster_with_capacity, list, ok, refusal, service, string,
};

/// The image every container here starts from: a public image that needs no principal to
/// pull, so no row drops Fargate for want of an execution role.
const IMAGE: &str = "public.ecr.aws/docker/library/nginx:1.27";

/// A legal Fargate task size, from `RegisterTaskDefinition$memory`'s documented cpu/memory
/// pairs.
const TASK_CPU: &str = "512";
const TASK_MEMORY: &str = "1024";

/// Every member of the `Compatibility` enum (`smithy_query(query="shape",
/// shape="com.amazonaws.ecs#Compatibility")`), so the gate suite asks about the launch types
/// the model carries rather than only the ones a row expects back.
const COMPATIBILITY_VALUES: [&str; 4] = ["EC2", "FARGATE", "EXTERNAL", "MANAGED_INSTANCES"];

/// The launch types a placement may name OUTRIGHT in its `launchType`, as
/// `CreateService$launchType` ("The infrastructure that you run your service on") publishes
/// them — managed-instances capacity is reached through `capacityProviderStrategy` instead.
const PLACEMENT_LAUNCH_TYPES: [&str; 3] = ["EC2", "FARGATE", "EXTERNAL"];

/// One container that publishes a port, with the container-level reservation a revision of
/// this shape carries.
fn mapped_container(name: &str, mapping: Value) -> Value {
    json!({
        "name": name,
        "image": IMAGE,
        "essential": true,
        "cpu": 128,
        "memory": 128,
        "portMappings": [mapping],
    })
}

/// The `compatibilities` of one projection, in the order it reports them.
fn compatibilities(definition: &Value, case: &str) -> Vec<String> {
    strings(definition, "compatibilities", case)
}

/// The strings of a list member of one projection, an absent member reading as empty.
fn strings(definition: &Value, member: &str, case: &str) -> Vec<String> {
    list(definition, member, case)
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a string in {member}, got {value}")
                })
                .to_string()
        })
        .collect()
}

/// One row of the derivation table: the request beside its family, what it asks to be
/// validated against, and the launch types the stored revision must report.
struct Row {
    case: &'static str,
    request: Value,
    requires: &'static [&'static str],
    expected: &'static [&'static str],
}

/// The rows, spanning the port-mapping shapes and the parameters the derivation moves along.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "bridge publishing a port on a dynamically assigned host port",
            request: json!({
                "networkMode": "bridge",
                "containerDefinitions": [
                    mapped_container(
                        "app",
                        json!({ "containerPort": 80, "hostPort": 0, "protocol": "tcp" }),
                    ),
                ],
                "requiresCompatibilities": ["EC2"],
            }),
            requires: &["EC2"],
            expected: &["EXTERNAL", "EC2"],
        },
        Row {
            case: "bridge publishing a port on a non-reserved static host port",
            request: json!({
                "networkMode": "bridge",
                "containerDefinitions": [
                    mapped_container(
                        "app",
                        json!({ "containerPort": 80, "hostPort": 8080, "protocol": "tcp" }),
                    ),
                ],
                "requiresCompatibilities": ["EC2"],
            }),
            requires: &["EC2"],
            expected: &["EXTERNAL", "EC2"],
        },
        Row {
            case: "bridge leaving the host port and the protocol to Amazon ECS",
            request: json!({
                "networkMode": "bridge",
                "containerDefinitions": [
                    mapped_container("app", json!({ "containerPort": 80 })),
                ],
            }),
            requires: &[],
            expected: &["EXTERNAL", "EC2"],
        },
        Row {
            // A task-level size is not a launch-type requirement of its own: Fargate needs
            // `awsvpc` networking beside it, which this revision does not declare.
            case: "bridge publishing a port at a task-level size",
            request: json!({
                "networkMode": "bridge",
                "cpu": TASK_CPU,
                "memory": TASK_MEMORY,
                "containerDefinitions": [
                    mapped_container(
                        "app",
                        json!({ "containerPort": 80, "hostPort": 0, "protocol": "tcp" }),
                    ),
                ],
            }),
            requires: &[],
            expected: &["EXTERNAL", "EC2"],
        },
        Row {
            case: "the documented default network mode, publishing a port",
            request: json!({
                "containerDefinitions": [
                    mapped_container(
                        "app",
                        json!({ "containerPort": 8080, "hostPort": 0, "protocol": "tcp" }),
                    ),
                ],
                "requiresCompatibilities": ["EC2"],
            }),
            requires: &["EC2"],
            expected: &["EXTERNAL", "EC2"],
        },
        Row {
            case: "host mapping the container port onto the same host port",
            request: json!({
                "networkMode": "host",
                "containerDefinitions": [
                    mapped_container(
                        "app",
                        json!({ "containerPort": 80, "hostPort": 80, "protocol": "tcp" }),
                    ),
                ],
                "requiresCompatibilities": ["EC2", "EXTERNAL"],
            }),
            requires: &["EC2", "EXTERNAL"],
            expected: &["EXTERNAL", "EC2"],
        },
        Row {
            case: "bridge with two mapped containers",
            request: json!({
                "networkMode": "bridge",
                "containerDefinitions": [
                    mapped_container(
                        "app",
                        json!({ "containerPort": 80, "hostPort": 0, "protocol": "tcp" }),
                    ),
                    mapped_container(
                        "sidecar",
                        json!({ "containerPort": 9000, "hostPort": 0, "protocol": "udp" }),
                    ),
                ],
                "requiresCompatibilities": ["EC2"],
            }),
            requires: &["EC2"],
            expected: &["EXTERNAL", "EC2"],
        },
        Row {
            // The other side of the network-mode dimension: `awsvpc` at a task size Fargate
            // takes is validated against Fargate, and its networking rules the external
            // launch type out.
            case: "awsvpc publishing a port, validated against Fargate at a task size",
            request: json!({
                "networkMode": "awsvpc",
                "cpu": TASK_CPU,
                "memory": TASK_MEMORY,
                "containerDefinitions": [
                    mapped_container("app", json!({ "containerPort": 80, "protocol": "tcp" })),
                ],
                "requiresCompatibilities": ["FARGATE"],
            }),
            requires: &["FARGATE"],
            expected: &["EC2", "FARGATE"],
        },
    ]
}

/// RULE: the derived list is the launch types the revision's own parameters support, in the
/// order Amazon ECS reports them, and nothing a container publishes moves it.
#[tokio::test]
async fn a_port_mapped_revision_reports_only_the_launch_types_its_parameters_support() {
    let router = service();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let family = format!("portmapfam{index}");
        let mut request = row.request.clone();
        request["family"] = json!(family);
        let registered = ok(&router, "RegisterTaskDefinition", request, case).await;
        let described = ok(
            &router,
            "DescribeTaskDefinition",
            json!({ "taskDefinition": family }),
            case,
        )
        .await;

        // Both projections of the SAME stored revision, so a list rendered off the request
        // rather than off the record shows up as a difference between them.
        for (source, definition) in [
            ("RegisterTaskDefinition", &registered["taskDefinition"]),
            ("DescribeTaskDefinition", &described["taskDefinition"]),
        ] {
            let actual = compatibilities(definition, case);
            assert_eq!(
                actual, row.expected,
                "case {case:?}: {source} expected compatibilities {:?}, got {actual:?} in \
                 {definition}",
                row.expected
            );
            // The POSITION matters as well as the membership: a client that reads the list
            // positionally must name the launch type the real service named there.
            for (position, launch_type) in row.expected.iter().enumerate() {
                assert_eq!(
                    actual.get(position).map(String::as_str),
                    Some(*launch_type),
                    "case {case:?}: {source} expected {launch_type} at position {position}, \
                     got {actual:?}"
                );
            }
            // What the REGISTRATION asked to be validated against is reported separately
            // and unchanged.
            let requested = strings(definition, "requiresCompatibilities", case);
            assert_eq!(
                requested, row.requires,
                "case {case:?}: {source} expected requiresCompatibilities {:?}, got \
                 {requested:?} in {definition}",
                row.requires
            );
        }
    }
}

/// RULE, the other side of it: a placement may name a launch type only when the revision
/// reports it AND a placement names that launch type outright, so the admitted set of each
/// revision is its derived list intersected with [`PLACEMENT_LAUNCH_TYPES`] — every other
/// member of the `Compatibility` enum is refused with the sentence that names it.
#[tokio::test]
async fn the_launch_type_gate_admits_exactly_what_the_revision_reports() {
    let router = service();

    // (the family, the request's own members, the launch types the revision reports).
    let shapes: [(&str, Value, &[&str]); 2] = [
        (
            "gatebridge",
            json!({
                "networkMode": "bridge",
                "containerDefinitions": [
                    mapped_container(
                        "app",
                        json!({ "containerPort": 80, "hostPort": 0, "protocol": "tcp" }),
                    ),
                ],
                "requiresCompatibilities": ["EC2"],
            }),
            &["EXTERNAL", "EC2"],
        ),
        (
            "gatefargate",
            json!({
                "networkMode": "awsvpc",
                "cpu": TASK_CPU,
                "memory": TASK_MEMORY,
                "containerDefinitions": [
                    mapped_container("app", json!({ "containerPort": 80, "protocol": "tcp" })),
                ],
                "requiresCompatibilities": ["FARGATE"],
            }),
            &["EC2", "FARGATE"],
        ),
    ];

    for (family, shape, reported) in shapes {
        // One cluster with one container instance per shape, so a placement a row makes has
        // capacity of its own and no row's placement crowds another's.
        let cluster = format!("{family}cluster");
        cluster_with_capacity(&router, &cluster, family).await;
        let mut request = shape.clone();
        request["family"] = json!(family);
        let awsvpc = shape["networkMode"] == json!("awsvpc");
        ok(&router, "RegisterTaskDefinition", request, family).await;

        for launch_type in COMPATIBILITY_VALUES {
            let case = format!("{family} placed with {launch_type}");
            let case = case.as_str();
            let admitted =
                reported.contains(&launch_type) && PLACEMENT_LAUNCH_TYPES.contains(&launch_type);
            let mut placement = json!({
                "cluster": cluster,
                "taskDefinition": family,
                "launchType": launch_type,
            });
            if awsvpc {
                placement["networkConfiguration"] = awsvpc_network_configuration();
            }
            if admitted {
                let run = ok(&router, "RunTask", placement, case).await;
                let tasks = list(&run, "tasks", case);
                assert_eq!(
                    tasks.len(),
                    1,
                    "case {case:?}: a launch type the revision reports places the task, got \
                     {run}"
                );
                let placed = string(&tasks[0], "launchType", case);
                assert_eq!(
                    placed, launch_type,
                    "case {case:?}: expected the task to record launchType {launch_type}, got \
                     {placed:?}"
                );
                continue;
            }
            let (code, message) = refusal(&router, "RunTask", placement, case).await;
            assert_eq!(
                code, "InvalidParameterException",
                "case {case:?}: expected the modelled refusal, got {code:?} with {message:?}"
            );
            assert_eq!(
                message,
                format!("Task definition does not support launch_type {launch_type}."),
                "case {case:?}: the refusal must name the launch type asked for, got \
                 {message:?}"
            );
        }
    }
}

/// RULE, through the OTHER operation that is handed a `launchType`: the scheduler's create
/// reads the same derived list, so a launch type a bridge-networked revision does not report
/// is refused there too, with the same sentence.
#[tokio::test]
async fn the_scheduler_reads_the_same_derived_list() {
    let router = service();
    let case = "a bridge revision published on a dynamic host port";
    cluster_with_capacity(&router, "schedulercluster", case).await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": "schedulerfam",
            "networkMode": "bridge",
            "containerDefinitions": [
                mapped_container(
                    "app",
                    json!({ "containerPort": 80, "hostPort": 0, "protocol": "tcp" }),
                ),
            ],
            "requiresCompatibilities": ["EC2"],
        }),
        case,
    )
    .await;

    // The launch types the revision does NOT report, each refused by `CreateService`.
    for launch_type in ["FARGATE", "MANAGED_INSTANCES"] {
        let case = format!("a service placed with {launch_type}");
        let case = case.as_str();
        let mut request = json!({
            "cluster": "schedulercluster",
            "serviceName": format!("svc-{}", launch_type.to_lowercase()),
            "taskDefinition": "schedulerfam",
            "desiredCount": 1,
            "launchType": launch_type,
        });
        if launch_type == "FARGATE" {
            // A Fargate placement would need the interface configuration an `awsvpc` task
            // takes; the launch-type gate answers before any of that is read.
            request["networkConfiguration"] = awsvpc_network_configuration();
        }
        let (code, message) = refusal(&router, "CreateService", request, case).await;
        assert_eq!(
            code, "InvalidParameterException",
            "case {case:?}: expected the modelled refusal, got {code:?} with {message:?}"
        );
        assert_eq!(
            message,
            format!("Task definition does not support launch_type {launch_type}."),
            "case {case:?}: the refusal must name the launch type asked for, got {message:?}"
        );
    }

    // And the launch type it DOES report is served, with the service recording it.
    let case = "a service placed with EC2";
    let created = ok(
        &router,
        "CreateService",
        json!({
            "cluster": "schedulercluster",
            "serviceName": "svc-ec2",
            "taskDefinition": "schedulerfam",
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    assert_eq!(
        string(&created["service"], "launchType", case),
        "EC2",
        "case {case:?}: expected the create to report the launch type it was given, got \
         {created}"
    );
}
