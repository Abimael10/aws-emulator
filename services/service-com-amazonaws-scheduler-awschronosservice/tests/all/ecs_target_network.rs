// SPDX-License-Identifier: CC0-1.0

//! The ECS target's cross-member network rule, driven over the real `restJson1`
//! front door on BOTH operations that write a schedule.
//!
//! RULE: `AwsVpcConfiguration$AssignPublicIp` states "you can specify `ENABLED` only
//! when `LaunchType` in `EcsParameters` is set to `FARGATE`", so a task asking for a
//! public address beside any other launch type the request states is refused 400
//! `ValidationException` with `Parameter AssignPublicIp for the target is not
//! supported when launch type is <LaunchType>` — the sentence naming the launch type
//! the request stated — and nothing is persisted. A configuration that asks for no
//! public address, and one that states no launch type for the refusal to be about, are
//! accepted and the whole `EcsParameters` block round-trips.
//!
//! The table walks every value `LaunchType` admits (`EC2 | FARGATE | EXTERNAL`,
//! `smithy_query(query="shape", shape="com.amazonaws.scheduler#LaunchType")`) and
//! every value `AssignPublicIp` admits (`ENABLED | DISABLED`) beside the member left
//! out, rather than the one recorded request.

use smithy_server_runtime::MultiServiceRouter;

use crate::wire_harness::{call, error_code, error_message, scheduler_router};

/// The cluster an ECS target names, and the task definition its parameters run.
const CLUSTER: &str = "arn:aws:ecs:eu-west-2:000000000000:cluster/nightly";
const TASK_DEFINITION: &str = "arn:aws:ecs:eu-west-2:000000000000:task-definition/roll-up:1";

/// The expression a seeded schedule carries, which a refused update must leave on it.
const SEEDED_EXPRESSION: &str = "rate(1 day)";

/// An ECS target whose parameters state `launch_type` and `assign_public_ip`, each
/// where a row gives one.
fn ecs_target(
    launch_type: Option<&str>,
    assign_public_ip: Option<&str>,
) -> serde_json::Value {
    let mut configuration = serde_json::json!({ "Subnets": ["subnet-0a1b2c3d4e5f60718"] });
    if let Some(assign_public_ip) = assign_public_ip {
        configuration
            .as_object_mut()
            .expect("the configuration under construction")
            .insert(
                "AssignPublicIp".to_string(),
                serde_json::json!(assign_public_ip),
            );
    }
    let mut parameters = serde_json::json!({
        "TaskDefinitionArn": TASK_DEFINITION,
        "NetworkConfiguration": { "awsvpcConfiguration": configuration }
    });
    if let Some(launch_type) = launch_type {
        parameters
            .as_object_mut()
            .expect("the parameters under construction")
            .insert("LaunchType".to_string(), serde_json::json!(launch_type));
    }
    serde_json::json!({
        "Arn": CLUSTER,
        "RoleArn": "arn:aws:iam::000000000000:role/r",
        "EcsParameters": parameters
    })
}

/// A write body carrying `target`.
fn body(target: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "ScheduleExpression": SEEDED_EXPRESSION,
        "FlexibleTimeWindow": { "Mode": "OFF" },
        "State": "DISABLED",
        "Target": target
    })
}

/// The refusal a public address asked for under `launch_type` carries.
fn unsupported(launch_type: &str) -> String {
    format!(
        "Parameter AssignPublicIp for the target is not supported when launch type is \
         {launch_type}"
    )
}

/// Create the schedule a row's update is applied to, and answer its name.
async fn seeded(router: &MultiServiceRouter, name: &str) -> String {
    let (status, answer) = call(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(body(ecs_target(Some("FARGATE"), Some("DISABLED")))),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateSchedule {name} must succeed, got {status} {answer}"
    );
    name.to_string()
}

#[tokio::test]
async fn a_public_address_is_asked_for_only_under_the_launch_type_that_assigns_one() {
    /// One row: the case, the launch type stated, the value `AssignPublicIp` states,
    /// and the launch type the refusal names (`None` for a row the service accepts).
    type NetworkRow = (
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
    );
    let rows: [NetworkRow; 8] = [
        (
            "a public address under the launch type that assigns one",
            Some("FARGATE"),
            Some("ENABLED"),
            None,
        ),
        (
            "a public address under instances of the account's own",
            Some("EC2"),
            Some("ENABLED"),
            Some("EC2"),
        ),
        (
            "a public address under external instances",
            Some("EXTERNAL"),
            Some("ENABLED"),
            Some("EXTERNAL"),
        ),
        (
            "no public address under instances of the account's own",
            Some("EC2"),
            Some("DISABLED"),
            None,
        ),
        (
            "no public address under external instances",
            Some("EXTERNAL"),
            Some("DISABLED"),
            None,
        ),
        (
            "the member left out under instances of the account's own",
            Some("EC2"),
            None,
            None,
        ),
        (
            "a public address with no launch type stated",
            None,
            Some("ENABLED"),
            None,
        ),
        ("no launch type and no public address", None, None, None),
    ];

    let router = scheduler_router();
    let held = seeded(&router, "held.ecs_schedule").await;

    for (index, (case, launch_type, assign_public_ip, refused_for)) in rows.iter().enumerate() {
        let name = format!("ecs-{index}");
        let target = ecs_target(*launch_type, *assign_public_ip);
        let (status, answer) = call(
            &router,
            "POST",
            &format!("/schedules/{name}"),
            Some(body(target.clone())),
        )
        .await;
        let (read_status, read) = call(&router, "GET", &format!("/schedules/{name}"), None).await;

        match refused_for {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: CreateSchedule must accept it, got {status} {answer}"
                );
                assert_eq!(
                    read_status, 200,
                    "case {case:?}: the schedule must be persisted, got {read_status} {read}"
                );
                // The whole parameter block round-trips as it was written.
                assert_eq!(
                    read.get("Target").and_then(|target| target.get("EcsParameters")),
                    target.get("EcsParameters"),
                    "case {case:?}: expected the EcsParameters block read back unchanged, \
                     got {read}"
                );
            }
            Some(launch_type) => {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: CreateSchedule must refuse it 400 ValidationException, \
                     got {status} {answer}"
                );
                assert_eq!(
                    error_message(&answer),
                    unsupported(launch_type),
                    "case {case:?}: expected {:?}, got {:?}",
                    unsupported(launch_type),
                    error_message(&answer)
                );
                assert_eq!(
                    (read_status, error_code(&read).as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {case:?}: a refused create persists nothing, got {read_status} {read}"
                );
                assert_eq!(
                    error_message(&read),
                    format!("Schedule {name} does not exist."),
                    "case {case:?}: expected the schedule reported missing by name, got {read}"
                );
            }
        }

        // The same rule on the other write path: an update that would leave the stored
        // schedule with that configuration is refused and changes nothing.
        let (status, answer) = call(
            &router,
            "PUT",
            &format!("/schedules/{held}"),
            Some(body(target.clone())),
        )
        .await;
        let (_, held_read) = call(&router, "GET", &format!("/schedules/{held}"), None).await;
        match refused_for {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: UpdateSchedule must accept it, got {status} {answer}"
                );
                assert_eq!(
                    held_read
                        .get("Target")
                        .and_then(|target| target.get("EcsParameters")),
                    target.get("EcsParameters"),
                    "case {case:?}: an accepted update must be applied, got {held_read}"
                );
            }
            Some(launch_type) => {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: UpdateSchedule must refuse it 400 ValidationException, \
                     got {status} {answer}"
                );
                assert_eq!(
                    error_message(&answer),
                    unsupported(launch_type),
                    "case {case:?}: expected the same sentence from the update, got {:?}",
                    error_message(&answer)
                );
                assert_ne!(
                    held_read
                        .get("Target")
                        .and_then(|target| target.get("EcsParameters"))
                        .and_then(|parameters| parameters.get("NetworkConfiguration")),
                    target
                        .get("EcsParameters")
                        .and_then(|parameters| parameters.get("NetworkConfiguration")),
                    "case {case:?}: a refused update changes nothing, got {held_read}"
                );
            }
        }
        // Leave the held schedule as the next row expects to find it.
        let (status, answer) = call(
            &router,
            "PUT",
            &format!("/schedules/{held}"),
            Some(body(ecs_target(Some("FARGATE"), Some("DISABLED")))),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: restoring the held schedule, got {status} {answer}"
        );
    }
}
