// SPDX-License-Identifier: CC0-1.0

//! What a failed lifecycle event REPORTS about the script that failed, driven over the wire.
//!
//! RULE under test: `Diagnostics$logTail` is "the last portion of the diagnostic log" of the
//! script the event ran — what that script WROTE, on both of the streams a program writes on,
//! since a hook reports why it failed the way a program does: on standard error. So an event
//! whose script exited non-zero carries the `ScriptFailed` code the model names, that script's
//! own location, and a `logTail` holding every line it wrote; an event whose script wrote
//! nothing carries no log at all rather than an empty one.
//!
//! The rows differ along the stream the script writes on — standard error, standard output,
//! both, neither — which is exactly what the rule generalises over, and each row's lines are
//! minted in this process so no canned log could produce them.
//!
//! Every call goes through the front door a client uses, over the harness the sibling suite
//! `server_platform_instance_deployments.rs` builds (CodeDeploy, Amazon EC2 and Amazon S3 on
//! ONE engine). Nothing waits a fixed time: each leg looks again until its deployment is
//! terminal.

use serde_json::{json, Value};

use crate::server_platform_instance_deployments::{app_spec, bundle, event_named, Fleet};

/// The lifecycle event whose script fails in every row: the first scriptable event after the
/// agent's own Install, so the events before it have run and the ones after it are skipped.
const HOOK_EVENT: &str = "AfterInstall";

/// The `LifecycleErrorCode` of a script that "failed to run as expected".
const SCRIPT_FAILED: &str = "ScriptFailed";

/// The tag the host carries and every row's deployment group selects on.
const FLEET_TAG: (&str, &str) = ("Name", "hook-diagnostics-host");

/// What a row's failing script writes, and where it writes it.
#[derive(Debug, Clone, Copy)]
struct Case {
    case: &'static str,
    /// The line the script writes to its standard output, if any.
    stdout: Option<&'static str>,
    /// The line it writes to its standard error, if any.
    stderr: Option<&'static str>,
    /// The status it exits with — non-zero, so the event fails.
    status: i32,
}

/// The table.
fn rows() -> Vec<Case> {
    vec![
        Case {
            case: "a hook that reports why it failed on standard error",
            stdout: None,
            stderr: Some("hook-diagnostics-stderr-only"),
            status: 1,
        },
        Case {
            case: "a hook that reports it on standard output",
            stdout: Some("hook-diagnostics-stdout-only"),
            stderr: None,
            status: 2,
        },
        Case {
            case: "a hook that writes on both streams",
            stdout: Some("hook-diagnostics-stdout-of-both"),
            stderr: Some("hook-diagnostics-stderr-of-both"),
            status: 9,
        },
        Case {
            case: "a hook that writes nothing at all",
            stdout: None,
            stderr: None,
            status: 7,
        },
    ]
}

/// The script of one row: what it writes, on the streams the row names, and the status it
/// exits with.
fn hook_script(row: &Case) -> String {
    let mut script = String::from("#!/bin/bash\n");
    if let Some(line) = row.stdout {
        script.push_str(&format!("echo {line}\n"));
    }
    if let Some(line) = row.stderr {
        script.push_str(&format!("echo {line} >&2\n"));
    }
    script.push_str(&format!("exit {}\n", row.status));
    script
}

/// The revision of one row: a `files` section copying a payload to the row's own destination,
/// and the failing hook above.
fn revision(index: usize, row: &Case) -> Vec<u8> {
    let document = app_spec(
        &[("payload.txt", &format!("/opt/hook-diagnostics-{index}"))],
        &[(HOOK_EVENT, "scripts/hook.sh")],
    );
    bundle(&[
        ("appspec.yml", document.as_str()),
        ("payload.txt", "payload"),
        ("scripts/hook.sh", hook_script(row).as_str()),
    ])
}

/// RULE: see the module docs — a failed event's `logTail` carries what the script wrote, on
/// either stream, and nothing where it wrote nothing.
#[tokio::test]
async fn a_failed_events_log_tail_carries_what_the_script_wrote_on_either_stream() {
    let fleet = Fleet::new();
    let host = fleet
        .launch("setup", &[(FLEET_TAG.0, FLEET_TAG.1)], None)
        .await;
    fleet.wait_until_running("setup", &host).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let key = format!("hook-diagnostics-{index}.zip");
        fleet.upload(case, &key, &revision(index, row)).await;
        let (application, group) = fleet
            .group(
                case,
                &format!("hook-diagnostics-{index}"),
                json!({
                    "ec2TagFilters": [
                        {"Key": FLEET_TAG.0, "Value": FLEET_TAG.1, "Type": "KEY_AND_VALUE"},
                    ],
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
        assert_eq!(
            event["status"].as_str(),
            Some("Failed"),
            "case {case:?}: the event whose script exited {} must be Failed, got {event:?}",
            row.status
        );
        let diagnostics = event["diagnostics"].clone();
        assert_eq!(
            diagnostics["errorCode"].as_str(),
            Some(SCRIPT_FAILED),
            "case {case:?}: expected the {SCRIPT_FAILED} code, got {diagnostics:?}"
        );
        assert_eq!(
            diagnostics["scriptName"].as_str(),
            Some("scripts/hook.sh"),
            "case {case:?}: expected the script the AppSpec named, got {diagnostics:?}"
        );
        let log_tail = diagnostics["logTail"].clone();
        for written in [row.stdout, row.stderr].into_iter().flatten() {
            let carried = log_tail.as_str().unwrap_or_default();
            assert!(
                carried.contains(written),
                "case {case:?}: the log of the failed script must carry the line it wrote \
                 ({written:?}), and the event reports {log_tail:?}"
            );
        }
        if row.stdout.is_none() && row.stderr.is_none() {
            assert_eq!(
                log_tail,
                Value::Null,
                "case {case:?}: a script that wrote nothing has no log to report, and the \
                 event reports {log_tail:?}"
            );
        }
    }
}
