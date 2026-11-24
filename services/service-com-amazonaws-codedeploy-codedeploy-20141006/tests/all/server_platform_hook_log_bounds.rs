// SPDX-License-Identifier: CC0-1.0

//! How much of a failed hook's log a lifecycle event reports, and which script's log it is.
//!
//! RULE under test: `Diagnostics$logTail` is "the last portion of the diagnostic log" of the
//! script the lifecycle event ran, and "if available, CodeDeploy returns up to the last 4 KB
//! of the diagnostic log"
//! (`smithy_query(query="doc",shape="Diagnostics",member="logTail")`) — so what an event
//! reports is a SUFFIX of what its script wrote: the whole of it while it fits in that bound,
//! and otherwise its last 4 KB, the bytes nearest the failure. The script whose log it is is
//! the one the event actually ran, which for the ApplicationStop of a REDEPLOY is the
//! previously deployed revision's ("the AppSpec file and scripts used for the ApplicationStop
//! … are the ones from the previously deployed revision"), and the event reads the same
//! whichever of the modelled views a client reads it through — `InstanceTarget$lifecycleEvents`
//! (GetDeploymentTarget, BatchGetDeploymentTargets) and the deprecated
//! `InstanceSummary$lifecycleEvents` (GetDeploymentInstance).
//!
//! The first case's rows differ along the SIZE of the log — which is exactly what the bound
//! generalises over: a few short lines, a log just under 4 KB, one just past it and one many
//! times it, each line minted in this process so no canned log could produce it.
//!
//! Every call goes through the front door a client uses, over the harness the sibling suite
//! `server_platform_instance_deployments.rs` builds (CodeDeploy, Amazon EC2 and Amazon S3 on
//! ONE engine). Nothing waits a fixed time: each leg looks again until its deployment is
//! terminal.

use serde_json::json;

use crate::server_platform_instance_deployments::{app_spec, bundle, event_named, Fleet};

/// How many bytes of a script's log one event reports, from the model's own sentence above:
/// "up to the last 4 KB of the diagnostic log".
///
/// Written out here on purpose rather than imported from the implementation: the number is
/// the MODEL'S, so a case that read it off the code under test would agree with whatever that
/// code happened to hold and could never fail.
const MAX_LOG_TAIL_BYTES: usize = 4 * 1024;

/// The `LifecycleErrorCode` of a script that "failed to run as expected".
const SCRIPT_FAILED: &str = "ScriptFailed";

/// The lifecycle event whose script fails in the size rows: the first scriptable event after
/// the agent's own Install, so everything before it has run.
const HOOK_EVENT: &str = "AfterInstall";

/// The event whose script a redeploy takes from the revision the host already holds.
const APPLICATION_STOP: &str = "ApplicationStop";

/// How much of a log one row's script writes: `lines` lines of `width` bytes each.
#[derive(Debug, Clone, Copy)]
struct Case {
    case: &'static str,
    lines: usize,
    width: usize,
}

/// The table: the published bound, one row under it, one row just under it, one just past it
/// and one many times it.
fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "a log of a few short lines",
            lines: 3,
            width: 40,
        },
        Case {
            case: "a log just under the bound",
            lines: 40,
            width: 100,
        },
        Case {
            case: "a log just past the bound",
            lines: 42,
            width: 100,
        },
        Case {
            case: "a log many times the bound",
            lines: 200,
            width: 100,
        },
    ]
}

/// The lines one row's script writes, each one `width` bytes long and naming its own position
/// so the row can tell the front of the log from its end.
fn written_lines(row: &Case) -> Vec<String> {
    (0..row.lines)
        .map(|index| {
            let head = format!("log-line-{index:04}-");
            let padding = row.width.saturating_sub(head.len());
            format!("{head}{}", "x".repeat(padding))
        })
        .collect()
}

/// The script of one row: it writes the lines above and exits non-zero, so the event it ran
/// for fails with a log to report.
fn hook_script(lines: &[String]) -> String {
    let mut script = String::from("#!/bin/bash\n");
    for line in lines {
        script.push_str(&format!("echo {line}\n"));
    }
    script.push_str("exit 1\n");
    script
}

/// RULE: see the module docs — the log an event reports is the last portion of what its
/// script wrote, whole while it fits in the published bound and its last 4 KB past it.
#[tokio::test]
async fn a_failed_events_log_tail_is_the_last_portion_within_the_published_bound() {
    let fleet = Fleet::new();
    let tag = ("Name", "hook-log-bounds-host");
    let host = fleet.launch("setup", &[(tag.0, tag.1)], None).await;
    fleet.wait_until_running("setup", &host).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let lines = written_lines(row);
        // What the script wrote, as a log reads: the lines it wrote, in the order it wrote
        // them.
        let whole = lines.join("\n");
        let key = format!("hook-log-bounds-{index}.zip");
        let document = app_spec(
            &[("payload.txt", &format!("/opt/hook-log-bounds-{index}"))],
            &[(HOOK_EVENT, "scripts/hook.sh")],
        );
        let script = hook_script(&lines);
        fleet
            .upload(
                case,
                &key,
                &bundle(&[
                    ("appspec.yml", document.as_str()),
                    ("payload.txt", "payload"),
                    ("scripts/hook.sh", script.as_str()),
                ]),
            )
            .await;
        let (application, group) = fleet
            .group(
                case,
                &format!("hook-log-bounds-{index}"),
                json!({
                    "ec2TagFilters": [{"Key": tag.0, "Value": tag.1, "Type": "KEY_AND_VALUE"}],
                }),
            )
            .await;
        let deployment = fleet
            .deploy(case, &application, &group, &key, json!({}))
            .await;
        let info = fleet.terminal(case, &deployment).await;
        assert_eq!(
            info["status"].as_str(),
            Some("Failed"),
            "case {case:?}: a deployment whose hook exits non-zero must fail, got {info:?}"
        );

        let target = fleet.instance_target(case, &deployment, &host).await;
        let event = event_named(&target, HOOK_EVENT).unwrap_or_else(|| {
            panic!("case {case:?}: the target reports a {HOOK_EVENT} event, got {target:?}")
        });
        let diagnostics = event["diagnostics"].clone();
        assert_eq!(
            diagnostics["errorCode"].as_str(),
            Some(SCRIPT_FAILED),
            "case {case:?}: expected the {SCRIPT_FAILED} code, got {diagnostics:?}"
        );
        let carried = diagnostics["logTail"].as_str().unwrap_or_default().to_string();
        assert!(
            carried.len() <= MAX_LOG_TAIL_BYTES,
            "case {case:?}: a log is reported up to {MAX_LOG_TAIL_BYTES} bytes, and the event \
             reports {} of them",
            carried.len()
        );
        assert!(
            whole.ends_with(&carried),
            "case {case:?}: the reported log must be the LAST portion of the {} bytes the \
             script wrote, and it reports {carried:?}",
            whole.len()
        );
        if whole.len() <= MAX_LOG_TAIL_BYTES {
            assert_eq!(
                carried, whole,
                "case {case:?}: a log inside the bound is reported whole, and the event \
                 reports {} of its {} bytes",
                carried.len(),
                whole.len()
            );
            continue;
        }
        // Past the bound the tail FILLS it, so an event cannot pass by reporting only the
        // last line: one more line would not fit in what is left of the 4 KB.
        assert!(
            carried.len() + row.width + 1 > MAX_LOG_TAIL_BYTES,
            "case {case:?}: the reported log must fill the {MAX_LOG_TAIL_BYTES}-byte bound, \
             and it carries {} bytes",
            carried.len()
        );
        let last = lines.last().expect("the row writes at least one line");
        assert!(
            carried.contains(last.as_str()),
            "case {case:?}: the bytes nearest the failure must survive, and the event reports \
             a log ending {:?}",
            &carried[carried.len().saturating_sub(row.width)..]
        );
        let first = &lines[0];
        assert!(
            !carried.contains(first.as_str()),
            "case {case:?}: a log past the bound is truncated at the FRONT, and the event \
             still reports the first of its {} bytes",
            whole.len()
        );
    }
}

/// RULE: see the module docs — the log a redeploy's failed ApplicationStop reports is the one
/// the script it actually ran wrote, which is the previously deployed revision's, and every
/// modelled view of that event reports it alike.
#[tokio::test]
async fn a_redeploys_failed_application_stop_reports_the_previous_revisions_script_log() {
    let fleet = Fleet::new();
    let case = "a redeploy whose ApplicationStop script exits non-zero";
    let tag = ("Name", "hook-log-stop-host");
    let host = fleet.launch(case, &[(tag.0, tag.1)], None).await;
    fleet.wait_until_running(case, &host).await;
    let (application, group) = fleet
        .group(
            case,
            "hook-log-stop",
            json!({
                "ec2TagFilters": [{"Key": tag.0, "Value": tag.1, "Type": "KEY_AND_VALUE"}],
            }),
        )
        .await;

    // Revision A's ApplicationStop script reports on both streams and exits non-zero. It is
    // not run by A's own deployment — there is no revision on the host to stop yet — so the
    // log below can only be the one it wrote when B's deployment ran it.
    let on_stdout = "stop-script-wrote-this-on-stdout";
    let on_stderr = "stop-script-wrote-this-on-stderr";
    let stop_script =
        format!("#!/bin/bash\necho {on_stdout}\necho {on_stderr} >&2\nexit 3\n");
    let first = app_spec(
        &[("site/index.html", "/var/www/hook-log-stop")],
        &[(APPLICATION_STOP, "scripts/stop.sh")],
    );
    fleet
        .upload(
            case,
            "stop-log-a.zip",
            &bundle(&[
                ("appspec.yml", first.as_str()),
                ("site/index.html", "<p>A</p>"),
                ("scripts/stop.sh", stop_script.as_str()),
            ]),
        )
        .await;
    // Revision B declares no hook at all, so nothing of ITS bundle could have written a log.
    let second = app_spec(&[("site/index.html", "/var/www/hook-log-stop")], &[]);
    fleet
        .upload(
            case,
            "stop-log-b.zip",
            &bundle(&[
                ("appspec.yml", second.as_str()),
                ("site/index.html", "<p>B</p>"),
            ]),
        )
        .await;

    let one = fleet
        .deploy(case, &application, &group, "stop-log-a.zip", json!({}))
        .await;
    let info = fleet.terminal(case, &one).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Succeeded"),
        "case {case:?}: the first deployment has no revision to stop, got {info:?}"
    );

    let two = fleet
        .deploy(case, &application, &group, "stop-log-b.zip", json!({}))
        .await;
    let info = fleet.terminal(case, &two).await;
    assert_eq!(
        info["status"].as_str(),
        Some("Failed"),
        "case {case:?}: a redeploy whose ApplicationStop fails must fail, got {info:?}"
    );

    // The same event through every modelled view of it: the target's own, the batch read of
    // it, and the deprecated instance summary.
    let target = fleet.instance_target(case, &two, &host).await;
    let batched = fleet
        .ok(
            case,
            "BatchGetDeploymentTargets",
            json!({"deploymentId": two, "targetIds": [host]}),
        )
        .await["deploymentTargets"][0]["instanceTarget"]
        .clone();
    let summary = fleet
        .ok(
            case,
            "GetDeploymentInstance",
            json!({"deploymentId": two, "instanceId": host}),
        )
        .await["instanceSummary"]
        .clone();

    for (view, read) in [
        ("InstanceTarget$lifecycleEvents", &target),
        ("the batch read of that target", &batched),
        ("InstanceSummary$lifecycleEvents", &summary),
    ] {
        let event = event_named(read, APPLICATION_STOP).unwrap_or_else(|| {
            panic!("case {case:?}: {view} reports a {APPLICATION_STOP} event, got {read:?}")
        });
        assert_eq!(
            event["status"].as_str(),
            Some("Failed"),
            "case {case:?}: {view} must report the failed event, got {event:?}"
        );
        let diagnostics = event["diagnostics"].clone();
        assert_eq!(
            (
                diagnostics["errorCode"].as_str(),
                diagnostics["scriptName"].as_str()
            ),
            (Some(SCRIPT_FAILED), Some("scripts/stop.sh")),
            "case {case:?}: {view} must name the script that failed, got {diagnostics:?}"
        );
        let carried = diagnostics["logTail"].as_str().unwrap_or_default();
        for written in [on_stdout, on_stderr] {
            assert!(
                carried.contains(written),
                "case {case:?}: {view} must report the log of the script the event RAN, which \
                 wrote {written:?}, and it reports {carried:?}"
            );
        }
    }

    assert_eq!(
        target["status"].as_str(),
        Some("Failed"),
        "case {case:?}: the host whose ApplicationStop failed must report it, got {target:?}"
    );
}
