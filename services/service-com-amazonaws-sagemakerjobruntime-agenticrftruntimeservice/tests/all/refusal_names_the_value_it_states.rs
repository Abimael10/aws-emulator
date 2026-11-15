// SPDX-License-Identifier: CC0-1.0

//! A refusal this service words ITSELF must state the value it is about.
//!
//! RULE: a service's own wording stands in for the framework's constraint clauses only
//! where it tells the caller MORE than they do — which, for the malformed-ARN sentence,
//! is the value it names — so a refused value that carries no text a bare-prose sentence
//! can show (the empty value, or one holding nothing but whitespace) is refused with the
//! framework's own clauses instead, which quote the value between `'` and state every
//! requirement it missed.
//!
//! Here that is `JobArn`: `@length` 1..256 plus
//! `^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:job/[a-zA-Z0-9_\-]+/[a-zA-Z0-9](-*[a-zA-Z0-9]){0,62}$`
//! (`smithy_query(query="shape",shape="JobArn")`), bound by every operation the model
//! declares. A value with text that misses the pattern is reported as a malformed ARN
//! naming it; the empty value — which an SDK sends as a PRESENT header holding nothing —
//! misses the `@length` minimum as well, and that clause is what a caller can act on, so
//! it is the one that reaches the wire.
//!
//! Rows differ along what the rule generalises over: whether the refused value has text to
//! name (several shapes of value that do, the empty value, and whitespace-only values of
//! three lengths over two whitespace characters) and which operation carries it (all four).
//!
//! Every answer goes over the wire through a [`MultiServiceRouter`], and the store is read
//! afterwards the way the NEXT request would read it, so what is pinned is the emulator's
//! HTTP behaviour.

use serde_json::json;
use service_com_amazonaws_sagemakerjobruntime_agenticrftruntimeservice::ComAmazonawsSagemakerjobruntimeAgenticrftruntimeserviceState as ServiceState;
use smithy_server_runtime::MultiServiceRouter;

// The shared harness: the wire-answer reduction and the job-directory setup every suite
// of this crate drives (`tests/all/harness.rs`).
use crate::harness::{
    create_job, job_arn, mounted_service, send, Reply, COMPLETE_ROLLOUT_PATH, JOB_ARN_HEADER,
    SAMPLE_PATH, SAMPLE_STREAM_PATH, TRAJECTORY_ID_HEADER, UPDATE_REWARD_PATH,
};

/// The four operations the model binds, each named by the path it is bound to
/// (`smithy_query(query="digest")`).
const OPERATIONS: [(&str, &str); 4] = [
    ("Sample", SAMPLE_PATH),
    ("SampleWithResponseStream", SAMPLE_STREAM_PATH),
    ("CompleteRollout", COMPLETE_ROLLOUT_PATH),
    ("UpdateReward", UPDATE_REWARD_PATH),
];

/// What the refusal of one row must say.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// This service's own malformed-ARN sentence, naming the value the request carried.
    ItsOwnWording,
    /// The framework's clauses, which must state each of these fragments — the member path
    /// and the requirement the value missed — and must NOT be this service's wording.
    TheFrameworkClauses(&'static [&'static str]),
}

/// One row: the `JobArn` the call carries and the wording its refusal must carry.
struct Row {
    case: &'static str,
    job_arn: String,
    expected: Expected,
}

/// The fragments the framework's clauses state for a value that is present but holds no
/// text: the member path and the ARN requirement it missed. The wording is the framework's
/// own (`Violation::message`), and the `@pattern` is the member's
/// (`smithy_query(query="shape",shape="JobArn")`).
const THE_ARN_REQUIREMENT: &[&str] = &[
    "at 'jobArn' failed to satisfy constraint: Member must satisfy regular expression pattern:",
];

/// One call of the operation bound at `path`, carrying `job_arn` in the header the model
/// binds it to. Built the way each operation's model binds its members: the sampling
/// operations carry `TrajectoryId` in a header, the rollout operations in the JSON body.
async fn call(router: &MultiServiceRouter, path: &str, job_arn: &str) -> Reply {
    let trajectory_id = "traj-1";
    let mut headers: Vec<(&str, &str)> = vec![
        (JOB_ARN_HEADER, job_arn),
        ("content-type", "application/json"),
    ];
    let body = match path {
        SAMPLE_PATH | SAMPLE_STREAM_PATH => {
            headers.push((TRAJECTORY_ID_HEADER, trajectory_id));
            json!({ "messages": [{ "role": "user", "content": "go" }] })
        }
        COMPLETE_ROLLOUT_PATH => {
            json!({ "TrajectoryId": trajectory_id, "Status": "ready", "ClientToken": "tok" })
        }
        UPDATE_REWARD_PATH => {
            json!({ "TrajectoryId": trajectory_id, "Rewards": [1.0], "ClientToken": "tok" })
        }
        other => unreachable!("this model binds no operation at {other}"),
    };
    send(
        router,
        path,
        &headers,
        serde_json::to_vec(&body).expect("a serializable body"),
    )
    .await
}

/// RULE: every operation refuses a `JobArn` outside the modelled constraints
/// `400 ValidationException`, worded as a malformed ARN naming the value only when the
/// value has text to name — and with the framework's clauses, stating the member and the
/// requirement, when it has none.
#[tokio::test]
async fn a_refusal_states_its_own_wording_only_when_it_can_name_the_value() {
    let rows = [
        Row {
            case: "a value that is not an ARN at all",
            job_arn: "not-a-job-arn".to_string(),
            expected: Expected::ItsOwnWording,
        },
        Row {
            case: "an ARN naming another service",
            job_arn: job_arn("aws", "us-east-1", "000000000000", "AgentRFT", "run-1")
                .replace(":sagemaker:", ":bedrock:"),
            expected: Expected::ItsOwnWording,
        },
        Row {
            case: "a one-character value",
            job_arn: "x".to_string(),
            expected: Expected::ItsOwnWording,
        },
        Row {
            case: "a value of punctuation alone",
            job_arn: "::::".to_string(),
            expected: Expected::ItsOwnWording,
        },
        Row {
            case: "a value whose text is surrounded by spaces, which is named verbatim",
            job_arn: " not-a-job-arn ".to_string(),
            expected: Expected::ItsOwnWording,
        },
        Row {
            case: "the empty value, which misses the length minimum too",
            job_arn: String::new(),
            expected: Expected::TheFrameworkClauses(&[
                "Value '' at 'jobArn' failed to satisfy constraint: Member must have length \
                 greater than or equal to 1",
            ]),
        },
        Row {
            case: "a value of one space",
            job_arn: " ".to_string(),
            expected: Expected::TheFrameworkClauses(THE_ARN_REQUIREMENT),
        },
        Row {
            case: "a value of three spaces",
            job_arn: "   ".to_string(),
            expected: Expected::TheFrameworkClauses(THE_ARN_REQUIREMENT),
        },
        Row {
            case: "a value of one tab",
            job_arn: "\t".to_string(),
            expected: Expected::TheFrameworkClauses(THE_ARN_REQUIREMENT),
        },
    ];

    for Row {
        case,
        job_arn,
        expected,
    } in rows
    {
        for (operation, path) in OPERATIONS {
            let (engine, router) = mounted_service();
            // A job DOES exist, under the ARN a correct request would carry, so a row is
            // refused because of its own value and not because the account holds no job.
            create_job(&engine, "AgentRFT", "run-1");

            let reply = call(&router, path, &job_arn).await;
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, "ValidationException"),
                "case {case:?}/{operation}: expected 400 ValidationException, got {} {:?} / {}",
                reply.status,
                reply.error_type,
                reply.text()
            );
            assert_eq!(
                reply.content_type.as_deref(),
                Some("application/json"),
                "case {case:?}/{operation}: expected a restJson1 error document, got {:?}",
                reply.content_type
            );

            let message = reply.message();
            match expected {
                Expected::ItsOwnWording => {
                    let wording = format!("Input arn {job_arn} is malformed");
                    assert_eq!(
                        message, wording,
                        "case {case:?}/{operation}: expected the malformed-ARN sentence \
                         {wording:?}, got {message:?}"
                    );
                }
                Expected::TheFrameworkClauses(fragments) => {
                    assert!(
                        message.contains(" validation error"),
                        "case {case:?}/{operation}: expected the framework's constraint \
                         envelope, got {message:?}"
                    );
                    for fragment in fragments {
                        assert!(
                            message.contains(fragment),
                            "case {case:?}/{operation}: expected the refusal to state \
                             {fragment:?}, got {message:?}"
                        );
                    }
                    assert!(
                        !message.contains("is malformed"),
                        "case {case:?}/{operation}: expected the framework's clauses rather than \
                         a sentence naming no value, got {message:?}"
                    );
                }
            }

            let held = engine.state::<ServiceState>().lock().trajectories.len();
            assert_eq!(
                held, 0,
                "case {case:?}/{operation}: expected a refused call to write nothing, got {held} \
                 trajectory/ies"
            );
        }
    }
}
