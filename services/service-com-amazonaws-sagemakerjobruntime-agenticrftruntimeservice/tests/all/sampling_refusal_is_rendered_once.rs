// SPDX-License-Identifier: CC0-1.0

//! WHICH modelled error a refused sampling call is reported as is decided in ONE
//! place.
//!
//! RULE: the shared sampling path answers a call it will not serve with a
//! `state::SamplingRefusal` that carries both the decision and the sentence —
//! whether a quota was reached (`SamplingRefusal::is_quota`) and what to report
//! (`SamplingRefusal::into_message`) — and each sampling operation renders that
//! one refusal through its OWN two modelled error constructors
//! (`SamplingRefusal::into_modelled_error`): a quota as its
//! `ServiceQuotaExceededException`, anything else as its `ValidationException`.
//! So for every class of refusal the wire answer follows from the shared
//! decision alone — the same status, the same error code and the same sentence
//! on `/sample` and on `/sample-with-response-stream` — and neither operation
//! can come to report a class differently from the other.
//!
//! The rows span the classes of refusal the shared path can answer with, which is
//! what the rule generalises over: a request that carries no inference payload, a
//! `JobArn` that resolves to no created job, a trajectory `CompleteRollout`
//! sealed (with either terminal status), a payload larger than one captured turn
//! may retain, the per-trajectory turn bound, and the store-wide trajectory
//! bound — four of them invalid and two of them quotas, so the two arms of the
//! shared decision are both driven. Each row is asked of the shared path
//! directly, and then driven over the wire through BOTH operations.
//!
//! Every wire answer goes through a [`MultiServiceRouter`], and the store is read
//! afterwards the way the NEXT request would read it.

use std::time::SystemTime;

use serde_json::json;
use service_com_amazonaws_sagemakerjobruntime_agenticrftruntimeservice::state;
use service_com_amazonaws_sagemakerjobruntime_agenticrftruntimeservice::ComAmazonawsSagemakerjobruntimeAgenticrftruntimeserviceState as ServiceState;
use smithy_server_runtime::MultiServiceRouter;

// The shared harness: the wire-answer reduction and the job-directory setup every suite
// of this crate drives (`tests/all/harness.rs`).
use crate::harness::{
    create_job_at_arn as create_job, job_arn, mounted_service, send, Reply, JOB_ARN_HEADER,
    SAMPLE_PATH, SAMPLE_STREAM_PATH, TRAJECTORY_ID_HEADER,
};

/// What a refusal the shared path calls a quota is answered with, and what every
/// other refusal is answered with.
///
/// Source: the errors both sampling operations bind
/// (`smithy_query(query="operation",operation="Sample,SampleWithResponseStream")`
/// — `ServiceQuotaExceededException` at `httpError` 402 and `ValidationException`
/// at `httpError` 400 on each of them).
const QUOTA_ANSWER: (u16, &str) = (402, "ServiceQuotaExceededException");
const INVALID_ANSWER: (u16, &str) = (400, "ValidationException");

/// The state a row puts the emulator in before the sampling call under test.
enum Setup {
    /// The job the call names was never created.
    NoJob,
    /// The job exists and the trajectory holds no turn yet.
    JobOnly,
    /// The job exists and the trajectory is sealed with this terminal status.
    Sealed(state::CompletionStatus),
    /// The job exists and the trajectory already holds the per-trajectory turn
    /// bound.
    AtTurnBound,
    /// The job exists and the store already holds the trajectory bound, under
    /// ids other than the one the row addresses.
    AtTrajectoryBound,
}

/// One row: a state, a payload, the class of refusal the shared path owes it, and
/// the sentence it is reported with.
struct Case {
    case: &'static str,
    setup: Setup,
    job_arn: String,
    trajectory_id: &'static str,
    payload: Vec<u8>,
    /// Whether this class of refusal is a quota being reached, rather than a
    /// request that is invalid or conflicts with the trajectory's state.
    quota: bool,
    message: String,
}

/// An OpenAI-compatible chat-completions request body.
fn chat_payload(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "messages": [{ "role": "user", "content": text }] }))
        .expect("a serializable payload")
}

fn cases() -> Vec<Case> {
    let created = job_arn("aws", "us-east-1", "123456789012", "AgentRFT", "mapped-run");
    let missing = job_arn(
        "aws",
        "eu-central-1",
        "210987654321",
        "AgentRFTEvaluation",
        "never-created",
    );
    vec![
        Case {
            case: "a request carrying no inference payload",
            setup: Setup::JobOnly,
            job_arn: created.clone(),
            trajectory_id: "traj-empty",
            payload: Vec::new(),
            quota: false,
            message: state::EMPTY_BODY_MESSAGE.to_string(),
        },
        Case {
            case: "a JobArn no create ever minted",
            setup: Setup::NoJob,
            job_arn: missing.clone(),
            trajectory_id: "traj-no-job",
            payload: chat_payload("route me somewhere"),
            quota: false,
            message: state::job_not_found_message(&missing),
        },
        Case {
            case: "a trajectory sealed as ready",
            setup: Setup::Sealed(state::CompletionStatus::Ready),
            job_arn: created.clone(),
            trajectory_id: "traj-sealed-ready",
            payload: chat_payload("one more turn"),
            quota: false,
            message: state::AppendRefusal::Sealed {
                status: state::TrajectoryStatus::Complete,
                completion_status: Some(state::CompletionStatus::Ready),
            }
            .message("traj-sealed-ready"),
        },
        Case {
            case: "a trajectory sealed as failed, addressed with a non-JSON payload",
            setup: Setup::Sealed(state::CompletionStatus::Failed),
            job_arn: created.clone(),
            trajectory_id: "traj-sealed-failed",
            payload: b"not JSON at all".to_vec(),
            quota: false,
            message: state::AppendRefusal::Sealed {
                status: state::TrajectoryStatus::Complete,
                completion_status: Some(state::CompletionStatus::Failed),
            }
            .message("traj-sealed-failed"),
        },
        Case {
            case: "a payload larger than one captured turn may retain",
            setup: Setup::JobOnly,
            job_arn: created.clone(),
            trajectory_id: "traj-too-large",
            payload: vec![b'x'; state::MAX_TURN_BODY_BYTES + 1],
            quota: false,
            message: state::AppendRefusal::BodyTooLarge {
                bytes: state::MAX_TURN_BODY_BYTES + 1,
            }
            .message("traj-too-large"),
        },
        Case {
            case: "a trajectory already holding the turn bound",
            setup: Setup::AtTurnBound,
            job_arn: created.clone(),
            trajectory_id: "traj-full",
            payload: chat_payload("one more turn"),
            quota: true,
            message: state::AppendRefusal::TurnQuota.message("traj-full"),
        },
        Case {
            case: "a store already holding the trajectory bound",
            setup: Setup::AtTrajectoryBound,
            job_arn: created,
            trajectory_id: "traj-one-too-many",
            payload: chat_payload("a rollout that does not fit"),
            quota: true,
            message: state::AppendRefusal::TrajectoryQuota.message("traj-one-too-many"),
        },
    ]
}

/// One tiny captured turn, for seeding a trajectory that has reached a bound.
fn seed_turn(index: usize, now: SystemTime) -> state::Turn {
    state::Turn {
        request_body: b"{}".to_vec(),
        request_content_type: Some(state::JSON_CONTENT_TYPE.to_string()),
        response_body: b"{}".to_vec(),
        response_content_type: Some(state::JSON_CONTENT_TYPE.to_string()),
        streamed: false,
        index,
        at: now,
    }
}

/// Put the emulator in the row's state. Seeded directly into the store rather
/// than driven through a thousand requests, so a row about a bound costs one
/// insert.
fn apply_setup(engine: &engine::InMemoryEngine, case: &Case) {
    if !matches!(case.setup, Setup::NoJob) {
        create_job(engine, &case.job_arn);
    }
    let store = engine.state::<ServiceState>();
    let now = SystemTime::now();
    match case.setup {
        Setup::NoJob | Setup::JobOnly => {}
        Setup::Sealed(completion_status) => {
            let mut trajectory = state::Trajectory::new(&case.job_arn, case.trajectory_id, now);
            trajectory.turns.push(seed_turn(0, now));
            trajectory.status = state::TrajectoryStatus::Complete;
            trajectory.completion_status = Some(completion_status);
            store.lock().trajectories.insert(
                state::trajectory_key(&case.job_arn, case.trajectory_id),
                trajectory,
            );
        }
        Setup::AtTurnBound => {
            let mut trajectory = state::Trajectory::new(&case.job_arn, case.trajectory_id, now);
            trajectory.turns = (0..state::MAX_TURNS_PER_TRAJECTORY)
                .map(|index| seed_turn(index, now))
                .collect();
            store.lock().trajectories.insert(
                state::trajectory_key(&case.job_arn, case.trajectory_id),
                trajectory,
            );
        }
        Setup::AtTrajectoryBound => {
            let mut guard = store.lock();
            for index in 0..state::MAX_TRAJECTORIES {
                let id = format!("seeded-{index}");
                guard.trajectories.insert(
                    state::trajectory_key(&case.job_arn, &id),
                    state::Trajectory::new(&case.job_arn, &id, now),
                );
            }
        }
    }
}

/// How many turns the row's trajectory holds, read the way the next request would
/// read it.
fn captured_turns(engine: &engine::InMemoryEngine, case: &Case) -> usize {
    let store = engine.state::<ServiceState>();
    let guard = store.lock();
    guard
        .trajectory(&case.job_arn, case.trajectory_id)
        .map_or(0, |trajectory| trajectory.turns.len())
}

/// What the SHARED sampling path answers the row's call with, before any
/// operation names an error for it.
fn shared_refusal(case: &Case) -> state::SamplingRefusal {
    let (engine, _router) = mounted_service();
    apply_setup(&engine, case);
    let store = state::service_state(&*engine).expect("the mounted service holds its own store");
    let headers = ::http::HeaderMap::new();
    let answer = state::serve_sampling_turn(
        &*engine,
        &store,
        &state::TurnRequest {
            job_arn: &case.job_arn,
            trajectory_id: case.trajectory_id,
            request_body: &case.payload,
            headers: &headers,
            streamed: false,
            at: SystemTime::now(),
        },
        // No row of the table is refused by a rule that measures the response
        // half, so nothing has to be rendered to reach its refusal.
        |_| (Vec::new(), state::JSON_CONTENT_TYPE.to_string()),
    );
    match answer {
        Ok(served) => panic!(
            "case {:?}: expected the shared path to refuse the call, it served {served:?}",
            case.case
        ),
        Err(refusal) => refusal,
    }
}

/// One sampling call on `path`.
async fn sample(
    router: &MultiServiceRouter,
    path: &str,
    job_arn: &str,
    trajectory_id: &str,
    payload: &[u8],
) -> Reply {
    send(
        router,
        path,
        &[
            (JOB_ARN_HEADER, job_arn),
            (TRAJECTORY_ID_HEADER, trajectory_id),
            ("content-type", "application/json"),
        ],
        payload.to_vec(),
    )
    .await
}

/// RULE: the quota-vs-invalid decision is the shared path's, and each sampling
/// operation only names its own modelled error for it — so for every class of
/// refusal the status, the error code and the sentence both operations answer
/// follow from the one `SamplingRefusal`, and a refused call captures nothing.
#[tokio::test]
async fn one_refusal_class_renders_as_each_operations_own_error() {
    for case in cases() {
        let refusal = shared_refusal(&case);
        assert_eq!(
            refusal.is_quota(),
            case.quota,
            "case {:?}: expected the shared path to decide quota = {}, got {}",
            case.case,
            case.quota,
            refusal.is_quota()
        );
        let (expected_status, expected_code) = if refusal.is_quota() {
            QUOTA_ANSWER
        } else {
            INVALID_ANSWER
        };
        let shared_message = refusal.into_message();
        assert_eq!(
            shared_message, case.message,
            "case {:?}: expected the shared sentence {:?}, got {shared_message:?}",
            case.case, case.message
        );

        for path in [SAMPLE_PATH, SAMPLE_STREAM_PATH] {
            let (engine, router) = mounted_service();
            apply_setup(&engine, &case);
            let before = captured_turns(&engine, &case);

            let reply = sample(
                &router,
                path,
                &case.job_arn,
                case.trajectory_id,
                &case.payload,
            )
            .await;

            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (expected_status, expected_code),
                "case {:?} on {path}: expected the shared decision to be rendered as \
                 {expected_status} {expected_code}, got {} {:?} / {}",
                case.case,
                reply.status,
                reply.error_type,
                reply.text()
            );
            assert_eq!(
                reply.message(),
                shared_message,
                "case {:?} on {path}: expected the shared sentence {shared_message:?}, got {:?}",
                case.case,
                reply.message()
            );
            let after = captured_turns(&engine, &case);
            assert_eq!(
                after, before,
                "case {:?} on {path}: expected a refused call to capture nothing, got {after} \
                 turns after {before}",
                case.case
            );
        }
    }
}
