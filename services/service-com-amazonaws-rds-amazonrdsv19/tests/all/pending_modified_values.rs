// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a `ModifyDBInstance` change takes effect, and for the three
//! settings this service reports a resource's monitoring and maintenance schedule through.
//!
//! Every call goes over the wire through `db_instances::emulator`, as a real awsQuery form
//! POST carrying the SigV4 credential scope the router keys on.
//!
//! The rules under test:
//!
//! * `ApplyImmediately`: "If this parameter is disabled, changes to the DB instance are
//!   applied during the next maintenance window." A setting the modelled
//!   `PendingModifiedValues` can report is one the service defers to that window — the live
//!   attribute keeps the value the instance runs under and the staged one is reported as
//!   pending — and a setting that structure carries no member for is applied as soon as
//!   possible whatever the parameter says. `ApplyImmediately` enabled applies "the
//!   modifications in this request and any pending modifications" at once, and so does the
//!   restart `RebootDBInstance` performs.
//! * `PerformanceInsightsRetentionPeriod` is held to the values the member publishes (7,
//!   `month * 31` for 1-23 months, and 731) and reads back from the instance it was set on.
//! * `PreferredMaintenanceWindow` is stored with its day names folded to lower case,
//!   whatever case the request wrote, on a DB instance and on a DB cluster alike.
//!
//! The tables below span what each rule generalises over: a row per deferrable setting
//! (string, integer, boolean and the identifier a rename stages) beside the settings that
//! are applied at once, every published retention value and the shapes the member refuses,
//! and the case forms a window can be written in.
//!
//! Nothing here waits a span of wall-clock for a change this service's clock owes: every
//! wait is the one poll loop this test target shares (`lifecycle_poll_loops::poll_until`),
//! which re-reads a REPORTING call on a monotonic deadline until the condition the case
//! needs holds, so the verdict is the same on a loaded host as on an idle one. For the same
//! reason the window a case needs OPEN is derived from the instant the service reads it
//! rather than from a calendar day, and the window a case needs CLOSED names a weekday days
//! away.

use service_com_amazonaws_rds_amazonrdsv19::state::window_is_open;
use std::time::SystemTime;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts,
};
use crate::lifecycle_poll_loops::{poll_until, read_instance};

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// One row of the deferral table: a setting `ModifyDBInstance` takes, the value it is
/// modified to, and the member the instance reports it under.
struct DeferralCase {
    case: &'static str,
    identifier: &'static str,
    /// The member the request carries the new value in.
    member: &'static str,
    value: &'static str,
    /// The member of `DBInstance` the live value is reported under.
    reported_as: &'static str,
    /// The member of `PendingModifiedValues` the staged value is reported under, which is
    /// not always the member the instance itself reports it as: the port a cluster member
    /// listens on is `DbInstancePort` on the instance and `Port` among the pending values.
    pending_as: &'static str,
    /// What the instance reports for `reported_as` before the modification.
    was: &'static str,
}

/// The settings the modelled `PendingModifiedValues` carries a member for, which are the
/// ones the service defers to the maintenance window: one row per shape of value the
/// structure holds — a string, an integer, a boolean and the identifier a rename stages.
const DEFERRED: &[DeferralCase] = &[
    DeferralCase {
        case: "allocated storage",
        identifier: "emu-defer-storage",
        member: "AllocatedStorage",
        value: "40",
        reported_as: "AllocatedStorage",
        pending_as: "AllocatedStorage",
        was: "20",
    },
    DeferralCase {
        case: "the DB instance class",
        identifier: "emu-defer-class",
        member: "DBInstanceClass",
        value: "db.t3.small",
        reported_as: "DBInstanceClass",
        pending_as: "DBInstanceClass",
        was: "db.t3.micro",
    },
    DeferralCase {
        case: "a Multi-AZ deployment",
        identifier: "emu-defer-multi-az",
        member: "MultiAZ",
        value: "true",
        reported_as: "MultiAZ",
        pending_as: "MultiAZ",
        was: "false",
    },
    DeferralCase {
        case: "the port the engine accepts connections on",
        identifier: "emu-defer-port",
        member: "DBPortNumber",
        value: "15433",
        reported_as: "DbInstancePort",
        pending_as: "Port",
        was: "0",
    },
    DeferralCase {
        case: "a rename",
        identifier: "emu-defer-rename",
        member: "NewDBInstanceIdentifier",
        value: "emu-defer-renamed",
        reported_as: "DBInstanceIdentifier",
        pending_as: "DBInstanceIdentifier",
        was: "emu-defer-rename",
    },
];

/// The settings `PendingModifiedValues` carries NO member for, which are applied as soon as
/// possible whatever `ApplyImmediately` says: `(case, member, value, reported as)`.
const APPLIED_AT_ONCE: &[(&str, &str, &str, &str)] = &[
    (
        "deletion protection",
        "DeletionProtection",
        "true",
        "DeletionProtection",
    ),
    (
        "copying tags to snapshots",
        "CopyTagsToSnapshot",
        "true",
        "CopyTagsToSnapshot",
    ),
    (
        "the monitoring interval",
        "MonitoringInterval",
        "60",
        "MonitoringInterval",
    ),
    (
        "automatic minor version upgrades",
        "AutoMinorVersionUpgrade",
        "false",
        "AutoMinorVersionUpgrade",
    ),
];

/// Every `PerformanceInsightsRetentionPeriod` the member publishes: "7", "month * 31, where
/// month is a number of months from 1-23" — tested at the first month, the 465 the advanced
/// Database Insights mode requires, and the last — and "731".
const PUBLISHED_RETENTION_PERIODS: &[i32] = &[7, 31, 93, 465, 713, 731];

/// Values outside that set, which "Amazon RDS returns an error" for: zero, the 94 the member
/// names as an example of an invalid period, a multiple of 31 past the 23-month ceiling, and
/// one past the published maximum.
const REFUSED_RETENTION_PERIODS: &[i32] = &[0, 30, 94, 744, 762];

/// Create an `available` DB instance of this file's own shape.
async fn an_instance(
    router: &smithy_server_runtime::MultiServiceRouter,
    identifier: &str,
    extra: &[(&str, String)],
) {
    let mut settings: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", identifier.to_string()),
        ("Engine", "postgres".to_string()),
        ("DBInstanceClass", "db.t3.micro".to_string()),
        ("AllocatedStorage", "20".to_string()),
        ("MasterUsername", "pgadmin".to_string()),
        ("MasterUserPassword", "a-strong-password-1".to_string()),
    ];
    // Every instance this suite creates keeps a maintenance window that is NOT open, unless
    // the case names its own: a staged change is applied during the window, so a fixture
    // whose window happened to be open would apply what the case stages for it.
    if !extra
        .iter()
        .any(|(member, _)| *member == "PreferredMaintenanceWindow")
    {
        settings.push(("PreferredMaintenanceWindow", window_days_ahead()));
    }
    settings.extend(extra.iter().cloned());
    expect_ok(
        identifier,
        call(router, "CreateDBInstance", &settings).await,
    );
    // The create owes the instance `available`, which is the state a modification needs it
    // in: it is READ until it reports that, the way a client's waiter does, rather than
    // waited on for a span of wall-clock.
    settled_instance(router, identifier, identifier).await;
}

/// The one `<DBInstance>` `DescribeDBInstances` reports for `identifier`.
async fn describe(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => identifier],
        )
        .await,
    );
    the_one_instance(case, &body)
}

/// The one `<DBInstance>` a report carries, whichever call answered it.
fn the_one_instance(case: &str, body: &str) -> String {
    let found = subtrees(body, "DBInstance");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one DBInstance, got {found:?}"
    );
    found.into_iter().next().expect("checked just above")
}

/// Read `identifier` through `DescribeDBInstances` again and again until the instance it
/// reports satisfies `holds`, and answer that instance.
///
/// This is how every wait in this suite is made: `holds` is the CONDITION the case needs —
/// a status this service's clock owes, a staged change applied, an export listed — and the
/// loop is the shared one this test target polls every owed transition through
/// ([`poll_until`]), bounded by a monotonic deadline that is generous beside the span a
/// transition is owed in. `what` names the condition for the panic a deadline raises.
async fn poll_instance(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    what: &str,
    identifier: &str,
    holds: impl Fn(&str) -> bool,
) -> String {
    let (body, _) = poll_until(
        case,
        what,
        || read_instance(router, identifier),
        |status, body| {
            status == 200
                && subtrees(body, "DBInstance")
                    .first()
                    .is_some_and(|instance| holds(instance))
        },
    )
    .await;
    the_one_instance(case, &body)
}

/// [`poll_instance`] for the state every transition this service owes an instance settles
/// on: `available`, which is also the state a modification requires it to be in.
async fn settled_instance(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    poll_instance(
        router,
        case,
        &format!("{identifier} reaching available"),
        identifier,
        |instance| optional_text(instance, "DBInstanceStatus").as_deref() == Some("available"),
    )
    .await
}

/// What `instance` reports as pending, or `None` when it reports no pending change.
///
/// The element itself is reported on every instance envelope — "specific changes are
/// identified by subelements", and an instance with nothing pending carries it with no
/// subelement inside — so an EMPTY element is what "nothing is pending" reads as here.
fn pending(instance: &str) -> Option<String> {
    subtrees(instance, "PendingModifiedValues")
        .into_iter()
        .zip(texts(instance, "PendingModifiedValues"))
        .find(|(_, inside)| !inside.trim().is_empty())
        .map(|(staged, _)| staged)
}

#[tokio::test]
async fn apply_immediately_disabled_stages_every_deferrable_setting_for_the_window() {
    let router = emulator();
    for case in DEFERRED {
        an_instance(&router, case.identifier, &[]).await;
        let modified = expect_ok(
            case.case,
            call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => case.identifier,
                    "ApplyImmediately" => false,
                    case.member => case.value,
                ],
            )
            .await,
        );
        let answered = subtrees(&modified, "DBInstance")
            .into_iter()
            .next()
            .expect("the modification answers with the instance");
        // The live attribute keeps what the instance is running under.
        let live = optional_text(&answered, case.reported_as);
        assert_eq!(
            live.as_deref(),
            Some(case.was),
            "case {:?}: expected the live {} to stay {:?}, got {live:?}",
            case.case,
            case.reported_as,
            case.was
        );
        // And the staged value is what the instance reports as pending.
        let staged = pending(&answered);
        let staged = staged.unwrap_or_else(|| {
            panic!(
                "case {:?}: a deferred modification must report PendingModifiedValues, got \
                 {answered}",
                case.case
            )
        });
        let reported = optional_text(&staged, case.pending_as);
        assert_eq!(
            reported.as_deref(),
            Some(case.value),
            "case {:?}: expected {} staged as {:?}, got {reported:?}",
            case.case,
            case.pending_as,
            case.value
        );
        // A read of the instance says the same: the deferral is in state, not in the one
        // answer. The read is made once the instance is settled, which it is read until —
        // a deferred modification leaves the status alone, so the condition holds at once
        // and nothing is waited out.
        let read_back = settled_instance(&router, case.case, case.identifier).await;
        let live = optional_text(&read_back, case.reported_as);
        assert_eq!(
            live.as_deref(),
            Some(case.was),
            "case {:?}: expected a later read of {} to stay {:?}, got {live:?}",
            case.case,
            case.reported_as,
            case.was
        );
    }
}

#[tokio::test]
async fn apply_immediately_enabled_applies_the_request_and_what_was_pending() {
    let router = emulator();
    an_instance(&router, "emu-apply-now", &[]).await;
    // One staged change, then a second request that asks for its own change at once: both
    // land, because "the modifications in this request and any pending modifications are
    // asynchronously applied as soon as possible".
    expect_ok(
        "stage the storage",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-apply-now",
                "ApplyImmediately" => false,
                "AllocatedStorage" => 40,
            ],
        )
        .await,
    );
    settled_instance(&router, "the staged change", "emu-apply-now").await;
    expect_ok(
        "apply with a class change",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-apply-now",
                "ApplyImmediately" => true,
                "DBInstanceClass" => "db.t3.small",
            ],
        )
        .await,
    );
    // Both are owed "as soon as possible", which is an instant of its own: the instance is
    // read until it has nothing pending any more rather than waited on for a fixed span.
    let read_back = poll_instance(
        &router,
        "both changes applied",
        "the instance applying the request and what was pending",
        "emu-apply-now",
        |instance| pending(instance).is_none(),
    )
    .await;
    for (member, expected) in [("AllocatedStorage", "40"), ("DBInstanceClass", "db.t3.small")] {
        let actual = optional_text(&read_back, member);
        assert_eq!(
            actual.as_deref(),
            Some(expected),
            "case {member:?}: expected {expected:?} applied, got {actual:?}"
        );
    }
    assert!(
        pending(&read_back).is_none(),
        "an instance with everything applied reports no PendingModifiedValues, got \
         {read_back}"
    );
}

/// The log types the export-configuration cases turn on and off, in the order the member
/// takes them. The types come from `EnableCloudwatchLogsExports`' own publication for the
/// PostgreSQL engines — `postgresql` and `upgrade` — so the table spans more than the one
/// type a single case would.
const EXPORTED_LOG_TYPES: &[&str] = &["postgresql", "upgrade"];

#[tokio::test]
async fn a_modification_asked_for_at_once_is_pending_until_the_instance_is_available() {
    let router = emulator();
    // The rule is about WHEN a staged setting lands, so the table is the same set of
    // deferrable settings the window case is driven over: each one asked for AS SOON AS
    // POSSIBLE this time.
    for case in DEFERRED {
        let identifier = format!("{}-now", case.identifier);
        an_instance(&router, &identifier, &[]).await;
        let modified = expect_ok(
            case.case,
            call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => &identifier,
                    "ApplyImmediately" => true,
                    case.member => case.value,
                ],
            )
            .await,
        );
        let answered = subtrees(&modified, "DBInstance")
            .into_iter()
            .next()
            .expect("the modification answers with the instance");
        // A rename is the one change the control plane applies at once: "an instance reboot
        // occurs immediately if you enable ApplyImmediately", and the rename is what that
        // reboot follows. Every other setting is applied when the instance finishes
        // modifying, so until then the instance reports what it is RUNNING and the requested
        // value is reported as pending.
        let renaming = case.member == "NewDBInstanceIdentifier";
        let live = optional_text(&answered, case.reported_as);
        let expected_live = if renaming { case.value } else { case.was };
        assert_eq!(
            live.as_deref(),
            Some(expected_live),
            "case {:?}: expected the live {} to read {expected_live:?} while the change is \
             applied, got {live:?}",
            case.case,
            case.reported_as
        );
        if !renaming {
            let staged = pending(&answered).unwrap_or_else(|| {
                panic!(
                    "case {:?}: a modification being applied must report \
                     PendingModifiedValues, got {answered}",
                    case.case
                )
            });
            let reported = optional_text(&staged, case.pending_as);
            assert_eq!(
                reported.as_deref(),
                Some(case.value),
                "case {:?}: expected {} reported as pending, got {reported:?}",
                case.case,
                case.pending_as
            );
            let status = optional_text(&answered, "DBInstanceStatus");
            assert_eq!(
                status.as_deref(),
                Some("modifying"),
                "case {:?}: an instance with a change still being applied reports modifying, \
                 got {status:?}",
                case.case
            );
        }
        // The change CLEARS as it is applied, and the instance is available again at the
        // same moment: a client that waits for the pending value to go finds the instance
        // available, never one that is still modifying. Read until it holds rather than
        // waited on for a fixed time. A rename has nothing pending — it was applied in the
        // call — so what is waited for there is the instance coming back from the reboot the
        // rename causes.
        let settled_identifier = if renaming { case.value } else { &identifier };
        let read_back = poll_instance(
            &router,
            case.case,
            "the change asked for at once being applied",
            settled_identifier,
            |instance| {
                if renaming {
                    optional_text(instance, "DBInstanceStatus").as_deref() == Some("available")
                } else {
                    pending(instance).is_none()
                }
            },
        )
        .await;
        let status = optional_text(&read_back, "DBInstanceStatus");
        assert_eq!(
            status.as_deref(),
            Some("available"),
            "case {:?}: the instance must be available once nothing is pending, got \
             {read_back}",
            case.case
        );
        let live = optional_text(&read_back, case.reported_as);
        // `DbInstancePort` reports the port of a CLUSTER MEMBER and zero for an instance
        // that belongs to no DB cluster, which every instance here is, so the applied value
        // is not observable on that member — what this row asserts is that the staged port
        // stops being pending.
        if case.reported_as != "DbInstancePort" {
            assert_eq!(
                live.as_deref(),
                Some(case.value),
                "case {:?}: expected {} applied to {:?}, got {live:?}",
                case.case,
                case.reported_as,
                case.value
            );
        }
    }
}

#[tokio::test]
async fn a_log_export_configuration_is_pending_and_then_lists_the_types_it_enabled() {
    let router = emulator();
    an_instance(&router, "emu-log-exports", &[]).await;
    let created = describe(&router, "the created instance", "emu-log-exports").await;
    assert_eq!(
        subtrees(&created, "EnabledCloudwatchLogsExports").len(),
        1,
        "an instance created with no export reports an empty list, got {created}"
    );
    assert_eq!(
        texts_in(&created, "EnabledCloudwatchLogsExports"),
        Vec::<String>::new(),
        "an instance created with no export exports nothing, got {created}"
    );

    // Each type is enabled, read back, and disabled again: "the enabled types appear in
    // PendingModifiedValues.PendingCloudwatchLogsExports while pending and in
    // DBInstance.EnabledCloudwatchLogsExports once applied, and are removed again when
    // disabled."
    for log_type in EXPORTED_LOG_TYPES {
        let modified = expect_ok(
            log_type,
            call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => "emu-log-exports",
                    "ApplyImmediately" => true,
                    "CloudwatchLogsExportConfiguration.EnableLogTypes.member.1" => log_type,
                ],
            )
            .await,
        );
        let answered = subtrees(&modified, "DBInstance")
            .into_iter()
            .next()
            .expect("the modification answers with the instance");
        let staged = pending(&answered).unwrap_or_else(|| {
            panic!("case {log_type:?}: the change must be reported as pending, got {answered}")
        });
        let pending_exports = subtrees(&staged, "PendingCloudwatchLogsExports")
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                panic!("case {log_type:?}: expected PendingCloudwatchLogsExports, got {staged}")
            });
        let enabling = texts_in(&pending_exports, "LogTypesToEnable");
        assert_eq!(
            enabling,
            vec![(*log_type).to_string()],
            "case {log_type:?}: expected the type reported as being enabled, got {enabling:?}"
        );
        let listed = poll_until_exports(&router, log_type, |exports| {
            exports.contains(&(*log_type).to_string())
        })
        .await;
        assert!(
            listed.contains(&(*log_type).to_string()),
            "case {log_type:?}: expected the applied export to be listed, got {listed:?}"
        );

        // And disabling it takes it off the list again.
        expect_ok(
            log_type,
            call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => "emu-log-exports",
                    "ApplyImmediately" => true,
                    "CloudwatchLogsExportConfiguration.DisableLogTypes.member.1" => log_type,
                ],
            )
            .await,
        );
        let listed = poll_until_exports(&router, log_type, |exports| {
            !exports.contains(&(*log_type).to_string())
        })
        .await;
        assert!(
            !listed.contains(&(*log_type).to_string()),
            "case {log_type:?}: expected the disabled export to be gone, got {listed:?}"
        );
    }
}

/// The values of every `<member>` of the list element `named` of `xml`, in order.
fn texts_in(xml: &str, named: &str) -> Vec<String> {
    subtrees(xml, named)
        .into_iter()
        .flat_map(|list| texts(&list, "member"))
        .collect()
}

/// Read `emu-log-exports` until its `EnabledCloudwatchLogsExports` satisfies `holds`, and
/// answer the list it reported then.
async fn poll_until_exports(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    holds: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let read_back = poll_instance(
        router,
        case,
        "the export configuration being applied",
        "emu-log-exports",
        |instance| holds(&texts_in(instance, "EnabledCloudwatchLogsExports")),
    )
    .await;
    texts_in(&read_back, "EnabledCloudwatchLogsExports")
}

#[tokio::test]
async fn a_reboot_applies_what_the_window_was_waiting_for() {
    let router = emulator();
    an_instance(&router, "emu-defer-reboot", &[]).await;
    expect_ok(
        "stage the storage",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-defer-reboot",
                "ApplyImmediately" => false,
                "AllocatedStorage" => 40,
            ],
        )
        .await,
    );
    settled_instance(&router, "the staged change", "emu-defer-reboot").await;
    expect_ok(
        "reboot",
        call(
            &router,
            "RebootDBInstance",
            &params!["DBInstanceIdentifier" => "emu-defer-reboot"],
        )
        .await,
    );
    // The restart is a transition this service owes: the instance is read until it is back
    // rather than waited on for a span of wall-clock.
    let read_back = settled_instance(&router, "after the reboot", "emu-defer-reboot").await;
    let actual = optional_text(&read_back, "AllocatedStorage");
    assert_eq!(
        actual.as_deref(),
        Some("40"),
        "the restart applies what was staged for the window, got {actual:?}"
    );
    assert!(
        pending(&read_back).is_none(),
        "nothing is pending once the restart applied it, got {read_back}"
    );
}

#[tokio::test]
async fn the_maintenance_window_is_what_a_staged_change_waits_for() {
    let router = emulator();
    // A window that is DAYS away: the change staged against it stays pending.
    an_instance(
        &router,
        "emu-defer-window",
        &[(
            "PreferredMaintenanceWindow",
            window_days_ahead().to_string(),
        )],
    )
    .await;
    expect_ok(
        "stage the storage",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-defer-window",
                "ApplyImmediately" => false,
                "AllocatedStorage" => 40,
            ],
        )
        .await,
    );
    // The read is made once the instance is settled, which it is READ until: a deferred
    // modification leaves the status alone, so this holds at once and nothing is waited out.
    let read_back = settled_instance(&router, "before the window", "emu-defer-window").await;
    assert_eq!(
        optional_text(&read_back, "AllocatedStorage").as_deref(),
        Some("20"),
        "a change staged for a window days away stays pending, got {read_back}"
    );
    let staged = pending(&read_back).unwrap_or_else(|| {
        panic!(
            "a change staged for a window days away must still be reported as pending, got \
             {read_back}"
        )
    });
    let waiting = optional_text(&staged, "AllocatedStorage");
    assert_eq!(
        waiting.as_deref(),
        Some("40"),
        "the staged value waits for the window as AllocatedStorage, got {waiting:?}"
    );
    // Moving the window to one that has already started makes the staged change due: the
    // window CONTROLS when a pending change is applied, so the instance is read until it
    // reports the new value rather than waited on for a fixed time.
    expect_ok(
        "move the window",
        call(
            &router,
            "ModifyDBInstance",
            &params![
                "DBInstanceIdentifier" => "emu-defer-window",
                "PreferredMaintenanceWindow" => window_that_started(),
            ],
        )
        .await,
    );
    let read_back = poll_instance(
        &router,
        "in the window",
        "the change staged for the window being applied once that window has come round",
        "emu-defer-window",
        |instance| optional_text(instance, "AllocatedStorage").as_deref() == Some("40"),
    )
    .await;
    let applied = optional_text(&read_back, "AllocatedStorage");
    assert_eq!(
        applied.as_deref(),
        Some("40"),
        "the change staged for the window must be applied once that window has come round, \
         last read {read_back}"
    );
    assert!(
        pending(&read_back).is_none(),
        "nothing stays pending once the window applied it, got {read_back}"
    );
}

#[tokio::test]
async fn a_setting_pending_modified_values_cannot_report_is_applied_at_once() {
    let router = emulator();
    for (case, member, value, reported_as) in APPLIED_AT_ONCE {
        let identifier = format!("emu-now-{}", member.to_ascii_lowercase());
        an_instance(&router, &identifier, &[]).await;
        let modified = expect_ok(
            case,
            call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => &identifier,
                    "ApplyImmediately" => false,
                    *member => *value,
                ],
            )
            .await,
        );
        let answered = subtrees(&modified, "DBInstance")
            .into_iter()
            .next()
            .expect("the modification answers with the instance");
        let actual = optional_text(&answered, reported_as);
        assert_eq!(
            actual.as_deref(),
            Some(*value),
            "case {case:?}: expected {reported_as} applied at once as {value:?}, got {actual:?}"
        );
        assert!(
            pending(&answered).is_none(),
            "case {case:?}: a setting the structure cannot report leaves nothing pending, \
             got {answered}"
        );
        // And a read of the instance says the same, once every transition this row's
        // modification owed has settled — read until it holds, not waited out.
        let read_back = settled_instance(&router, case, &identifier).await;
        let reported = optional_text(&read_back, reported_as);
        assert_eq!(
            reported.as_deref(),
            Some(*value),
            "case {case:?}: expected a later read of {reported_as} to stay {value:?}, got \
             {reported:?}"
        );
    }
}

#[tokio::test]
async fn performance_insights_settings_read_back_and_are_held_to_the_published_values() {
    let router = emulator();
    for (index, period) in PUBLISHED_RETENTION_PERIODS.iter().enumerate() {
        let identifier = format!("emu-insights-{index}");
        an_instance(
            &router,
            &identifier,
            &[
                ("EnablePerformanceInsights", "true".to_string()),
                ("PerformanceInsightsRetentionPeriod", period.to_string()),
                (
                    "PerformanceInsightsKMSKeyId",
                    "alias/aws/rds".to_string(),
                ),
            ],
        )
        .await;
        let read_back = describe(&router, &identifier, &identifier).await;
        for (member, expected) in [
            ("PerformanceInsightsEnabled", "true".to_string()),
            ("PerformanceInsightsRetentionPeriod", period.to_string()),
            ("PerformanceInsightsKMSKeyId", "alias/aws/rds".to_string()),
        ] {
            let actual = optional_text(&read_back, member);
            assert_eq!(
                actual.as_deref(),
                Some(expected.as_str()),
                "case {period}: expected {member} to read back as {expected:?}, got {actual:?}"
            );
        }
    }
    // An instance that collects no Performance Insights data retains none, so it reports no
    // retention period at all.
    an_instance(&router, "emu-insights-off", &[]).await;
    let read_back = describe(&router, "insights off", "emu-insights-off").await;
    assert_eq!(
        optional_text(&read_back, "PerformanceInsightsRetentionPeriod"),
        None,
        "an instance with Performance Insights off reports no retention period, got \
         {read_back}"
    );
    // A period outside the published values is refused, on the create and on the
    // modification alike, and nothing is stored for it.
    for period in REFUSED_RETENTION_PERIODS {
        expect_error(
            &format!("create with retention {period}"),
            call(
                &router,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => format!("emu-insights-bad-{period}"),
                    "Engine" => "postgres",
                    "DBInstanceClass" => "db.t3.micro",
                    "AllocatedStorage" => 20,
                    "EnablePerformanceInsights" => true,
                    "PerformanceInsightsRetentionPeriod" => period,
                ],
            )
            .await,
            400,
            "InvalidParameterValue",
        );
        expect_error(
            &format!("modify to retention {period}"),
            call(
                &router,
                "ModifyDBInstance",
                &params![
                    "DBInstanceIdentifier" => "emu-insights-off",
                    "ApplyImmediately" => true,
                    "PerformanceInsightsRetentionPeriod" => period,
                ],
            )
            .await,
            400,
            "InvalidParameterValue",
        );
    }
}

/// The case forms a `PreferredMaintenanceWindow` can be written in, each with the form the
/// service stores: the day names fold, and nothing else in the value has case to fold.
const WINDOW_CASES: &[(&str, &str)] = &[
    ("mon:00:00-mon:03:00", "mon:00:00-mon:03:00"),
    ("Mon:00:00-Mon:03:00", "mon:00:00-mon:03:00"),
    ("TUE:23:30-WED:00:00", "tue:23:30-wed:00:00"),
    ("sAt:04:15-sAt:04:45", "sat:04:15-sat:04:45"),
];

#[tokio::test]
async fn a_preferred_maintenance_window_is_stored_with_its_day_names_folded() {
    let router = emulator();
    for (index, (requested, stored)) in WINDOW_CASES.iter().enumerate() {
        let identifier = format!("emu-window-{index}");
        an_instance(
            &router,
            &identifier,
            &[("PreferredMaintenanceWindow", (*requested).to_string())],
        )
        .await;
        let read_back = describe(&router, requested, &identifier).await;
        let actual = optional_text(&read_back, "PreferredMaintenanceWindow");
        assert_eq!(
            actual.as_deref(),
            Some(*stored),
            "case {requested:?}: expected the instance to report {stored:?}, got {actual:?}"
        );
        // The same rule on a DB CLUSTER, whose own create and modify take the member too.
        let cluster = format!("emu-window-cluster-{index}");
        expect_ok(
            requested,
            call(
                &router,
                "CreateDBCluster",
                &params![
                    "DBClusterIdentifier" => &cluster,
                    "Engine" => "aurora-mysql",
                    "MasterUsername" => "clusteradmin",
                    "MasterUserPassword" => "a-strong-password-1",
                    "PreferredMaintenanceWindow" => *requested,
                ],
            )
            .await,
        );
        let created = expect_ok(
            requested,
            call(
                &router,
                "DescribeDBClusters",
                &params!["DBClusterIdentifier" => &cluster],
            )
            .await,
        );
        assert_eq!(
            text(&created, "PreferredMaintenanceWindow"),
            *stored,
            "case {requested:?}: expected the cluster to report {stored:?}, got {created}"
        );
    }
}

/// A window whose next occurrence is DAYS away, whatever day the test runs on: the day after
/// tomorrow, at a fixed time of day.
///
/// `pub(crate)` so the sibling suites of this one test target stage a change for a window
/// that is not open the same way rather than keeping a second copy of this calendar walk.
pub(crate) fn window_days_ahead() -> String {
    let day = weekday_index(2);
    format!("{day}:12:00-{day}:12:30")
}

/// A window that is OPEN at the instant this service reads it — it has already started and
/// has not yet ended: the span from two hours before now to two hours after it, in the
/// `ddd:hh24:mi-ddd:hh24:mi` form the member documents (the span wraps the week's end on its
/// own where now is near it, which is a form the member admits).
///
/// Derived from the INSTANT rather than from a calendar day, so no reading of the clock can
/// fall outside it: a whole-day span is closed again in the minute after its last one and
/// names yesterday's weekday once midnight has passed, and either would leave a change
/// staged against this window waiting for a week.
fn window_that_started() -> String {
    let now = chrono::Utc::now();
    let margin = chrono::Duration::hours(2);
    format!(
        "{}-{}",
        window_point(now - margin),
        window_point(now + margin)
    )
}

/// One `ddd:hh24:mi` point of a window: the UTC weekday, hour and minute of `at`, read
/// through the calendar of the `chrono` crate rather than by hand.
fn window_point(at: chrono::DateTime<chrono::Utc>) -> String {
    use chrono::Timelike;

    format!("{}:{:02}:{:02}", weekday_name(at), at.hour(), at.minute())
}

/// The `ddd` day name `days` days after today in UTC, in the lower-case form the member
/// documents.
fn weekday_index(days: i64) -> &'static str {
    weekday_name(chrono::Utc::now() + chrono::Duration::days(days))
}

/// The `ddd` day name of `at` in UTC, in the lower-case form the member documents, read
/// through the calendar of the `chrono` crate rather than by hand.
fn weekday_name(at: chrono::DateTime<chrono::Utc>) -> &'static str {
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    DAYS[chrono::Datelike::weekday(&at).num_days_from_monday() as usize]
}

/// The windows used above have to be ones this service can read as a schedule, and each has
/// to be what it is chosen FOR: the one a case moves a staged change into has to be OPEN at
/// the instant the service reads it, and the one a change is staged against has to be
/// CLOSED. Both are judged by the service's own reader of a window, so a fixture that would
/// make a case pass or fail by the clock rather than by the behaviour fails here instead.
#[test]
fn the_fixtures_are_well_formed() {
    for (what, window, open) in [
        ("the window days away", window_days_ahead(), false),
        ("the window that started", window_that_started(), true),
    ] {
        let (start, end) = window
            .split_once('-')
            .unwrap_or_else(|| panic!("{what} {window:?} must carry a start and an end"));
        for half in [start, end] {
            assert_eq!(
                half.split(':').count(),
                3,
                "window half {half:?} must be ddd:hh24:mi"
            );
        }
        let actual = window_is_open(&window, SystemTime::now());
        assert_eq!(
            actual, open,
            "{what} {window:?}: expected open = {open}, got {actual}"
        );
    }
}
