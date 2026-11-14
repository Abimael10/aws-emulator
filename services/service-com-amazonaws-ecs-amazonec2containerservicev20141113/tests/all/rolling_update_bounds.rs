// SPDX-License-Identifier: CC0-1.0

//! The two bounds a ROLLING UPDATE is held to, observed end to end over the shared wire
//! harness in [`crate::ecs_wire`] by SAMPLING a service all the way through a rollout.
//!
//! The rule under test is `DeploymentConfiguration`'s pair of percentages, each a
//! percentage of the service's `desiredCount`:
//!
//! * `minimumHealthyPercent` is "a lower limit on the number of your service's tasks that
//!   must remain in the `RUNNING` state during a deployment, as a percentage of the
//!   `desiredCount` (rounded up to the nearest integer)", so no sample taken during the
//!   rollout may report fewer `RUNNING` tasks than that floor;
//! * `maximumPercent` is "an upper limit on the number of your service's tasks that are
//!   allowed in the `RUNNING` or `PENDING` state during a deployment, as a percentage of
//!   the `desiredCount` (rounded down to the nearest integer)", so no sample may report
//!   more tasks in those two states than that ceiling — and the surge room it buys is the
//!   ORDERING: "if your service ... has a `desiredCount` of four tasks and a
//!   `maximumPercent` value of 200%, the scheduler may start four new tasks before stopping
//!   the four older tasks", which a sample above the desired count is the evidence of.
//!
//! The rows differ along what the rule generalises over: the two percentages (the replica
//! defaults, a floor of none, a floor that rounds up from a fraction, and a ceiling that
//! leaves no surge room at all) and the `desiredCount` they are a percentage of (one, two
//! and three tasks). Each row states what its own pair REQUIRES of the samples — the floor
//! every sample has to meet, the ceiling none may cross, and whether the rollout has to be
//! seen above or below the desired count — so the table holds the rule rather than one
//! configuration's arithmetic.
//!
//! Every value asserted is read back through `DescribeServices`: nothing here reaches into
//! the state struct.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, count, list, ok, register_definition, service, string};

/// The cluster every service below runs in.
const CLUSTER: &str = "rollout";

/// How often the rollout is sampled. Far below the span one scheduler step takes, so every
/// step of a rollout is sampled several times.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(60);

/// How long a rollout is sampled for before the test gives up on it. Generous: a rollout
/// takes one scheduler step per task it replaces, and sampling stops the instant the
/// rollout reports itself complete.
const SAMPLE_BUDGET: Duration = Duration::from_secs(60);

/// One row of the table: the deployment configuration a service rolls out under, and what
/// the samples taken through that rollout have to show.
#[derive(Debug, Clone, Copy)]
struct BoundsCase {
    case: &'static str,
    /// The service's `desiredCount`, which both percentages are a percentage of.
    desired: i64,
    /// `minimumHealthyPercent`, when the request names one.
    minimum: Option<i64>,
    /// `maximumPercent`, when the request names one.
    maximum: Option<i64>,
    /// The fewest `RUNNING` tasks any sample may report: the floor, rounded UP from
    /// `minimum` as its own doc requires.
    floor: i64,
    /// The most `RUNNING` + `PENDING` tasks any sample may report: the ceiling, rounded
    /// DOWN from `maximum`.
    ceiling: i64,
    /// Whether the rollout has to be SEEN above the desired count — the surge a ceiling
    /// over 100% allows, which is how a client observes that replacements start before the
    /// tasks they replace are stopped.
    surges: bool,
}

/// What one sample of a service reports: the two counts the bounds are about.
#[derive(Debug, Clone, Copy)]
struct Sample {
    running: i64,
    pending: i64,
}

impl Sample {
    /// The count `maximumPercent` bounds: "the number of your service's tasks that are
    /// allowed in the RUNNING or PENDING state".
    fn live(&self) -> i64 {
        self.running + self.pending
    }
}

/// The rows: every combination the rule has to hold over.
fn rows() -> [BoundsCase; 5] {
    [
        BoundsCase {
            // The replica defaults, which `maximumPercent` and `minimumHealthyPercent`
            // document as 200% and 100%: the floor is the whole desired count, so the
            // replacements have to run beside the tasks they replace.
            case: "the replica defaults on two tasks",
            desired: 2,
            minimum: None,
            maximum: None,
            floor: 2,
            ceiling: 4,
            surges: true,
        },
        BoundsCase {
            case: "a floor of the whole count on a single task",
            desired: 1,
            minimum: Some(100),
            maximum: Some(200),
            floor: 1,
            ceiling: 2,
            surges: true,
        },
        BoundsCase {
            // 50% of three tasks is 1.5, which the floor rounds UP to two and the ceiling
            // rounds DOWN: 150% of three is 4.5, so four.
            case: "a floor and a ceiling that both round on three tasks",
            desired: 3,
            minimum: Some(50),
            maximum: Some(150),
            floor: 2,
            ceiling: 4,
            surges: true,
        },
        BoundsCase {
            // A ceiling of 100% leaves no surge room at all, so the scheduler has to free
            // capacity before it can fill it: the service is seen BELOW its desired count.
            case: "a ceiling that leaves no surge room",
            desired: 2,
            minimum: Some(50),
            maximum: Some(100),
            floor: 1,
            ceiling: 2,
            surges: false,
        },
        BoundsCase {
            // No floor at all: the scheduler may take the service down to nothing, which is
            // what a client asking for an in-place replacement gets.
            case: "no floor at all",
            desired: 2,
            minimum: Some(0),
            maximum: Some(100),
            floor: 0,
            ceiling: 2,
            surges: false,
        },
    ]
}

/// The service `DescribeServices` reports for `name`.
async fn describe(router: &MultiServiceRouter, name: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
        case,
    )
    .await;
    list(&body, "services", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the service to be described, got {body}"))
}

/// The `rolloutState` of the service's current (`PRIMARY`) deployment.
fn rollout_state(view: &Value, case: &str) -> String {
    let deployments = list(view, "deployments", case);
    let primary = deployments
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {view}"));
    string(primary, "rolloutState", case)
}

/// Sample `name` until its current rollout reports itself COMPLETED, and answer every
/// sample taken (including the one that found it complete).
///
/// The rollout is polled for, never waited out: each sample is one `DescribeServices`, and
/// the loop ends on the CONDITION the rollout reports.
async fn sample_rollout(router: &MultiServiceRouter, name: &str, case: &str) -> Vec<Sample> {
    let deadline = Instant::now() + SAMPLE_BUDGET;
    let mut samples = Vec::new();
    loop {
        let view = describe(router, name, case).await;
        samples.push(Sample {
            running: count(&view, "runningCount", case),
            pending: count(&view, "pendingCount", case),
        });
        if rollout_state(&view, case) == "COMPLETED" {
            return samples;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the rollout to complete, got {view}"
        );
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_rolling_update_holds_the_floor_and_the_ceiling_its_configuration_states() {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "setup").await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let family = format!("rollingbounds{index}");
        let first = register_definition(&router, &family, case).await;
        let second = register_definition(&router, &family, case).await;
        let name = format!("rollingbounds{index}");

        let mut configuration = json!({});
        if let Some(minimum) = row.minimum {
            configuration["minimumHealthyPercent"] = json!(minimum);
        }
        if let Some(maximum) = row.maximum {
            configuration["maximumPercent"] = json!(maximum);
        }
        ok(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": name,
                "taskDefinition": first,
                "desiredCount": row.desired,
                "launchType": "EC2",
                "deploymentConfiguration": configuration,
            }),
            case,
        )
        .await;
        // The service has to be AT its desired count before the rollout that replaces that
        // task set says anything about the bounds.
        let settling = sample_rollout(&router, &name, case).await;
        let settled = settling.last().copied().expect("a sample was taken");
        assert_eq!(
            (settled.running, settled.pending),
            (row.desired, 0),
            "case {case:?}: expected the created service to reach its desired count, got \
             {settled:?}"
        );

        // The rollout under test: the same service onto the next revision.
        ok(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": name,
                "taskDefinition": second,
            }),
            case,
        )
        .await;
        let samples = sample_rollout(&router, &name, case).await;

        let lowest = samples
            .iter()
            .map(|sample| sample.running)
            .min()
            .expect("a sample was taken");
        assert!(
            lowest >= row.floor,
            "case {case:?}: minimumHealthyPercent is a floor on the tasks that stay \
             RUNNING, so no sample may report fewer than {}, got {lowest} (samples \
             {samples:?})",
            row.floor
        );
        let highest = samples
            .iter()
            .map(Sample::live)
            .max()
            .expect("a sample was taken");
        assert!(
            highest <= row.ceiling,
            "case {case:?}: maximumPercent is a ceiling on the tasks RUNNING or PENDING, \
             so no sample may report more than {}, got {highest} (samples {samples:?})",
            row.ceiling
        );
        let peak_running = samples
            .iter()
            .map(|sample| sample.running)
            .max()
            .expect("a sample was taken");
        if row.surges {
            assert!(
                peak_running > row.desired,
                "case {case:?}: a ceiling above the desired count lets the scheduler start \
                 the replacements before stopping the tasks they replace, so the service \
                 must be seen above {} RUNNING, got {peak_running} (samples {samples:?})",
                row.desired
            );
        } else {
            assert!(
                highest <= row.desired,
                "case {case:?}: a ceiling of the desired count leaves no surge room, so no \
                 sample may exceed {} tasks, got {highest} (samples {samples:?})",
                row.desired
            );
            assert!(
                lowest < row.desired,
                "case {case:?}: with no surge room the scheduler has to free capacity \
                 before it can fill it, so the service must be seen below {} RUNNING, got \
                 {lowest} (samples {samples:?})",
                row.desired
            );
        }

        // The rollout ends with the service at its desired count, running the revision it
        // rolled out: the bounds hold a rollout that CONVERGES.
        let settled = describe(&router, &name, case).await;
        assert_eq!(
            (
                count(&settled, "runningCount", case),
                count(&settled, "pendingCount", case),
                string(&settled, "taskDefinition", case),
            ),
            (row.desired, 0, second.clone()),
            "case {case:?}: expected the rollout to converge on the new revision, got \
             {settled}"
        );
    }
}

#[tokio::test]
async fn the_surge_holds_for_an_awsvpc_service_whose_tasks_take_their_own_launch_steps() {
    // The same rule over a launch whose tasks are given an elastic network interface before
    // they start (an `awsvpc` Fargate service): the replacement takes several more steps to
    // come up, and the task it replaces still has to stay RUNNING until it has.
    let case = "an awsvpc service at a floor of the whole count";
    let router = service();
    cluster_with_capacity(&router, CLUSTER, case).await;
    let family = "rollingboundsawsvpc";
    let first = register_awsvpc_definition(&router, family, case).await;
    let second = register_awsvpc_definition(&router, family, case).await;
    let name = "rollingboundsawsvpc";
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": name,
            "taskDefinition": first,
            "desiredCount": 2,
            "launchType": "FARGATE",
            "networkConfiguration": crate::ecs_wire::awsvpc_network_configuration(),
            "deploymentConfiguration": {
                "minimumHealthyPercent": 100,
                "maximumPercent": 200,
            },
        }),
        case,
    )
    .await;
    sample_rollout(&router, name, case).await;
    ok(
        &router,
        "UpdateService",
        json!({ "cluster": CLUSTER, "service": name, "taskDefinition": second }),
        case,
    )
    .await;
    let samples = sample_rollout(&router, name, case).await;
    let lowest = samples
        .iter()
        .map(|sample| sample.running)
        .min()
        .expect("a sample was taken");
    let peak_running = samples
        .iter()
        .map(|sample| sample.running)
        .max()
        .expect("a sample was taken");
    let highest = samples
        .iter()
        .map(Sample::live)
        .max()
        .expect("a sample was taken");
    assert!(
        lowest >= 2,
        "case {case:?}: the floor holds however long the replacement takes to come up, so \
         no sample may report fewer than 2 RUNNING, got {lowest} (samples {samples:?})"
    );
    assert!(
        peak_running > 2,
        "case {case:?}: the replacements run beside the tasks they replace, so the service \
         must be seen above 2 RUNNING, got {peak_running} (samples {samples:?})"
    );
    assert!(
        highest <= 4,
        "case {case:?}: 200% of two tasks is a ceiling of four, got {highest} (samples \
         {samples:?})"
    );
}

/// Register one Fargate-compatible `awsvpc` revision of `family`, and answer its ARN.
async fn register_awsvpc_definition(
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
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["FARGATE"],
            "containerDefinitions": [
                crate::task_lifecycle::container("web", "public.ecr.aws/nginx/nginx:latest"),
            ],
        }),
        case,
    )
    .await;
    string(&body["taskDefinition"], "taskDefinitionArn", case)
}
