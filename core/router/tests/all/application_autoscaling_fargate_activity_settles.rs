// SPDX-License-Identifier: CC0-1.0

//! The same rule as [`crate::application_autoscaling_activity_settles_on_its_ecs_service`], on
//! the axis that suite's rows do not cross: the DIRECTION the movement goes.
//!
//! RULE: a capacity movement settles once the service that owns the scaled resource is RUNNING
//! the count the movement set — whichever way that count moved. A scale in asks Amazon ECS for
//! FEWER tasks than it holds, so what the activity waits for is the task set coming DOWN to the
//! new `Service$desiredCount` ("the desired number of instantiations of the task definition to
//! keep running on the service") rather than up to it; at the instant `Service$runningCount`
//! ("the number of tasks in the cluster that are in the RUNNING state") is that count, the
//! activity reaches `Successful` and is stamped with an `EndTime`.
//!
//! The row drives the SAME runner the sibling suite's rows do
//! ([`crate::application_autoscaling_activity_settles_on_its_ecs_service::movement_settles`]),
//! so the two suites differ in their rows and not in what they assert — a scale in is one more
//! input to the one rule, not a rule of its own. It lives in its own file because every file
//! under `core/router/tests/all/` is one suite of the shared test target.

use crate::application_autoscaling_activity_settles_on_its_ecs_service::{
    movement_settles, Row,
};

/// A scale IN on the recorded estate: a load-balanced Fargate service holding three tasks whose
/// containers state a long wait, with a single negative step that takes it to two.
const SCALE_IN: Row = Row {
    case: "a scale in on a load-balanced service whose containers state a long wait",
    behind_a_load_balancer: true,
    command: &["sh", "-c", "sleep 3600"],
    starting: 3,
    adjustment: -1,
};

#[tokio::test]
async fn a_scale_in_settles_once_its_ecs_service_runs_the_reduced_count() {
    movement_settles(&SCALE_IN).await;
}
