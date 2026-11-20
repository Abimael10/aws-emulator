// SPDX-License-Identifier: CC0-1.0

//! The time zone a stored backup rule reports its schedule runs in, driven as
//! real HTTP through the router.
//!
//! The rule under test: a `BackupRule` stored without a
//! `ScheduleExpressionTimezone` is stored with the service's own default zone and
//! every read of the rule reports it — "the timezone in which the schedule
//! expression is set. By default, ScheduleExpressions are in UTC"
//! (`smithy_query(query="doc",shape="BackupRule",member="ScheduleExpressionTimezone")`),
//! which is the IANA zone `Etc/UTC` — while a rule that declares a zone of its
//! own keeps exactly that zone. A rule read back therefore always states the zone
//! its schedule really runs in, whichever operation stored it and whichever read
//! reports it.
//!
//! The rule is asserted over a table whose rows differ in the zone declared (none
//! at all, UTC named explicitly, zones east and west of it), with every row read
//! back three ways — the unversioned read, the `VersionId`-pinned read of the same
//! version, and the version an update stores — and with two rules of one plan in
//! the same body, so a defaulted rule cannot take a sibling's zone.
//!
//! The request harness (`router`, `call`) is the one `vault_and_backup_job.rs`
//! exposes to this test binary, so there is ONE wire path for the service's
//! suites.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::vault_and_backup_job::{call, create_vault, router};

/// The zone a rule that declares none is scheduled in (`crate::state::schedule`'s
/// `DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE`).
const DEFAULT_TIMEZONE: &str = "Etc/UTC";

/// The zones the rule generalises over, as the IANA names `Timezone` takes: a
/// rule that declares none, the default named explicitly, and one zone either
/// side of UTC — none of them the zone the recorded case used.
fn declared_zones() -> Vec<(&'static str, Option<&'static str>, &'static str)> {
    vec![
        ("no zone at all", None, DEFAULT_TIMEZONE),
        ("the default zone, named", Some("Etc/UTC"), "Etc/UTC"),
        ("a zone west of UTC", Some("America/Denver"), "America/Denver"),
        ("a zone east of UTC", Some("Asia/Tokyo"), "Asia/Tokyo"),
        ("a zone of the same offset", Some("Europe/London"), "Europe/London"),
    ]
}

/// A body member as a string, or `"<unset>"` when the member is absent.
fn member(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// One rule named `rule_name`, declaring `zone` when the row states one.
fn rule(rule_name: &str, zone: Option<&str>) -> Value {
    let mut rule = json!({
        "RuleName": rule_name,
        "TargetBackupVaultName": "timezone-vault",
        "ScheduleExpression": "cron(0 5 ? * * *)",
    });
    if let Some(zone) = zone {
        rule["ScheduleExpressionTimezone"] = json!(zone);
    }
    rule
}

/// `PUT /backup/plans` — `CreateBackupPlan`'s HTTP binding.
async fn create_plan(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "PUT", "/backup/plans", Some(&body.to_string())).await
}

/// `POST /backup/plans/{BackupPlanId}` — `UpdateBackupPlan`'s.
async fn update_plan(router: &MultiServiceRouter, plan_id: &str, body: &Value) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/backup/plans/{plan_id}"),
        Some(&body.to_string()),
    )
    .await
}

/// `GET /backup/plans/{BackupPlanId}` — `GetBackupPlan`'s, optionally pinned to
/// a version.
async fn get_plan(
    router: &MultiServiceRouter,
    plan_id: &str,
    version: Option<&str>,
) -> (u16, Value) {
    let path = match version {
        None => format!("/backup/plans/{plan_id}"),
        Some(version) => format!("/backup/plans/{plan_id}?versionId={version}"),
    };
    call(router, "GET", &path, None).await
}

/// The `ScheduleExpressionTimezone` the rule named `rule_name` of a plan read
/// reports, or `"<unset>"` when the member is absent.
fn reported_zone(read: &Value, rule_name: &str) -> String {
    let rules = read["BackupPlan"]["Rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    rules
        .iter()
        .find(|rule| member(rule, "RuleName") == rule_name)
        .map_or_else(|| "<no such rule>".to_string(), |rule| {
            member(rule, "ScheduleExpressionTimezone")
        })
}

/// A rule reports the zone it declared, and one that declared none reports the
/// service's default — on the unversioned read, on the `VersionId`-pinned read of
/// that same version, and on the version an update stores.
#[tokio::test]
async fn a_stored_rule_reports_the_timezone_its_schedule_runs_in() {
    for (case, declared, expected) in declared_zones() {
        let router = router();
        // The vault the rows' rules target: an update resolves every rule's
        // `TargetBackupVaultName` in the caller's Region before it stores the
        // body, so the account holds it first.
        let (status, vault) = create_vault(&router, "timezone-vault", "{}").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupVault 200, got {status} {vault:?}"
        );
        // Two rules in ONE body: the row's rule beside a sibling that always
        // declares a zone, so a defaulted zone can never be a sibling's.
        let body = json!({
            "BackupPlan": {
                "BackupPlanName": "timezone-plan",
                "Rules": [rule("row", declared), rule("sibling", Some("Australia/Sydney"))],
            }
        });
        let (status, created) = create_plan(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateBackupPlan 200, got {status} {created:?}"
        );
        let plan_id = member(&created, "BackupPlanId");
        let version_id = member(&created, "VersionId");

        for (read_case, version) in [
            ("the unversioned read", None),
            ("the version-pinned read", Some(version_id.as_str())),
        ] {
            let (status, read) = get_plan(&router, &plan_id, version).await;
            assert_eq!(
                (status, reported_zone(&read, "row")),
                (200, expected.to_string()),
                "case {case:?} on {read_case}: expected the rule to report {expected:?}, got \
                 {status} {read:?}"
            );
            assert_eq!(
                (status, reported_zone(&read, "sibling")),
                (200, "Australia/Sydney".to_string()),
                "case {case:?} on {read_case}: the sibling must keep its own zone, got \
                 {status} {read:?}"
            );
        }

        // The update stores the next version of the same plan, so the same rule
        // holds for the body it stored.
        let (status, updated) = update_plan(
            &router,
            &plan_id,
            &json!({
                "BackupPlan": {
                    "BackupPlanName": "timezone-plan",
                    "Rules": [rule("row", declared)],
                }
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateBackupPlan 200, got {status} {updated:?}"
        );
        let (status, read) = get_plan(&router, &plan_id, Some(&member(&updated, "VersionId"))).await;
        assert_eq!(
            (status, reported_zone(&read, "row")),
            (200, expected.to_string()),
            "case {case:?} on the updated version: expected {expected:?}, got {status} \
             {read:?}"
        );
        // ...and the version the create stored still reports what it stored.
        let (status, read) = get_plan(&router, &plan_id, Some(&version_id)).await;
        assert_eq!(
            (status, reported_zone(&read, "row")),
            (200, expected.to_string()),
            "case {case:?} on the superseded version: expected {expected:?}, got {status} \
             {read:?}"
        );
    }
}
