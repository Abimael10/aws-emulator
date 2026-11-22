// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the cap a project puts on how many of its builds may be IN FLIGHT at
//! once, driven over the wire the router serves: `POST /` with `X-Amz-Target:
//! CodeBuild_20161006.<Operation>` on awsJson1_1, through the harness
//! [`super::project_build_lifecycle`] owns.
//!
//! The rule under test: `Project$concurrentBuildLimit` is "the maximum number of concurrent
//! builds that are allowed for this project. New builds are only started if the current
//! number of builds is less than or equal to this limit. If the current build count meets
//! this limit, new builds are throttled and are not run" — so WHICHEVER operation would
//! create a build of a project that already holds that many unfinished builds creates none
//! and refuses it with the modelled `AccountLimitExceededException` (HTTP 400), and a project
//! that advertises no limit caps nothing.
//!
//! Nothing here waits on the wall clock for the builds whose COUNT each row is about: a row
//! holds the one machine of a compute fleet with a build of another project, so every build
//! it starts afterwards sits in its `QUEUED` phase — created, unfinished, and therefore
//! counted — for the whole wait a queued build is given (five minutes at the least), and
//! every wait a row does make ends on the CONDITION it is waiting for.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::fleet_lifecycle::{batch_get_fleets, create_fleet};
use super::project_build_lifecycle::{
    call, create_project, emulator, error_type, listed_ids, settled_build,
};

/// How long a poll below gives the condition it waits for. Generous on purpose: the wait ends
/// on the condition, and the deadline is only there so a condition that never holds fails the
/// test instead of hanging it.
const CONDITION_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits before looking again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// The `BuildPhaseType` a build waits for a machine in.
const QUEUED: &str = "QUEUED";

/// A buildspec whose `build` phase states it takes fifteen minutes: what the build that HOLDS
/// the one machine of a row's fleet runs, so it keeps it for the whole row.
const HOLDING_BUILDSPEC: &str =
    "version: 0.2\nphases:\n  build:\n    commands:\n      - sleep 900\n";

/// A buildspec no command of which states any time: the builds a row only needs to see
/// created, refused or finished rather than holding anything.
const INSTANT_BUILDSPEC: &str = "version: 0.2";

/// What CodeBuild answers a request that would start a build of a project already holding
/// `limit` builds in flight — AWS's own wording for this refusal, with the project's
/// configured limit in it.
fn limit_exceeded_message(limit: i32) -> String {
    format!(
        "Concurrent build limit exceeded. This build project has a limit of {limit} concurrent \
         build(s). Start the build again after other builds are complete, or edit project \
         configuration."
    )
}

/// Create a one-machine compute fleet whose overflow builds WAIT for that machine, look again
/// until its capacity is provisioned, and hand back its ARN.
async fn single_machine_queueing_fleet(router: &MultiServiceRouter, name: &str) -> String {
    let fleet = create_fleet(
        router,
        name,
        json!({"baseCapacity": 1, "overflowBehavior": "QUEUE"}),
    )
    .await;
    let arn = fleet["arn"].as_str().expect("a fleet ARN").to_string();
    let deadline = tokio::time::Instant::now() + CONDITION_DEADLINE;
    loop {
        let read = batch_get_fleets(router, &[name]).await;
        if read["fleets"][0]["status"]["statusCode"] == json!("ACTIVE") {
            return arn;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "fleet {name:?}: expected it to become ACTIVE, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create a project whose builds run `buildspec`, pinned to `fleet_arn` when one is given,
/// advertising `limit` concurrent builds when one is given.
async fn project_with_limit(
    router: &MultiServiceRouter,
    name: &str,
    fleet_arn: Option<&str>,
    limit: Option<i32>,
    buildspec: &str,
) {
    let mut environment = json!({
        "type": "LINUX_CONTAINER",
        "image": "aws/codebuild/standard:7.0",
        "computeType": "BUILD_GENERAL1_SMALL",
    });
    if let (Value::Object(target), Some(arn)) = (&mut environment, fleet_arn) {
        target.insert("fleet".to_string(), json!({"fleetArn": arn}));
    }
    let mut extra = json!({
        "source": {"type": "NO_SOURCE", "buildspec": buildspec},
        "environment": environment,
    });
    if let (Value::Object(target), Some(limit)) = (&mut extra, limit) {
        target.insert("concurrentBuildLimit".to_string(), json!(limit));
    }
    create_project(router, name, extra).await;
}

/// `StartBuild` of `project`, with `extra`'s members merged in, as it answered.
async fn start_build_call(
    router: &MultiServiceRouter,
    project: &str,
    extra: Value,
) -> (u16, Value) {
    let mut body = json!({"projectName": project});
    if let (Value::Object(target), Value::Object(members)) = (&mut body, extra) {
        target.extend(members);
    }
    call(router, "StartBuild", body).await
}

/// `RetryBuild` of the build `id`, as it answered.
async fn retry_build_call(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "RetryBuild", json!({"id": id})).await
}

/// The build `id` as `BatchGetBuilds` reports it.
async fn read_build(router: &MultiServiceRouter, id: &str) -> Value {
    let (status, body) = call(router, "BatchGetBuilds", json!({"ids": [id]})).await;
    assert_eq!(
        status, 200,
        "BatchGetBuilds {id:?}: expected 200, got {status} with {body}"
    );
    body["builds"][0].clone()
}

/// The ids `ListBuildsForProject` reports for `project` — what says whether a refused request
/// left a build behind.
async fn builds_of(router: &MultiServiceRouter, project: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "ListBuildsForProject",
        json!({"projectName": project}),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListBuildsForProject {project:?}: expected 200, got {status} with {body}"
    );
    listed_ids(&body)
}

/// Start the build that HOLDS the one machine of `project`'s fleet and look again until it has
/// been admitted onto it, so every build started behind it stays in the queue — created and
/// unfinished, which is what a project's limit counts.
async fn hold_the_machine(router: &MultiServiceRouter, project: &str) {
    let (status, body) = start_build_call(router, project, json!({})).await;
    assert_eq!(
        status, 200,
        "the holding build of {project:?}: expected 200, got {status} with {body}"
    );
    let id = body["build"]["id"]
        .as_str()
        .expect("a build id")
        .to_string();
    let deadline = tokio::time::Instant::now() + CONDITION_DEADLINE;
    loop {
        let record = read_build(router, &id).await;
        if record["currentPhase"] != json!(QUEUED) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the holding build of {project:?}: expected it to be admitted onto the fleet's \
             machine, last read {record}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Assert `status`/`body` is the refusal a request past `limit` gets, and nothing else.
fn assert_limit_refusal(case: &str, limit: i32, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: a request past the limit is refused with 400, got {status} with {body}"
    );
    assert_eq!(
        error_type(body),
        "AccountLimitExceededException",
        "case {case:?}: expected AccountLimitExceededException, got {body}"
    );
    assert_eq!(
        body["message"],
        json!(limit_exceeded_message(limit)),
        "case {case:?}: expected the concurrent-build-limit wording naming the limit {limit}, \
         got {body}"
    );
    assert!(
        body["build"].is_null(),
        "case {case:?}: a refused request reports no build, got {body}"
    );
}

/// One row of the start table: what the project advertises, and how many of its builds are in
/// flight when one more is started.
struct StartRow {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The project's `concurrentBuildLimit` — `None` for a project that advertises none.
    limit: Option<i32>,
    /// How many builds the row starts before the one whose answer it is about.
    in_flight: usize,
}

/// RULE: a project's `concurrentBuildLimit` caps how many of its builds may be in flight at
/// once, and a `StartBuild` that would create one past that cap creates no build and is
/// refused with `AccountLimitExceededException` naming the project's own limit; a project that
/// advertises no limit caps nothing.
///
/// The rows span what the rule generalises over: the limit the project advertises (one, two,
/// three, and none at all), and so the number of builds that have to be in flight before the
/// cap bites — the recorded case is the one-build row.
#[tokio::test]
async fn starting_a_build_past_the_projects_concurrent_build_limit_is_refused() {
    let rows = [
        StartRow {
            case: "no limit at all",
            limit: None,
            in_flight: 3,
        },
        StartRow {
            case: "a limit of one build",
            limit: Some(1),
            in_flight: 1,
        },
        StartRow {
            case: "a limit of two builds",
            limit: Some(2),
            in_flight: 2,
        },
        StartRow {
            case: "a limit of three builds",
            limit: Some(3),
            in_flight: 3,
        },
    ];

    let router = emulator();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let fleet = single_machine_queueing_fleet(&router, &format!("start-fleet-{index}")).await;
        let holder = format!("start-holder-{index}");
        project_with_limit(&router, &holder, Some(&fleet), None, HOLDING_BUILDSPEC).await;
        hold_the_machine(&router, &holder).await;

        let project = format!("start-limited-{index}");
        project_with_limit(
            &router,
            &project,
            Some(&fleet),
            row.limit,
            INSTANT_BUILDSPEC,
        )
        .await;

        // The builds that fill the project's slots: every one of them is in flight, which is
        // what the next start is answered against.
        for slot in 0..row.in_flight {
            let (status, body) = start_build_call(&router, &project, json!({})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: build {slot} of {} is inside the limit, got {status} with \
                 {body}",
                row.in_flight
            );
            let id = body["build"]["id"].as_str().expect("a build id").to_string();
            let record = read_build(&router, &id).await;
            assert_eq!(
                record["buildComplete"],
                json!(false),
                "case {case:?}: build {slot} must still be in flight, got {record}"
            );
        }

        let (status, body) = start_build_call(&router, &project, json!({})).await;
        match row.limit {
            Some(limit) => assert_limit_refusal(case, limit, status, &body),
            None => assert_eq!(
                status, 200,
                "case {case:?}: a project advertising no limit caps nothing, got {status} with \
                 {body}"
            ),
        }

        // And what the project's build set is afterwards: a refused request leaves no build
        // behind, and an accepted one is there.
        let accepted = row.in_flight + usize::from(row.limit.is_none());
        let listed = builds_of(&router, &project).await;
        assert_eq!(
            listed.len(),
            accepted,
            "case {case:?}: expected {accepted} build(s) of the project, got {listed:?}"
        );
    }
}

/// One row of the retry table: what the project advertises, and how many retries of its build
/// fit inside that.
struct RetryRow {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The project's `concurrentBuildLimit` — `None` for a project that advertises none.
    limit: Option<i32>,
    /// How many retries the row makes before the one whose answer it is about.
    retries_inside: usize,
}

/// RULE: the same cap governs `RetryBuild`, which also creates a build of the project — a
/// retry that would put the project past its `concurrentBuildLimit` creates none and is
/// refused with `AccountLimitExceededException` naming that limit.
///
/// The rows span the same ground as the start table — the limit the project advertises, and so
/// how many retries fit inside it before the cap bites.
#[tokio::test]
async fn retrying_a_build_past_the_projects_concurrent_build_limit_is_refused() {
    let rows = [
        RetryRow {
            case: "a limit of one build, held by the build being retried",
            limit: Some(1),
            retries_inside: 0,
        },
        RetryRow {
            case: "a limit of three builds",
            limit: Some(3),
            retries_inside: 2,
        },
        RetryRow {
            case: "no limit at all",
            limit: None,
            retries_inside: 2,
        },
    ];

    let router = emulator();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let fleet = single_machine_queueing_fleet(&router, &format!("retry-fleet-{index}")).await;
        let holder = format!("retry-holder-{index}");
        project_with_limit(&router, &holder, Some(&fleet), None, HOLDING_BUILDSPEC).await;
        hold_the_machine(&router, &holder).await;

        let project = format!("retry-limited-{index}");
        project_with_limit(
            &router,
            &project,
            Some(&fleet),
            row.limit,
            INSTANT_BUILDSPEC,
        )
        .await;

        let (status, body) = start_build_call(&router, &project, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first build of the project is inside the limit, got {status} \
             with {body}"
        );
        let retried = body["build"]["id"].as_str().expect("a build id").to_string();

        for attempt in 0..row.retries_inside {
            let (status, body) = retry_build_call(&router, &retried).await;
            assert_eq!(
                status, 200,
                "case {case:?}: retry {attempt} of {} is inside the limit, got {status} with \
                 {body}",
                row.retries_inside
            );
        }

        let (status, body) = retry_build_call(&router, &retried).await;
        match row.limit {
            Some(limit) => assert_limit_refusal(case, limit, status, &body),
            None => assert_eq!(
                status, 200,
                "case {case:?}: a project advertising no limit caps nothing, got {status} with \
                 {body}"
            ),
        }

        let accepted = 1 + row.retries_inside + usize::from(row.limit.is_none());
        let listed = builds_of(&router, &project).await;
        assert_eq!(
            listed.len(),
            accepted,
            "case {case:?}: expected {accepted} build(s) of the project, got {listed:?}"
        );
    }
}

/// RULE: the cap counts the builds that are in FLIGHT, so a build of a project whose builds
/// have finished may be started again — the refusal's own remedy, "start the build again after
/// other builds are complete".
///
/// The rows span the limit the project advertises: one build, and more than one.
#[tokio::test]
async fn a_build_may_be_started_again_once_the_projects_builds_are_complete() {
    let router = emulator();

    for limit in [1, 2] {
        let case = format!("a limit of {limit} build(s), run one after another");
        let project = format!("sequential-{limit}");
        project_with_limit(&router, &project, None, Some(limit), INSTANT_BUILDSPEC).await;

        // One more build than the limit allows at once, each waited out before the next is
        // started: every one of them is accepted, because none of the earlier ones is in
        // flight any more.
        let runs = usize::try_from(limit).expect("a small limit") + 1;
        for run in 0..runs {
            let settled = settled_build(&router, &project, json!({})).await;
            assert_eq!(
                settled["buildComplete"],
                json!(true),
                "case {case:?}: build {run} must have completed, got {settled}"
            );
        }

        let listed = builds_of(&router, &project).await;
        assert_eq!(
            listed.len(),
            runs,
            "case {case:?}: expected {runs} build(s) of the project, got {listed:?}"
        );
    }
}

/// RULE: a repeat of a request this service already answered mints no build, so it needs no
/// slot: a `StartBuild` carrying an `idempotencyToken` it has already seen resolves to the
/// build that token started, even with the project's `concurrentBuildLimit` saturated.
#[tokio::test]
async fn a_replayed_start_at_the_limit_resolves_to_its_own_build() {
    let case = "a replay at a saturated limit of one build";
    let router = emulator();
    let fleet = single_machine_queueing_fleet(&router, "replay-fleet").await;
    project_with_limit(
        &router,
        "replay-holder",
        Some(&fleet),
        None,
        HOLDING_BUILDSPEC,
    )
    .await;
    hold_the_machine(&router, "replay-holder").await;

    project_with_limit(
        &router,
        "replay-limited",
        Some(&fleet),
        Some(1),
        INSTANT_BUILDSPEC,
    )
    .await;

    let token = json!({"idempotencyToken": "the-same-request"});
    let (status, first) = start_build_call(&router, "replay-limited", token.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first build is inside the limit, got {status} with {first}"
    );
    let (status, replayed) = start_build_call(&router, "replay-limited", token).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a replay resolves rather than being refused, got {status} with {replayed}"
    );
    assert_eq!(
        replayed["build"]["id"], first["build"]["id"],
        "case {case:?}: the replay must resolve to the build its token started, got {replayed}"
    );

    let listed = builds_of(&router, "replay-limited").await;
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: the replay must mint no second build, got {listed:?}"
    );
}
