// SPDX-License-Identifier: CC0-1.0

//! Modifying a maintenance window already created: `UpdateMaintenanceWindow` edits what
//! `CreateMaintenanceWindow` persisted, and `GetMaintenanceWindow` reports what the edit
//! left behind.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a response
//! that is not computed from persisted state cannot pass: every read follows a write.
//!
//! The rules under test, each asserted over a TABLE whose rows differ along what the rule
//! generalises over rather than over one recorded request:
//!
//! 1. **An update is a PATCH of the stored window.** "Only specified parameters are
//!    modified": every member the request supplies is applied and every member it leaves out
//!    keeps the value the window holds, and what the update reports is what a later read of
//!    that window reports.
//! 2. **A REPLACING update clears what it leaves out.** Under `Replace`, "optional fields
//!    that aren't specified are set to null", while the members `CreateMaintenanceWindow`
//!    requires are required here too — so a replacing update leaving one of them out is
//!    refused, naming each member it left out, and writes nothing.
//! 3. **The window's identity does not move.** No input member carries the minted id or the
//!    instant the create recorded, so an update leaves both as the create persisted them,
//!    mints no window and leaves the order a listing walks the account's windows in.
//! 4. **The id is resolved against the Region's records.** An id no create minted, a window
//!    of another Region and a window a delete already removed are each refused
//!    `DoesNotExistException`, and a refused update writes nothing.
//! 5. **A write of a window is held to the rules the create is held to.** The schedule, the
//!    zone it is read in and the cutoff taken against the duration are judged on the window
//!    the update WOULD leave behind, so a window that could never open cannot be updated
//!    into existence either.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// A second Region, so a window is shown to be reachable only through the endpoint of the
/// Region it lives in.
const OTHER_REGION: &str = "eu-central-1";

/// A `WindowId` of the form the modelled `MaintenanceWindowId` pattern requires
/// (`^mw-[0-9a-f]{17}$`) that no create has minted.
const UNKNOWN_WINDOW_ID: &str = "mw-00000000000000000";

/// The members a window carries only when a write stated them, which a replacing update
/// clears: the optional members of `UpdateMaintenanceWindowRequest` that are not required by
/// `CreateMaintenanceWindow`
/// (`smithy_query(query="operation",operation="UpdateMaintenanceWindow,CreateMaintenanceWindow")`).
const CLEARABLE_MEMBERS: [&str; 5] = [
    "Description",
    "StartDate",
    "EndDate",
    "ScheduleTimezone",
    "ScheduleOffset",
];

/// The members `CreateMaintenanceWindow` requires, in the spelling this service's refusals
/// carry a member in — so a replacing update that leaves one out is refused by name
/// (`smithy_query(query="operation",operation="CreateMaintenanceWindow")`).
const REQUIRED_MEMBERS: [&str; 5] = [
    "name",
    "schedule",
    "duration",
    "cutoff",
    "allowUnassociatedTargets",
];

/// A create body carrying every member a window can hold, so a row that omits one shows what
/// an omitted member keeps and a replacement that omits one shows what it clears.
fn created(name: &str) -> Value {
    json!({
        "Name": name,
        "Schedule": "cron(0 3 ? * SUN *)",
        "ScheduleTimezone": "Europe/Dublin",
        "ScheduleOffset": 2,
        "Duration": 4,
        "Cutoff": 1,
        "AllowUnassociatedTargets": false,
        "Description": "the created description",
        "StartDate": "2030-01-01T00:00:00",
        "EndDate": "2031-01-01T00:00:00",
    })
}

/// What a read of the window `created` minted reports: its create's members, plus the
/// `Enabled` a created window carries (`CreateMaintenanceWindow` has no `Enabled` member at
/// all, so only an update can disable one).
fn created_members(name: &str) -> Value {
    let mut expected = created(name);
    expected["Enabled"] = json!(true);
    expected
}

/// `base` with every member `changes` names set to what it names.
fn with(base: &Value, changes: Value) -> Value {
    let mut merged = base.clone();
    for (member, value) in changes.as_object().expect("the changed members") {
        merged[member.as_str()] = value.clone();
    }
    merged
}

/// `base` without any of `members`, which is how a window that carries nothing for one of
/// them is expected to read.
fn without(base: &Value, members: &[&str]) -> Value {
    let mut kept = base.clone();
    let object = kept.as_object_mut().expect("the expected members");
    for member in members {
        object.remove(*member);
    }
    kept
}

/// Create a window at `region`'s endpoint with `body` and answer the id it minted.
async fn create(router: &MultiServiceRouter, case: &str, region: &str, body: &Value) -> String {
    let (status, response) = call_in_region(router, region, "CreateMaintenanceWindow", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateMaintenanceWindow {body} must succeed, got {status} {response}"
    );
    response["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a WindowId, got {response}"))
        .to_string()
}

/// `UpdateMaintenanceWindow` at `region`'s endpoint with `body`.
async fn update(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "UpdateMaintenanceWindow", body).await
}

/// The same update, for the window `window_id`, carrying `body`'s members.
async fn update_for(
    router: &MultiServiceRouter,
    region: &str,
    window_id: &str,
    body: &Value,
) -> (u16, Value) {
    let mut request = body.clone();
    request["WindowId"] = Value::String(window_id.to_string());
    update(router, region, &request).await
}

/// `GetMaintenanceWindow` at `region`'s endpoint for `window_id`.
async fn get(router: &MultiServiceRouter, region: &str, window_id: &str) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "GetMaintenanceWindow",
        &json!({ "WindowId": window_id }),
    )
    .await
}

/// The same read, asserting it succeeded.
async fn read(router: &MultiServiceRouter, case: &str, region: &str, window_id: &str) -> Value {
    let (status, response) = get(router, region, window_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetMaintenanceWindow {window_id:?} must succeed, got {status} {response}"
    );
    response
}

/// Assert that `reported` carries every member `expected` names, and nothing for a member
/// `expected` leaves out.
fn assert_members(case: &str, reported: &Value, expected: &Value) {
    for (member, value) in expected.as_object().expect("the expected members") {
        assert_eq!(
            &reported[member.as_str()], value,
            "case {case:?}: expected {member} {value}, got {reported}"
        );
    }
    for member in CLEARABLE_MEMBERS {
        if expected.get(member).is_none() {
            assert!(
                reported.get(member).is_none_or(Value::is_null),
                "case {case:?}: expected no {member} to be carried, got {reported}"
            );
        }
    }
}

/// RULE 1: an update is a patch of the stored window — a member supplied is applied, a member
/// left out keeps what the window holds — and what the update reports is what a later read
/// reports.
///
/// The rows differ along what the rule generalises over: which members the update supplies
/// (a label, the schedule, the span, a flag, the bounding instants, the days the schedule
/// waits, nothing at all), the form the new schedule takes, and whether the update states
/// `Replace` as false or leaves it out.
#[tokio::test]
async fn an_update_applies_what_it_carries_and_keeps_what_it_omits() {
    let router = router();
    let base = created_members("window-patches");

    // (case, the update's members, the members the window must carry afterwards)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "the name alone",
            json!({ "Name": "patched-name" }),
            with(&base, json!({ "Name": "patched-name" })),
        ),
        (
            "the description alone",
            json!({ "Description": "the patched description" }),
            with(&base, json!({ "Description": "the patched description" })),
        ),
        (
            "a rate schedule in place of the cron, read in another zone",
            json!({ "Schedule": "rate(7 days)", "ScheduleTimezone": "America/Los_Angeles" }),
            with(
                &base,
                json!({ "Schedule": "rate(7 days)", "ScheduleTimezone": "America/Los_Angeles" }),
            ),
        ),
        (
            "the span of the window",
            json!({ "Duration": 6, "Cutoff": 2 }),
            with(&base, json!({ "Duration": 6, "Cutoff": 2 })),
        ),
        (
            "the window disabled",
            json!({ "Enabled": false }),
            with(&base, json!({ "Enabled": false })),
        ),
        (
            "the unassociated-targets flag",
            json!({ "AllowUnassociatedTargets": true }),
            with(&base, json!({ "AllowUnassociatedTargets": true })),
        ),
        (
            "the instants the window is bounded by",
            json!({ "StartDate": "2032-03-01T00:00:00", "EndDate": "2032-04-01T00:00:00" }),
            with(
                &base,
                json!({ "StartDate": "2032-03-01T00:00:00", "EndDate": "2032-04-01T00:00:00" }),
            ),
        ),
        (
            "the days the schedule waits",
            json!({ "ScheduleOffset": 5 }),
            with(&base, json!({ "ScheduleOffset": 5 })),
        ),
        (
            "nothing at all, which modifies nothing",
            json!({}),
            base.clone(),
        ),
        (
            "a member stated beside Replace as false",
            json!({ "Replace": false, "Name": "patched-beside-replace" }),
            with(&base, json!({ "Name": "patched-beside-replace" })),
        ),
    ];

    for (why, body, expected) in rows {
        let window_id = create(&router, why, REGION, &created("window-patches")).await;
        let (status, response) = update_for(&router, REGION, &window_id, &body).await;
        assert_eq!(
            status, 200,
            "case {why:?}: the update {body} must be served, got {status} {response}"
        );
        assert_eq!(
            response["WindowId"].as_str(),
            Some(window_id.as_str()),
            "case {why:?}: expected the update to report the id it was asked about, got {response}"
        );
        assert_members(why, &response, &expected);

        // The read back is the same answer: the update persisted it.
        let held = read(&router, why, REGION, &window_id).await;
        assert_members(why, &held, &expected);
    }
}

/// RULE 2: a replacing update clears every optional member it leaves out, and carries the
/// members the create requires.
///
/// The rows differ along which optional members the replacement keeps: all of them, one of
/// them, none of them.
#[tokio::test]
async fn a_replacing_update_clears_the_members_it_leaves_out() {
    let router = router();
    let name = "window-replacements";

    // What every replacement in the table states, being the members the create requires.
    let required = json!({
        "Replace": true,
        "Name": "replaced-name",
        "Schedule": "cron(0 6 ? * MON *)",
        "Duration": 8,
        "Cutoff": 3,
        "AllowUnassociatedTargets": true,
    });
    // The same members as a read reports them, which every row below expects.
    let replaced = json!({
        "Name": "replaced-name",
        "Schedule": "cron(0 6 ? * MON *)",
        "Duration": 8,
        "Cutoff": 3,
        "AllowUnassociatedTargets": true,
    });

    // (case, the replacing update's members, the members the window must carry afterwards)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a replacement carrying every member",
            with(
                &required,
                json!({
                    "Description": "the replacing description",
                    "StartDate": "2033-01-01T00:00:00",
                    "EndDate": "2034-01-01T00:00:00",
                    "ScheduleTimezone": "Asia/Seoul",
                    "ScheduleOffset": 4,
                    "Enabled": false,
                }),
            ),
            with(
                &replaced,
                json!({
                    "Description": "the replacing description",
                    "StartDate": "2033-01-01T00:00:00",
                    "EndDate": "2034-01-01T00:00:00",
                    "ScheduleTimezone": "Asia/Seoul",
                    "ScheduleOffset": 4,
                    "Enabled": false,
                }),
            ),
        ),
        (
            "a replacement carrying one optional member",
            with(&required, json!({ "Description": "the only one kept" })),
            with(
                &replaced,
                json!({ "Description": "the only one kept", "Enabled": true }),
            ),
        ),
        (
            "a replacement carrying no optional member at all",
            required.clone(),
            with(&replaced, json!({ "Enabled": true })),
        ),
    ];

    for (why, body, expected) in rows {
        let window_id = create(&router, why, REGION, &created(name)).await;
        let (status, response) = update_for(&router, REGION, &window_id, &body).await;
        assert_eq!(
            status, 200,
            "case {why:?}: the replacing update {body} must be served, got {status} {response}"
        );
        assert_members(why, &response, &expected);
        let held = read(&router, why, REGION, &window_id).await;
        assert_members(why, &held, &expected);
    }

    // A replacement states nothing for `Enabled`, which no create carries either — so it
    // leaves the enabled window a create mints, whatever an earlier update made of it.
    let why = "a replacement of a window an earlier update disabled";
    let window_id = create(&router, why, REGION, &created(name)).await;
    let (status, response) =
        update_for(&router, REGION, &window_id, &json!({ "Enabled": false })).await;
    assert_eq!(
        status, 200,
        "case {why:?}: the update that disables the window must be served, got {status} \
         {response}"
    );
    let (status, response) = update_for(&router, REGION, &window_id, &required).await;
    assert_eq!(
        status, 200,
        "case {why:?}: the replacing update must be served, got {status} {response}"
    );
    let held = read(&router, why, REGION, &window_id).await;
    assert_members(why, &held, &with(&replaced, json!({ "Enabled": true })));
}

/// RULE 2, the other half: a replacing update that leaves out a member
/// `CreateMaintenanceWindow` requires is refused, naming every member it left out, and
/// writes nothing.
///
/// The rows differ along how many of those members the replacement leaves out: all of them,
/// all but one, one.
#[tokio::test]
async fn a_replacing_update_requires_what_the_create_requires() {
    let router = router();
    let name = "window-replacement-refusals";
    let full = json!({
        "Name": "replaced-name",
        "Schedule": "cron(0 6 ? * MON *)",
        "Duration": 8,
        "Cutoff": 3,
        "AllowUnassociatedTargets": true,
    });

    // (case, the members the replacement states beside `Replace`, the members it leaves out)
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "a replacement carrying no member at all",
            json!({}),
            REQUIRED_MEMBERS.to_vec(),
        ),
        (
            "a replacement naming the window alone",
            json!({ "Name": "replaced-name" }),
            vec!["schedule", "duration", "cutoff", "allowUnassociatedTargets"],
        ),
        (
            "a replacement leaving out the cutoff",
            without(&full, &["Cutoff"]),
            vec!["cutoff"],
        ),
        (
            "a replacement leaving out the unassociated-targets flag",
            without(&full, &["AllowUnassociatedTargets"]),
            vec!["allowUnassociatedTargets"],
        ),
    ];

    for (why, stated, missing) in rows {
        let window_id = create(&router, why, REGION, &created(name)).await;
        let body = with(&stated, json!({ "Replace": true }));
        let (status, response) = update_for(&router, REGION, &window_id, &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {why:?}: expected the replacement {body} to be refused, got {status} {response}"
        );
        let message = error_message(&response);
        for member in &missing {
            assert!(
                message.contains(&format!("'{member}'")),
                "case {why:?}: expected the refusal to name the missing member {member:?}, got \
                 {message:?}"
            );
        }
        for member in REQUIRED_MEMBERS {
            if !missing.contains(&member) {
                assert!(
                    !message.contains(&format!("'{member}'")),
                    "case {why:?}: expected the refusal to name only the members it wanted, got \
                     {message:?} naming {member:?}"
                );
            }
        }

        // A refused update wrote nothing: the window still reads as its create left it.
        let held = read(&router, why, REGION, &window_id).await;
        assert_members(why, &held, &created_members(name));
    }
}

/// RULE 3 and RULE 4: an update leaves the window's identity alone and resolves the id
/// against the Region's records — an id no create minted, a window of another Region and a
/// window a delete already removed are each refused, and a refused update writes nothing.
#[tokio::test]
async fn an_update_keeps_the_identity_and_resolves_the_id_against_the_region() {
    let router = router();
    let case = "the identity and refusal table";
    let here = create(&router, case, REGION, &created("window-identity")).await;
    let next = create(&router, case, REGION, &created("window-identity-next")).await;
    let elsewhere = create(&router, case, OTHER_REGION, &created("window-elsewhere")).await;
    let deleted = create(&router, case, REGION, &created("window-deleted")).await;

    // The instant the create recorded, which no update may move.
    let minted = read(&router, case, REGION, &here).await;
    let created_date = minted["CreatedDate"].clone();

    let (status, response) = update_for(
        &router,
        REGION,
        &here,
        &json!({ "Name": "window-identity-renamed", "Duration": 5 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the update must be served, got {status} {response}"
    );
    let held = read(&router, case, REGION, &here).await;
    assert_eq!(
        (held["WindowId"].as_str(), &held["CreatedDate"]),
        (Some(here.as_str()), &created_date),
        "case {case:?}: expected the update to leave the minted id and the created instant, got \
         {held}"
    );

    // The window a delete removed, which an update can no longer reach.
    let (status, response) = call_in_region(
        &router,
        REGION,
        "DeleteMaintenanceWindow",
        &json!({ "WindowId": deleted }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteMaintenanceWindow {deleted:?} must succeed, got {status} {response}"
    );

    // (case, the Region the update is signed for, the window it names)
    let rows: Vec<(&str, &str, &str)> = vec![
        ("a window id no create minted", REGION, UNKNOWN_WINDOW_ID),
        ("a window of another Region", REGION, elsewhere.as_str()),
        ("this Region's window at the other endpoint", OTHER_REGION, here.as_str()),
        ("a window a delete already removed", REGION, deleted.as_str()),
    ];

    for (why, region, window_id) in rows {
        let (status, response) = update_for(
            &router,
            region,
            window_id,
            &json!({ "Name": "a-name-nothing-must-carry" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {why:?}: expected the update to be refused, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            format!("Maintenance window {window_id} does not exist"),
            "case {why:?}: expected the refusal to name what was asked for, got {response}"
        );
        let (status, response) = get(&router, region, window_id).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {why:?}: expected a refused update to have written nothing, got {status} \
             {response}"
        );
    }

    // No update minted a window, and the order a listing walks this Region's windows in is
    // the order they were created.
    let (status, listed) =
        call_in_region(&router, REGION, "DescribeMaintenanceWindows", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account's windows are listed, got {status} {listed}"
    );
    let walked: Vec<&str> = listed["WindowIdentities"]
        .as_array()
        .expect("a listing reports WindowIdentities")
        .iter()
        .filter_map(|identity| identity["WindowId"].as_str())
        .collect();
    assert_eq!(
        walked,
        vec![here.as_str(), next.as_str()],
        "case {case:?}: expected the updates to have minted no window and to have left the order \
         the windows were created in"
    );
}

/// RULE 5: a write of a window is held to the rules the create is held to, judged on the
/// window the update WOULD leave behind — so a member the request leaves out is judged
/// against what the window already holds, and a refused update writes nothing.
///
/// The rows differ along which rule the merged window would break: the schedule's form, the
/// zone it is read in, and the cutoff taken against the duration — the last one over each
/// published bound of the modelled ranges (`Cutoff` 0..23, `Duration` 1..24) and over a pair
/// one half of which is stored rather than supplied.
#[tokio::test]
async fn an_update_is_held_to_the_rules_every_write_of_a_window_holds_to() {
    let router = router();
    let name = "window-write-rules";

    // (case, the update's members, the refusal it must carry, or None when it is served)
    let rows: Vec<(&str, Value, Option<String>)> = vec![
        (
            "a schedule of prose",
            json!({ "Schedule": "every 5 minutes" }),
            Some(
                "The schedule is invalid. Verify your cron or rate expression and try again."
                    .to_string(),
            ),
        ),
        (
            "a cron of five fields",
            json!({ "Schedule": "cron(0 8 1 * ?)" }),
            Some(
                // A `cron(...)` body is refused by what its parse found: the body runs out
                // of fields before the parser has read the ones it needs, so the parse
                // failed at the body's end.
                "Invalid CRON expression: error at position 9, details: Unexpected end of \
                 expression."
                    .to_string(),
            ),
        ),
        (
            "a rate of zero",
            json!({ "Schedule": "rate(0 minutes)" }),
            Some(
                "The schedule is invalid. Verify your cron or rate expression and try again."
                    .to_string(),
            ),
        ),
        (
            "a zone no IANA database carries",
            json!({ "ScheduleTimezone": "Mars/Olympus" }),
            Some("Invalid schedule timezone Mars/Olympus.".to_string()),
        ),
        (
            "a cutoff equal to the stored duration",
            json!({ "Cutoff": 4 }),
            Some("Cutoff was 4 but must be smaller than 3 (Duration  - 1 )".to_string()),
        ),
        (
            "a duration the stored cutoff would fill",
            json!({ "Cutoff": 3, "Duration": 3 }),
            Some("Cutoff was 3 but must be smaller than 2 (Duration  - 1 )".to_string()),
        ),
        (
            "a duration shortened under the cutoff the window already holds",
            json!({ "Duration": 1 }),
            Some("Cutoff was 1 but must be smaller than 0 (Duration  - 1 )".to_string()),
        ),
        (
            "the largest cutoff under the largest duration",
            json!({ "Cutoff": 23, "Duration": 24 }),
            None,
        ),
        (
            "no cutoff under the smallest duration",
            json!({ "Cutoff": 0, "Duration": 1 }),
            None,
        ),
        (
            "a one-time instant as the schedule",
            json!({ "Schedule": "at(2030-06-01T12:00:00)" }),
            None,
        ),
    ];

    for (why, body, refusal) in rows {
        let window_id = create(&router, why, REGION, &created(name)).await;
        let (status, response) = update_for(&router, REGION, &window_id, &body).await;
        match refusal {
            Some(message) => {
                assert_eq!(
                    (status, error_code(&response).as_str()),
                    (400, "ValidationException"),
                    "case {why:?}: expected the update {body} to be refused, got {status} \
                     {response}"
                );
                assert_eq!(
                    error_message(&response),
                    message,
                    "case {why:?}: expected the refusal the create states for the same rule, got \
                     {response}"
                );
                // Nothing was written for a refused update.
                let held = read(&router, why, REGION, &window_id).await;
                assert_members(why, &held, &created_members(name));
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {why:?}: the update {body} must be served, got {status} {response}"
                );
                let expected = with(&created_members(name), without(&body, &["Replace"]));
                let held = read(&router, why, REGION, &window_id).await;
                assert_members(why, &held, &expected);
            }
        }
    }
}
