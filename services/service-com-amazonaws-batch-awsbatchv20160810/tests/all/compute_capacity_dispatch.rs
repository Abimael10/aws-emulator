// SPDX-License-Identifier: CC0-1.0

//! The CAPACITY a compute environment bounds, and the ORDER the jobs waiting on it are
//! dispatched in — end to end over the wire.
//!
//! RULES under test (`smithy_query(query="shape", shape="ComputeResource")`,
//! `smithy_query(query="operation", operation="CreateJobQueue")`,
//! `smithy_query(query="shape", shape="ShareAttributes")`):
//!
//! * `ComputeResource$maxvCpus` is "the maximum number of vCPUs that a compute environment
//!   can support" and `ResourceRequirement$value` for `VCPU` is "the number of vCPUs reserved
//!   for the container", so the maximum is a CONCURRENCY bound: the vCPUs reserved by the jobs
//!   placed on one environment never exceed it at any observed instant, a job that does not
//!   fit waits where a job waits to be placed (`JobStatus$RUNNABLE`), and it runs once a job
//!   holding capacity reaches a terminal status;
//! * `CreateJobQueue$priority`: "job queues with a higher priority (or a higher integer value
//!   for the priority parameter) are evaluated first when associated with the same compute
//!   environment. Priority is determined in descending order" — so when capacity frees up on
//!   an environment two queues share, the job on the higher-priority queue is dispatched
//!   ahead of one that has been waiting on the lower-priority queue;
//! * on a FAIR-SHARE queue `GetJobQueueSnapshot` reports the `RUNNABLE` jobs "ordered based on
//!   their job priority and share usage", and `ShareAttributes$weightFactor` is "the weight
//!   factor for the share identifier. The default value is 1.0. A lower value has a higher
//!   priority for compute resources" — so a share's usage weighted by its own factor ranks the
//!   jobs of the queue, a job's own `schedulingPriority` decides between jobs that rank
//!   equally, and the same order is what the scheduler dispatches in when capacity frees.
//!
//! The rows of each table differ along what the rule generalises over: the published maximum,
//! the vCPUs one job reserves, how many jobs compete, the queues' priorities, which share has
//! used capacity, the weights the policy gives those shares, and the jobs' own scheduling
//! priorities.

use std::time::Duration;

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::job_lifecycle::{
    describe_compute_environment, describe_job, job_definition, until_status, Batch,
};

/// How many times a loop looks for a condition before giving up.
///
/// A job's whole life here is a handful of clock-owed moves, each falling due
/// `state::JOB_PHASE_MILLIS` after the one before it, and the jobs of a case are serialised by
/// the capacity they share, so this many looks at [`poll_gap`] apart is many spans of headroom
/// — two minutes of them, which a host running every other suite of this workspace beside this
/// one still settles well inside — and every loop ends on the CONDITION, never on a deadline of
/// its own.
const POLLS: usize = 2_400;

/// How long one look waits before the next: well below one span of the clock, so a condition
/// that holds for only one span is still observed several times on a machine whose other work
/// keeps interrupting this one.
fn poll_gap() -> Duration {
    Duration::from_millis(50)
}

/// The statuses of a job that is LIVE past its queue — admitted onto capacity and not finished
/// with it, which is exactly when it is holding the vCPUs it reserved. Source: the `JobStatus`
/// enum (`smithy_query(query="shape", shape="JobStatus")`): SUBMITTED, PENDING, RUNNABLE,
/// STARTING, RUNNING, SUCCEEDED, FAILED.
const LIVE_PAST_THE_QUEUE: [&str; 2] = [state::JOB_STARTING, state::JOB_RUNNING];

/// The terminal pair of that enum: a job no later call can move again, holding nothing.
const TERMINAL: [&str; 2] = [state::JOB_SUCCEEDED, state::JOB_FAILED];

/// A `MANAGED` Fargate compute environment called `name` publishing `max_vcpus` as its
/// maximum, created in `ce_state` and polled until it reports `VALID`; answers its ARN.
///
/// The crate's shared helper (`array_jobs_and_placement::managed_compute_environment`) fixes
/// the maximum at sixteen, and the maximum is what every case here varies, so the create is
/// made from this suite with the two required members of `ComputeResource` (`type`,
/// `maxvCpus`) plus the networking a Fargate environment needs.
async fn compute_environment(
    batch: &Batch,
    name: &str,
    max_vcpus: i32,
    ce_state: &str,
) -> String {
    let body = batch
        .ok(
            "/v1/createcomputeenvironment",
            json!({
                "computeEnvironmentName": name,
                "type": "MANAGED",
                "state": ce_state,
                "computeResources": {
                    "type": "FARGATE",
                    "maxvCpus": max_vcpus,
                    "subnets": ["subnet-1111"],
                    "securityGroupIds": ["sg-1111"],
                },
            }),
        )
        .await;
    let arn = body["computeEnvironmentArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case create {name}: expected an ARN, got {body}"))
        .to_string();
    for _ in 0..POLLS {
        let described = describe_compute_environment(batch, name).await;
        if described["status"].as_str() == Some(state::STATUS_VALID) {
            return arn;
        }
        tokio::time::sleep(poll_gap()).await;
    }
    panic!("case {name}: the compute environment never reached VALID");
}

/// Put `environment` into `ENABLED`, which is what publishes its capacity to the jobs waiting
/// on the queues that order it.
async fn enable(batch: &Batch, environment: &str) {
    batch
        .ok(
            "/v1/updatecomputeenvironment",
            json!({"computeEnvironment": environment, "state": state::STATE_ENABLED}),
        )
        .await;
}

/// A queue called `name` at `priority` over `environment`, optionally bound to the scheduling
/// policy `policy_arn`; answers its ARN.
async fn job_queue(
    batch: &Batch,
    name: &str,
    priority: i32,
    environment: &str,
    policy_arn: Option<&str>,
) -> String {
    let mut body = json!({
        "jobQueueName": name,
        "priority": priority,
        "computeEnvironmentOrder": [{"order": 1, "computeEnvironment": environment}],
    });
    if let Some(policy_arn) = policy_arn {
        body["schedulingPolicyArn"] = json!(policy_arn);
    }
    let created = batch.ok("/v1/createjobqueue", body).await;
    created["jobQueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case create {name}: expected a jobQueueArn, got {created}"))
        .to_string()
}

/// A fair-share scheduling policy called `name` whose `shareDistribution` carries
/// `distribution`; answers its ARN.
async fn fairshare_policy(
    batch: &Batch,
    name: &str,
    distribution: &[(&str, f64)],
) -> String {
    let shares: Vec<Value> = distribution
        .iter()
        .map(|(share, weight)| json!({"shareIdentifier": share, "weightFactor": weight}))
        .collect();
    let created = batch
        .ok(
            "/v1/createschedulingpolicy",
            json!({
                "name": name,
                "fairsharePolicy": {"shareDistribution": shares},
            }),
        )
        .await;
    created["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case create {name}: expected an arn, got {created}"))
        .to_string()
}

/// A `container` job definition called `name` whose container reserves `vcpus` vCPUs.
///
/// `vcpus` is the string `ResourceRequirement$value` carries ("the number of vCPUs reserved
/// for the container"), so a case can reserve a whole vCPU or the fraction a Fargate job may
/// ask for.
async fn definition_reserving(batch: &Batch, name: &str, vcpus: &str) -> String {
    let (arn, _) = job_definition(
        batch,
        name,
        json!({
            "image": "busybox",
            "command": ["echo", "hi"],
            "resourceRequirements": [
                {"type": "VCPU", "value": vcpus},
                {"type": "MEMORY", "value": "512"},
            ],
        }),
    )
    .await;
    arn
}

/// One submitted job: the id a poll addresses it by and the ARN a snapshot reports.
#[derive(Debug, Clone)]
struct Submitted {
    id: String,
    arn: String,
}

/// Submit one job called `name` into `queue_arn`, optionally naming its share and overriding
/// its scheduling priority.
async fn submit(
    batch: &Batch,
    queue_arn: &str,
    definition: &str,
    name: &str,
    share: Option<&str>,
    priority: Option<i32>,
) -> Submitted {
    let mut body = json!({
        "jobName": name,
        "jobQueue": queue_arn,
        "jobDefinition": definition,
    });
    if let Some(share) = share {
        body["shareIdentifier"] = json!(share);
    }
    if let Some(priority) = priority {
        body["schedulingPriorityOverride"] = json!(priority);
    }
    let answered = batch.ok("/v1/submitjob", body).await;
    Submitted {
        id: answered["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case submit {name}: expected a jobId, got {answered}"))
            .to_string(),
        arn: answered["jobArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case submit {name}: expected a jobArn, got {answered}"))
            .to_string(),
    }
}

/// Poll `job_id` until it reports a terminal `JobStatus`, and answer its last `JobDetail`.
async fn until_terminal(batch: &Batch, case: &str, job_id: &str) -> Value {
    for _ in 0..POLLS {
        let detail = describe_job(batch, job_id).await;
        if TERMINAL.contains(&detail["status"].as_str().unwrap_or_default()) {
            return detail;
        }
        tokio::time::sleep(poll_gap()).await;
    }
    let detail = describe_job(batch, job_id).await;
    panic!("case {case}: expected the job to become terminal, got {detail}");
}

/// One row of the capacity table: what the environment publishes, what one job reserves, and
/// how many jobs compete for it.
#[derive(Debug, Clone, Copy)]
struct CapacityRow {
    case: &'static str,
    /// `ComputeResource$maxvCpus`, the maximum the environment publishes.
    max_vcpus: i64,
    /// The `VCPU` value each job's container reserves, as the member carries it.
    reservation: &'static str,
    /// The whole vCPUs that reservation costs the environment: a fraction of a vCPU is held as
    /// one, because the capacity a compute environment publishes is whole vCPUs.
    reserved: i64,
    /// How many jobs are submitted at once.
    jobs: usize,
}

/// The rows: a maximum that admits one job, one that admits two, jobs reserving more than one
/// vCPU each, a reservation smaller than a whole vCPU, and a maximum no job count here can
/// reach.
const CAPACITY_ROWS: &[CapacityRow] = &[
    CapacityRow {
        case: "one vCPU published, three jobs reserving one each",
        max_vcpus: 1,
        reservation: "1",
        reserved: 1,
        jobs: 3,
    },
    CapacityRow {
        case: "two vCPUs published, four jobs reserving one each",
        max_vcpus: 2,
        reservation: "1",
        reserved: 1,
        jobs: 4,
    },
    CapacityRow {
        case: "two vCPUs published, three jobs reserving two each",
        max_vcpus: 2,
        reservation: "2",
        reserved: 2,
        jobs: 3,
    },
    CapacityRow {
        case: "two vCPUs published, three jobs reserving a quarter of one each",
        max_vcpus: 2,
        reservation: "0.25",
        reserved: 1,
        jobs: 3,
    },
    CapacityRow {
        case: "four vCPUs published, three jobs reserving one each",
        max_vcpus: 4,
        reservation: "1",
        reserved: 1,
        jobs: 3,
    },
];

/// RULE: the vCPUs reserved by the jobs placed on one compute environment never exceed the
/// maximum it publishes at any observed instant; a job that does not fit waits where a job
/// waits to be placed and runs once a job holding capacity is done, so every job submitted
/// eventually reaches a terminal status having run.
///
/// The rows are driven by two tests rather than one, so the cases — whose jobs the cap
/// SERIALISES, and which therefore take a span each — run beside each other rather than one
/// after the other.
#[tokio::test]
async fn the_published_maximum_caps_what_one_vcpu_jobs_hold() {
    assert_the_maximum_caps_what_is_held(&CAPACITY_ROWS[..2], "a").await;
}

/// The other half of the table above: a reservation of more than one vCPU, a reservation
/// smaller than one, and a maximum no job count here can reach.
#[tokio::test]
async fn the_published_maximum_caps_what_other_reservations_hold() {
    assert_the_maximum_caps_what_is_held(&CAPACITY_ROWS[2..], "b").await;
}

/// Drive `rows` through the rule above, naming this run's resources after `group` so two
/// tests of the same table cannot collide.
async fn assert_the_maximum_caps_what_is_held(rows: &[CapacityRow], group: &str) {
    for (position, row) in rows.iter().enumerate() {
        let index = format!("{group}{position}");
        let case = row.case;
        let batch = Batch::new();
        let environment = compute_environment(
            &batch,
            &format!("ce-cap{index}"),
            i32::try_from(row.max_vcpus).unwrap_or(1),
            state::STATE_ENABLED,
        )
        .await;
        let queue_arn = job_queue(&batch, &format!("q-cap{index}"), 1, &environment, None).await;
        let definition =
            definition_reserving(&batch, &format!("jd-cap{index}"), row.reservation).await;

        let mut submitted: Vec<Submitted> = Vec::with_capacity(row.jobs);
        for position in 0..row.jobs {
            submitted.push(
                submit(
                    &batch,
                    &queue_arn,
                    &definition,
                    &format!("cap{index}-{position}"),
                    None,
                    None,
                )
                .await,
            );
        }

        // What is asserted at EVERY look is the invariant, from both sides: the jobs live past
        // the queue are holding what they reserved, and the service's own accounting of what is
        // held on the environment agrees — so a job admitted but not yet reported past the
        // queue (one whose task is owed to Amazon ECS) counts too. Neither may exceed what the
        // environment published. The loop ends on the condition — every job terminal — and
        // never on a deadline of its own.
        let mut witnessed = 0_i64;
        let mut statuses: Vec<String> = Vec::new();
        for _ in 0..POLLS {
            statuses.clear();
            for job in &submitted {
                let detail = describe_job(&batch, &job.id).await;
                statuses.push(detail["status"].as_str().unwrap_or_default().to_string());
            }
            let live = statuses
                .iter()
                .filter(|status| LIVE_PAST_THE_QUEUE.contains(&status.as_str()))
                .count();
            let held = i64::try_from(live).unwrap_or_default() * row.reserved;
            assert!(
                held <= row.max_vcpus,
                "case {case}: expected at most {} vCPUs held at once, got {held} with \
                 {statuses:?}",
                row.max_vcpus
            );
            let accounted = {
                let store = batch.store();
                let store = store.lock();
                i64::from(store.vcpus_held_on(&environment))
            };
            assert!(
                accounted <= row.max_vcpus,
                "case {case}: expected the environment to hold at most {} vCPUs, it accounts \
                 for {accounted} with {statuses:?}",
                row.max_vcpus
            );
            witnessed = witnessed.max(held.max(accounted));
            if statuses
                .iter()
                .all(|status| TERMINAL.contains(&status.as_str()))
            {
                break;
            }
            tokio::time::sleep(poll_gap()).await;
        }

        assert!(
            witnessed > 0,
            "case {case}: no job was ever observed holding the capacity it reserved, last \
             {statuses:?}"
        );
        for (position, job) in submitted.iter().enumerate() {
            let detail = until_terminal(&batch, case, &job.id).await;
            assert_eq!(
                detail["status"].as_str(),
                Some(state::JOB_SUCCEEDED),
                "case {case}: job {position} is admitted in its turn and runs, got {detail}"
            );
            assert!(
                detail["startedAt"].as_i64().unwrap_or_default() > 0,
                "case {case}: job {position} ran, so it reports when it started, got {detail}"
            );
        }
    }
}

/// One row of the cross-queue table: the two queues' priorities and which of the two jobs the
/// freed capacity must go to.
#[derive(Debug, Clone, Copy)]
struct QueuePriorityRow {
    case: &'static str,
    /// `CreateJobQueue$priority` of the queue the FIRST job is submitted to, and of the queue
    /// the second is submitted to.
    priorities: (i32, i32),
    /// Which of the two submitted jobs is dispatched first: 0 is the one submitted first.
    dispatched_first: usize,
}

/// The rows: the higher priority on the second queue and on the first — so submission order
/// cannot be what decides — and two queues of EQUAL priority, where nothing separates them and
/// the job that was submitted first goes first.
const QUEUE_PRIORITY_ROWS: &[QueuePriorityRow] = &[
    QueuePriorityRow {
        case: "the queue submitted to second has the higher priority",
        priorities: (1, 100),
        dispatched_first: 1,
    },
    QueuePriorityRow {
        case: "the queue submitted to first has the higher priority",
        priorities: (100, 1),
        dispatched_first: 0,
    },
    QueuePriorityRow {
        case: "equal priorities: the job submitted first",
        priorities: (5, 5),
        dispatched_first: 0,
    },
];

/// RULE: queues that order the SAME compute environment are evaluated in descending
/// `priority`, so the capacity that environment publishes goes to the job on the
/// highest-priority queue waiting for it, and the job on the lower-priority queue runs after
/// it.
#[tokio::test]
async fn queue_priority_decides_which_queue_the_freed_capacity_goes_to() {
    for (index, row) in QUEUE_PRIORITY_ROWS.iter().enumerate() {
        let case = row.case;
        let batch = Batch::new();
        // The environment is created DISABLED — Batch "doesn't place jobs" on one — so both
        // jobs are waiting in their queues before any capacity exists, and the call that
        // publishes it is what makes both placements due at ONE instant. The maximum admits
        // one of them.
        let environment = compute_environment(
            &batch,
            &format!("ce-prio{index}"),
            1,
            state::STATE_DISABLED,
        )
        .await;
        let first_queue = job_queue(
            &batch,
            &format!("q-prio{index}-a"),
            row.priorities.0,
            &environment,
            None,
        )
        .await;
        let second_queue = job_queue(
            &batch,
            &format!("q-prio{index}-b"),
            row.priorities.1,
            &environment,
            None,
        )
        .await;
        let definition = definition_reserving(&batch, &format!("jd-prio{index}"), "1").await;

        let submitted = vec![
            submit(
                &batch,
                &first_queue,
                &definition,
                &format!("prio{index}-a"),
                None,
                None,
            )
            .await,
            submit(
                &batch,
                &second_queue,
                &definition,
                &format!("prio{index}-b"),
                None,
                None,
            )
            .await,
        ];
        for job in &submitted {
            until_status(&batch, case, &job.id, state::JOB_RUNNABLE).await;
        }

        enable(&batch, &environment).await;

        let winner = &submitted[row.dispatched_first];
        let loser = &submitted[1 - row.dispatched_first];
        let won = until_terminal(&batch, case, &winner.id).await;
        let lost = until_terminal(&batch, case, &loser.id).await;
        for (label, detail) in [("the first dispatched", &won), ("the second", &lost)] {
            assert_eq!(
                detail["status"].as_str(),
                Some(state::JOB_SUCCEEDED),
                "case {case}: {label} job runs once it is admitted, got {detail}"
            );
        }
        let won_started = won["startedAt"].as_i64().unwrap_or_default();
        let lost_started = lost["startedAt"].as_i64().unwrap_or_default();
        assert!(
            won_started > 0 && lost_started > won_started,
            "case {case}: expected the job on the higher-priority queue to start first, got \
             {won} then {lost}"
        );
        // The maximum admits one at a time, so the second job started only once the first had
        // given the capacity back.
        assert!(
            won["stoppedAt"].as_i64().unwrap_or_default() <= lost_started,
            "case {case}: expected the second job to start after the first stopped, got {won} \
             then {lost}"
        );
    }
}

/// One row of the fair-share snapshot table.
struct ShareOrderRow {
    case: &'static str,
    /// Whether the queue carries the fair-share policy at all.
    fair_share: bool,
    /// The policy's `shareDistribution`: one `(shareIdentifier, weightFactor)` per entry.
    distribution: &'static [(&'static str, f64)],
    /// The shares whose jobs have already been given the queue's capacity, one job of one vCPU
    /// each.
    used: &'static [&'static str],
    /// The jobs waiting in the queue, in submission order: the share each names and its
    /// `schedulingPriority` override.
    waiting: &'static [(&'static str, Option<i32>)],
    /// The positions in `waiting`, in the order the queue must dispatch them.
    dispatched: &'static [usize],
}

/// The rows: usage alone deciding (with the policy's own weights and with the default weight
/// the member documents for a share the policy does not name), equal usage decided by the
/// weight, a weight an asterisk entry gives a whole family of shares, equal usage and weight
/// decided by the job's own scheduling priority, and a queue with no policy at all, where
/// nothing of the shares matters.
const SHARE_ORDER_ROWS: &[ShareOrderRow] = &[
    ShareOrderRow {
        case: "the share that has used capacity ranks behind the one that has not",
        fair_share: true,
        distribution: &[("busy", 10.0), ("idle", 0.1)],
        used: &["busy"],
        waiting: &[("busy", None), ("idle", None)],
        dispatched: &[1, 0],
    },
    ShareOrderRow {
        case: "a share the policy does not name is weighted at the documented default",
        fair_share: true,
        distribution: &[],
        used: &["alpha"],
        waiting: &[("alpha", None), ("beta", None)],
        dispatched: &[1, 0],
    },
    ShareOrderRow {
        case: "equal usage: the lower weight factor ranks first",
        fair_share: true,
        distribution: &[("busy", 10.0), ("idle", 0.1)],
        used: &["busy", "idle"],
        waiting: &[("busy", None), ("idle", None)],
        dispatched: &[1, 0],
    },
    ShareOrderRow {
        case: "equal usage, the weights swapped: the other share ranks first",
        fair_share: true,
        distribution: &[("busy", 0.1), ("idle", 10.0)],
        used: &["busy", "idle"],
        waiting: &[("busy", None), ("idle", None)],
        dispatched: &[0, 1],
    },
    ShareOrderRow {
        case: "an asterisk entry gives its weight to every share that starts with it",
        fair_share: true,
        distribution: &[("team*", 10.0), ("solo", 1.0)],
        used: &["teamA", "solo"],
        waiting: &[("teamA", None), ("solo", None)],
        dispatched: &[1, 0],
    },
    ShareOrderRow {
        case: "equal usage and equal weights: the higher scheduling priority first",
        fair_share: true,
        distribution: &[("x", 1.0), ("y", 1.0)],
        used: &["x", "y"],
        waiting: &[("x", Some(1)), ("y", Some(9))],
        dispatched: &[1, 0],
    },
    ShareOrderRow {
        case: "no scheduling policy: submission order, whatever each share has used",
        fair_share: false,
        distribution: &[],
        used: &["busy"],
        waiting: &[("busy", None), ("idle", None)],
        dispatched: &[0, 1],
    },
];

/// RULE: a fair-share queue's `RUNNABLE` jobs are ordered by what each job's share has USED,
/// weighted by that share's `weightFactor` — lower first, so an idle share is ahead of a busy
/// one and a lower weight is ahead of a higher one on equal usage — with the job's own
/// `schedulingPriority` deciding between jobs that rank equally; `GetJobQueueSnapshot` reports
/// that order, and a queue with no policy attached reports submission order.
#[tokio::test]
async fn the_snapshot_orders_a_fair_share_queue_by_weighted_share_usage() {
    for (index, row) in SHARE_ORDER_ROWS.iter().enumerate() {
        let case = row.case;
        let batch = Batch::new();
        // DISABLED capacity places nothing, so every job submitted below waits in the queue
        // and the order the snapshot reports is the order the queue would dispatch in.
        let environment = compute_environment(
            &batch,
            &format!("ce-share{index}"),
            1,
            state::STATE_DISABLED,
        )
        .await;
        let policy_arn = if row.fair_share {
            Some(fairshare_policy(&batch, &format!("sp-share{index}"), row.distribution).await)
        } else {
            None
        };
        let queue_arn = job_queue(
            &batch,
            &format!("q-share{index}"),
            1,
            &environment,
            policy_arn.as_deref(),
        )
        .await;
        let definition = definition_reserving(&batch, &format!("jd-share{index}"), "1").await;

        // The usage each share has already been given: one job per share, which this case
        // STATES is running rather than racing the clock for it — what the case is about is
        // the ordering, and the hold is stated the same way the consumable-resource suite
        // states one.
        for (position, share) in row.used.iter().enumerate() {
            let holder = submit(
                &batch,
                &queue_arn,
                &definition,
                &format!("share{index}-held-{position}"),
                Some(share),
                None,
            )
            .await;
            until_status(&batch, case, &holder.id, state::JOB_RUNNABLE).await;
            {
                let store = batch.store();
                let mut store = store.lock();
                let job = store
                    .jobs
                    .get_mut(&holder.id)
                    .unwrap_or_else(|| panic!("case {case}: the submitted job is stored"));
                job.status = state::JOB_RUNNING.to_string();
                job.started_at = Some(state::now_millis());
                job.next_phase_due_at = None;
            }
        }

        let mut submitted: Vec<Submitted> = Vec::with_capacity(row.waiting.len());
        for (position, (share, priority)) in row.waiting.iter().enumerate() {
            submitted.push(
                submit(
                    &batch,
                    &queue_arn,
                    &definition,
                    &format!("share{index}-{position}"),
                    Some(share),
                    *priority,
                )
                .await,
            );
        }
        for job in &submitted {
            until_status(&batch, case, &job.id, state::JOB_RUNNABLE).await;
        }

        let answered = batch
            .ok("/v1/getjobqueuesnapshot", json!({"jobQueue": &queue_arn}))
            .await;
        let listed: Vec<String> = answered["frontOfQueue"]["jobs"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|job| job["jobArn"].as_str().unwrap_or_default().to_string())
            .collect();
        let expected: Vec<String> = row
            .dispatched
            .iter()
            .map(|position| submitted[*position].arn.clone())
            .collect();
        assert_eq!(
            listed, expected,
            "case {case}: expected the queue to be ordered by weighted share usage, got \
             {answered}"
        );
        // A read changes nothing: the same order again.
        let again = batch
            .ok("/v1/getjobqueuesnapshot", json!({"jobQueue": &queue_arn}))
            .await;
        assert_eq!(
            again["frontOfQueue"]["jobs"], answered["frontOfQueue"]["jobs"],
            "case {case}: a second snapshot must report the same order, got {again}"
        );
    }
}

/// One row of the fair-share dispatch table: which shares have used the queue's capacity, what
/// the policy weighs them at, and which of the two waiting jobs the freed capacity goes to.
#[derive(Debug, Clone, Copy)]
struct ShareDispatchRow {
    case: &'static str,
    /// The policy's `shareDistribution`.
    distribution: &'static [(&'static str, f64)],
    /// The shares that have already had one vCPU of the queue's capacity.
    used: &'static [&'static str],
    /// The share whose job the capacity must go to first.
    dispatched_first: &'static str,
}

/// The rows: the issue's own case (the busy share weighted above the idle one), equal usage
/// where the lower weight wins, and the same with the weights swapped — so the share's name and
/// the submission order cannot be what decides.
const SHARE_DISPATCH_ROWS: &[ShareDispatchRow] = &[
    ShareDispatchRow {
        case: "the idle share is dispatched ahead of the busy one",
        distribution: &[("busy", 10.0), ("idle", 0.1)],
        used: &["busy"],
        dispatched_first: "idle",
    },
    ShareDispatchRow {
        case: "equal usage: the share weighted lower is dispatched first",
        distribution: &[("busy", 10.0), ("idle", 0.1)],
        used: &["busy", "idle"],
        dispatched_first: "idle",
    },
    ShareDispatchRow {
        case: "equal usage, the weights swapped: the other share is dispatched first",
        distribution: &[("busy", 0.1), ("idle", 10.0)],
        used: &["busy", "idle"],
        dispatched_first: "busy",
    },
];

/// RULE: the order a fair-share queue reports is the order it DISPATCHES in — when the capacity
/// its environment publishes frees up, the job whose share has used the least of it, weighted
/// by that share's `weightFactor`, is the one admitted onto it.
#[tokio::test]
async fn a_fair_share_queue_dispatches_the_share_that_has_used_the_least_first() {
    for (index, row) in SHARE_DISPATCH_ROWS.iter().enumerate() {
        let case = row.case;
        let batch = Batch::new();
        let environment = compute_environment(
            &batch,
            &format!("ce-fss{index}"),
            1,
            state::STATE_DISABLED,
        )
        .await;
        let policy_arn =
            fairshare_policy(&batch, &format!("sp-fss{index}"), row.distribution).await;
        let queue_arn = job_queue(
            &batch,
            &format!("q-fss{index}"),
            1,
            &environment,
            Some(&policy_arn),
        )
        .await;
        let definition = definition_reserving(&batch, &format!("jd-fss{index}"), "1").await;

        // The usage each share has already had: one job per share, stated as running and then
        // ENDED through the API, so what the shares carry when the capacity is published is
        // the usage of jobs that really held it.
        for (position, share) in row.used.iter().enumerate() {
            let holder = submit(
                &batch,
                &queue_arn,
                &definition,
                &format!("fss{index}-held-{position}"),
                Some(share),
                None,
            )
            .await;
            until_status(&batch, case, &holder.id, state::JOB_RUNNABLE).await;
            {
                let store = batch.store();
                let mut store = store.lock();
                let job = store
                    .jobs
                    .get_mut(&holder.id)
                    .unwrap_or_else(|| panic!("case {case}: the submitted job is stored"));
                job.status = state::JOB_RUNNING.to_string();
                job.started_at = Some(state::now_millis());
                job.next_phase_due_at = None;
            }
            batch
                .ok(
                    "/v1/terminatejob",
                    json!({"jobId": &holder.id, "reason": "the share has had its turn"}),
                )
                .await;
        }

        // Both contenders are waiting before any capacity exists, so the call that publishes it
        // makes both placements due at ONE instant and the queue's own order is what decides.
        // "busy" is submitted first every time, so submission order can never be what the
        // assertion below observes.
        let busy = submit(
            &batch,
            &queue_arn,
            &definition,
            &format!("fss{index}-busy"),
            Some("busy"),
            Some(7),
        )
        .await;
        let idle = submit(
            &batch,
            &queue_arn,
            &definition,
            &format!("fss{index}-idle"),
            Some("idle"),
            Some(7),
        )
        .await;
        for job in [&busy, &idle] {
            until_status(&batch, case, &job.id, state::JOB_RUNNABLE).await;
        }

        enable(&batch, &environment).await;

        let (winner, loser) = if row.dispatched_first == "idle" {
            (&idle, &busy)
        } else {
            (&busy, &idle)
        };
        let won = until_terminal(&batch, case, &winner.id).await;
        let lost = until_terminal(&batch, case, &loser.id).await;
        let won_started = won["startedAt"].as_i64().unwrap_or_default();
        let lost_started = lost["startedAt"].as_i64().unwrap_or_default();
        assert_eq!(
            (won["status"].as_str(), lost["status"].as_str()),
            (Some(state::JOB_SUCCEEDED), Some(state::JOB_SUCCEEDED)),
            "case {case}: both jobs are admitted in turn and run, got {won} then {lost}"
        );
        assert!(
            won_started > 0 && lost_started > won_started,
            "case {case}: expected the {} share to be dispatched first, got {won} then {lost}",
            row.dispatched_first
        );
    }
}
