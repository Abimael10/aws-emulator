// SPDX-License-Identifier: CC0-1.0

//! The EXECUTION instants a described job reports, over the wire.
//!
//! One rule, over a set of inputs: an instant a described job reports is the instant of a
//! transition the job's record says it MADE, so a job that has not made that transition
//! reports no instant for it at all — the member is absent from the document rather than
//! carrying a value nothing happened at.
//!
//! `JobDetail$startedAt` is "the Unix timestamp (in milliseconds) for when the job was
//! started. More specifically, it's when the job transitioned from the `STARTING` state to
//! the `RUNNING` state" and `JobDetail$stoppedAt` is "when the job transitioned from the
//! `RUNNING` state to a terminal state"; `DescribeServiceJobResponse$startedAt` is the same
//! member for a service job ("for when the service job was started"). Both `startedAt`
//! members are modelled `@required` AND `@clientOptional`
//! (`smithy_query(query="shape",shape="JobDetail")`,
//! `smithy_query(query="operation",operation="DescribeServiceJob")`) — Smithy's statement
//! that a server legally omits them — and `createdAt` ("specifically at the time
//! `SubmitJob` was called") is what every accepted job has, whatever has happened to it
//! since.
//!
//! The rows below differ along how FAR each job got: a job only accepted into a queue, a
//! job a placement ran to completion, a job a caller ended before it ran, and — for the
//! service-job plane — every status the model's `ServiceJobStatus` enum declares.
//!
//! A member the projection renamed or dropped would read exactly like a member the record
//! does not hold, so the last two suites below pin the other side of the same rule: every
//! member a job's record DOES carry is reported, under the name the model gives it, and the
//! hand-written schemas carry exactly the members the model declares.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::ops::types::{
    DescribeServiceJobOutput, JobDetail, SERVICE_JOB_STATUS_VALUES,
};
use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{
    compute_environment, container_properties, describe_job, job_definition, job_queue,
    until_status, Batch,
};
use crate::service_job_termination::{described, seed, Filled};

/// Every member `JobDetail` declares, in the model's own order
/// (`smithy_query(query="shape",shape="JobDetail",model_path="services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json")`).
///
/// A described job may report any subset of these and nothing else: the member names are
/// the shape's, so a reported key outside this set is a projection inventing a member.
const JOB_DETAIL_MEMBERS: &[&str] = &[
    "jobArn",
    "jobName",
    "jobId",
    "jobQueue",
    "status",
    "shareIdentifier",
    "schedulingPriority",
    "attempts",
    "statusReason",
    "createdAt",
    "retryStrategy",
    "startedAt",
    "stoppedAt",
    "dependsOn",
    "jobDefinition",
    "parameters",
    "container",
    "nodeDetails",
    "nodeProperties",
    "arrayProperties",
    "timeout",
    "tags",
    "propagateTags",
    "platformCapabilities",
    "eksProperties",
    "eksAttempts",
    "ecsProperties",
    "isCancelled",
    "isTerminated",
    "consumableResourceProperties",
];

/// Every member `DescribeServiceJobResponse` declares, in the model's own order
/// (`smithy_query(query="operation",operation="DescribeServiceJob",model_path="services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json")`).
const SERVICE_JOB_MEMBERS: &[&str] = &[
    "attempts",
    "capacityUsage",
    "createdAt",
    "isTerminated",
    "jobArn",
    "jobId",
    "jobName",
    "jobQueue",
    "latestAttempt",
    "retryStrategy",
    "scheduledAt",
    "schedulingPriority",
    "serviceRequestPayload",
    "serviceJobType",
    "shareIdentifier",
    "quotaShareName",
    "preemptionConfiguration",
    "preemptionSummary",
    "startedAt",
    "status",
    "statusReason",
    "stoppedAt",
    "tags",
    "timeoutConfig",
];

/// Whether `detail` carries `member` AT ALL — a member the document does not hold is what
/// "this has not happened" looks like on the wire, which a `null` value would not be.
fn carries(detail: &Value, member: &str) -> bool {
    detail
        .as_object()
        .map(|members| members.contains_key(member))
        .unwrap_or(false)
}

/// Assert that every member `detail` reports is one `declared` names.
fn assert_members_are_modelled(case: &str, detail: &Value, declared: &[&str]) {
    for member in detail
        .as_object()
        .map(|members| members.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        assert!(
            declared.contains(&member.as_str()),
            "case {case}: {member:?} is not a member of the described shape, got {detail}"
        );
    }
}

/// The hand-written schemas of the two described shapes carry exactly the members the model
/// declares, in the model's order and each at its own index.
///
/// Both schemas are written out rather than derived (see the notes on `JobDetail` and
/// `DescribeServiceJobOutput` in `src/ops/types/`), so they can drift from the model. This
/// pins them against the catalogues above: a member the block drops, renames, reorders or
/// re-indexes fails here rather than silently changing the wire.
#[test]
fn the_hand_written_schemas_carry_the_models_members() {
    let shapes: [(&str, &[&::aws_smithy_schema::Schema], &[&str]); 2] = [
        ("JobDetail", JobDetail::SCHEMA.members(), JOB_DETAIL_MEMBERS),
        (
            "DescribeServiceJobResponse",
            DescribeServiceJobOutput::SCHEMA.members(),
            SERVICE_JOB_MEMBERS,
        ),
    ];
    for (shape, members, declared) in shapes {
        let names: Vec<&str> = members
            .iter()
            .map(|member| member.member_name().unwrap_or("<unnamed>"))
            .collect();
        assert_eq!(
            names,
            declared.to_vec(),
            "case {shape}: the schema must carry exactly the model's members, in order"
        );
        for (position, member) in members.iter().enumerate() {
            let name = member.member_name().unwrap_or("<unnamed>");
            assert_eq!(
                member.member_index(),
                Some(position),
                "case {shape}.{name}: member_index must equal the member's position, got {:?}",
                member.member_index()
            );
        }
    }
}

/// How far the job of a row got before it was described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Accepted into a queue nothing can place it on, so it rests in `RUNNABLE`.
    Queued,
    /// Placed on capacity the service schedules onto, so its container ran to completion.
    Ran,
    /// Accepted, then ended by `TerminateJob` before any placement took it.
    EndedBeforeItRan,
}

/// One row: the phase a job reached, and the instants that phase owes.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    phase: Phase,
    /// The status the job reports once it is there.
    status: &'static str,
    /// Does the record hold a `STARTING` -> `RUNNING` transition for it?
    started: bool,
    /// Does it hold a `RUNNING` -> terminal one?
    stopped: bool,
}

/// `DescribeJobs` reports an execution instant only for the transition its job made.
#[tokio::test]
async fn a_described_job_reports_only_the_execution_instants_it_made() {
    let rows = [
        Row {
            case: "accepted into a queue with nothing to place it on",
            phase: Phase::Queued,
            status: state::JOB_RUNNABLE,
            started: false,
            stopped: false,
        },
        Row {
            case: "placed on capacity, container ran",
            phase: Phase::Ran,
            status: state::JOB_SUCCEEDED,
            started: true,
            stopped: true,
        },
        Row {
            case: "terminated before any placement took it",
            phase: Phase::EndedBeforeItRan,
            status: state::JOB_FAILED,
            started: false,
            stopped: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let batch = Batch::new();
        let environment = match row.phase {
            Phase::Ran => {
                managed_compute_environment(&batch, "ce-instants", state::STATE_ENABLED).await
            }
            _ => compute_environment(&batch, "ce-instants").await,
        };
        let queue = job_queue(&batch, "q-instants", &environment).await;
        let (definition, _) = job_definition(&batch, "jd-instants", container_properties()).await;
        let submitted = batch
            .ok(
                "/v1/submitjob",
                json!({
                    "jobName": "job-instants",
                    "jobQueue": &queue,
                    "jobDefinition": &definition,
                }),
            )
            .await;
        let job_id = submitted["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a jobId, got {submitted}"))
            .to_string();

        // The FIRST read is immediately after the submit, before any clock-owed move: the
        // only event that has happened is the acceptance, so `createdAt` is the only
        // instant the answer carries.
        let accepted = describe_job(&batch, &job_id).await;
        assert_members_are_modelled(case, &accepted, JOB_DETAIL_MEMBERS);
        assert!(
            accepted["createdAt"].as_i64().unwrap_or_default() > 0,
            "case {case}: an accepted job carries the instant it was created, got {accepted}"
        );
        for member in ["startedAt", "stoppedAt"] {
            assert!(
                !carries(&accepted, member),
                "case {case}: nothing has run, so a just-accepted job carries no {member}, \
                 got {accepted}"
            );
        }
        assert_eq!(
            accepted["attempts"].as_array().map(Vec::len),
            Some(0),
            "case {case}: a job no placement took reports the empty attempts list, got {accepted}"
        );

        if row.phase == Phase::EndedBeforeItRan {
            let (status, body) = batch
                .call(
                    "/v1/terminatejob",
                    json!({"jobId": &job_id, "reason": "ended by the test"}),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case}: expected TerminateJob to answer 200, got {status} {body}"
            );
        }

        let detail = until_status(&batch, case, &job_id, row.status).await;
        assert_members_are_modelled(case, &detail, JOB_DETAIL_MEMBERS);
        assert_eq!(
            carries(&detail, "startedAt"),
            row.started,
            "case {case}: startedAt is reported only for a job whose container started, \
             got {detail}"
        );
        assert_eq!(
            carries(&detail, "stoppedAt"),
            row.stopped,
            "case {case}: stoppedAt is reported only for a job whose run ended, got {detail}"
        );

        // Where the instants ARE reported they are ordered as the transitions they stand
        // for: the job was created, then started, then stopped.
        let created_at = detail["createdAt"].as_i64().unwrap_or_default();
        if row.started {
            let started_at = detail["startedAt"].as_i64().unwrap_or_default();
            assert!(
                started_at >= created_at,
                "case {case}: a job starts no earlier than it was created ({created_at}), \
                 got {detail}"
            );
            if row.stopped {
                assert!(
                    detail["stoppedAt"].as_i64().unwrap_or_default() >= started_at,
                    "case {case}: a job stops no earlier than it started ({started_at}), \
                     got {detail}"
                );
            }
        }
    }
}

/// Beside the instants, the projection reports every OTHER member the job's record carries
/// under the name the model gives it — the half the rows above cannot see, because a member
/// renamed or dropped by the projection would read exactly like a member the record does not
/// hold.
///
/// The two rows differ along how much the submit states: one states every member a submit
/// can, the other only the three the operation requires.
#[tokio::test]
async fn a_described_job_reports_every_member_its_record_carries() {
    for stated in [true, false] {
        let case = if stated {
            "every optional member stated"
        } else {
            "only the required members stated"
        };
        let batch = Batch::new();
        let environment = compute_environment(&batch, "ce-members").await;
        let queue = job_queue(&batch, "q-members", &environment).await;
        // `platformCapabilities` is "the platform capabilities required by the job
        // definition" (`JobDetail$platformCapabilities`), so it is stated on the DEFINITION
        // and read back off the job that resolved to it.
        let definition = batch
            .ok(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": "jd-members",
                    "type": "container",
                    "platformCapabilities": ["EC2"],
                    "containerProperties": container_properties(),
                }),
            )
            .await["jobDefinitionArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let dependency = batch
            .ok(
                "/v1/submitjob",
                json!({
                    "jobName": "job-members-dependency",
                    "jobQueue": &queue,
                    "jobDefinition": &definition,
                }),
            )
            .await["jobId"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let mut request = json!({
            "jobName": "job-members",
            "jobQueue": &queue,
            "jobDefinition": &definition,
        });
        if stated {
            request["parameters"] = json!({"greeting": "hi"});
            request["tags"] = json!({"env": "test"});
            request["propagateTags"] = json!(true);
            // "The minimum value for the timeout is 60 seconds" (`SubmitJob$timeout`).
            request["timeout"] = json!({"attemptDurationSeconds": 120});
            request["shareIdentifier"] = json!("share-a");
            request["schedulingPriorityOverride"] = json!(7);
            request["dependsOn"] = json!([{"jobId": &dependency}]);
        }
        let job_id = batch.ok("/v1/submitjob", request).await["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a jobId"))
            .to_string();

        let detail = describe_job(&batch, &job_id).await;
        assert_members_are_modelled(case, &detail, JOB_DETAIL_MEMBERS);
        // The members every described job carries, whatever the submit stated: the ones the
        // model requires, the instant of the only event that has happened, and the
        // collections a read materialises.
        assert_eq!(
            detail["jobName"].as_str(),
            Some("job-members"),
            "case {case}: expected the submitted jobName, got {detail}"
        );
        assert_eq!(
            detail["jobId"].as_str(),
            Some(job_id.as_str()),
            "case {case}: expected the minted jobId, got {detail}"
        );
        assert_eq!(
            detail["jobQueue"].as_str(),
            Some(queue.as_str()),
            "case {case}: expected the queue's ARN, got {detail}"
        );
        assert_eq!(
            detail["jobDefinition"].as_str(),
            Some(definition.as_str()),
            "case {case}: expected the definition's ARN, got {detail}"
        );
        assert_eq!(
            detail["platformCapabilities"],
            json!(["EC2"]),
            "case {case}: expected the definition's platformCapabilities, got {detail}"
        );
        assert_eq!(
            detail["container"]["image"].as_str(),
            Some("busybox"),
            "case {case}: expected the effective container, got {detail}"
        );
        for (member, expected) in [
            ("isCancelled", json!(false)),
            ("isTerminated", json!(false)),
            ("attempts", json!([])),
            ("eksAttempts", json!([])),
        ] {
            assert_eq!(
                detail[member], expected,
                "case {case}: expected {member} to be {expected}, got {detail}"
            );
        }

        // And the members the row's submit decided, each reported exactly where it was
        // stated and absent where it was not.
        let expected: &[(&str, Value)] = if stated {
            &[
                ("parameters", json!({"greeting": "hi"})),
                ("tags", json!({"env": "test"})),
                ("propagateTags", json!(true)),
                ("timeout", json!({"attemptDurationSeconds": 120})),
                ("shareIdentifier", json!("share-a")),
                ("schedulingPriority", json!(7)),
            ]
        } else {
            // `parameters`, `dependsOn` and `tags` are materialised by the reader, so a job
            // the submit stated none for reports them present and empty
            // (`described_empty_collections.rs`); the scalars are absent.
            &[
                ("parameters", json!({})),
                ("tags", json!({})),
                ("dependsOn", json!([])),
            ]
        };
        for (member, value) in expected {
            assert_eq!(
                &detail[member], value,
                "case {case}: expected {member} to be {value}, got {detail}"
            );
        }
        if stated {
            assert_eq!(
                detail["dependsOn"][0]["jobId"].as_str(),
                Some(dependency.as_str()),
                "case {case}: expected the stated dependency, got {detail}"
            );
        } else {
            for member in [
                "propagateTags",
                "timeout",
                "shareIdentifier",
                "schedulingPriority",
                "statusReason",
            ] {
                assert!(
                    !carries(&detail, member),
                    "case {case}: the record carries no {member}, so the answer must not \
                     either, got {detail}"
                );
            }
        }
    }
}

/// `DescribeServiceJob` reports `startedAt` only for a service job whose work started —
/// one row per status the model's `ServiceJobStatus` enum declares.
#[tokio::test]
async fn a_described_service_job_reports_started_at_only_once_its_work_started() {
    let batch = Batch::new();
    assert!(
        !SERVICE_JOB_STATUS_VALUES.is_empty(),
        "case the model's statuses: expected ServiceJobStatus to declare values"
    );
    for status in SERVICE_JOB_STATUS_VALUES {
        let case = format!("seeded in {status}");
        // The seeded record carries the instants its status implies: `startedAt` only once
        // the job is running, which is the record this read projects.
        let seeded = seed(&batch, status, Filled::Everything);
        let detail = described(&batch, &case, &seeded.job_id).await;
        assert_members_are_modelled(&case, &detail, SERVICE_JOB_MEMBERS);
        assert_eq!(
            carries(&detail, "startedAt"),
            seeded.started_at.is_some(),
            "case {case}: startedAt is reported only for a job whose work started, got {detail}"
        );
        assert_eq!(
            detail["startedAt"].as_i64(),
            seeded.started_at,
            "case {case}: the reported startedAt is the stored one, got {detail}"
        );
        assert_eq!(
            carries(&detail, "stoppedAt"),
            seeded.stopped_at.is_some(),
            "case {case}: stoppedAt is reported only for a job whose work ended, got {detail}"
        );
        assert_eq!(
            detail["createdAt"].as_i64(),
            Some(seeded.created_at),
            "case {case}: every accepted service job carries the instant it was created, \
             got {detail}"
        );
    }
}
