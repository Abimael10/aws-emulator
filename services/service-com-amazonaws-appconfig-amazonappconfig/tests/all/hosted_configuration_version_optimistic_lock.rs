// SPDX-License-Identifier: CC0-1.0

//! `CreateHostedConfigurationVersion`'s optimistic lock, and the two sentences AppConfig
//! refuses one with.
//!
//! `LatestVersionNumber` (bound to the `Latest-Version-Number` header) is "an optional
//! locking token used to prevent race conditions from overwriting configuration updates
//! when creating a new version"
//! (`smithy_query(query="operation",operation="CreateHostedConfigurationVersion")`). A write
//! that supplies it reads the version the profile holds, demands that it is the one it
//! named, and then creates the version AFTER it — two steps, each of which can fail on its
//! own, and AppConfig words them differently:
//!
//! * the conditional write LOST, because another writer holding the SAME lock created that
//!   version first: HTTP 409 `ConflictException` with
//!   `Hosted configuration version <lock + 1> already exists` — the version the refused
//!   write would have created, no trailing full stop, and nothing of the application, the
//!   profile or the lock in the sentence;
//! * the lock was already STALE when it was read, and the version it would have created was
//!   not written by a racer holding that lock: HTTP 409 `ConflictException` with
//!   `Expected latest version <lock> does not match actual latest version <actual>`.
//!
//! Either way exactly one writer wins, the refused writes store nothing, and the profile's
//! version set is the one the winner left behind.
//!
//! The tables span what the rules generalise over: the number of writers racing for one
//! version, the version the race starts from, and — for the stale half — how far the lock
//! is from the version the profile holds, below and above it.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_profile, json_call, router, send, Answer,
};

/// `POST …/hostedconfigurationversions`, with `LatestVersionNumber` travelling as the
/// `Latest-Version-Number` header the model binds it to.
async fn host(
    router: &MultiServiceRouter,
    application: &str,
    profile: &str,
    content: &str,
    lock: Option<i32>,
) -> Answer {
    let headers: Vec<(&str, String)> = lock
        .map(|number| vec![("Latest-Version-Number", number.to_string())])
        .unwrap_or_default();
    send(
        router,
        "POST",
        &format!(
            "/applications/{application}/configurationprofiles/{profile}/hostedconfigurationversions"
        ),
        "text/plain",
        content.as_bytes().to_vec(),
        &headers,
    )
    .await
}

/// The version numbers the profile holds, read back through
/// `ListHostedConfigurationVersions`.
async fn version_numbers(
    router: &MultiServiceRouter,
    case: &str,
    application: &str,
    profile: &str,
) -> Vec<i64> {
    let listed = json_call(
        router,
        "GET",
        &format!(
            "/applications/{application}/configurationprofiles/{profile}/hostedconfigurationversions"
        ),
        &Value::Null,
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: the listing must be served, got {} / {}",
        listed.status,
        String::from_utf8_lossy(&listed.body)
    );
    let mut numbers: Vec<i64> = listed
        .json()
        .get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item.get("VersionNumber").and_then(Value::as_i64))
        .collect();
    numbers.sort_unstable();
    numbers
}

/// The version a create reported, which this operation binds to the `Version-Number`
/// response header (its `Content` is the `@httpPayload`).
fn created_version(case: &str, answer: &Answer) -> i64 {
    answer
        .header("Version-Number")
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: no Version-Number on {:?}", answer.status))
}

/// Assert one refusal's status, code and message.
fn assert_conflict(case: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        answer.status,
        409,
        "case {case:?}: expected 409, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert!(
        answer.error_code().contains("ConflictException"),
        "case {case:?}: expected ConflictException, got {:?}",
        answer.error_code()
    );
    let stated = answer
        .string("Message")
        .unwrap_or_else(|| String::from_utf8_lossy(&answer.body).to_string());
    assert_eq!(
        stated, expected,
        "case {case:?}: expected message {expected:?}, got {stated:?}"
    );
}

/// RULE: a writer whose version number was taken by another writer holding the SAME lock is
/// refused with the sentence naming the version it could not create, exactly one writer
/// wins, and the profile ends with the winner's version and nothing else.
///
/// Which of the two sentences a refused write gets is decided from the profile — the lock
/// each stored version was created under — so these rows drive the writers one after
/// another; `concurrent_writers_leave_one_winner_and_four_such_refusals` drives the same
/// rule with every writer in flight at once.
#[tokio::test]
async fn a_writer_beaten_to_its_version_names_the_version_it_could_not_create() {
    let router = router();
    let application = create_application(&router, "lock-race").await;

    // Rows differ along how many writers race for one version and the version the race
    // starts from, so the sentence is shown to be built from the lock rather than from any
    // one recorded run.
    for (racers, base) in [(2_usize, 1_i32), (5, 1), (3, 4)] {
        let case = format!("{racers} writers racing for version {} ", base + 1);
        let profile = create_profile(&router, &application, &format!("race-{racers}-{base}")).await;
        // The versions the race starts from, each written without a lock.
        for version in 1..=base {
            let seeded = host(
                &router,
                &application,
                &profile,
                &format!("seed {version}"),
                None,
            )
            .await;
            assert_eq!(
                seeded.status,
                201,
                "case {case:?}: seeding version {version} must answer 201, got {} / {}",
                seeded.status,
                String::from_utf8_lossy(&seeded.body)
            );
        }

        // Every racer pins the same lock and carries content of its own, so the winner is
        // identifiable and no write can be served by the byte-identical-content path.
        let mut answers: Vec<Answer> = Vec::new();
        for racer in 0..racers {
            answers.push(
                host(
                    &router,
                    &application,
                    &profile,
                    &format!("racer {racer} for {base}"),
                    Some(base),
                )
                .await,
            );
        }

        let winners: Vec<&Answer> = answers
            .iter()
            .filter(|answer| answer.status == 201)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "case {case:?}: exactly one racer may win, got {:?}",
            answers
                .iter()
                .map(|answer| answer.status)
                .collect::<Vec<u16>>()
        );
        assert_eq!(
            created_version(&case, winners[0]),
            i64::from(base) + 1,
            "case {case:?}: the winner must create the version after the lock it pinned"
        );

        let expected = format!("Hosted configuration version {} already exists", base + 1);
        for answer in answers.iter().filter(|answer| answer.status != 201) {
            assert_conflict(&case, answer, &expected);
        }

        let held = version_numbers(&router, &case, &application, &profile).await;
        let wanted: Vec<i64> = (1..=i64::from(base) + 1).collect();
        assert_eq!(
            held, wanted,
            "case {case:?}: the losers store nothing, so the profile must hold {wanted:?}, \
             got {held:?}"
        );
    }
}

/// RULE: the same rule with every writer in flight at once — five creates pinning one lock
/// leave one winner and four refusals naming the version they could not create, and the
/// profile holds exactly the version the winner wrote.
#[tokio::test]
async fn concurrent_writers_leave_one_winner_and_four_such_refusals() {
    let router = router();
    let application = create_application(&router, "lock-concurrent").await;
    let profile = create_profile(&router, &application, "concurrent").await;
    let seeded = host(&router, &application, &profile, "the base version", None).await;
    assert_eq!(
        seeded.status,
        201,
        "the base version must be hosted, got {} / {}",
        seeded.status,
        String::from_utf8_lossy(&seeded.body)
    );

    // All five in flight together, each pinning the version the profile holds and carrying
    // content of its own.
    let (first, second, third, fourth, fifth) = tokio::join!(
        host(&router, &application, &profile, "racer a", Some(1)),
        host(&router, &application, &profile, "racer b", Some(1)),
        host(&router, &application, &profile, "racer c", Some(1)),
        host(&router, &application, &profile, "racer d", Some(1)),
        host(&router, &application, &profile, "racer e", Some(1)),
    );
    let answers = [first, second, third, fourth, fifth];

    let case = "five concurrent writers pinning version 1";
    let winners: Vec<&Answer> = answers
        .iter()
        .filter(|answer| answer.status == 201)
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "case {case:?}: exactly one writer may win, got {:?}",
        answers
            .iter()
            .map(|answer| answer.status)
            .collect::<Vec<u16>>()
    );
    assert_eq!(
        created_version(case, winners[0]),
        2,
        "case {case:?}: the winner must create version 2"
    );
    for answer in answers.iter().filter(|answer| answer.status != 201) {
        assert_conflict(case, answer, "Hosted configuration version 2 already exists");
    }
    let held = version_numbers(&router, case, &application, &profile).await;
    assert_eq!(
        held,
        vec![1, 2],
        "case {case:?}: the refused writers store nothing, so the profile must hold [1, 2], \
         got {held:?}"
    );
}

/// RULE: a lock that was already stale when it was read — the version it would have created
/// was written by no racer holding that lock — is refused with both version numbers, and
/// the refused write stores nothing.
#[tokio::test]
async fn a_lock_stale_against_an_unraced_version_states_both_numbers() {
    let router = router();
    let application = create_application(&router, "lock-stale").await;
    let profile = create_profile(&router, &application, "stale").await;

    // Four unlocked writes, so every version the profile holds was written by a writer
    // holding no lock at all: a later lock is therefore stale rather than beaten.
    for version in 1..=4 {
        let seeded = host(
            &router,
            &application,
            &profile,
            &format!("seed {version}"),
            None,
        )
        .await;
        assert_eq!(
            seeded.status,
            201,
            "seeding version {version} must answer 201, got {} / {}",
            seeded.status,
            String::from_utf8_lossy(&seeded.body)
        );
    }
    let actual = 4;

    // Rows differ along the DISTANCE and DIRECTION between the lock and the version the
    // profile holds: at the floor, just below, just above, and far above.
    for submitted in [0, actual - 1, actual + 1, actual + 97] {
        let case = format!("a lock of {submitted} while {actual} is latest");
        let before = version_numbers(&router, &case, &application, &profile).await;
        let refused = host(
            &router,
            &application,
            &profile,
            &format!("stale {submitted}"),
            Some(submitted),
        )
        .await;
        assert_conflict(
            &case,
            &refused,
            &format!(
                "Expected latest version {submitted} does not match actual latest version \
                 {actual}"
            ),
        );
        let after = version_numbers(&router, &case, &application, &profile).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused write stores nothing, so the profile must still hold \
             {before:?}, got {after:?}"
        );
    }

    // The positive control: the lock that names the version the profile holds is served, so
    // the rows above are refused for the number and nothing else.
    let accepted = host(&router, &application, &profile, "fresh", Some(actual)).await;
    assert_eq!(
        accepted.status,
        201,
        "naming the version the profile holds must be served, got {} / {}",
        accepted.status,
        String::from_utf8_lossy(&accepted.body)
    );
    assert_eq!(
        created_version("the positive control", &accepted),
        i64::from(actual) + 1,
        "the served write must create the version after the lock it pinned"
    );
}
