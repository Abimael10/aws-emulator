// SPDX-License-Identifier: CC0-1.0

//! What a CLIENT spends to observe the DS Data feature switch, across the two services that
//! share a directory.
//!
//! `ds:EnableDirectoryDataAccess` and `ds:DisableDirectoryDataAccess` switch a setting of a
//! directory the control plane already holds — neither is modelled with a single output
//! member (`smithy_query(query="operation", operation="EnableDirectoryDataAccess")`: input
//! `{DirectoryId}`, output `{}`), so neither reports a status a caller could be told to wait
//! out, and nothing is built or torn down for either. The rule these rows pin is the COST
//! that has for a client: because the switch is applied by the call that moves it, the FIRST
//! read after a toggle — `ds:DescribeDirectoryDataAccess` in the control plane, any `ds-data`
//! route in the data plane — already answers for the new position, so a client that waits for
//! the switch spends exactly ONE read per toggle and never a second look. The directory's own
//! BUILD is the transition the clock does owe, and the only leg of a cycle a client polls,
//! which is why every row waits for `Stage` `Active` and for nothing else.
//!
//! Each row therefore COUNTS the reads a leg costs instead of only checking the value they
//! report: a switch that settled a moment after its call would still satisfy a client that
//! polls, and is exactly what costs it an extra interval per leg — here it needs a second
//! read and the case fails. The rows span the class along the cycle's LENGTH (one, two and
//! three disable/enable round trips) and along the `ds-data` resource family the switch is
//! observed through (a user listing, a group listing, a membership projection), with a
//! directory in the same store that is never toggled as the control, so a switch that is not
//! per-directory, or that needs a wait for one family and not another, cannot pass. The
//! objects the cycle is driven around are read back at the end, so a cycle that bought its
//! cheap reads by losing the directory's contents fails too.
//!
//! What the refusal behind a shut directory LOOKS like, and the whole sentence it carries, is
//! the sibling suite `directory_data_access_cycle`; what the first read after a leg reports
//! when NOTHING polls is `directory_data_access_cycle_unpolled`.

use std::time::Duration;

use serde_json::{json, Value};

use super::directory_data_plane_authorization::{create_microsoft_ad, Answer, Emulator};

/// The settled `DataAccessStatus` values a toggle moves a directory between, from the
/// modelled enum (`smithy_query(query="shape", shape="DataAccessStatus")` — `Disabled`,
/// `Disabling`, `Enabled`, `Enabling`, `Failed`).
const DISABLED: &str = "Disabled";
const ENABLED: &str = "Enabled";

/// The `DirectoryStage` a directory whose build has finished reports, which is the one state
/// a toggle is admitted from.
const ACTIVE: &str = "Active";

/// The error a data-plane call gets for a directory whose feature is out of service, and the
/// modelled `AccessDeniedReason` it carries (`smithy_query(query="shape",
/// shape="AccessDeniedException")`: `@httpError` 403, members `Message` / `Reason`;
/// `AccessDeniedReason` is `IAM_AUTH`, `DIRECTORY_AUTH`, `DATA_DISABLED`).
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const DATA_DISABLED: &str = "DATA_DISABLED";

/// The reads a leg of the cycle must settle within, and how long a row waits between looks.
///
/// A generous bound on a WEDGED status rather than a wait for a known duration: a leg that
/// needs more than the first read has already failed its case, and the remaining looks only
/// make the failure message name what the status ended up as.
const LOOK_AGAIN: Duration = Duration::from_millis(250);
const SETTLE_ATTEMPTS: usize = 120;

/// How many looks the directory's build — the one clock-owed leg here — gets before a row
/// calls it wedged. The product is a minute, many times over the chain the control plane
/// publishes, so it bounds a stuck stage rather than timing a transition.
const BUILD_POLL_ATTEMPTS: usize = 240;

/// The reads a leg of the switch may cost a client: the toggle is applied by the call, so the
/// first read after it already answers for the new position.
const READS_PER_LEG: usize = 1;

/// The objects every row's cycle is driven around: a user, a group and the membership edge
/// between them, all created while the feature is in service.
const USER: &str = "costSurvivor";
const GROUP: &str = "costGroupSurvivor";

/// One row: how the directory is created, how many disable/enable round trips it is driven
/// through, and the `ds-data` route the switch is observed through for it.
struct Row {
    case: &'static str,
    body: Value,
    cycles: usize,
    route: &'static str,
    probe: Value,
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
        .control_plane(
            "DescribeDirectoryDataAccess",
            json!({"DirectoryId": directory}),
        )
        .await;
    described.text("DataAccessStatus")
}

/// Look again until the directory's build has finished, as a client waiting on a create does.
/// The build IS a clock-owed transition, so this is the one wait a row makes.
async fn await_active(emulator: &Emulator, directory: &str, case: &str) {
    for _ in 0..BUILD_POLL_ATTEMPTS {
        if stage(emulator, directory).await == ACTIVE {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!(
        "case {case:?}: the directory never reached Stage {ACTIVE}, it stayed in {:?}",
        stage(emulator, directory).await
    );
}

/// How many control-plane reads a client spends before `directory` reports `wanted`.
async fn reads_until_status(
    emulator: &Emulator,
    directory: &str,
    wanted: &str,
    case: &str,
) -> usize {
    let mut reads = 0;
    let mut last = String::new();
    for _ in 0..SETTLE_ATTEMPTS {
        reads += 1;
        last = data_access_status(emulator, directory).await;
        if last == wanted {
            return reads;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!(
        "case {case:?}: expected DataAccessStatus {wanted} within {reads} reads, the last one \
         reported {last:?}"
    )
}

/// How many data-plane calls a client spends on `route` before it is answered the way `served`
/// says — served while the feature is in service, refused for `DATA_DISABLED` while it is not.
///
/// Answers the last response as well, so a case can assert on what a served route reported.
async fn calls_until_answered(
    emulator: &Emulator,
    directory: &str,
    row: &Row,
    served: bool,
    case: &str,
) -> (usize, Answer) {
    let mut calls = 0;
    let mut last = emulator
        .data_plane(row.route, directory, row.probe.clone())
        .await;
    loop {
        calls += 1;
        let refused = last.status == 403
            && last.body.get("Reason").and_then(Value::as_str) == Some(DATA_DISABLED);
        let matches = if served { last.status == 200 } else { refused };
        if matches || calls >= SETTLE_ATTEMPTS {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
        last = emulator
            .data_plane(row.route, directory, row.probe.clone())
            .await;
    }
    let wanted = if served { "served" } else { "refused" };
    assert!(
        calls < SETTLE_ATTEMPTS,
        "case {case:?}: expected {} to be {wanted} within {calls} calls, the last one answered \
         {} with {:?}",
        row.route,
        last.status,
        last.body
    );
    (calls, last)
}

/// Assert `answer` is the refusal a directory out of service gets: HTTP 403,
/// `AccessDeniedException` and `Reason` `DATA_DISABLED`.
fn assert_data_disabled(case: &str, route: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 403,
        "case {case:?}: expected HTTP 403 from {route}, got {} with {:?}",
        answer.status, answer.body
    );
    assert!(
        answer.error_code().contains(ACCESS_DENIED_EXCEPTION),
        "case {case:?}: expected {ACCESS_DENIED_EXCEPTION} from {route}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body.get("Reason").and_then(Value::as_str),
        Some(DATA_DISABLED),
        "case {case:?}: expected Reason {DATA_DISABLED} from {route}, got {:?}",
        answer.body
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

/// One served data-plane call that must succeed, returning its answer.
async fn served(
    emulator: &Emulator,
    route: &str,
    directory: &str,
    body: Value,
    case: &str,
) -> Answer {
    let answer = emulator.data_plane(route, directory, body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected {route} to be served, got {} with {:?}",
        answer.status, answer.body
    );
    answer
}

/// Fill the directory with the user, the group and the membership edge the cycle is driven
/// around, and answer the SIDs the data plane minted for the user and the group.
async fn fill(emulator: &Emulator, directory: &str, case: &str) -> (String, String) {
    let user = served(
        emulator,
        "/Users/CreateUser",
        directory,
        json!({"SAMAccountName": USER, "GivenName": "Read", "Surname": "Cost"}),
        case,
    )
    .await;
    let group = served(
        emulator,
        "/Groups/CreateGroup",
        directory,
        json!({"SAMAccountName": GROUP}),
        case,
    )
    .await;
    served(
        emulator,
        "/GroupMemberships/AddGroupMember",
        directory,
        json!({"GroupName": GROUP, "MemberName": USER}),
        case,
    )
    .await;
    let user_sid = user.text("SID");
    let group_sid = group.text("SID");
    assert!(
        !user_sid.is_empty() && !group_sid.is_empty(),
        "case {case:?}: expected the created objects to carry SIDs, got {:?} and {:?}",
        user.body,
        group.body
    );
    (user_sid, group_sid)
}

/// RULE: each leg of the switch costs a client exactly one read of either plane, because the
/// toggle is applied by the call that makes it — over cycles of three different lengths, three
/// `ds-data` resource families and a control directory nothing toggles.
#[tokio::test]
async fn each_leg_of_the_switch_costs_one_read_of_either_plane() {
    let emulator = Emulator::new();

    let rows = [
        Row {
            case: "a minimal Managed Microsoft AD observed through its user listing, one cycle",
            body: create_microsoft_ad("cost-one.example.com"),
            cycles: 1,
            route: "/Users/ListUsers",
            probe: json!({}),
        },
        Row {
            case: "a Standard-edition directory observed through its group listing, two cycles",
            body: {
                let mut body = create_microsoft_ad("cost-two.example.com");
                body["Edition"] = json!("Standard");
                body["ShortName"] = json!("COSTTWO");
                body
            },
            cycles: 2,
            route: "/Groups/ListGroups",
            probe: json!({}),
        },
        Row {
            case: "an Enterprise-edition directory observed through a membership projection, \
                   three cycles",
            body: {
                let mut body = create_microsoft_ad("cost-three.example.com");
                body["Edition"] = json!("Enterprise");
                body
            },
            cycles: 3,
            route: "/GroupMemberships/ListGroupMembers",
            probe: json!({"SAMAccountName": GROUP}),
        },
    ];
    let control = Row {
        case: "the control directory, never toggled",
        body: create_microsoft_ad("cost-control.example.com"),
        cycles: 0,
        route: "/Users/ListUsers",
        probe: json!({}),
    };

    // Every directory of the table plus the control in ONE store, so a switch that is not
    // per-directory cannot pass, and the builds of all of them are waited out together.
    let mut created: Vec<(&Row, String)> = Vec::new();
    for row in rows.iter().chain(std::iter::once(&control)) {
        let answer = emulator
            .control_plane("CreateMicrosoftAD", row.body.clone())
            .await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected CreateMicrosoftAD to be accepted, got {} with {:?}",
            row.case, answer.status, answer.body
        );
        created.push((row, answer.text("DirectoryId")));
    }
    for (row, directory) in &created {
        await_active(&emulator, directory, row.case).await;
        // A create opens nothing: the switch, and only the switch, does.
        let reported = data_access_status(&emulator, directory).await;
        assert_eq!(
            reported, DISABLED,
            "case {:?}: expected a freshly built directory to report {DISABLED}, got \
             {reported:?}",
            row.case
        );
        let shut = emulator
            .data_plane(row.route, directory, row.probe.clone())
            .await;
        assert_data_disabled(row.case, row.route, &shut);
    }

    // The first enable of each row, and the objects its cycle is driven around. Both planes
    // are read ONCE for the leg: the count is the assertion.
    let mut identities: Vec<(String, String)> = Vec::new();
    for (row, directory) in &created {
        if row.cycles == 0 {
            identities.push((String::new(), String::new()));
            continue;
        }
        let case = format!("{} (the opening enable)", row.case);
        toggle(&emulator, "EnableDirectoryDataAccess", directory, &case).await;
        let reads = reads_until_status(&emulator, directory, ENABLED, &case).await;
        assert_eq!(
            reads, READS_PER_LEG,
            "case {case:?}: expected the control plane to report {ENABLED} on the first read \
             after the enable, it took {reads}"
        );
        identities.push(fill(&emulator, directory, &case).await);
    }

    // Every row's round trips: each toggle is followed by one read of each plane, and the
    // count of reads that leg cost is what every row asserts.
    for (index, (row, directory)) in created.iter().enumerate() {
        for cycle in 1..=row.cycles {
            for (operation, wanted, is_served) in [
                ("DisableDirectoryDataAccess", DISABLED, false),
                ("EnableDirectoryDataAccess", ENABLED, true),
            ] {
                let case = format!("{} (cycle {cycle}, {operation})", row.case);
                toggle(&emulator, operation, directory, &case).await;

                let reads = reads_until_status(&emulator, directory, wanted, &case).await;
                assert_eq!(
                    reads, READS_PER_LEG,
                    "case {case:?}: expected the control plane to report {wanted} on the \
                     first read after the toggle, it took {reads}"
                );

                let (calls, answer) =
                    calls_until_answered(&emulator, directory, row, is_served, &case).await;
                assert_eq!(
                    calls, READS_PER_LEG,
                    "case {case:?}: expected {} to follow the toggle on the first call, it \
                     took {calls}",
                    row.route
                );
                if !is_served {
                    assert_data_disabled(&case, row.route, &answer);
                }

                // The switch moves the data plane, not the directory.
                let stage_now = stage(&emulator, directory).await;
                assert_eq!(
                    stage_now, ACTIVE,
                    "case {case:?}: expected the directory to stay {ACTIVE} while its data \
                     plane moves, got {stage_now:?}"
                );
            }
        }

        // The cycle kept the directory's contents: the SIDs the data plane minted cannot be
        // re-derived, so a cycle that lost the objects answers with different ones.
        if row.cycles == 0 {
            continue;
        }
        let (user_sid, group_sid) = &identities[index];
        let case = format!("{} (the objects after every cycle)", row.case);
        let user = served(
            &emulator,
            "/Users/DescribeUser",
            directory,
            json!({"SAMAccountName": USER}),
            &case,
        )
        .await;
        assert_eq!(
            &user.text("SID"),
            user_sid,
            "case {case:?}: expected the same user across the cycles, got {:?}",
            user.body
        );
        let group = served(
            &emulator,
            "/Groups/DescribeGroup",
            directory,
            json!({"SAMAccountName": GROUP}),
            &case,
        )
        .await;
        assert_eq!(
            &group.text("SID"),
            group_sid,
            "case {case:?}: expected the same group across the cycles, got {:?}",
            group.body
        );
        let members = served(
            &emulator,
            "/GroupMemberships/ListGroupMembers",
            directory,
            json!({"SAMAccountName": GROUP}),
            &case,
        )
        .await;
        let listed: Vec<String> = members.body["Members"]
            .as_array()
            .map(|members| {
                members
                    .iter()
                    .filter_map(|member| member["SAMAccountName"].as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            listed.iter().any(|member| member == USER),
            "case {case:?}: expected {USER:?} among the group's members, got {listed:?}"
        );
    }

    // Every row ends where its own last toggle left it, read after all the other rows' legs:
    // the ones whose cycle ended on an enable in service, the never-toggled control still shut.
    for (row, directory) in &created {
        let expected = if row.cycles > 0 { ENABLED } else { DISABLED };
        let reported = data_access_status(&emulator, directory).await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected {expected} after every row's legs, got {reported:?}",
            row.case
        );
        let answer = emulator
            .data_plane(row.route, directory, row.probe.clone())
            .await;
        if row.cycles > 0 {
            assert_eq!(
                answer.status, 200,
                "case {:?}: expected {} to be served after every row's legs, got {} with {:?}",
                row.case, row.route, answer.status, answer.body
            );
        } else {
            assert_data_disabled(row.case, row.route, &answer);
        }
    }
}
