// SPDX-License-Identifier: CC0-1.0

//! How a service reports a placement its scheduler CANNOT make, over a table of the kinds
//! of capacity a placement can be short of.
//!
//! The rule under test: while the scheduler holds fewer tasks than the service wants
//! because the capacity to place them does not exist, the service reports that shortfall in
//! both the places Amazon ECS reports it, and the two agree:
//!
//! * `Service$events` — "The event stream for your service" — carries the scheduler's own
//!   sentence for a placement that found nowhere to run, whose `reason` clause names WHICH
//!   capacity was missing, behind AWS's fixed connective `The reason for failure is`;
//! * `Service$runStatus` carries the `Error.`-prefixed CODE of that same condition, which is
//!   what lets a client polling a service tell "still converging" from "will never converge"
//!   without parsing the sentence.
//!
//! A service whose tasks the scheduler CAN place reports neither: no placement-failure event
//! and no error code, whichever way it got its capacity.
//!
//! The rows span the kind of capacity the placement needs (a launch type whose capacity is
//! registered to the cluster — each of the two the model has — a capacity provider that will
//! not scale its group, and the Fargate launch type whose capacity Amazon Web Services
//! manages), and the call that asks for the tasks (a create that asks for them up front and
//! an update that scales up to them later).
//!
//! Every case goes through the shared wire harness in [`crate::ecs_wire`], the same front
//! door a client uses, so a member that is not computed from persisted state cannot pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    awsvpc_network_configuration, count, create_auto_scaling_group, create_cluster, list, ok,
    register_capacity, register_definition, service, string,
};
use crate::task_sets::register_fargate_definition;

/// The fixed part of the Amazon ECS service event message for a placement that found no
/// instance to run on.
const UNABLE_TO_PLACE: &str =
    "was unable to place a task because no container instance met all of its requirements.";

/// `Service$runStatus` of a service whose cluster holds no container instance to place its
/// tasks on.
const ERROR_CLUSTER_EMPTY: &str = "Error.ClusterEmpty";

/// `Service$runStatus` of a service whose capacity provider holds none.
const ERROR_CAPACITY_PROVIDER_EMPTY: &str = "Error.CapacityProviderEmpty";

/// The `reason` clause the event carries for each, behind AWS's fixed connective.
const CLUSTER_EMPTY_REASON: &str =
    "The reason for failure is No Container Instances were found in your cluster.";
const CAPACITY_PROVIDER_EMPTY_REASON: &str =
    "The reason for failure is No Container Instances were found in your capacity provider.";

/// Where a row's placement takes its capacity from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capacity {
    /// A launch type whose capacity is registered to the cluster, with no instance
    /// registered.
    Empty(&'static str),
    /// The same launch type with one container instance registered, so the placement is
    /// made.
    Registered(&'static str),
    /// An Auto Scaling group capacity provider whose managed scaling is off, so no capacity
    /// is coming for the cluster either.
    UnscaledProvider,
    /// The Fargate launch type, whose capacity Amazon Web Services manages.
    Fargate,
}

/// Whether the row asks for its tasks in the create or scales up to them afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AskedBy {
    Create,
    Update,
}

/// One row of the table.
struct Row {
    case: &'static str,
    capacity: Capacity,
    asked_by: AskedBy,
    /// How many tasks the service ends up wanting.
    desired: i64,
    /// The `runStatus` the service reports, or `None` where the placement is made and the
    /// shortfall never happens.
    run_status: Option<&'static str>,
    /// The `reason` clause the placement-failure event carries, where there is one.
    reason: Option<&'static str>,
}

/// Describe one service.
async fn describe(
    router: &MultiServiceRouter,
    cluster: &str,
    service_name: &str,
    case: &str,
) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": cluster, "services": [service_name] }),
        case,
    )
    .await;
    assert!(
        list(&body, "failures", case).is_empty(),
        "case {case:?}: expected the service to be described, got {body}"
    );
    list(&body, "services", case)
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected one described service, got {body}"))
}

/// The messages of a service's event stream.
fn event_messages(service_view: &Value, case: &str) -> Vec<String> {
    list(service_view, "events", case)
        .iter()
        .map(|event| string(event, "message", case))
        .collect()
}

#[tokio::test]
async fn a_service_reports_the_capacity_its_scheduler_is_short_of_in_code_and_in_words() {
    let rows: [Row; 7] = [
        Row {
            case: "an EC2 service on a cluster with no capacity",
            capacity: Capacity::Empty("EC2"),
            asked_by: AskedBy::Create,
            desired: 2,
            run_status: Some(ERROR_CLUSTER_EMPTY),
            reason: Some(CLUSTER_EMPTY_REASON),
        },
        Row {
            case: "an EXTERNAL service on a cluster with no capacity",
            capacity: Capacity::Empty("EXTERNAL"),
            asked_by: AskedBy::Create,
            desired: 1,
            run_status: Some(ERROR_CLUSTER_EMPTY),
            reason: Some(CLUSTER_EMPTY_REASON),
        },
        Row {
            case: "an EC2 service scaled up on a cluster with no capacity",
            capacity: Capacity::Empty("EC2"),
            asked_by: AskedBy::Update,
            desired: 3,
            run_status: Some(ERROR_CLUSTER_EMPTY),
            reason: Some(CLUSTER_EMPTY_REASON),
        },
        Row {
            case: "a provider that will not scale its group",
            capacity: Capacity::UnscaledProvider,
            asked_by: AskedBy::Create,
            desired: 2,
            run_status: Some(ERROR_CAPACITY_PROVIDER_EMPTY),
            reason: Some(CAPACITY_PROVIDER_EMPTY_REASON),
        },
        Row {
            case: "a provider that will not scale its group, scaled up later",
            capacity: Capacity::UnscaledProvider,
            asked_by: AskedBy::Update,
            desired: 1,
            run_status: Some(ERROR_CAPACITY_PROVIDER_EMPTY),
            reason: Some(CAPACITY_PROVIDER_EMPTY_REASON),
        },
        Row {
            case: "an EC2 service on a cluster that holds capacity",
            capacity: Capacity::Registered("EC2"),
            asked_by: AskedBy::Create,
            desired: 1,
            run_status: None,
            reason: None,
        },
        Row {
            case: "a Fargate service needs no registered capacity",
            capacity: Capacity::Fargate,
            asked_by: AskedBy::Create,
            desired: 2,
            run_status: None,
            reason: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = service();
        register_definition(&router, "shortfallfamily", case).await;
        register_fargate_definition(&router, "shortfallfargate", case).await;
        let cluster = format!("shortfall{index}");
        create_cluster(&router, &cluster, case).await;

        // What the create asks for: a row that scales up later is created wanting nothing,
        // so the shortfall is the UPDATE's and the create cannot have reported it.
        let created_count = match row.asked_by {
            AskedBy::Create => row.desired,
            AskedBy::Update => 0,
        };
        let mut request = json!({
            "cluster": cluster,
            "serviceName": "web",
            "desiredCount": created_count,
        });
        match row.capacity {
            Capacity::Empty(launch_type) => {
                request["taskDefinition"] = json!("shortfallfamily");
                request["launchType"] = json!(launch_type);
            }
            Capacity::Registered(launch_type) => {
                register_capacity(&router, &cluster, case).await;
                request["taskDefinition"] = json!("shortfallfamily");
                request["launchType"] = json!(launch_type);
            }
            Capacity::Fargate => {
                request["taskDefinition"] = json!("shortfallfargate");
                request["launchType"] = json!("FARGATE");
                request["networkConfiguration"] = awsvpc_network_configuration();
            }
            Capacity::UnscaledProvider => {
                request["taskDefinition"] = json!("shortfallfamily");
                create_auto_scaling_group(&router, "asg-unscaled", case).await;
                ok(
                    &router,
                    "CreateCapacityProvider",
                    json!({
                        "name": "unscaled",
                        "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-unscaled" },
                    }),
                    case,
                )
                .await;
                ok(
                    &router,
                    "PutClusterCapacityProviders",
                    json!({
                        "cluster": cluster,
                        "capacityProviders": ["unscaled"],
                        "defaultCapacityProviderStrategy": [],
                    }),
                    case,
                )
                .await;
                request["capacityProviderStrategy"] =
                    json!([{ "capacityProvider": "unscaled", "weight": 1 }]);
            }
        }
        ok(&router, "CreateService", request, case).await;

        if row.asked_by == AskedBy::Update {
            ok(
                &router,
                "UpdateService",
                json!({
                    "cluster": cluster,
                    "service": "web",
                    "desiredCount": row.desired,
                }),
                case,
            )
            .await;
        }

        let read = describe(&router, &cluster, "web", case).await;
        let messages = event_messages(&read, case);

        match row.reason {
            Some(reason) => {
                assert_eq!(
                    messages.len(),
                    1,
                    "case {case:?}: expected exactly the placement-failure event, got \
                     {messages:?}"
                );
                assert!(
                    messages[0].contains(UNABLE_TO_PLACE) && messages[0].contains(reason),
                    "case {case:?}: expected the event to read {UNABLE_TO_PLACE:?} with \
                     {reason:?}, got {messages:?}"
                );
                assert!(
                    messages[0].starts_with("(service web) "),
                    "case {case:?}: the event names the service it is about, got \
                     {messages:?}"
                );
                assert_eq!(
                    count(&read, "runningCount", case),
                    0,
                    "case {case:?}: nothing was placed, so nothing is running, got {read}"
                );
                assert_eq!(
                    count(&read, "desiredCount", case),
                    row.desired,
                    "case {case:?}: a failed placement does not lower desiredCount, got \
                     {read}"
                );
            }
            None => {
                assert!(
                    !messages
                        .iter()
                        .any(|message| message.contains(UNABLE_TO_PLACE)),
                    "case {case:?}: a placement that was made writes no placement-failure \
                     event, got {messages:?}"
                );
            }
        }

        match row.run_status {
            Some(expected) => assert_eq!(
                string(&read, "runStatus", case),
                expected,
                "case {case:?}: expected the error code of the shortfall, got {read}"
            ),
            None => {
                let reported = read["runStatus"].as_str().unwrap_or_default().to_string();
                assert!(
                    !reported.starts_with("Error."),
                    "case {case:?}: a service whose tasks are placed reports no error \
                     code, got {reported:?}"
                );
            }
        }
    }
}

/// The code is a property of the SHORTFALL, not of the read or of the call that caused it:
/// it stays while the capacity is missing, and it is gone once the capacity exists and the
/// scheduler has placed the tasks.
#[tokio::test]
async fn the_error_code_lasts_exactly_as_long_as_the_shortfall() {
    let case = "capacity registered after the service asked for tasks";
    let router = service();
    let cluster = "shortfallsettles";
    register_definition(&router, "settlefamily", case).await;
    create_cluster(&router, cluster, case).await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": cluster,
            "serviceName": "web",
            "taskDefinition": "settlefamily",
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        case,
    )
    .await;

    // Repeated reads report the same code: nothing about reading the service moves it.
    for attempt in 0..3 {
        let read = describe(&router, cluster, "web", case).await;
        assert_eq!(
            string(&read, "runStatus", case),
            ERROR_CLUSTER_EMPTY,
            "case {case:?}: read {attempt} must report the shortfall, got {read}"
        );
    }

    // Capacity registers, and the scheduler's own pass places the task the service was
    // short of: the code is gone, and the event the shortfall wrote is still in the stream
    // (an event stream is a history, not a status).
    register_capacity(&router, cluster, case).await;
    let deadline = ::std::time::Instant::now() + ::std::time::Duration::from_secs(30);
    loop {
        let read = describe(&router, cluster, "web", case).await;
        let reported = read["runStatus"].as_str().unwrap_or_default().to_string();
        if !reported.starts_with("Error.") && count(&read, "runningCount", case) == 1 {
            assert!(
                event_messages(&read, case)
                    .iter()
                    .any(|message| message.contains(UNABLE_TO_PLACE)),
                "case {case:?}: the event the shortfall wrote stays in the stream, got \
                 {read}"
            );
            break;
        }
        assert!(
            ::std::time::Instant::now() < deadline,
            "case {case:?}: expected the scheduler to place the task once capacity exists, \
             got {read}"
        );
        tokio::time::sleep(::std::time::Duration::from_millis(25)).await;
    }
}
