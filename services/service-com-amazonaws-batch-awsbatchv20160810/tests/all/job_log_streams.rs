// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch Logs log stream of a Batch job that has been PLACED, end to end.
//!
//! RULE under test: a job AWS Batch has placed as a task has a CloudWatch Logs log stream —
//! `AttemptContainerDetail$logStreamName` is "the name of the CloudWatch Logs log stream
//! that's associated with the container. The log group for Batch jobs is `/aws/batch/job`"
//! — and that stream EXISTS where CloudWatch Logs holds it, in the log group the
//! container's `awslogs` configuration delivers to ("awslogs ... specifies the Amazon
//! CloudWatch Logs logging driver", `LogConfiguration$logDriver`; the driver's
//! `awslogs-group`, `awslogs-region` and `awslogs-create-group` options name the group, its
//! Region and whether the driver creates the group too). So a client that lists, reads or
//! DELETES the stream of a job that has run finds it: `DeleteLogStream` of it answers 200
//! with an empty body and removes it, and only a stream that is really not there is refused
//! with `ResourceNotFoundException`. A container whose output goes to another log driver has
//! no CloudWatch Logs stream at all, and a job nothing placed has none either.
//!
//! The rows differ along what the rule generalises over: which log driver the job's
//! container configures, which group and Region its options name, whether the group was
//! already there, how many jobs (and array children) are placed, and whether anything
//! placed the job at all.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{
    compute_environment, container_properties, error_code, job_definition, job_queue, until_status,
    Batch, REGION,
};

/// The log group Batch delivers a job's container logs to when its configuration names no
/// other one, spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make a test pass. Source: `AttemptContainerDetail$logStreamName`
/// ("The log group for Batch jobs is `/aws/batch/job`").
const DEFAULT_LOG_GROUP: &str = "/aws/batch/job";

/// How many times a poll looks before it gives up: each clock-owed move falls due
/// `state::JOB_PHASE_MILLIS` after the one before it and the stream is created right after
/// the placement that named it, so this is many phases of headroom on a loaded machine.
const POLLS: usize = 240;

/// How long one look waits before the next, well under one clock-owed span.
fn poll_interval() -> std::time::Duration {
    let span = u64::try_from(state::JOB_PHASE_MILLIS).unwrap_or(500);
    std::time::Duration::from_millis((span / 10).max(5))
}

/// One emulator with a MANAGED compute environment behind one queue — the capacity a job
/// has to be placed on before it has a log stream at all.
struct Fixture {
    batch: Batch,
    queue_arn: String,
}

/// A fresh emulator whose queue offers the scheduler capacity (`prefix` keeps the names of
/// one test's resources apart from another's).
async fn placeable(prefix: &str) -> Fixture {
    let batch = Batch::new();
    let environment =
        managed_compute_environment(&batch, &format!("ce-{prefix}"), state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, &format!("q-{prefix}"), &environment).await;
    Fixture { batch, queue_arn }
}

/// `containerProperties` carrying `log_configuration`, if any.
fn container_with_logging(log_configuration: Option<Value>) -> Value {
    let mut properties = container_properties();
    if let Some(configuration) = log_configuration {
        properties["logConfiguration"] = configuration;
    }
    properties
}

/// Submit one job into `fixture`'s queue against a definition named `definition_name` that
/// carries `log_configuration`; answers the job id.
async fn submit(
    fixture: &Fixture,
    definition_name: &str,
    job_name: &str,
    log_configuration: Option<Value>,
) -> String {
    let (definition, _) = job_definition(
        &fixture.batch,
        definition_name,
        container_with_logging(log_configuration),
    )
    .await;
    let answered = fixture
        .batch
        .ok(
            "/v1/submitjob",
            json!({
                "jobName": job_name,
                "jobQueue": &fixture.queue_arn,
                "jobDefinition": definition,
            }),
        )
        .await;
    answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case submit {job_name}: expected a jobId, got {answered}"))
        .to_string()
}

/// The log streams CloudWatch Logs holds in `group` whose name starts with `prefix`.
///
/// A group CloudWatch Logs holds nothing for holds no such stream either, so the
/// `ResourceNotFoundException` for an absent group reads as the empty list.
async fn streams_in(batch: &Batch, group: &str, prefix: &str) -> Vec<Value> {
    let (status, body) = batch
        .logs(
            "DescribeLogStreams",
            json!({"logGroupName": group, "logStreamNamePrefix": prefix}),
        )
        .await;
    if status == 400 {
        return Vec::new();
    }
    assert_eq!(
        status, 200,
        "case describe streams of {group:?}: expected 200, got {status} {body}"
    );
    body["logStreams"].as_array().cloned().unwrap_or_default()
}

/// Poll until CloudWatch Logs holds the stream `name` in `group`, then answer its record.
async fn until_stream_exists(batch: &Batch, case: &str, group: &str, name: &str) -> Value {
    for _ in 0..POLLS {
        if let Some(found) = streams_in(batch, group, name)
            .await
            .into_iter()
            .find(|stream| stream["logStreamName"].as_str() == Some(name))
        {
            return found;
        }
        tokio::time::sleep(poll_interval()).await;
    }
    panic!(
        "case {case}: expected CloudWatch Logs to hold the stream {name:?} in {group:?}, got {:?}",
        streams_in(batch, group, "").await
    );
}

/// The one attempt of a job `DescribeJobs` reports as placed and run to its end.
async fn placed_attempt(batch: &Batch, case: &str, job_id: &str) -> Value {
    let detail = until_status(batch, case, job_id, state::JOB_SUCCEEDED).await;
    let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        attempts.len(),
        1,
        "case {case}: a placed job has had exactly one placement, got {detail}"
    );
    attempts[0].clone()
}

/// What one row of the table configures, and where its stream must then be.
struct Row {
    case: &'static str,
    /// The job definition's `logConfiguration`, or `None` for a container that configures
    /// no log driver at all (which gets the `awslogs` default).
    log_configuration: Option<fn() -> Value>,
    /// A log group the CALLER creates before the job is submitted, for a configuration that
    /// delivers into a group it does not ask the driver to create.
    pre_created_group: Option<&'static str>,
    /// The log group the stream must be in, or `None` when there must be no CloudWatch Logs
    /// stream for this container at all.
    expected_group: Option<&'static str>,
    /// The Region whose log store holds it — the job's own unless an option names another.
    expected_region: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a container that configures no log driver",
        log_configuration: None,
        pre_created_group: None,
        expected_group: Some(DEFAULT_LOG_GROUP),
        expected_region: REGION,
    },
    Row {
        case: "the awslogs driver with no options",
        log_configuration: Some(|| json!({"logDriver": "awslogs"})),
        pre_created_group: None,
        expected_group: Some(DEFAULT_LOG_GROUP),
        expected_region: REGION,
    },
    Row {
        case: "awslogs into a group the caller created",
        log_configuration: Some(|| {
            json!({"logDriver": "awslogs", "options": {"awslogs-group": "/caller/made/this"}})
        }),
        pre_created_group: Some("/caller/made/this"),
        expected_group: Some("/caller/made/this"),
        expected_region: REGION,
    },
    Row {
        case: "awslogs creating the group itself",
        log_configuration: Some(|| {
            json!({
                "logDriver": "awslogs",
                "options": {
                    "awslogs-group": "/driver/made/this",
                    "awslogs-create-group": "true",
                },
            })
        }),
        pre_created_group: None,
        expected_group: Some("/driver/made/this"),
        expected_region: REGION,
    },
    Row {
        case: "awslogs into another Region's log store",
        log_configuration: Some(|| {
            json!({"logDriver": "awslogs", "options": {"awslogs-region": "eu-west-3"}})
        }),
        pre_created_group: None,
        expected_group: Some(DEFAULT_LOG_GROUP),
        expected_region: "eu-west-3",
    },
    Row {
        case: "a driver that is not awslogs",
        log_configuration: Some(|| {
            json!({"logDriver": "splunk", "options": {"splunk-token": "t", "splunk-url": "https://splunk.invalid"}})
        }),
        pre_created_group: None,
        expected_group: None,
        expected_region: REGION,
    },
    Row {
        case: "awslogs whose group name no log group may carry",
        log_configuration: Some(|| {
            json!({"logDriver": "awslogs", "options": {"awslogs-group": "not a group name"}})
        }),
        pre_created_group: None,
        expected_group: None,
        expected_region: REGION,
    },
];

/// RULE: a placed job's log stream exists where its `awslogs` configuration delivers it, and
/// a container delivering to no CloudWatch Logs stream has none.
#[tokio::test]
async fn a_placed_job_s_log_stream_exists_where_its_driver_delivers_it() {
    let fixture = placeable("streams").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let definition_name = format!("jd-stream-{index}");
        if let Some(group) = row.pre_created_group {
            let (status, body) = fixture
                .batch
                .logs("CreateLogGroup", json!({"logGroupName": group}))
                .await;
            assert_eq!(
                status, 200,
                "case {case}: CreateLogGroup {group:?} answered {status} {body}"
            );
        }
        let job_id = submit(
            &fixture,
            &definition_name,
            &format!("j-stream-{index}"),
            row.log_configuration.map(|build| build()),
        )
        .await;
        let attempt = placed_attempt(&fixture.batch, case, &job_id).await;
        let task_arn = attempt["container"]["taskArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the attempt must name its task, got {attempt}"));
        let task_id = task_arn.rsplit('/').next().unwrap_or_default();
        // The naming the placement decides: `<job definition name>/default/<ECS task id>`.
        let wanted = format!("{definition_name}/default/{task_id}");

        let Some(group) = row.expected_group else {
            assert!(
                attempt["container"]["logStreamName"].is_null(),
                "case {case}: a container delivering to no CloudWatch Logs stream reports \
                 none, got {attempt}"
            );
            // Nothing was ever queued for this placement, so the absence is settled the
            // moment the job is placed: no group this suite touches holds a stream of it.
            for group in [DEFAULT_LOG_GROUP, "/caller/made/this", "/driver/made/this"] {
                let found = streams_in(&fixture.batch, group, &definition_name).await;
                assert!(
                    found.is_empty(),
                    "case {case}: {group:?} must hold no stream for this job, got {found:?}"
                );
            }
            continue;
        };

        assert_eq!(
            attempt["container"]["logStreamName"].as_str(),
            Some(wanted.as_str()),
            "case {case}: expected the attempt's log stream {wanted:?}, got {attempt}"
        );
        let stream = until_stream_exists(&fixture.batch, case, group, &wanted).await;
        let expected_arn = format!(
            "arn:aws:logs:{}:{}:log-group:{group}:log-stream:{wanted}",
            row.expected_region,
            engine::EMULATOR_ACCOUNT_ID
        );
        assert_eq!(
            stream["arn"].as_str(),
            Some(expected_arn.as_str()),
            "case {case}: expected the stream {expected_arn:?}, got {stream}"
        );
        if group != DEFAULT_LOG_GROUP {
            let found = streams_in(&fixture.batch, DEFAULT_LOG_GROUP, &definition_name).await;
            assert!(
                found.is_empty(),
                "case {case}: a configured group is where the stream goes, but \
                 {DEFAULT_LOG_GROUP:?} holds {found:?}"
            );
        }

        // The requirement the issue is about: the delete of a stream that is there is
        // PERFORMED — 200 with an empty body — and the stream and its events are gone.
        let (status, body) = fixture
            .batch
            .logs(
                "DeleteLogStream",
                json!({"logGroupName": group, "logStreamName": &wanted}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteLogStream of a stream that exists answered {status} {body}"
        );
        assert!(
            body.is_null() || body.as_object().is_some_and(serde_json::Map::is_empty),
            "case {case}: DeleteLogStream's output is Unit, so it carries no members, got \
             {body}"
        );
        let left = streams_in(&fixture.batch, group, &wanted).await;
        assert!(
            left.is_empty(),
            "case {case}: the deleted stream must be gone, got {left:?}"
        );
        // And only a stream that is really not there is refused.
        let (status, body) = fixture
            .batch
            .logs(
                "DeleteLogStream",
                json!({"logGroupName": group, "logStreamName": &wanted}),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: deleting it again is refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case}: expected the modelled not-found refusal, got {body}"
        );
    }
}

/// RULE: the stream belongs to the PLACEMENT — so each child of an array job placed on
/// managed capacity has its own stream in the log group, while the parent, which runs no
/// container of its own and is never placed, has none.
#[tokio::test]
async fn every_placed_array_child_has_its_own_log_stream_and_the_parent_has_none() {
    const SIZE: i32 = 3;
    let fixture = placeable("arraystreams").await;
    let definition_name = "jd-array-streams".to_string();
    let (definition, _) = job_definition(
        &fixture.batch,
        &definition_name,
        container_with_logging(None),
    )
    .await;
    let answered = fixture
        .batch
        .ok(
            "/v1/submitjob",
            json!({
                "jobName": "shards",
                "jobQueue": &fixture.queue_arn,
                "jobDefinition": definition,
                "arrayProperties": {"size": SIZE},
            }),
        )
        .await;
    let parent_id = answered["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case the array: expected a jobId, got {answered}"))
        .to_string();

    let mut streams: Vec<String> = Vec::new();
    for index in 0..SIZE {
        let case = format!("child {index}");
        let child_id = state::array_child_job_id(&parent_id, index);
        let attempt = placed_attempt(&fixture.batch, &case, &child_id).await;
        let name = attempt["container"]["logStreamName"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a log stream, got {attempt}"))
            .to_string();
        until_stream_exists(&fixture.batch, &case, DEFAULT_LOG_GROUP, &name).await;
        streams.push(name);
    }

    let mut unique = streams.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        streams.len(),
        "case the array: each child's placement has its own stream, got {streams:?}"
    );
    let held = streams_in(&fixture.batch, DEFAULT_LOG_GROUP, &definition_name).await;
    assert_eq!(
        held.len(),
        streams.len(),
        "case the array: the group holds one stream per placed child and no more, got {held:?}"
    );

    let parent = until_status(&fixture.batch, "the parent", &parent_id, state::JOB_SUCCEEDED).await;
    assert!(
        parent["attempts"].as_array().is_none_or(Vec::is_empty),
        "case the parent: an array parent is never placed, so it has no attempt, got {parent}"
    );
}

/// RULE: the stream comes with the PLACEMENT, so a job nothing placed has none — a queue
/// whose compute environment offers the scheduler no capacity leaves its job resting in
/// `RUNNABLE`, and CloudWatch Logs holds no stream for it.
#[tokio::test]
async fn a_job_nothing_placed_has_no_log_stream() {
    let batch = Batch::new();
    let environment = compute_environment(&batch, "ce-resting-stream").await;
    let queue_arn = job_queue(&batch, "q-resting-stream", &environment).await;
    let fixture = Fixture { batch, queue_arn };
    let definition_name = "jd-resting-stream".to_string();
    let job_id = submit(&fixture, &definition_name, "j-resting", None).await;

    let resting = until_status(
        &fixture.batch,
        "resting in RUNNABLE",
        &job_id,
        state::JOB_RUNNABLE,
    )
    .await;
    assert!(
        resting["attempts"].as_array().is_none_or(Vec::is_empty),
        "case resting in RUNNABLE: nothing placed this job, got {resting}"
    );
    let found = streams_in(&fixture.batch, DEFAULT_LOG_GROUP, &definition_name).await;
    assert!(
        found.is_empty(),
        "case resting in RUNNABLE: a job nothing placed has no log stream, got {found:?}"
    );
}
