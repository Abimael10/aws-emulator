// SPDX-License-Identifier: CC0-1.0

//! Whether a service job can be PLACED, end to end over the wire.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! * a service job is placed only when the capacity its request asks for fits the capacity
//!   limits the service environments of its queue publish; a job that fits is PLACED, so its
//!   work is owed to the service it is dispatched to, and one that does NOT fit is never
//!   placed, so it RESTS in the non-terminal `RUNNABLE` status owing nothing — and either
//!   way no `statusReason`, `stoppedAt` or attempt is reported, because nothing mounted
//!   beside this emulator serves that dispatch (`service_job_dispatch` is the suite that
//!   mounts it);
//! * a job resting in `RUNNABLE` stays readable: `DescribeServiceJob` reads every member
//!   the submit carried back unchanged — the integers as submitted, the payload
//!   byte-identical — and reports an optional member the submit never carried as ABSENT;
//! * a read never moves a resting job, and the capacity a later call publishes — a bigger
//!   `capacityLimits` on the environment (`UpdateServiceEnvironment`) or a
//!   `serviceEnvironmentOrder` naming an environment that can hold it (`UpdateJobQueue`) —
//!   is what makes its next move due;
//! * every caller-filled list this rule reads is kept to a maximum — the payload's bytes,
//!   an environment's `capacityLimits` and a queue's `serviceEnvironmentOrder` — accepted
//!   on the maximum and refused one past it with the modelled `ClientException`.
//!
//! The capacity a `SAGEMAKER_TRAINING` request asks for is its `ResourceConfig`
//! (`InstanceType`/`InstanceCount`, or one `InstanceGroups` entry per group), and a limit
//! is measured either in an instance type or in `NUM_INSTANCES` —
//! `CapacityLimit$capacityUnit`: "For `SAGEMAKER_TRAINING` jobs in a quota management
//! enabled service environment, specify the instance type (for example, `ml.m5.large`).
//! Otherwise, use `NUM_INSTANCES`."

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::job_lifecycle::{error_code, error_message, Batch};

/// The only `ServiceJobType`/`ServiceEnvironmentType` the model declares
/// (`smithy_query(query="shape", shape="ServiceJobType")`).
const SAGEMAKER_TRAINING: &str = "SAGEMAKER_TRAINING";

/// An instance type a `ResourceConfig` can ask for, and a `capacityUnit` can be measured
/// in: the example the `capacityUnit` doc itself names.
const SMALL_INSTANCE_TYPE: &str = "ml.m5.large";

/// A second instance type, so a limit published for one type can be shown NOT to admit
/// another. Named in the `ResourceConfig` doc's instance-type list.
const LARGE_INSTANCE_TYPE: &str = "ml.p4d.24xlarge";

/// How long a poll below gives the clock: far longer than the spans the state gives a
/// service job's moves, so a loaded host still settles well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Look again until `job_id`'s status is `status`, answering the read that saw it.
pub(crate) async fn polled_until_status(
    batch: &Batch,
    case: &str,
    job_id: &str,
    status: &str,
) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let detail = described(batch, case, job_id).await;
        if detail["status"].as_str() == Some(status) {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected the status {status:?}, last read {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether the scheduler has PLACED `job_id`: its work is owed to, or already with, the
/// service it is dispatched to.
///
/// Read from the store the handlers read and write (`Batch::store`), because the dispatch is
/// the one part of a placement a client cannot see when nothing serves it: a placed job and a
/// resting one are both `RUNNABLE` on the wire until that service answers.
pub(crate) fn is_placed(batch: &Batch, job_id: &str) -> bool {
    let store = batch.store();
    let store = store.lock();
    store.service_jobs.get(job_id).is_some_and(|job| {
        job.dispatch_state != state::ServiceJobDispatchState::Waiting
    })
}

/// Look again until the scheduler has placed `job_id` ([`is_placed`]).
pub(crate) async fn polled_until_placed(batch: &Batch, case: &str, job_id: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        // A read is what drives the dispatch the clock made due, so the job is described
        // before the store is looked at.
        let detail = described(batch, case, job_id).await;
        if is_placed(batch, job_id) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected the job to be placed, last read {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A service environment publishing `capacity_limits`, and a `SAGEMAKER_TRAINING` queue
/// over it; answers the queue's name.
pub(crate) async fn environment_and_queue(
    batch: &Batch,
    suffix: &str,
    capacity_limits: Value,
) -> String {
    let environment = format!("se-{suffix}");
    batch
        .ok(
            "/v1/createserviceenvironment",
            json!({
                "serviceEnvironmentName": &environment,
                "serviceEnvironmentType": SAGEMAKER_TRAINING,
                "capacityLimits": capacity_limits,
            }),
        )
        .await;
    let queue = format!("jq-{suffix}");
    batch
        .ok(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": &queue,
                "priority": 1,
                "jobQueueType": SAGEMAKER_TRAINING,
                "serviceEnvironmentOrder": [{"order": 1, "serviceEnvironment": &environment}],
            }),
        )
        .await;
    queue
}

/// `SubmitServiceJob` on `queue` with `payload`, answering the minted job id.
pub(crate) async fn submit(batch: &Batch, suffix: &str, queue: &str, payload: &str) -> String {
    let accepted = batch
        .ok(
            "/v1/submitservicejob",
            json!({
                "jobName": format!("sj-{suffix}"),
                "jobQueue": queue,
                "serviceJobType": SAGEMAKER_TRAINING,
                "serviceRequestPayload": payload,
            }),
        )
        .await;
    accepted["jobId"].as_str().unwrap_or_default().to_string()
}

/// `DescribeServiceJob`, requiring the 200.
async fn described(batch: &Batch, case: &str, job_id: &str) -> Value {
    let (status, body) = batch
        .call("/v1/describeservicejob", json!({"jobId": job_id}))
        .await;
    assert_eq!(
        status, 200,
        "case {case}: expected DescribeServiceJob to answer 200, got {status} {body}"
    );
    body
}

/// One placement row: the capacity an environment publishes, the capacity a submit asks
/// for, and whether the job can therefore be placed.
struct Placement {
    /// What this row varies; named in every assertion.
    label: &'static str,
    /// The suffix the row's environment, queue and job are named with.
    suffix: &'static str,
    capacity_limits: Value,
    payload: String,
    /// Whether the published capacity admits the request.
    placeable: bool,
}

/// The table: rows differ along what the rule generalises over — the unit a limit is
/// measured in (`NUM_INSTANCES` or an instance type), the maximum it publishes, how many
/// instances of which type the request asks for, whether the request states ONE
/// `ResourceConfig` or several `InstanceGroups`, and whether it states any capacity at all.
/// Each published maximum is also tested ON the bound and one past it.
fn placements() -> Vec<Placement> {
    vec![
        Placement {
            label: "two instances against a NUM_INSTANCES maximum of one",
            suffix: "over-num",
            capacity_limits: json!([{"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}]),
            payload: json!({
                "ResourceConfig": {
                    "InstanceType": SMALL_INSTANCE_TYPE,
                    "InstanceCount": 2,
                    "VolumeSizeInGB": 13,
                },
                "StoppingCondition": {"MaxRuntimeInSeconds": 45},
            })
            .to_string(),
            placeable: false,
        },
        Placement {
            label: "one instance against a NUM_INSTANCES maximum of one (on the bound)",
            suffix: "on-num",
            capacity_limits: json!([{"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}]),
            payload: json!({
                "ResourceConfig": {"InstanceType": SMALL_INSTANCE_TYPE, "InstanceCount": 1},
            })
            .to_string(),
            placeable: true,
        },
        Placement {
            label: "two instance groups summing past a NUM_INSTANCES maximum of four",
            suffix: "over-groups",
            capacity_limits: json!([{"maxCapacity": 4, "capacityUnit": "NUM_INSTANCES"}]),
            payload: json!({
                "ResourceConfig": {
                    "InstanceGroups": [
                        {"InstanceType": SMALL_INSTANCE_TYPE, "InstanceCount": 2},
                        {"InstanceType": LARGE_INSTANCE_TYPE, "InstanceCount": 3},
                    ],
                },
            })
            .to_string(),
            placeable: false,
        },
        Placement {
            label: "two instance groups summing exactly to a NUM_INSTANCES maximum of four",
            suffix: "on-groups",
            capacity_limits: json!([{"maxCapacity": 4, "capacityUnit": "NUM_INSTANCES"}]),
            payload: json!({
                "ResourceConfig": {
                    "InstanceGroups": [
                        {"InstanceType": SMALL_INSTANCE_TYPE, "InstanceCount": 2},
                        {"InstanceType": LARGE_INSTANCE_TYPE, "InstanceCount": 2},
                    ],
                },
            })
            .to_string(),
            placeable: true,
        },
        Placement {
            label: "two instances of the type a limit of two is measured in",
            suffix: "on-type",
            capacity_limits: json!([{"maxCapacity": 2, "capacityUnit": SMALL_INSTANCE_TYPE}]),
            payload: json!({
                "ResourceConfig": {"InstanceType": SMALL_INSTANCE_TYPE, "InstanceCount": 2},
            })
            .to_string(),
            placeable: true,
        },
        Placement {
            label: "three instances of the type a limit of two is measured in",
            suffix: "over-type",
            capacity_limits: json!([{"maxCapacity": 2, "capacityUnit": SMALL_INSTANCE_TYPE}]),
            payload: json!({
                "ResourceConfig": {"InstanceType": SMALL_INSTANCE_TYPE, "InstanceCount": 3},
            })
            .to_string(),
            placeable: false,
        },
        Placement {
            label: "an instance type no published limit is measured in",
            suffix: "other-type",
            capacity_limits: json!([{"maxCapacity": 8, "capacityUnit": SMALL_INSTANCE_TYPE}]),
            payload: json!({
                "ResourceConfig": {"InstanceType": LARGE_INSTANCE_TYPE, "InstanceCount": 1},
            })
            .to_string(),
            placeable: false,
        },
        Placement {
            label: "a request that states no ResourceConfig at all",
            suffix: "no-config",
            capacity_limits: json!([{"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}]),
            payload: json!({"TrainingJobName": "t-no-config"}).to_string(),
            placeable: true,
        },
        Placement {
            label: "an environment that publishes no capacity limits",
            suffix: "no-limits",
            capacity_limits: json!([]),
            payload: json!({
                "ResourceConfig": {"InstanceType": LARGE_INSTANCE_TYPE, "InstanceCount": 64},
            })
            .to_string(),
            placeable: true,
        },
    ]
}

/// RULE: a service job is placed only when the capacity its request asks for fits what the
/// service environments of its queue publish; one that does not fit RESTS in the
/// non-terminal `RUNNABLE` status owing nothing, with nothing terminal invented for it,
/// while one that fits is PLACED — its work owed to the service it is dispatched to.
#[tokio::test]
async fn a_service_job_is_placed_only_within_the_capacity_its_environment_publishes() {
    let batch = Batch::new();

    // Every row is submitted first, so the polls below cover them all at once.
    let mut submitted: Vec<(Placement, String)> = Vec::new();
    for row in placements() {
        let queue = environment_and_queue(&batch, row.suffix, row.capacity_limits.clone()).await;
        let job_id = submit(&batch, row.suffix, &queue, &row.payload).await;
        submitted.push((row, job_id));
    }

    for (row, job_id) in &submitted {
        let case = row.label;
        if row.placeable {
            polled_until_placed(&batch, case, job_id).await;
            let detail = described(&batch, case, job_id).await;
            assert_eq!(
                detail["status"].as_str(),
                Some(state::SERVICE_JOB_RUNNABLE),
                "case {case}: a placed job's work is owed to a service nothing mounted beside \
                 this emulator serves, so it waits in RUNNABLE, got {detail}"
            );
            assert_eq!(
                detail["statusReason"], Value::Null,
                "case {case}: a job whose dispatch reached nothing reports no reason, got \
                 {detail}"
            );
            continue;
        }
        let detail = polled_until_status(&batch, case, job_id, state::SERVICE_JOB_RUNNABLE).await;
        assert!(
            !is_placed(&batch, job_id),
            "case {case}: a job the published capacity cannot hold is never placed, got \
             {detail}"
        );
        // Nothing placed or dispatched the job, so nothing a placement would produce is
        // reported for it.
        for member in [
            "statusReason",
            "stoppedAt",
            "scheduledAt",
            "attempts",
            "latestAttempt",
            "capacityUsage",
            "preemptionSummary",
        ] {
            assert_eq!(
                detail[member], Value::Null,
                "case {case}: a resting job reports no {member}, got {detail}"
            );
        }
        assert_eq!(
            detail["isTerminated"].as_bool(),
            Some(false),
            "case {case}: nothing terminated a resting job, got {detail}"
        );

        // A read never moves it.
        let again = described(&batch, case, job_id).await;
        assert_eq!(
            again, detail,
            "case {case}: two reads at the same instant answer identically, got {again}"
        );
    }

    // Nor does more time passing: every resting row is read again and again over a window
    // far longer than the spans this service's moves take, and must not move.
    let settled = Instant::now() + Duration::from_secs(5);
    while Instant::now() < settled {
        for (row, job_id) in &submitted {
            if row.placeable {
                continue;
            }
            let case = row.label;
            let later = described(&batch, case, job_id).await;
            assert_eq!(
                later["status"].as_str(),
                Some(state::SERVICE_JOB_RUNNABLE),
                "case {case}: a job that still cannot be placed still rests in RUNNABLE, \
                 got {later}"
            );
            assert!(
                !is_placed(&batch, job_id),
                "case {case}: a job that still cannot be placed is still not placed"
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a service job resting in `RUNNABLE` reads back every member the submit carried,
/// unchanged — the integers as submitted and the payload byte-identical — and an optional
/// member the submit never carried is ABSENT rather than defaulted.
#[tokio::test]
async fn a_resting_service_job_reads_every_submitted_member_back_unchanged() {
    let batch = Batch::new();
    let queue = environment_and_queue(
        &batch,
        "rest-roundtrip",
        json!([{"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}]),
    )
    .await;

    // The rows differ along the values the rule must preserve: the priorities and attempt
    // counts on the bounds their own docs publish, the timeout in between, and payloads
    // whose embedded JSON numbers must keep their precision (a long integer, a decimal and
    // an exponent).
    let rows: Vec<(&str, i64, i64, i64, &str, String)> = vec![
        (
            "the recorded shape: priority 17, one attempt, 999 seconds",
            17,
            1,
            999,
            "default",
            r#"{"ResourceConfig":{"InstanceType":"ml.m5.large","InstanceCount":2,"VolumeSizeInGB":13},"StoppingCondition":{"MaxRuntimeInSeconds":45}}"#
                .to_string(),
        ),
        (
            "both members on their published minima, a decimal in the payload",
            i64::from(state::MIN_SCHEDULING_PRIORITY),
            i64::from(state::MIN_RETRY_ATTEMPTS),
            1,
            "s",
            r#"{"ResourceConfig":{"InstanceCount":9,"KeepAlivePeriodInSeconds":1800},"HyperParameters":{"lr":0.000012345678901234567}}"#
                .to_string(),
        ),
        (
            "both members on their published maxima, a long integer and an exponent",
            i64::from(state::MAX_SCHEDULING_PRIORITY),
            i64::from(state::MAX_RETRY_ATTEMPTS),
            86400,
            "shareWithALongerName",
            r#"{"ResourceConfig":{"InstanceCount":3,"VolumeSizeInGB":12345678901234567890},"StoppingCondition":{"MaxRuntimeInSeconds":1.0e3}}"#
                .to_string(),
        ),
    ];

    // Every row is submitted first, so ONE wait covers the transitions all of them owe.
    let mut submitted: Vec<String> = Vec::new();
    for (index, (_, priority, attempts, seconds, share, payload)) in rows.iter().enumerate() {
        let accepted = batch
            .ok(
                "/v1/submitservicejob",
                json!({
                    "jobName": format!("sj-rest-{index}"),
                    "jobQueue": &queue,
                    "serviceJobType": SAGEMAKER_TRAINING,
                    "serviceRequestPayload": payload,
                    "schedulingPriority": priority,
                    "shareIdentifier": share,
                    "timeoutConfig": {"attemptDurationSeconds": seconds},
                    "retryStrategy": {"attempts": attempts},
                }),
            )
            .await;
        submitted.push(accepted["jobId"].as_str().unwrap_or_default().to_string());
    }
    for ((case, priority, attempts, seconds, share, payload), job_id) in
        rows.iter().zip(&submitted)
    {
        let detail = polled_until_status(&batch, case, job_id, state::SERVICE_JOB_RUNNABLE).await;
        assert_eq!(
            detail["status"].as_str(),
            Some(state::SERVICE_JOB_RUNNABLE),
            "case {case}: the request asks for more instances than the environment \
             publishes, so the job rests in RUNNABLE, got {detail}"
        );
        assert_eq!(
            detail["schedulingPriority"].as_i64(),
            Some(*priority),
            "case {case}: expected the submitted schedulingPriority, got {detail}"
        );
        assert_eq!(
            detail["retryStrategy"]["attempts"].as_i64(),
            Some(*attempts),
            "case {case}: expected the submitted retryStrategy.attempts, got {detail}"
        );
        assert_eq!(
            detail["timeoutConfig"]["attemptDurationSeconds"].as_i64(),
            Some(*seconds),
            "case {case}: expected the submitted attemptDurationSeconds, got {detail}"
        );
        assert_eq!(
            detail["shareIdentifier"].as_str(),
            Some(*share),
            "case {case}: expected the submitted shareIdentifier, got {detail}"
        );
        assert_eq!(
            detail["serviceRequestPayload"].as_str(),
            Some(payload.as_str()),
            "case {case}: the payload is persisted byte-identical, got {detail}"
        );
        assert_eq!(
            detail["quotaShareName"], Value::Null,
            "case {case}: a member the submit never carried stays absent, got {detail}"
        );
    }
}

/// RULE: the capacity a later call publishes is what moves a resting job on — raising the
/// environment's `capacityLimits` makes the next move of a job that now fits due, while an
/// `UpdateServiceEnvironment` that publishes capacity still too small leaves it resting.
#[tokio::test]
async fn capacity_published_later_is_what_moves_a_resting_service_job_on() {
    let batch = Batch::new();
    let environment = "se-rearm";
    batch
        .ok(
            "/v1/createserviceenvironment",
            json!({
                "serviceEnvironmentName": environment,
                "serviceEnvironmentType": SAGEMAKER_TRAINING,
                "capacityLimits": [{"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}],
            }),
        )
        .await;
    batch
        .ok(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": "jq-rearm",
                "priority": 1,
                "jobQueueType": SAGEMAKER_TRAINING,
                "serviceEnvironmentOrder": [{"order": 1, "serviceEnvironment": environment}],
            }),
        )
        .await;
    let payload = json!({
        "ResourceConfig": {"InstanceType": SMALL_INSTANCE_TYPE, "InstanceCount": 4},
    })
    .to_string();
    let job_id = submit(&batch, "rearm", "jq-rearm", &payload).await;

    let resting = polled_until_status(
        &batch,
        "before the capacity grows",
        &job_id,
        state::SERVICE_JOB_RUNNABLE,
    )
    .await;
    assert!(
        !is_placed(&batch, &job_id),
        "case before the capacity grows: expected the job to rest unplaced, got {resting}"
    );

    // Still too small: the job owes nothing, so it stays where it is — read again and again
    // over a window far longer than the spans this service's moves take.
    batch
        .ok(
            "/v1/updateserviceenvironment",
            json!({
                "serviceEnvironment": environment,
                "capacityLimits": [{"maxCapacity": 3, "capacityUnit": "NUM_INSTANCES"}],
            }),
        )
        .await;
    let settled = Instant::now() + Duration::from_secs(5);
    while Instant::now() < settled {
        let still_resting = described(&batch, "capacity still too small", &job_id).await;
        assert_eq!(
            still_resting["status"].as_str(),
            Some(state::SERVICE_JOB_RUNNABLE),
            "case capacity still too small: expected the job to keep resting, got \
             {still_resting}"
        );
        assert!(
            !is_placed(&batch, &job_id),
            "case capacity still too small: expected the job to stay unplaced"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Enough capacity: the placement is owed again, and a span later it has been made.
    batch
        .ok(
            "/v1/updateserviceenvironment",
            json!({
                "serviceEnvironment": environment,
                "capacityLimits": [{"maxCapacity": 4, "capacityUnit": "NUM_INSTANCES"}],
            }),
        )
        .await;
    let straight_after = described(&batch, "straight after the capacity grows", &job_id).await;
    assert!(
        !is_placed(&batch, &job_id),
        "case straight after the capacity grows: the owed placement takes a span, so the \
         unplaced state is still observable, got {straight_after}"
    );
    polled_until_placed(&batch, "after the capacity grows", &job_id).await;
}

/// RULE: the same holds for the OTHER call that changes which capacity a job's queue can
/// reach — an `UpdateJobQueue` that points the queue's `serviceEnvironmentOrder` at an
/// environment publishing enough capacity makes the resting job's next move due.
#[tokio::test]
async fn pointing_a_queue_at_a_bigger_environment_moves_a_resting_service_job_on() {
    let batch = Batch::new();
    let queue = environment_and_queue(
        &batch,
        "reorder-small",
        json!([{"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}]),
    )
    .await;
    // A second environment, publishing room for the request the small one cannot hold.
    batch
        .ok(
            "/v1/createserviceenvironment",
            json!({
                "serviceEnvironmentName": "se-reorder-large",
                "serviceEnvironmentType": SAGEMAKER_TRAINING,
                "capacityLimits": [{"maxCapacity": 16, "capacityUnit": "NUM_INSTANCES"}],
            }),
        )
        .await;
    let payload = json!({
        "ResourceConfig": {"InstanceType": SMALL_INSTANCE_TYPE, "InstanceCount": 6},
    })
    .to_string();
    let job_id = submit(&batch, "reorder", &queue, &payload).await;

    let resting = polled_until_status(
        &batch,
        "before the queue is re-pointed",
        &job_id,
        state::SERVICE_JOB_RUNNABLE,
    )
    .await;
    assert!(
        !is_placed(&batch, &job_id),
        "case before the queue is re-pointed: expected the job to rest unplaced, got {resting}"
    );

    batch
        .ok(
            "/v1/updatejobqueue",
            json!({
                "jobQueue": &queue,
                "serviceEnvironmentOrder": [
                    {"order": 1, "serviceEnvironment": "se-reorder-large"},
                ],
            }),
        )
        .await;
    polled_until_placed(&batch, "after the queue is re-pointed", &job_id).await;
}

/// RULE: every caller-filled list and payload this placement rule reads is kept to a
/// maximum, so one request cannot decide how much work a later one costs: a
/// `serviceRequestPayload` past `MAX_SERVICE_REQUEST_PAYLOAD_BYTES`, a `capacityLimits`
/// past `MAX_SERVICE_ENVIRONMENT_CAPACITY_LIMITS` and a `serviceEnvironmentOrder` past
/// `MAX_SERVICE_ENVIRONMENT_ORDER_ENTRIES` are each the modelled `ClientException` (400),
/// while a request ON each maximum is accepted.
#[tokio::test]
async fn the_lists_placement_reads_are_each_kept_to_their_maximum() {
    let batch = Batch::new();

    // ON the maximum: as many capacity limits as an environment keeps, each measured in a
    // distinct unit, and a queue over it with as many order entries as a queue keeps.
    let on_bound_limits: Vec<Value> = (0..state::limits::MAX_SERVICE_ENVIRONMENT_CAPACITY_LIMITS)
        .map(|index| json!({"maxCapacity": 1, "capacityUnit": format!("ml.unit{index}.large")}))
        .collect();
    batch
        .ok(
            "/v1/createserviceenvironment",
            json!({
                "serviceEnvironmentName": "se-bounds",
                "serviceEnvironmentType": SAGEMAKER_TRAINING,
                "capacityLimits": on_bound_limits,
            }),
        )
        .await;
    let mut order: Vec<Value> = Vec::new();
    for index in 0..state::limits::MAX_SERVICE_ENVIRONMENT_ORDER_ENTRIES {
        let name = format!("se-bounds-{index}");
        batch
            .ok(
                "/v1/createserviceenvironment",
                json!({
                    "serviceEnvironmentName": &name,
                    "serviceEnvironmentType": SAGEMAKER_TRAINING,
                    "capacityLimits": [{"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}],
                }),
            )
            .await;
        order.push(json!({"order": index + 1, "serviceEnvironment": name}));
    }
    batch
        .ok(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": "jq-bounds",
                "priority": 1,
                "jobQueueType": SAGEMAKER_TRAINING,
                "serviceEnvironmentOrder": order.clone(),
            }),
        )
        .await;
    // A payload ON the byte maximum is accepted, and the job it mints is weighed the same
    // way as any other.
    let prefix =
        format!(r#"{{"ResourceConfig":{{"InstanceType":"{SMALL_INSTANCE_TYPE}","InstanceCount":1}},"N":""#);
    let suffix = r#""}"#;
    let filler = state::limits::MAX_SERVICE_REQUEST_PAYLOAD_BYTES - prefix.len() - suffix.len();
    let on_bound_payload = format!("{prefix}{}{suffix}", "x".repeat(filler));
    assert_eq!(
        on_bound_payload.len(),
        state::limits::MAX_SERVICE_REQUEST_PAYLOAD_BYTES,
        "case the payload on the maximum: the row must sit exactly on the bound, got {} bytes",
        on_bound_payload.len()
    );
    let accepted = batch
        .ok(
            "/v1/submitservicejob",
            json!({
                "jobName": "sj-bounds",
                "jobQueue": "jq-bounds",
                "serviceJobType": SAGEMAKER_TRAINING,
                "serviceRequestPayload": &on_bound_payload,
            }),
        )
        .await;
    assert!(
        accepted["jobId"].as_str().is_some_and(|id| !id.is_empty()),
        "case on every maximum: the submit is accepted, got {accepted}"
    );

    // One PAST each maximum, each refused with the modelled ClientException and persisting
    // nothing.
    let mut too_many_limits = on_bound_limits.clone();
    too_many_limits.push(json!({"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}));
    let mut too_long_order = order.clone();
    too_long_order.push(json!({"order": 99, "serviceEnvironment": "se-bounds"}));
    let too_long_payload = format!(
        r#"{{"Note":"{}"}}"#,
        "x".repeat(state::limits::MAX_SERVICE_REQUEST_PAYLOAD_BYTES)
    );
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "capacityLimits one past the maximum, on a create",
            "/v1/createserviceenvironment",
            json!({
                "serviceEnvironmentName": "se-too-many-limits",
                "serviceEnvironmentType": SAGEMAKER_TRAINING,
                "capacityLimits": too_many_limits,
            }),
        ),
        (
            "capacityLimits one past the maximum, on an update",
            "/v1/updateserviceenvironment",
            json!({
                "serviceEnvironment": "se-bounds",
                "capacityLimits": too_many_limits,
            }),
        ),
        (
            "serviceEnvironmentOrder one past the maximum, on a create",
            "/v1/createjobqueue",
            json!({
                "jobQueueName": "jq-too-long-order",
                "priority": 1,
                "jobQueueType": SAGEMAKER_TRAINING,
                "serviceEnvironmentOrder": too_long_order,
            }),
        ),
        (
            "serviceEnvironmentOrder one past the maximum, on an update",
            "/v1/updatejobqueue",
            json!({"jobQueue": "jq-bounds", "serviceEnvironmentOrder": too_long_order}),
        ),
        (
            "serviceRequestPayload past the maximum",
            "/v1/submitservicejob",
            json!({
                "jobName": "sj-too-long-payload",
                "jobQueue": "jq-bounds",
                "serviceJobType": SAGEMAKER_TRAINING,
                "serviceRequestPayload": &too_long_payload,
            }),
        ),
    ];
    for (case, uri, body) in rows {
        let (status, refused) = batch.call(uri, body).await;
        assert_eq!(
            status, 400,
            "case {case}: expected the modelled ClientException 400, got {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ClientException",
            "case {case}: expected the modelled ClientException, got {refused}"
        );
        assert!(
            error_message(&refused).contains("at most"),
            "case {case}: the refusal states the maximum, got {refused}"
        );
    }
}

/// RULE: placement decides nothing about ACCEPTANCE — a submit to a queue whose
/// environments cannot hold it is still the 200 that mints the job, while the refusals the
/// operation models are unchanged by the published capacity.
#[tokio::test]
async fn capacity_never_turns_a_submit_into_a_refusal() {
    let batch = Batch::new();
    let queue = environment_and_queue(
        &batch,
        "accepts",
        json!([{"maxCapacity": 1, "capacityUnit": "NUM_INSTANCES"}]),
    )
    .await;
    let payload = json!({
        "ResourceConfig": {"InstanceType": SMALL_INSTANCE_TYPE, "InstanceCount": 32},
    })
    .to_string();

    let accepted = batch
        .ok(
            "/v1/submitservicejob",
            json!({
                "jobName": "sj-accepted-over-capacity",
                "jobQueue": &queue,
                "serviceJobType": SAGEMAKER_TRAINING,
                "serviceRequestPayload": &payload,
            }),
        )
        .await;
    assert!(
        accepted["jobId"].as_str().is_some_and(|id| !id.is_empty()),
        "case over capacity: a job the environment cannot hold is still accepted, got \
         {accepted}"
    );

    // A queue that resolves to nothing is still the modelled refusal, whatever the
    // capacity anything publishes.
    let (status, refused) = batch
        .call(
            "/v1/submitservicejob",
            json!({
                "jobName": "sj-absent-queue",
                "jobQueue": "jq-not-created",
                "serviceJobType": SAGEMAKER_TRAINING,
                "serviceRequestPayload": &payload,
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case an absent queue: expected the modelled ClientException 400, got {status} \
         {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ClientException",
        "case an absent queue: expected the modelled ClientException, got {refused}"
    );
    assert!(
        error_message(&refused).contains("jq-not-created"),
        "case an absent queue: the refusal names the reference the request gave, got \
         {refused}"
    );
}
