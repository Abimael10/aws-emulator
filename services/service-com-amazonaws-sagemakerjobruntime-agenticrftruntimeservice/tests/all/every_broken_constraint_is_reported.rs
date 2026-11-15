// SPDX-License-Identifier: CC0-1.0

//! What a refusal of this service STATES when a request breaks several modelled
//! constraints at once.
//!
//! RULE: one refusal states every constraint the request broke — one clause per violation,
//! counted and joined into the one `Message` the model declares — so the wording this
//! service puts in place of a clause of its own class (a `JobArn` that is not an ARN)
//! replaces that CLAUSE only, never the clauses collected for the request's other members.
//!
//! Rows differ along what the rule generalises over: how many constraints the request
//! breaks (one, two, three), WHICH classes they are (the ARN `@pattern`, the `@length`
//! maximum and minimum, the `CompletionStatus` value set), which member carries them
//! (`JobArn` breaking two at once, `TrajectoryId`, `Status`), the operation the request
//! names (every one of the four binds `JobArn` and `TrajectoryId`), and whether the ARN
//! clause has a value to name at all — the one thing the surviving clauses must NOT depend
//! on.
//!
//! Sources of the constraints each row breaks:
//! `smithy_query(query="operation",operation="CompleteRollout,UpdateReward,Sample")` —
//! `JobArn` `@length` min 1 max 256 plus the job-ARN `@pattern`, `TrajectoryId` `@length`
//! min 1 max 256 — and `smithy_query(query="shape",shape="CompletionStatus")` — the value
//! set `[ready, failed]`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    mounted_service, send, Reply, COMPLETE_ROLLOUT_PATH, JOB_ARN_HEADER, SAMPLE_PATH,
    SAMPLE_STREAM_PATH, TRAJECTORY_ID_HEADER, UPDATE_REWARD_PATH,
};

/// A `JobArn` satisfying every modelled constraint, so a row that varies another member
/// alone keeps the ARN out of the refusal.
fn valid_job_arn() -> String {
    crate::harness::minted_job_arn("AgentRFT", "run-1")
}

/// The framework's wording for an ARN-`@pattern` clause: the one sentence a refusal of this
/// service must NOT state, since it reports that class as a malformed ARN instead.
const PATTERN_REQUIREMENT: &str = "Member must satisfy regular expression pattern:";

/// The clause fragments the `TrajectoryId` `@length` bounds are stated with.
const OVER_THE_MAXIMUM: &str = "Member must have length less than or equal to 256";
const UNDER_THE_MINIMUM: &str = "Member must have length greater than or equal to 1";

/// The clause the `CompletionStatus` value set is stated with.
const VALUE_SET: &str = "Member must satisfy enum value set: [ready, failed]";

/// How this service words a `JobArn` that is not an ARN.
fn malformed_arn(value: &str) -> String {
    format!("Input arn {value} is malformed")
}

/// A `CompleteRollout` call, with `status` sent only when a row names one.
async fn complete_rollout(
    router: &MultiServiceRouter,
    job: &str,
    trajectory_id: &str,
    status: Option<&str>,
) -> Reply {
    let mut body = json!({ "TrajectoryId": trajectory_id, "ClientToken": "tok" });
    if let Some(status) = status {
        body["Status"] = Value::String(status.to_string());
    }
    send(
        router,
        COMPLETE_ROLLOUT_PATH,
        &[(JOB_ARN_HEADER, job), ("content-type", "application/json")],
        serde_json::to_vec(&body).expect("a serializable body"),
    )
    .await
}

/// An `UpdateReward` call.
async fn update_reward(router: &MultiServiceRouter, job: &str, trajectory_id: &str) -> Reply {
    let body = json!({ "TrajectoryId": trajectory_id, "Rewards": [1.0], "ClientToken": "tok" });
    send(
        router,
        UPDATE_REWARD_PATH,
        &[(JOB_ARN_HEADER, job), ("content-type", "application/json")],
        serde_json::to_vec(&body).expect("a serializable body"),
    )
    .await
}

/// A sampling call on either framing, both of which bind `TrajectoryId` to a header.
async fn sample(router: &MultiServiceRouter, path: &str, job: &str, trajectory_id: &str) -> Reply {
    send(
        router,
        path,
        &[
            (JOB_ARN_HEADER, job),
            (TRAJECTORY_ID_HEADER, trajectory_id),
            ("content-type", "application/json"),
        ],
        serde_json::to_vec(&json!({ "messages": [{ "role": "user", "content": "go" }] }))
            .expect("a serializable payload"),
    )
    .await
}

/// The envelope a refusal of `clauses` clauses opens with, as the shared constraint
/// renderer counts them.
fn counted(clauses: usize) -> String {
    if clauses == 1 {
        "1 validation error detected: ".to_string()
    } else {
        format!("{clauses} validation errors detected: ")
    }
}

/// Assert one refused answer: the status, the error shape, the clause count the envelope
/// states, and every clause the request's broken constraints owe it.
fn refusal_states(case: &str, reply: &Reply, clauses: usize, stated: &[&str], absent: &[&str]) {
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {} {:?} / {}",
        reply.status,
        reply.error_type,
        reply.text()
    );
    let message = reply.message();
    if clauses > 1 {
        assert!(
            message.starts_with(&counted(clauses)),
            "case {case:?}: expected the refusal to count its {clauses} clauses as {:?}, got {:?}",
            counted(clauses),
            message
        );
    }
    for fragment in stated {
        assert!(
            message.contains(fragment),
            "case {case:?}: expected the refusal to state {fragment:?}, got {message:?}"
        );
    }
    for fragment in absent {
        assert!(
            !message.contains(fragment),
            "case {case:?}: expected the refusal NOT to state {fragment:?}, got {message:?}"
        );
    }
}

/// RULE: a refusal states a clause for EVERY modelled constraint the request broke, counted
/// and joined into one `Message`; this service's malformed-ARN wording replaces the ARN
/// `@pattern` clause's own wording and nothing else.
#[tokio::test]
async fn a_malformed_arn_does_not_silence_the_other_clauses() {
    let not_an_arn = "not-a-valid-job-arn";
    let over_the_maximum = "t".repeat(257);
    // A `JobArn` over its own `@length` maximum AND outside the pattern: one member
    // breaking two constraints at once.
    let long_arn = "z".repeat(300);
    let arn_clause = malformed_arn(not_an_arn);
    let long_arn_clause = malformed_arn(&long_arn);

    for (case, clauses, stated, reply) in [
        (
            "the ARN, the trajectory id and the status, on CompleteRollout",
            3,
            vec![arn_clause.as_str(), "at 'trajectoryId'", OVER_THE_MAXIMUM, "at 'status'", VALUE_SET],
            {
                let (_engine, router) = mounted_service();
                complete_rollout(&router, not_an_arn, &over_the_maximum, Some("bogus-status")).await
            },
        ),
        (
            "the ARN and a status in the wrong case, on CompleteRollout",
            2,
            vec![arn_clause.as_str(), "at 'status'", VALUE_SET],
            {
                let (_engine, router) = mounted_service();
                complete_rollout(&router, not_an_arn, "traj-1", Some("Ready")).await
            },
        ),
        (
            "the ARN and a trajectory id under the minimum, on CompleteRollout",
            2,
            vec![arn_clause.as_str(), "at 'trajectoryId'", UNDER_THE_MINIMUM],
            {
                let (_engine, router) = mounted_service();
                complete_rollout(&router, not_an_arn, "", Some("ready")).await
            },
        ),
        (
            "an ARN over its own maximum and a bad status, on CompleteRollout",
            3,
            vec![
                long_arn_clause.as_str(),
                "at 'jobArn'",
                OVER_THE_MAXIMUM,
                "at 'status'",
                VALUE_SET,
            ],
            {
                let (_engine, router) = mounted_service();
                complete_rollout(&router, &long_arn, "traj-1", Some("bogus-status")).await
            },
        ),
        (
            "the ARN and the trajectory id, on UpdateReward",
            2,
            vec![arn_clause.as_str(), "at 'trajectoryId'", OVER_THE_MAXIMUM],
            {
                let (_engine, router) = mounted_service();
                update_reward(&router, not_an_arn, &over_the_maximum).await
            },
        ),
        (
            "the ARN and the trajectory id, on Sample",
            2,
            vec![arn_clause.as_str(), "at 'trajectoryId'", OVER_THE_MAXIMUM],
            {
                let (_engine, router) = mounted_service();
                sample(&router, SAMPLE_PATH, not_an_arn, &over_the_maximum).await
            },
        ),
        (
            "the ARN and the trajectory id, on SampleWithResponseStream",
            2,
            vec![arn_clause.as_str(), "at 'trajectoryId'", OVER_THE_MAXIMUM],
            {
                let (_engine, router) = mounted_service();
                sample(&router, SAMPLE_STREAM_PATH, not_an_arn, &over_the_maximum).await
            },
        ),
    ] {
        // The ARN clause is this service's own wording, so the regular expression it
        // stands in for is stated nowhere in the sentence.
        refusal_states(case, &reply, clauses, &stated, &[PATTERN_REQUIREMENT]);
    }
}

/// RULE: what a caller is told about its OTHER members does not depend on whether the ARN
/// clause had a value to name — the clause count and the other clauses are the same either
/// way, and only the ARN clause's wording differs.
#[tokio::test]
async fn the_other_clauses_do_not_depend_on_the_arn_clauses_value() {
    let over_the_maximum = "t".repeat(257);

    // An ARN with no text a sentence can name keeps the framework's own clauses for it (it
    // breaks the `@length` minimum as well as the pattern), and the trajectory clause is
    // stated beside them exactly as it is beside the named-value wording above.
    for (case, arn, arn_clauses, arn_stated) in [
        (
            "an empty ARN header",
            String::new(),
            2,
            vec!["at 'jobArn'", UNDER_THE_MINIMUM, PATTERN_REQUIREMENT],
        ),
        (
            "an ARN of one space",
            " ".to_string(),
            1,
            vec!["at 'jobArn'", PATTERN_REQUIREMENT],
        ),
        (
            "an ARN that is not an ARN at all",
            "not-a-valid-job-arn".to_string(),
            1,
            vec![],
        ),
    ] {
        let (_engine, router) = mounted_service();
        let reply = complete_rollout(&router, &arn, &over_the_maximum, Some("ready")).await;
        let mut stated = arn_stated;
        stated.push("at 'trajectoryId'");
        stated.push(OVER_THE_MAXIMUM);
        refusal_states(case, &reply, arn_clauses + 1, &stated, &[]);
    }
}

/// RULE: the malformed-ARN sentence is the WHOLE message only when the ARN is the only
/// thing refused — there is no other clause to count beside it then.
#[tokio::test]
async fn the_malformed_arn_sentence_stands_alone_when_nothing_else_is_refused() {
    for (case, arn) in [
        ("not an ARN at all", "not-a-valid-job-arn"),
        (
            "another service's namespace",
            "arn:aws:bedrock:us-east-1:000000000000:job/AgentRFT/run-1",
        ),
    ] {
        let (_engine, router) = mounted_service();
        let reply = complete_rollout(&router, arn, "traj-1", Some("ready")).await;
        assert_eq!(
            reply.message(),
            malformed_arn(arn),
            "case {case:?}: expected the malformed-ARN sentence alone, got {:?}",
            reply.message()
        );
    }

    // And a request that breaks nothing modelled is not refused for the ARN at all: the
    // valid ARN names a job that was never created, which is a resolution failure rather
    // than a validation one.
    let (_engine, router) = mounted_service();
    let reply = complete_rollout(&router, &valid_job_arn(), "traj-1", Some("ready")).await;
    assert!(
        reply.status != 400,
        "case \"a valid request\": expected no ValidationException, got {} / {}",
        reply.status,
        reply.text()
    );
}
