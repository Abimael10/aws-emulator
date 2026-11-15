// SPDX-License-Identifier: CC0-1.0

//! What CONCURRENT writers leave a CloudTrail Lake dashboard in, driven end to end over
//! the wire through the shared harness (`tests/all/emulator.rs`).
//!
//! THE RULE UNDER TEST: `UpdateDashboard` either applies a caller's whole patch or refuses
//! it — `ConflictException` is the modelled refusal of a dashboard another write holds
//! (`smithy_query(query="operation",operation="UpdateDashboard")`) — so with several
//! updates in flight at once at least one is accepted, every answer is a success or that
//! refusal, and the dashboard then reports `UPDATED` carrying exactly ONE accepted
//! writer's values. No answer reports a write that was lost, and no read reports a mixture
//! of two.
//!
//! `UpdateDashboardResponse` models no `Status` member, so the status a write leaves is
//! read back through `GetDashboard` — the one operation that reports it.

use serde_json::{json, Value};

use service_com_amazonaws_cloudtrail_cloudtrail_20131101::state;

use super::emulator::{Emulator, Reply};

/// The widget writer `nth` composes, so the dashboard a read reports names the writer
/// whose patch is in force. `QueryStatement` and `ViewProperties` are `RequestWidget`'s
/// required members.
fn widget(nth: usize) -> Value {
    json!({
        "QueryStatement": format!("SELECT eventName FROM EDS LIMIT {nth}"),
        "ViewProperties": {"type": "table", "colors": format!("{nth}")},
    })
}

/// Create a dashboard with termination protection OFF and one widget.
async fn create(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "CreateDashboard",
            &json!({
                "Name": name,
                "Widgets": [widget(0)],
                "TerminationProtectionEnabled": false,
            }),
        )
        .await
}

/// Read the dashboard back.
async fn get(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call("GetDashboard", &json!({"DashboardId": name}))
        .await
}

/// Whether `reply` is an accepted update, having asserted it is one of the two answers the
/// rule admits: a success, or the modelled conflict refusal.
fn accepted(case: &str, nth: usize, reply: &Reply) -> bool {
    if reply.status == 200 {
        return true;
    }
    assert_eq!(
        (reply.status, reply.error_code()),
        (
            state::DASHBOARD_CONFLICT_HTTP_STATUS,
            "ConflictException"
        ),
        "case {case:?} (writer {nth}): an update is served or refused as a conflict, got \
         {reply:?}"
    );
    false
}

/// RULE: concurrent updates leave the dashboard `UPDATED` with exactly one accepted
/// writer's values in force; every answer is a success or `ConflictException`, and at least
/// one is a success.
///
/// The rows differ in how MANY writers are in flight and in whether they write the same
/// patch or different ones — what the rule generalises over. Each writer's widget set
/// names it, so a read that reported a mixture of two patches would fail the last
/// assertion.
#[tokio::test]
async fn concurrent_updates_leave_one_writers_values_in_force() {
    let emulator = Emulator::new();
    // (case, the number of writers, whether each writer writes its OWN widget set)
    let rows: Vec<(&str, usize, bool)> = vec![
        ("two writers, each with its own patch", 2, true),
        ("four writers, each with its own patch", 4, true),
        ("four writers writing the same patch", 4, false),
    ];

    for (index, (case, writers, distinct)) in rows.into_iter().enumerate() {
        let name = format!("concurrent-{index}");
        create(&emulator, &name).await.expect_ok(case);

        // Every writer turns termination protection ON, and writes the widget set that
        // names it (or the one they all write).
        let patches: Vec<Value> = (0..writers)
            .map(|nth| {
                json!({
                    "DashboardId": &name,
                    "Widgets": [widget(if distinct { nth + 1 } else { 1 })],
                    "TerminationProtectionEnabled": true,
                })
            })
            .collect();

        // In flight AT ONCE: the futures are polled together by this runtime, so the
        // handlers race for the dashboard the way four clients would.
        let replies: Vec<Reply> = futures::future::join_all(
            patches
                .iter()
                .map(|patch| emulator.call("UpdateDashboard", patch)),
        )
        .await;

        let served: Vec<usize> = replies
            .iter()
            .enumerate()
            .filter(|(nth, reply)| accepted(case, *nth, reply))
            .map(|(nth, _)| nth)
            .collect();
        assert!(
            !served.is_empty(),
            "case {case:?}: at least one concurrent update must be served, got {replies:?}"
        );

        // The dashboard settles UPDATED, with the protection every writer asked for and
        // exactly one accepted writer's widget set — never a mixture.
        let read = get(&emulator, &name).await;
        let dashboard = read.expect_ok(case).clone();
        assert_eq!(
            dashboard["Status"],
            json!(state::DASHBOARD_STATUS_UPDATED),
            "case {case:?}: a dashboard an update has returned for reports UPDATED, got \
             {dashboard:?}"
        );
        assert_eq!(
            dashboard["TerminationProtectionEnabled"],
            json!(true),
            "case {case:?}: the value every writer wrote must be in force, got {dashboard:?}"
        );
        let in_force: Vec<Value> = served
            .iter()
            .map(|nth| json!([widget(if distinct { nth + 1 } else { 1 })]))
            .collect();
        assert!(
            in_force.contains(&dashboard["Widgets"]),
            "case {case:?}: expected one served writer's widgets out of {in_force:?}, got \
             {dashboard:?}"
        );

        // And the record is whole: reading it again reports the same dashboard, and the
        // ARN and name are still the created dashboard's.
        let again = get(&emulator, &name).await;
        assert_eq!(
            again.expect_ok(case),
            &dashboard,
            "case {case:?}: a read must report the same dashboard, got {again:?}"
        );
        assert_eq!(
            dashboard["DashboardArn"],
            json!(format!(
                "arn:aws:cloudtrail:{}:{}:dashboard/{name}",
                super::emulator::HOME_REGION,
                engine::emulator_account_id()
            )),
            "case {case:?}: the dashboard must still be the one created, got {dashboard:?}"
        );
    }
}

/// RULE (the sequential end of the same rule): an update applies the WHOLE patch it
/// carries, so a dashboard written twice reports the second write's values and `UPDATED`,
/// and nothing of the first patch's members survives in a member the second wrote.
#[tokio::test]
async fn a_later_update_replaces_the_earlier_writers_values() {
    let emulator = Emulator::new();
    let case = "two updates in sequence";
    create(&emulator, "sequential").await.expect_ok(case);

    for nth in 1..=3 {
        emulator
            .call(
                "UpdateDashboard",
                &json!({
                    "DashboardId": "sequential",
                    "Widgets": [widget(nth)],
                    "TerminationProtectionEnabled": nth % 2 == 1,
                }),
            )
            .await
            .expect_ok(&format!("{case}: write {nth}"));

        let read = get(&emulator, "sequential").await;
        let dashboard = read.expect_ok(case).clone();
        assert_eq!(
            (
                dashboard["Status"].clone(),
                dashboard["Widgets"].clone(),
                dashboard["TerminationProtectionEnabled"].clone()
            ),
            (
                json!(state::DASHBOARD_STATUS_UPDATED),
                json!([widget(nth)]),
                json!(nth % 2 == 1)
            ),
            "case {case:?} (write {nth}): expected the last write in force, got {dashboard:?}"
        );
    }
}
