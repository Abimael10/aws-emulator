// SPDX-License-Identifier: CC0-1.0

//! WHEN the DS Data feature switch takes effect, across the two services that share a
//! directory.
//!
//! `ds:EnableDirectoryDataAccess` and `ds:DisableDirectoryDataAccess` switch a setting of a
//! directory the control plane already holds: neither operation is modelled with a single
//! output member (`smithy_query(query="operation", operation="EnableDirectoryDataAccess")`
//! — input `{DirectoryId}`, output `{}`), so neither reports a status a caller could be told
//! to wait out, and nothing is built or torn down for either. The rule these rows pin is
//! therefore about the INSTANT the switch moves: it is applied by the call that makes it, so
//! the FIRST read after the toggle — `ds:DescribeDirectoryDataAccess` in the control plane
//! and any `ds-data` route in the data plane — already answers for the new position, and the
//! clock never moves it afterwards. The directory's own BUILD is the transition the clock
//! does owe: a toggle is refused from it with `DirectoryUnavailableException`, which is why
//! every row waits for `Stage` `Active` and nothing else.
//!
//! The rows span the class rather than one recorded call: directories differing in edition
//! and short name, each driven through a different NUMBER of enable/disable cycles, with a
//! directory in the same store that is never toggled as the control — so a switch that is
//! not per-directory, or that needs a wait anywhere in the cycle, cannot pass. What the
//! refusal behind a shut directory LOOKS like is the sibling suite
//! (`directory_data_access_cycle`); this one is about when it starts and stops applying.

use serde_json::{json, Value};

use super::directory_data_plane_authorization::{create_microsoft_ad, Answer, Emulator};

/// The `DataAccessStatus` values a toggle settles on, from the modelled enum
/// (`smithy_query(query="shape", shape="DataAccessStatus")` — `Disabled`, `Disabling`,
/// `Enabled`, `Enabling`, `Failed`).
const DISABLED: &str = "Disabled";
const ENABLED: &str = "Enabled";

/// The `DirectoryStage` a directory whose build has finished reports, which is the one state
/// a toggle is admitted from.
const ACTIVE: &str = "Active";

/// The error the data plane refuses a directory whose feature is out of service with
/// (`smithy_query(query="shape", shape="AccessDeniedException")`: `@httpError` 403, member
/// `Reason` of `AccessDeniedReason`).
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const DATA_DISABLED: &str = "DATA_DISABLED";

/// How long the rows give the directory's build before calling it wedged, and how often they
/// look again. A generous bound on a stuck stage rather than a wait for a known duration:
/// every row polls for the CONDITION and stops as soon as it holds.
const BUILD_POLL_ATTEMPTS: usize = 240;
const BUILD_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// One row: how the directory is created, and how many enable/disable cycles it is driven
/// through.
struct Row {
    case: &'static str,
    body: Value,
    cycles: usize,
}

/// The `DirectoryStage` the control plane reports for `directory`.
async fn stage(emulator: &Emulator, directory: &str) -> String {
    let described = emulator
        .control_plane("DescribeDirectories", json!({"DirectoryIds": [directory]}))
        .await;
    described.body["DirectoryDescriptions"][0]["Stage"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `DataAccessStatus` the control plane reports for `directory`.
async fn data_access_status(emulator: &Emulator, directory: &str) -> String {
    let described = emulator
        .control_plane("DescribeDirectoryDataAccess", json!({"DirectoryId": directory}))
        .await;
    described.text("DataAccessStatus")
}

/// Look again until the directory's build has finished, as a client waiting on a create
/// does. The build IS a clock-owed transition, so this is the one wait a row makes.
async fn await_active(emulator: &Emulator, directory: &str, case: &str) {
    for _ in 0..BUILD_POLL_ATTEMPTS {
        if stage(emulator, directory).await == ACTIVE {
            return;
        }
        tokio::time::sleep(BUILD_POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: the directory never reached Stage {ACTIVE}, it stayed in {:?}",
        stage(emulator, directory).await
    );
}

/// Drive one toggle and assert it was accepted with the empty result the model declares.
async fn toggle(emulator: &Emulator, operation: &str, directory: &str, case: &str) {
    let answer = emulator
        .control_plane(operation, json!({"DirectoryId": directory}))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected {operation} to be accepted, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body,
        json!({}),
        "case {case:?}: {operation} has no modelled output members, got {:?}",
        answer.body
    );
}

/// What the data plane answers for `directory` right now: one listing, as the cheapest route
/// that needs the feature.
async fn data_plane_listing(emulator: &Emulator, directory: &str) -> Answer {
    emulator
        .data_plane("/Users/ListUsers", directory, json!({}))
        .await
}

/// Assert the data plane serves `directory`.
fn assert_served(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the data plane to serve the listing, got {} with {:?}",
        answer.status, answer.body
    );
}

/// Assert the data plane refuses `directory` for the feature being out of service.
fn assert_refused(case: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 403,
        "case {case:?}: expected HTTP 403 from the data plane, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.error_code().contains(ACCESS_DENIED_EXCEPTION),
        "case {case:?}: expected {ACCESS_DENIED_EXCEPTION}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body.get("Reason").and_then(Value::as_str),
        Some(DATA_DISABLED),
        "case {case:?}: expected Reason {DATA_DISABLED}, got {:?}",
        answer.body
    );
}

/// Assert both planes agree that `directory`'s feature stands where `enabled` says, reading
/// each of them exactly once.
async fn assert_both_planes(emulator: &Emulator, directory: &str, enabled: bool, case: &str) {
    let listing = data_plane_listing(emulator, directory).await;
    let reported = data_access_status(emulator, directory).await;
    let expected = if enabled { ENABLED } else { DISABLED };
    assert_eq!(
        reported, expected,
        "case {case:?}: expected DataAccessStatus {expected}, got {reported:?}"
    );
    if enabled {
        assert_served(case, &listing);
    } else {
        assert_refused(case, &listing);
    }
}

/// The table: every row's toggle takes effect in the call that makes it, in both directions
/// and however many times the cycle is repeated, while the never-toggled control stays shut.
#[tokio::test]
async fn each_data_access_toggle_takes_effect_in_the_call_that_makes_it() {
    let emulator = Emulator::new();

    let rows = [
        Row {
            case: "a minimal Managed Microsoft AD, one cycle",
            body: create_microsoft_ad("one.example.com"),
            cycles: 1,
        },
        Row {
            case: "a Standard-edition directory with a short name, two cycles",
            body: {
                let mut body = create_microsoft_ad("two.example.com");
                body["Edition"] = json!("Standard");
                body["ShortName"] = json!("TWO");
                body
            },
            cycles: 2,
        },
        Row {
            case: "an Enterprise-edition directory, three cycles",
            body: {
                let mut body = create_microsoft_ad("three.example.com");
                body["Edition"] = json!("Enterprise");
                body
            },
            cycles: 3,
        },
    ];
    let control = Row {
        case: "the control directory, never toggled",
        body: create_microsoft_ad("control.example.com"),
        cycles: 0,
    };

    // Every directory of the table plus the control, in ONE store, so a status that is not
    // per-directory cannot pass.
    let mut created: Vec<(&Row, String)> = Vec::new();
    for row in rows.iter().chain(std::iter::once(&control)) {
        let answer = emulator
            .control_plane("CreateMicrosoftAD", row.body.clone())
            .await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: CreateMicrosoftAD must be accepted, got {} with {:?}",
            row.case, answer.status, answer.body
        );
        created.push((row, answer.text("DirectoryId")));
    }
    for (row, directory) in &created {
        await_active(&emulator, directory, row.case).await;
        // A create opens nothing: the switch, and only the switch, does.
        assert_both_planes(&emulator, directory, false, row.case).await;
    }

    for (row, directory) in &created {
        for cycle in 1..=row.cycles {
            let case = format!("{} (cycle {cycle} enable)", row.case);
            toggle(&emulator, "EnableDirectoryDataAccess", directory, &case).await;
            assert_both_planes(&emulator, directory, true, &case).await;

            let case = format!("{} (cycle {cycle} disable)", row.case);
            toggle(&emulator, "DisableDirectoryDataAccess", directory, &case).await;
            assert_both_planes(&emulator, directory, false, &case).await;
        }
    }

    // One last enable, then every directory is read again with the other rows' reads in
    // between: each is exactly where its own last call left it — the toggled ones enabled and
    // the control still shut — because a toggle leaves nothing for a later call or the clock
    // to settle.
    for (row, directory) in &created {
        if row.cycles > 0 {
            let case = format!("{} (the closing enable)", row.case);
            toggle(&emulator, "EnableDirectoryDataAccess", directory, &case).await;
        }
    }
    for (row, directory) in &created {
        let case = format!("{} (read again after every row's toggles)", row.case);
        assert_both_planes(&emulator, directory, row.cycles > 0, &case).await;
    }
}

/// The switch is refused while the directory is still being BUILT — the one transition here
/// the clock does owe — and is admitted, and in effect, as soon as that build has finished.
#[tokio::test]
async fn the_switch_waits_for_the_build_and_for_nothing_else() {
    let emulator = Emulator::new();

    let created = emulator
        .control_plane("CreateMicrosoftAD", create_microsoft_ad("build.example.com"))
        .await;
    assert_eq!(
        created.status, 200,
        "CreateMicrosoftAD must be accepted, got {} with {:?}",
        created.status, created.body
    );
    let directory = created.text("DirectoryId");

    // The create answers while the directory is still being built, so the toggle is refused
    // for the directory being out of service rather than for its feature's position.
    let refused = emulator
        .control_plane(
            "EnableDirectoryDataAccess",
            json!({"DirectoryId": &directory}),
        )
        .await;
    assert_eq!(
        refused.status, 400,
        "case a directory still being built: expected the enable to be refused, got {} with \
         {:?}",
        refused.status, refused.body
    );
    assert!(
        refused.error_code().contains("DirectoryUnavailableException"),
        "case a directory still being built: expected DirectoryUnavailableException, got {:?}",
        refused.body
    );
    assert_eq!(
        data_access_status(&emulator, &directory).await,
        DISABLED,
        "case a refused enable: expected the feature to be left at {DISABLED}"
    );

    // Once the build has finished the very same call is admitted, and the feature is in
    // service for the request that follows it.
    await_active(&emulator, &directory, "a directory whose build has finished").await;
    toggle(
        &emulator,
        "EnableDirectoryDataAccess",
        &directory,
        "a directory whose build has finished",
    )
    .await;
    assert_both_planes(
        &emulator,
        &directory,
        true,
        "a directory whose build has finished",
    )
    .await;

    // And a second enable asks for the position the first one already put it in.
    let again = emulator
        .control_plane(
            "EnableDirectoryDataAccess",
            json!({"DirectoryId": &directory}),
        )
        .await;
    assert_eq!(
        again.status, 400,
        "case a second enable: expected it to be refused, got {} with {:?}",
        again.status, again.body
    );
    assert!(
        again
            .error_code()
            .contains("DirectoryInDesiredStateException"),
        "case a second enable: expected DirectoryInDesiredStateException, got {:?}",
        again.body
    );
    assert_both_planes(&emulator, &directory, true, "after the refused second enable").await;
}
