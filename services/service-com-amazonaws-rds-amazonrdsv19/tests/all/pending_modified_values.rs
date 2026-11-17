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

use service_com_amazonaws_rds_amazonrdsv19::state::STATUS_TRANSITION_DELAY;
use std::time::{Duration, SystemTime};

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, subtrees, text, texts,
    wait_for_settle,
};

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// How long a poll for a change the clock owes keeps looking before giving up: far longer
/// than the span this service owes a transition in, so a loaded machine still gets there.
/// Nothing is asserted about elapsed time — only that the condition holds in the end.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How often that poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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
    wait_for_settle();
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
    let found = subtrees(&body, "DBInstance");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one DBInstance, got {found:?}"
    );
    found.into_iter().next().expect("checked just above")
}

/// What `instance` reports as pending, or `None` when it reports no pending element at all.
fn pending(instance: &str) -> Option<String> {
    subtrees(instance, "PendingModifiedValues").into_iter().next()
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
        // answer.
        wait_for_settle();
        let read_back = describe(&router, case.case, case.identifier).await;
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
    wait_for_settle();
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
    wait_for_settle();
    let read_back = describe(&router, "both changes applied", "emu-apply-now").await;
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
        let deadline = SystemTime::now() + POLL_DEADLINE;
        loop {
            let read_back = describe(&router, case.case, settled_identifier).await;
            let status = optional_text(&read_back, "DBInstanceStatus");
            let settled = if renaming {
                status.as_deref() == Some("available")
            } else {
                pending(&read_back).is_none()
            };
            if settled {
                assert_eq!(
                    status.as_deref(),
                    Some("available"),
                    "case {:?}: the instance must be available once nothing is pending, got \
                     {read_back}",
                    case.case
                );
                let live = optional_text(&read_back, case.reported_as);
                // `DbInstancePort` reports the port of a CLUSTER MEMBER and zero for an
                // instance that belongs to no DB cluster, which every instance here is, so
                // the applied value is not observable on that member — what this row asserts
                // is that the staged port stops being pending.
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
                break;
            }
            assert!(
                SystemTime::now() < deadline,
                "case {:?}: the change asked for at once must be applied, last read \
                 {read_back}",
                case.case
            );
            std::thread::sleep(POLL_INTERVAL);
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
    let deadline = SystemTime::now() + POLL_DEADLINE;
    loop {
        let read_back = describe(router, case, "emu-log-exports").await;
        let exports = texts_in(&read_back, "EnabledCloudwatchLogsExports");
        if holds(&exports) {
            return exports;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: the export configuration must be applied, last read {read_back}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
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
    wait_for_settle();
    expect_ok(
        "reboot",
        call(
            &router,
            "RebootDBInstance",
            &params!["DBInstanceIdentifier" => "emu-defer-reboot"],
        )
        .await,
    );
    wait_for_settle();
    let read_back = describe(&router, "after the reboot", "emu-defer-reboot").await;
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
    wait_for_settle();
    let read_back = describe(&router, "before the window", "emu-defer-window").await;
    assert_eq!(
        optional_text(&read_back, "AllocatedStorage").as_deref(),
        Some("20"),
        "a change staged for a window days away stays pending, got {read_back}"
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
    let deadline = SystemTime::now() + POLL_DEADLINE;
    loop {
        let read_back = describe(&router, "in the window", "emu-defer-window").await;
        if optional_text(&read_back, "AllocatedStorage").as_deref() == Some("40") {
            assert!(
                pending(&read_back).is_none(),
                "nothing stays pending once the window applied it, got {read_back}"
            );
            break;
        }
        assert!(
            SystemTime::now() < deadline,
            "the change staged for the window must be applied once that window has come \
             round, last read {read_back}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
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
        wait_for_settle();
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
fn window_days_ahead() -> String {
    let day = weekday_index(2);
    format!("{day}:12:00-{day}:12:30")
}

/// A window that has ALREADY started: today's weekday at the start of the day, so its
/// occurrence is behind the clock whenever the test runs.
fn window_that_started() -> String {
    let day = weekday_index(0);
    format!("{day}:00:00-{day}:23:59")
}

/// The `ddd` day name `days` days after today in UTC, in the lower-case form the member
/// documents, read through the calendar of the `chrono` crate rather than by hand.
fn weekday_index(days: i64) -> &'static str {
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let at = chrono::Utc::now() + chrono::Duration::days(days);
    DAYS[chrono::Datelike::weekday(&at).num_days_from_monday() as usize]
}

/// The window used above has to be one this service can read as a schedule, which is what
/// the rest of this suite depends on; the span a transition is owed in is what every wait
/// here is sized against.
#[test]
fn the_fixtures_are_well_formed() {
    assert!(
        STATUS_TRANSITION_DELAY < POLL_DEADLINE,
        "the poll deadline must be generous beside the span a transition is owed in"
    );
    for window in [window_days_ahead(), window_that_started()] {
        let (start, end) = window
            .split_once('-')
            .unwrap_or_else(|| panic!("window {window:?} must carry a start and an end"));
        for half in [start, end] {
            assert_eq!(
                half.split(':').count(),
                3,
                "window half {half:?} must be ddd:hh24:mi"
            );
        }
    }
}
