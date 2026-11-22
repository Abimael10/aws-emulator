// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `BatchDeleteBuilds`, driven over the wire the router serves
//! (`POST /` with `X-Amz-Target: CodeBuild_20161006.BatchDeleteBuilds` on awsJson1_1)
//! through the harness in `project_build_lifecycle`.
//!
//! RULE under test: `BatchDeleteBuilds` PARTITIONS the ids it is given on the state each
//! build is in. A build that has reached a terminal state is removed and reported in
//! `buildsDeleted`; a build that is still IN PROGRESS is withheld — it keeps running, and it
//! is reported in `buildsNotDeleted` as a `BuildNotDeleted` carrying its identifier and a
//! `statusCode` saying why — and the same call deletes it once it has ended. Both halves name
//! a build in the build-ARN form this service names a build by, and each named build is
//! reported exactly once. An id naming no build is reported deleted, because the state the
//! caller asked for is the state the account is already in, which is what makes a repeated
//! delete answer as the first one did. A removed build is gone from every later read.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::project_build_lifecycle::{
    call, create_project, emulator, error_type, expected_arn, start_build,
};

/// How long a wait below gives the transition the clock owes a started build. Generous on
/// purpose: the wait ends on the CONDITION, and the deadline is only there so a build that
/// never ends fails the test instead of hanging it.
const COMPLETE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a wait below leaves before looking again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// A buildspec whose `build` phase states it takes a minute, for the rows whose build has to
/// still be RUNNING when the delete arrives.
///
/// Its commands are performed in process and spend none of the host's clock, so this is the
/// time the build STATES it took; how soon its outcome may be READ is a shorter, compressed
/// span, which is what makes a started build observably in progress at all.
const RUNNING_BUILDSPEC: &str =
    "version: 0.2\nphases:\n  build:\n    commands:\n      - sleep 60\n";

/// Read one build back through `BatchGetBuilds`.
async fn read_build(router: &MultiServiceRouter, id: &str) -> Value {
    let (status, body) = call(router, "BatchGetBuilds", json!({"ids": [id]})).await;
    assert_eq!(
        status, 200,
        "BatchGetBuilds {id:?}: expected 200, got {status} with {body}"
    );
    body["builds"][0].clone()
}

/// Look again at `id` until it reports itself complete — the state a build has to have
/// reached before this operation deletes it rather than withholding it.
async fn poll_until_complete(router: &MultiServiceRouter, id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + COMPLETE_DEADLINE;
    loop {
        let build = read_build(router, id).await;
        if build["buildComplete"] == json!(true) {
            return build;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "build {id}: expected it to reach a terminal state, last read {build}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// How a row leaves a build before the delete call: the three states a stored build
/// can be in here — still running, finished by the clock, or stopped by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Left {
    InProgress,
    Settled,
    Stopped,
}

/// The `buildStatus` a build left in this state reports.
fn expected_status(left: Left) -> &'static str {
    match left {
        Left::InProgress => "IN_PROGRESS",
        // The default project's buildspec declares no command, so a build the clock
        // finished ran nothing that could fail and ends SUCCEEDED.
        Left::Settled => "SUCCEEDED",
        Left::Stopped => "STOPPED",
    }
}

/// One row of the table: the builds to set up, which of them the request names (by
/// index, in request order), how many ids naming no build to mix in, whether the
/// first id is repeated, and whether the ids are sent in their ARN form.
#[derive(Debug)]
struct Case {
    name: &'static str,
    project: &'static str,
    builds: &'static [Left],
    named: &'static [usize],
    unknown: usize,
    repeat_first: bool,
    as_arns: bool,
}

/// The form `BatchDeleteBuilds` reports a deleted build in: the build ARN the id
/// denotes, and an id already given as an ARN exactly as it was sent.
fn deleted_form(sent: &str) -> String {
    if sent.starts_with("arn:") {
        sent.to_string()
    } else {
        expected_arn(&format!("build/{sent}"))
    }
}

/// The build id a request entry addresses: the resource half of a build ARN, or the
/// value itself when it is already an id.
fn addressed_id(sent: &str) -> &str {
    sent.split_once(":build/").map_or(sent, |(_, id)| id)
}

/// A syntactically valid build id (`<projectName>:<uuid>`) that names no build: the
/// uuid is one this service never mints, since every build's uuid is derived from its
/// project, build number and start instant.
fn unknown_id(project: &str, nth: usize) -> String {
    format!("{project}:00000000-0000-8000-8000-{nth:012}")
}

/// Set up `case`'s builds and hand back their ids by index, plus the order they were
/// started in (which is `buildNumber` order, and so list order).
async fn set_up(router: &MultiServiceRouter, case: &Case) -> (Vec<String>, Vec<usize>) {
    create_project(router, case.project, json!({})).await;
    let mut ids = vec![String::new(); case.builds.len()];
    let mut start_order = Vec::new();

    // The builds the clock must have finished are started first and waited for on the
    // CONDITION they are waited for — their own records reporting themselves complete; the
    // rest are started after that, so one clock can leave different rows of one request in
    // different states.
    for (index, left) in case.builds.iter().enumerate() {
        if *left == Left::Settled {
            let build = start_build(router, json!({"projectName": case.project})).await;
            ids[index] = build["id"].as_str().unwrap_or_default().to_string();
            start_order.push(index);
        }
    }
    for (index, left) in case.builds.iter().enumerate() {
        if *left == Left::Settled {
            poll_until_complete(router, &ids[index]).await;
        }
    }
    for (index, left) in case.builds.iter().enumerate() {
        if *left == Left::Settled {
            continue;
        }
        // A build that must still be RUNNING when the delete arrives is started with a
        // buildspec that states it takes a minute, so the row's state is the build's own
        // and not a race against the span every build spends in the queue.
        let mut request = json!({"projectName": case.project});
        if let (Value::Object(target), Left::InProgress) = (&mut request, *left) {
            target.insert(
                "buildspecOverride".to_string(),
                json!(RUNNING_BUILDSPEC),
            );
        }
        let build = start_build(router, request).await;
        let id = build["id"].as_str().unwrap_or_default().to_string();
        if *left == Left::Stopped {
            let (status, body) = call(router, "StopBuild", json!({"id": id.clone()})).await;
            assert_eq!(
                status, 200,
                "case {:?}: StopBuild {id}: expected 200, got {status} with {body}",
                case.name
            );
        }
        ids[index] = id;
        start_order.push(index);
    }

    // Each row really is in the state it claims, so a delete assertion below is about
    // deletion and not about a mis-set-up build.
    let (status, before) = call(router, "BatchGetBuilds", json!({"ids": ids.clone()})).await;
    assert_eq!(
        status, 200,
        "case {:?}: BatchGetBuilds before the delete: expected 200, got {status} with {before}",
        case.name
    );
    for (index, left) in case.builds.iter().enumerate() {
        let reported = before["builds"]
            .as_array()
            .and_then(|builds| {
                builds
                    .iter()
                    .find(|build| build["id"].as_str() == Some(ids[index].as_str()))
            })
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            reported["buildStatus"],
            expected_status(*left),
            "case {:?}: build {index} must be {} before the delete, got {reported}",
            case.name,
            expected_status(*left)
        );
    }

    (ids, start_order)
}

/// The ids `case` names, in request order.
fn requested_ids(case: &Case, started: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for (position, index) in case.named.iter().enumerate() {
        ids.push(started[*index].clone());
        if position < case.unknown {
            ids.push(unknown_id(case.project, position));
        }
    }
    for position in case.named.len()..case.unknown {
        ids.push(unknown_id(case.project, position));
    }
    if case.repeat_first {
        if let Some(first) = ids.first().cloned() {
            ids.push(first);
        }
    }
    if case.as_arns {
        ids = ids
            .iter()
            .map(|id| expected_arn(&format!("build/{id}")))
            .collect();
    }
    ids
}

/// `items` with every repeat after the first dropped, order kept.
fn deduped(items: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

/// RULE: the request is partitioned on the state each named build is in — a build in a
/// terminal state is removed and reported once in `buildsDeleted`, a build still in progress
/// is withheld and reported in `buildsNotDeleted`, and an id naming no build is reported
/// deleted — with both halves naming a build by its ARN. The table varies the build's state,
/// the number of ids, the mix of held and unheld ids, the request order, a repeated id and the
/// form the ids are sent in.
#[tokio::test]
async fn batch_delete_builds_partitions_the_request_on_each_builds_state() {
    let cases: &[Case] = &[
        Case {
            name: "one build that is still running",
            project: "del-in-progress",
            builds: &[Left::InProgress],
            named: &[0],
            unknown: 0,
            repeat_first: false,
            as_arns: false,
        },
        Case {
            name: "one build the clock already finished",
            project: "del-settled",
            builds: &[Left::Settled],
            named: &[0],
            unknown: 0,
            repeat_first: false,
            as_arns: false,
        },
        Case {
            name: "one build the caller stopped",
            project: "del-stopped",
            builds: &[Left::Stopped],
            named: &[0],
            unknown: 0,
            repeat_first: false,
            as_arns: false,
        },
        Case {
            name: "every build of a project, named out of start order",
            project: "del-all",
            builds: &[Left::InProgress, Left::InProgress, Left::InProgress],
            named: &[2, 0, 1],
            unknown: 0,
            repeat_first: false,
            as_arns: false,
        },
        Case {
            name: "some builds, leaving the rest in place",
            project: "del-some",
            builds: &[
                Left::Settled,
                Left::InProgress,
                Left::Stopped,
                Left::InProgress,
            ],
            named: &[1, 3],
            unknown: 0,
            repeat_first: false,
            as_arns: false,
        },
        Case {
            name: "only ids naming no build",
            project: "del-unknown",
            builds: &[Left::InProgress],
            named: &[],
            unknown: 2,
            repeat_first: false,
            as_arns: false,
        },
        Case {
            name: "held and unheld ids mixed in one request",
            project: "del-mixed",
            builds: &[Left::Settled, Left::InProgress],
            named: &[0, 1],
            unknown: 2,
            repeat_first: false,
            as_arns: false,
        },
        Case {
            name: "the same build named twice",
            project: "del-repeat",
            builds: &[Left::InProgress],
            named: &[0],
            unknown: 0,
            repeat_first: true,
            as_arns: false,
        },
        Case {
            name: "held and unheld ids sent in their ARN form",
            project: "del-arn-form",
            builds: &[Left::Stopped, Left::InProgress],
            named: &[1, 0],
            unknown: 2,
            repeat_first: true,
            as_arns: true,
        },
    ];

    for case in cases {
        // A fresh emulator per row: a row's state must not depend on the one before.
        let router = emulator();
        let (started, start_order) = set_up(&router, case).await;
        let requested = requested_ids(case, &started);
        // Which named ids address a build that is still RUNNING: those are the ones the
        // call has to withhold, and every other id — a build in a terminal state, and an id
        // naming no build at all — is one it deletes.
        let withheld = |sent: &String| -> bool {
            started
                .iter()
                .position(|id| id.as_str() == addressed_id(sent))
                .is_some_and(|index| case.builds[index] == Left::InProgress)
        };
        // Every named build the call deletes, once, in the form the answer reports it in.
        let expected_deleted: Vec<String> = deduped(&requested)
            .iter()
            .filter(|sent| !withheld(sent))
            .map(|id| deleted_form(id))
            .collect();
        // And the other half of the partition, in the same form and the same order.
        let expected_withheld: Vec<String> = deduped(&requested)
            .iter()
            .filter(|sent| withheld(sent))
            .map(|id| deleted_form(id))
            .collect();
        // The builds the account held and this call removed, by store id.
        let gone: Vec<String> = deduped(&requested)
            .iter()
            .filter(|sent| !withheld(sent))
            .map(|sent| addressed_id(sent).to_string())
            .filter(|id| started.contains(id))
            .collect();

        let (status, body) = call(
            &router,
            "BatchDeleteBuilds",
            json!({"ids": requested.clone()}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: BatchDeleteBuilds {requested:?}: expected 200, got {status} with {body}",
            case.name
        );

        let deleted: Vec<String> = body["buildsDeleted"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .map(|id| id.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            deleted, expected_deleted,
            "case {:?}: buildsDeleted must report each deletable named id once, as a build \
             ARN, in request order; got {body}",
            case.name
        );
        for reported in &deleted {
            assert!(
                reported.contains(":build/"),
                "case {:?}: a deleted build is named by its ARN, got {reported:?} in {body}",
                case.name
            );
        }

        // The other half of the partition: one entry per build that was still running,
        // naming it the same way and saying why it was not deleted. The member is always
        // stated, `[]` when nothing was withheld, because `BuildsNotDeleted` carries no
        // `@length`.
        let not_deleted: Vec<Value> = body["buildsNotDeleted"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let not_deleted_ids: Vec<String> = not_deleted
            .iter()
            .map(|entry| entry["id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            not_deleted_ids, expected_withheld,
            "case {:?}: buildsNotDeleted must report every build that was still running, by \
             its ARN; got {body}",
            case.name
        );
        for entry in &not_deleted {
            assert!(
                entry["statusCode"]
                    .as_str()
                    .is_some_and(|code| !code.is_empty()),
                "case {:?}: a withheld build states why it was not deleted, got {entry} in \
                 {body}",
                case.name
            );
        }

        // Every later read is computed from the store the delete wrote: the deleted
        // builds are gone and the survivors are untouched.
        let (status, after) = call(&router, "BatchGetBuilds", json!({"ids": started.clone()})).await;
        assert_eq!(
            status, 200,
            "case {:?}: BatchGetBuilds after the delete: expected 200, got {status} with {after}",
            case.name
        );
        let reported: Vec<String> = after["builds"]
            .as_array()
            .map(|builds| {
                builds
                    .iter()
                    .map(|build| build["id"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let survivors: Vec<String> = started.iter().filter(|id| !gone.contains(id)).cloned().collect();
        for id in &gone {
            assert!(
                !reported.contains(id),
                "case {:?}: {id} was deleted, so BatchGetBuilds must not report it; got {after}",
                case.name
            );
        }
        for id in &survivors {
            assert!(
                reported.contains(id),
                "case {:?}: {id} was withheld or never named, so it must still be readable; \
                 got {after}",
                case.name
            );
        }
        let not_found: Vec<String> = after["buildsNotFound"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .map(|id| id.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        for id in &gone {
            assert!(
                not_found.contains(id),
                "case {:?}: a deleted build reads as not found; got {after}",
                case.name
            );
        }

        // And the project's own index: the survivors only, still in build-number
        // order (descending, which is the documented default).
        let (status, listed) = call(
            &router,
            "ListBuildsForProject",
            json!({"projectName": case.project}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: ListBuildsForProject: expected 200, got {status} with {listed}",
            case.name
        );
        let expected_listed: Vec<String> = start_order
            .iter()
            .map(|index| started[*index].clone())
            .filter(|id| !gone.contains(id))
            .rev()
            .collect();
        let actual_listed: Vec<String> = listed["ids"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .map(|id| id.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            actual_listed, expected_listed,
            "case {:?}: the project must list only the builds that survived, newest first; got {listed}",
            case.name
        );

        // A withheld build is not refused forever: it was left RUNNING by the call above,
        // and the very same call deletes it once it has reached a terminal state.
        if !expected_withheld.is_empty() {
            let still_running: Vec<String> = started
                .iter()
                .enumerate()
                .filter(|(index, _)| case.builds[*index] == Left::InProgress)
                .map(|(_, id)| id.clone())
                .collect();
            for id in &still_running {
                poll_until_complete(&router, id).await;
            }
            let (status, again) = call(
                &router,
                "BatchDeleteBuilds",
                json!({"ids": still_running.clone()}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: the delete of the withheld builds: expected 200, got {status} \
                 with {again}",
                case.name
            );
            assert_eq!(
                again["buildsDeleted"],
                json!(still_running
                    .iter()
                    .map(|id| expected_arn(&format!("build/{id}")))
                    .collect::<Vec<String>>()),
                "case {:?}: a build that has ended is deletable by the same call; got {again}",
                case.name
            );
            assert_eq!(
                again["buildsNotDeleted"],
                json!([]),
                "case {:?}: nothing is withheld once every named build has ended; got {again}",
                case.name
            );
        }
    }
}

/// RULE: a deleted build frees nothing a later build reuses — `buildNumber` keeps
/// counting up for a project whose builds were all deleted — and the delete is
/// IDEMPOTENT: repeating it answers exactly as the first call did, because the state it
/// asks for is already the state the account is in. The operations that ACT on a build
/// still refuse an absent one, naming it by the id the request gave.
#[tokio::test]
async fn deleting_a_build_neither_rewinds_the_build_number_nor_differs_when_repeated() {
    let router = emulator();
    create_project(&router, "del-numbers", json!({})).await;

    let mut ids = Vec::new();
    for expected in 1..=3_i64 {
        let build = start_build(&router, json!({"projectName": "del-numbers"})).await;
        assert_eq!(
            build["buildNumber"].as_i64(),
            Some(expected),
            "build {expected}: expected buildNumber {expected}, got {build}"
        );
        ids.push(build["id"].as_str().unwrap_or_default().to_string());
    }
    // Every one of them has to have ENDED before it can be deleted, so each is waited for
    // on that condition.
    for id in &ids {
        poll_until_complete(&router, id).await;
    }

    let (status, body) = call(
        &router,
        "BatchDeleteBuilds",
        json!({"ids": ids.clone()}),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchDeleteBuilds: expected 200, got {status} with {body}"
    );
    assert_eq!(
        body["buildsDeleted"],
        json!(ids
            .iter()
            .map(|id| expected_arn(&format!("build/{id}")))
            .collect::<Vec<String>>()),
        "every build of the project was named, so every one is reported deleted by its \
         ARN; got {body}"
    );
    assert_eq!(
        body["buildsNotDeleted"],
        json!([]),
        "nothing was left behind, so buildsNotDeleted is an empty list; got {body}"
    );

    // The project still exists, so its counter does: the next build is the fourth.
    let next = start_build(&router, json!({"projectName": "del-numbers"})).await;
    assert_eq!(
        next["buildNumber"].as_i64(),
        Some(4),
        "a deleted build must not free its buildNumber, got {next}"
    );

    // Deleting an already-deleted id answers as the first delete did: the caller asked
    // for a state the account is already in.
    let (status, again) = call(
        &router,
        "BatchDeleteBuilds",
        json!({"ids": [ids[0].clone()]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the repeated delete: expected 200, got {status} with {again}"
    );
    assert_eq!(
        again["buildsDeleted"],
        json!([expected_arn(&format!("build/{}", ids[0]))]),
        "the repeated delete reports the same ARN as the first; got {again}"
    );
    assert_eq!(
        again["buildsNotDeleted"],
        json!([]),
        "the repeated delete leaves nothing behind either; got {again}"
    );

    // The same rule, in the other operation that answers for a build it holds no
    // record of: an absent build is named by the id the request gave.
    let mut absent: Vec<String> = ids.clone();
    absent.push(unknown_id("del-numbers", 7));
    absent.push(unknown_id("del-never-created", 7));
    for id in &absent {
        let (status, body) = call(&router, "StopBuild", json!({"id": id.clone()})).await;
        assert_eq!(
            status, 400,
            "StopBuild {id}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            "ResourceNotFoundException",
            "StopBuild {id}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["message"],
            Value::String(format!("Build {id} does not exist")),
            "StopBuild {id}: the refusal names that id, got {body}"
        );
    }
}

/// RULE: `ids` is held to the bounds the model declares for it — `BuildIds`
/// `@length(min: 1, max: 100)`, with `ids` itself `@required` — so a request at either
/// published bound is served and one past it, or one leaving `ids` out, is refused with
/// the client error the operation models.
#[tokio::test]
async fn the_ids_list_is_held_to_its_declared_length_bounds() {
    let router = emulator();
    create_project(&router, "del-bounds", json!({})).await;
    let build = start_build(&router, json!({"projectName": "del-bounds"})).await;
    let held = build["id"].as_str().unwrap_or_default().to_string();
    poll_until_complete(&router, &held).await;

    // At the minimum: one id.
    let (status, body) = call(&router, "BatchDeleteBuilds", json!({"ids": [held.clone()]})).await;
    assert_eq!(
        status, 200,
        "one id, the declared minimum: expected 200, got {status} with {body}"
    );
    assert_eq!(
        body["buildsDeleted"],
        json!([expected_arn(&format!("build/{held}"))]),
        "one id, the declared minimum: expected it deleted, got {body}"
    );

    // At the maximum: 100 ids, one of them held.
    let second = start_build(&router, json!({"projectName": "del-bounds"})).await;
    let second_id = second["id"].as_str().unwrap_or_default().to_string();
    poll_until_complete(&router, &second_id).await;
    let mut at_max: Vec<String> = vec![second_id.clone()];
    while at_max.len() < 100 {
        at_max.push(unknown_id("del-bounds", at_max.len()));
    }
    let (status, body) = call(&router, "BatchDeleteBuilds", json!({"ids": at_max.clone()})).await;
    assert_eq!(
        status, 200,
        "100 ids, the declared maximum: expected 200, got {status} with {body}"
    );
    assert_eq!(
        body["buildsDeleted"],
        json!(at_max
            .iter()
            .map(|id| expected_arn(&format!("build/{id}")))
            .collect::<Vec<String>>()),
        "100 ids, the declared maximum: every one is reported deleted, got {body}"
    );
    assert_eq!(
        body["buildsNotDeleted"],
        json!([]),
        "100 ids, the declared maximum: nothing is left behind, got {body}"
    );

    let refused: Vec<(&str, Value)> = vec![
        ("an empty ids list, below the minimum", json!({"ids": []})),
        (
            "101 ids, past the maximum",
            json!({"ids": (0..101).map(|nth| unknown_id("del-bounds", nth)).collect::<Vec<String>>()}),
        ),
        ("ids left out entirely", json!({})),
    ];
    for (case, request) in refused {
        let (status, body) = call(&router, "BatchDeleteBuilds", request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_type(&body),
            "InvalidInputException",
            "case {case:?}: expected InvalidInputException, got {body}"
        );
    }
}
