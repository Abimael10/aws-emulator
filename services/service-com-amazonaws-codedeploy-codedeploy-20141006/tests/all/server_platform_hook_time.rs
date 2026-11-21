// SPDX-License-Identifier: CC0-1.0

//! How long an EC2/on-premises (`Server`) deployment TAKES, driven over the wire.
//!
//! RULE under test: what the agent does on a host takes however long that host's hook
//! scripts state they take, and a deployment spends that work once per WAVE of its fleet —
//! so over a fleet of h hosts whose resolved deployment configuration deploys to at most c of
//! them at once (`minimumHealthyHosts`: the fleet less the healthy hosts it requires), a
//! revision whose hook states it takes t makes the deployment report a window of at least
//! ceil(h / c) times t between its own `createTime` and `completeTime`. The lifecycle event
//! that ran the hook reports that very window as its own `startTime`/`endTime`, and the
//! events after it begin where it ended; a revision whose hooks state no time at all keeps
//! the window the emulator's own spans give it, which is what makes the declared work the
//! difference between two legs over one fleet.
//!
//! The time is REPORTED and not waited for: a hook is performed by the bounded, in-process
//! session the host's own programs run in, which spends none of this process's clock on a
//! program and answers what that program STATED it took — so these rows assert on the windows
//! the service reports, never on how long the suite itself took.
//!
//! The rows differ along what the rule generalises over: the FORM the bound is stated in (a
//! published configuration of CodeDeploy's own catalogue, a `HOST_COUNT` configuration of the
//! account's own, a `FLEET_PERCENT` one), and the time the revision's hook states it takes
//! (none, and a dwell) — so the waves the fleet is cut into and the work spent per wave both
//! vary, and the inequality is asserted over each row and between the rows that differ in
//! exactly one of them.
//!
//! Every call goes through the front door a client uses: an `ec2Query` form POST for the
//! launches, S3's `restXml` API for each revision's bundle and CodeDeploy's awsJson envelope
//! for the application, the configurations, the groups and the deployments — all three
//! services mounted on ONE engine, through the harness the sibling suite
//! `server_platform_instance_deployments.rs` builds.
//!
//! Nothing here waits a fixed time: every span is a stored due instant, so a leg LOOKS AGAIN
//! until the deployment is terminal or the harness's own generous deadline passes, and nothing
//! asserts on how long the HOST took — only on the window the service itself reports.

use serde_json::{json, Value};

use crate::server_platform_instance_deployments::{app_spec, bundle, event_named, Fleet};

/// How long the dwelling revision's `BeforeInstall` hook states it takes, as the operand it
/// states it with and the seconds that operand means.
///
/// Long enough that the window it opens cannot be mistaken for the emulator's own spans. It
/// costs the suite nothing: the session that performs the hook spends none of this process's
/// clock on it, so what this changes is the window the service REPORTS.
const DWELL: (&str, i64) = ("45", 45);

/// The same dwell stated in MINUTES — the other suffix `sleep` accepts ("sleep 1m") — so a row
/// covers both forms a hook states its time in.
const DWELL_IN_MINUTES: (&str, i64) = ("1m", 60);

/// A hook that states no time at all.
const NO_DWELL: (&str, i64) = ("", 0);

/// The lifecycle event the dwelling hook runs for; the events after it in the in-place
/// sequence begin where it ended.
const HOOK_EVENT: &str = "BeforeInstall";

/// The events of the in-place sequence that come AFTER [`HOOK_EVENT`], in order.
const EVENTS_AFTER_HOOK: &[&str] = &[
    "Install",
    "AfterInstall",
    "ApplicationStart",
    "ValidateService",
];

/// The tag every host of the fleet carries, which every row's deployment group selects on.
const FLEET_TAG: (&str, &str) = ("Role", "hook-time");

/// How many hosts the fleet holds. Two is the smallest fleet a bound can cut into waves, and
/// the fleet every row below deploys to.
const HOSTS: usize = 2;

/// The published EC2/on-premises configuration that bounds nothing, from the catalogue
/// CodeDeploy seeds: `HOST_COUNT` 0, so the whole fleet goes out of service at once.
const ALL_AT_ONCE: &str = "CodeDeployDefault.AllAtOnce";

/// The published one that keeps every host but one healthy (`FLEET_PERCENT` 99), so a fleet
/// of any size is deployed to one host at a time.
const ONE_AT_A_TIME: &str = "CodeDeployDefault.OneAtATime";

/// How a row's deployment group is bounded.
#[derive(Debug, Clone, Copy)]
enum Bound {
    /// A configuration of CodeDeploy's own published catalogue, by name.
    Published(&'static str),
    /// One of the account's own: its `MinimumHealthyHosts` type and value.
    Custom(&'static str, i32),
}

/// One row: how the fleet is bounded, what its revision's hook states it takes, and how many
/// waves the bound cuts the fleet into.
#[derive(Debug, Clone, Copy)]
struct Case {
    case: &'static str,
    bound: Bound,
    /// What the row's `BeforeInstall` hook states it takes: the `sleep` operand it states it
    /// with, and the seconds that operand means.
    dwell: (&'static str, i64),
    /// How many waves [`HOSTS`] hosts are deployed to in under this row's bound, which is how
    /// many times the declared work is spent.
    waves: i64,
}

/// The table.
fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "the whole fleet at once, of a revision whose hook states no time",
            bound: Bound::Published(ALL_AT_ONCE),
            dwell: NO_DWELL,
            waves: 1,
        },
        Case {
            case: "the whole fleet at once, of a revision whose hook dwells",
            bound: Bound::Published(ALL_AT_ONCE),
            dwell: DWELL,
            waves: 1,
        },
        Case {
            case: "one host at a time by a HOST_COUNT configuration of the account's own",
            bound: Bound::Custom("HOST_COUNT", 1),
            dwell: DWELL,
            waves: 2,
        },
        Case {
            case: "one host at a time by the published OneAtATime configuration, dwelling \
                   minutes",
            bound: Bound::Published(ONE_AT_A_TIME),
            dwell: DWELL_IN_MINUTES,
            waves: 2,
        },
        Case {
            case: "half the fleet at a time by a FLEET_PERCENT configuration of its own",
            bound: Bound::Custom("FLEET_PERCENT", 50),
            dwell: DWELL,
            waves: 2,
        },
    ]
}

/// The hook script of a row: a program that states it takes `dwell` and writes a line where
/// the host can be read for it, or one that only writes the line.
///
/// `sleep` is what a script states its time with ("the sleep utility shall suspend execution
/// for at least the integral number of seconds specified by the time operand").
fn hook_script(dwell: &str) -> String {
    let mut script = String::from("#!/bin/bash\nset -e\n");
    if !dwell.is_empty() {
        script.push_str(&format!("sleep {dwell}\n"));
    }
    script.push_str("echo \"$LIFECYCLE_EVENT\" > /tmp/hook-time-event\n");
    script
}

/// The revision of a row: a `files` section copying one payload to this row's own destination
/// — so no row's install meets a file another row's left behind — and a `BeforeInstall` hook
/// running [`hook_script`].
fn revision(index: usize, dwell: &str) -> Vec<u8> {
    let document = app_spec(
        &[("payload.txt", &format!("/opt/hook-time-{index}"))],
        &[(HOOK_EVENT, "scripts/hook.sh")],
    );
    bundle(&[
        ("appspec.yml", document.as_str()),
        ("payload.txt", &format!("payload of leg {index}")),
        ("scripts/hook.sh", hook_script(dwell).as_str()),
    ])
}

/// The epoch seconds a deployment member carries, as the awsJson wire writes a timestamp.
fn seconds(case: &str, info: &Value, member: &str) -> f64 {
    info[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: {member} must be a timestamp, got {info:?}"))
}

/// The window a deployment reports for itself, in milliseconds: `completeTime` less
/// `createTime`, which is what a client times a deployment by.
fn window_millis(case: &str, info: &Value) -> i64 {
    let created = seconds(case, info, "createTime");
    let completed = seconds(case, info, "completeTime");
    assert!(
        completed >= created,
        "case {case:?}: a deployment cannot complete before it was created, got \
         createTime {created} and completeTime {completed}"
    );
    ((completed - created) * 1_000.0).round() as i64
}

/// The window one lifecycle event of a target reports, in milliseconds.
fn event_window_millis(case: &str, target: &Value, name: &str) -> i64 {
    let event = event_named(target, name)
        .unwrap_or_else(|| panic!("case {case:?}: the target reports {name}, got {target:?}"));
    let started = seconds(case, &event, "startTime");
    let ended = seconds(case, &event, "endTime");
    assert!(
        ended >= started,
        "case {case:?}: the {name} event cannot end before it started, got {event:?}"
    );
    ((ended - started) * 1_000.0).round() as i64
}

/// RULE: see the module docs — the declared hook work is spent once per wave the
/// `minimumHealthyHosts` bound cuts the fleet into, and the deployment's own window and its
/// targets' lifecycle events report it.
#[tokio::test]
async fn the_work_a_revisions_hooks_declare_is_spent_once_per_wave_of_the_bounded_fleet() {
    let fleet = Fleet::new();
    // ONE fleet for every row: the rule is about how the same hosts are paced under
    // different bounds, so the hosts are launched once and every row's group selects them.
    let mut hosts: Vec<String> = Vec::new();
    for index in 0..HOSTS {
        let host = fleet
            .launch(
                &format!("setup: host {index}"),
                &[(FLEET_TAG.0, FLEET_TAG.1)],
                None,
            )
            .await;
        hosts.push(host);
    }
    for host in &hosts {
        fleet.wait_until_running("setup", host).await;
    }

    // The window each row's deployment reported, kept so the rows that differ in exactly one
    // dimension can be compared with each other below.
    let mut windows: Vec<i64> = Vec::new();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let key = format!("revision-{index}.zip");
        fleet
            .upload(case, &key, &revision(index, row.dwell.0))
            .await;
        let configuration = match row.bound {
            Bound::Published(name) => name.to_string(),
            Bound::Custom(kind, value) => {
                let name = format!("hook-time-{index}");
                fleet
                    .ok(
                        case,
                        "CreateDeploymentConfig",
                        json!({
                            "deploymentConfigName": name,
                            "computePlatform": "Server",
                            "minimumHealthyHosts": {"type": kind, "value": value},
                        }),
                    )
                    .await;
                name
            }
        };
        let (application, group) = fleet
            .group(
                case,
                &format!("hook-time-{index}"),
                json!({
                    "ec2TagFilters": [
                        {"Key": FLEET_TAG.0, "Value": FLEET_TAG.1, "Type": "KEY_AND_VALUE"},
                    ],
                    "deploymentConfigName": configuration,
                }),
            )
            .await;
        let deployment = fleet
            .deploy(case, &application, &group, &key, json!({}))
            .await;
        let info = fleet.terminal(case, &deployment).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Succeeded"),
            "case {case:?}: the deployment must succeed, got {info:?}"
        );

        // The window the service itself reports for the deployment holds the declared work
        // once per wave the bound cut the fleet into.
        let window = window_millis(case, &info);
        let owed = row.waves.saturating_mul(row.dwell.1).saturating_mul(1_000);
        assert!(
            window >= owed,
            "case {case:?}: {} host(s) in {} wave(s), each running a hook that states it \
             takes {}s, cannot report less than {owed}ms, and the service reported {window}ms",
            HOSTS,
            row.waves,
            row.dwell.1
        );
        windows.push(window);

        // And every host reports the hook's own window as the lifecycle event that ran it,
        // with the events after it beginning where it ended.
        for host in &hosts {
            let target = fleet.instance_target(case, &deployment, host).await;
            let event = event_named(&target, HOOK_EVENT).unwrap_or_else(|| {
                panic!("case {case:?}: {host} reports a {HOOK_EVENT} event, got {target:?}")
            });
            assert_eq!(
                event["status"].as_str(),
                Some("Succeeded"),
                "case {case:?}: {host}'s {HOOK_EVENT} must have succeeded, got {event:?}"
            );
            let ran = event_window_millis(case, &target, HOOK_EVENT);
            assert!(
                ran >= row.dwell.1.saturating_mul(1_000),
                "case {case:?}: {host}'s {HOOK_EVENT} ran a hook that states it takes {}s, so \
                 its window cannot be shorter than that, and the target reported {ran}ms",
                row.dwell.1
            );
            let hook_ended = seconds(case, &event, "endTime");
            for later in EVENTS_AFTER_HOOK {
                let event = event_named(&target, later).unwrap_or_else(|| {
                    panic!("case {case:?}: {host} reports a {later} event, got {target:?}")
                });
                let started = seconds(case, &event, "startTime");
                assert!(
                    started >= hook_ended,
                    "case {case:?}: {host}'s {later} begins where {HOOK_EVENT} ended \
                     ({hook_ended}), got {started}"
                );
            }
        }
    }

    // The rows that differ in exactly ONE dimension, compared with each other: the same
    // fleet, bounded the same way, reports a LONGER window for the revision whose hook dwells
    // than for the one whose hook states no time...
    let (instant_at_once, dwelling_at_once) = (windows[0], windows[1]);
    assert!(
        dwelling_at_once > instant_at_once,
        "the same fleet at once reports a longer window for a revision whose hook states it \
         takes {}s ({dwelling_at_once}ms) than for one that states no time \
         ({instant_at_once}ms)",
        DWELL.1
    );
    // ...and the same declared work over the same fleet is spent once per wave, so a bound
    // that cuts the fleet into waves reports a longer window than one that deploys to all of
    // it at once — whichever form that bound is stated in.
    for bounded in [windows[2], windows[3], windows[4]] {
        assert!(
            bounded > dwelling_at_once,
            "a fleet of {HOSTS} deployed to one host at a time spends the declared work once \
             per wave, so it cannot report {bounded}ms when the same fleet at once reported \
             {dwelling_at_once}ms for work that is no shorter"
        );
    }
}
