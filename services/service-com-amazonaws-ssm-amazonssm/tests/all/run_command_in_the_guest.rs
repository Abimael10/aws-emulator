// SPDX-License-Identifier: CC0-1.0

//! A Run Command invocation RUNNING in the guest of the node it was sent to, driven end to
//! end with Systems Manager, Amazon EC2 and IAM on one engine.
//!
//! RULE under test: AN INVOCATION IS THE DOCUMENT'S SCRIPT PERFORMED IN THE NODE'S GUEST —
//! so what a client reads back is what that guest wrote and the exit code its shell ended
//! on, the invocation reaches a terminal status on the clock without anything asking it to,
//! and a command the guest has no program for runs nothing and answers the status a shell
//! answers for a utility it could not find.
//!
//! Every case goes through the front door: `AmazonSSM.SendCommand` and
//! `AmazonSSM.GetCommandInvocation` over `awsJson1_1`, with the instance launched over
//! EC2's `ec2Query` wire and its role, policy and instance profile created over IAM's
//! `awsQuery` wire. The node itself is never seeded: it is the managed node the launched
//! instance became (`ec2_instance_managed_nodes`).
//!
//! The rows span what the rule generalises over: a script the guest performs, a script it
//! cannot, a script that writes the machine's own storage and reads it back, a script that
//! names what that storage holds, several lines
//! of output, and the parameter form the published document takes its script in.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};

/// The published command document that runs a shell script on a Linux node, which is what
/// [`send`] carries — so every suite that drives a script through this one sends it the same
/// way.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// The step of that document a Linux node runs — the name its content declares, which is
/// what `GetCommandInvocation`'s `PluginName` reports.
const LINUX_STEP: &str = "runShellScriptOnLinux";

/// The status a shell answers for a utility it could not find, which is what a guest with no
/// such program answers (POSIX, and
/// `service_common::posix_shell_session::Statuses::could_not_run`).
const COULD_NOT_RUN: i64 = 127;

/// How long a case looks for the invocation to finish, far longer than the span the clock
/// owes that move after.
const FINISH_DEADLINE: Duration = Duration::from_secs(60);

/// How often it looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `CommandInvocationStatus` values that are terminal, from the modelled enum
/// (`smithy_query(query="shape",shape="CommandInvocationStatus")` — the ones
/// `Command$StatusDetails` documents as final).
const TERMINAL: &[&str] = &["Success", "Cancelled", "Failed", "TimedOut"];

/// One row: the script sent, the status the invocation ends in, the exit code the guest's
/// shell answers, and the output it wrote.
struct ScriptCase {
    case: &'static str,
    commands: Vec<&'static str>,
    status: &'static str,
    response_code: i64,
    output: &'static str,
}

/// Launch an instance that becomes a managed node and answer its id.
async fn node(emulator: &Emulator, case: &str, role: &str) -> String {
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, role, &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = emulator.launch(case, &image, Some(&profile)).await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    let reported = emulator.nodes(case, REGION).await;
    assert!(
        reported.contains_key(&instance),
        "case {case:?}: the launched instance must become a managed node first, got {:?}",
        reported.keys().collect::<Vec<_>>()
    );
    instance
}

/// Send one script to `instance` and answer the command id.
///
/// `pub(crate)` for the same reason [`finish_due`] is: the suite about what an instance's
/// boot program wrote sends its reads the same way.
pub(crate) async fn send(
    emulator: &Emulator,
    case: &str,
    instance: &str,
    commands: &[&str],
) -> String {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "SendCommand",
            &json!({
                "DocumentName": RUN_SHELL_SCRIPT,
                "InstanceIds": [instance],
                "Parameters": {"commands": commands},
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the send must be served, got {status}: {answered}"
    );
    // "Pending: The command hasn't been sent to any managed nodes" is the status a send
    // answers with: the move off it is what the clock owes.
    assert_eq!(
        answered["Command"]["Status"].as_str(),
        Some("Pending"),
        "case {case:?}: a send answers Pending, got {answered}"
    );
    answered["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the send must mint a command id: {answered}"))
        .to_string()
}

/// Move the instant the clock owes `command_id`'s invocations their terminal status to now,
/// so a case about WHAT the guest answered does not wait out the span — the same way this
/// crate's other clock-owed suites move an owed instant through the store rather than
/// sleeping for it. One case below leaves it alone and waits on the real clock instead.
pub(crate) fn finish_due(emulator: &Emulator, command_id: &str) {
    // `pub(crate)`: the invocation-LISTING suite
    // (`tests/all/command_invocation_listing.rs`) needs the same move, and a second copy of
    // it would reach into the store a second way.
    let state = emulator.ssm_state();
    let mut store = state.lock();
    let command = store
        .commands
        .get_mut(&(REGION.to_string(), command_id.to_string()))
        .unwrap_or_else(|| panic!("the store must hold the command {command_id}"));
    for invocation in command.invocations.values_mut() {
        if let Some(outcome) = invocation.outcome.as_mut() {
            outcome.due_at = SystemTime::now();
        }
    }
}

/// Look at the invocation until it reaches a terminal status, up to a deadline far longer
/// than the span that move is owed after.
///
/// `pub(crate)`: every suite that reads an invocation back waits for it the same way, and the
/// catalog of terminal statuses it waits on belongs in one place.
pub(crate) async fn finished(
    emulator: &Emulator,
    case: &str,
    command_id: &str,
    instance: &str,
) -> Value {
    let deadline = Instant::now() + FINISH_DEADLINE;
    loop {
        let (status, answered) = emulator
            .ssm(
                REGION,
                "GetCommandInvocation",
                &json!({"CommandId": command_id, "InstanceId": instance}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read must be served, got {status}: {answered}"
        );
        let reached = answered["Status"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&reached.as_str()) {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the invocation to finish with nothing but the clock \
             running, still {reached:?}: {answered}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the invocation is the document's script performed in the guest, so its status, its
/// exit code and its output are the guest's own.
#[tokio::test]
async fn an_invocation_reports_what_the_guest_ran() {
    let emulator = Emulator::new();
    let instance = node(&emulator, "the script table", "script-role").await;

    let rows = [
        ScriptCase {
            case: "a script the guest performs",
            commands: vec!["echo hello"],
            status: "Success",
            response_code: 0,
            output: "hello\n",
        },
        ScriptCase {
            case: "a script of several lines",
            commands: vec!["echo first", "echo second"],
            status: "Success",
            response_code: 0,
            output: "first\nsecond\n",
        },
        ScriptCase {
            case: "a script that answers failure",
            commands: vec!["false"],
            status: "Failed",
            response_code: 1,
            output: "",
        },
        // A program the guest does not have is where the shell's own command search ends with
        // nothing, and a shell says so: the diagnostic naming that command reaches the
        // guest's console, which is the one stream this invocation reports.
        ScriptCase {
            case: "a script naming a program the guest does not have",
            commands: vec!["/usr/bin/definitely-not-installed --now"],
            status: "Failed",
            response_code: COULD_NOT_RUN,
            output: "sh: /usr/bin/definitely-not-installed: command not found\n",
        },
        ScriptCase {
            case: "a script that writes the machine's storage and reads it back",
            commands: vec!["echo kept > /tmp/run-command", "cat /tmp/run-command"],
            status: "Success",
            response_code: 0,
            output: "kept\n",
        },
        // The file utilities a script written against a machine's storage names: `touch`
        // makes the file and `ls` writes the path back, which is how such a script checks
        // that what it made is there.
        ScriptCase {
            case: "a script that makes a file and names it back",
            commands: vec!["touch /tmp/made && ls /tmp/made"],
            status: "Success",
            response_code: 0,
            output: "/tmp/made\n",
        },
        ScriptCase {
            case: "a script that lists a directory of the machine's storage",
            commands: vec![
                "mkdir -p /tmp/listed",
                "touch /tmp/listed/beta",
                "touch /tmp/listed/alpha",
                "ls /tmp/listed",
            ],
            status: "Success",
            response_code: 0,
            output: "alpha\nbeta\n",
        },
        // A path the machine does not hold is the utility's own failure, reported in its own
        // wording. The guest's session reports ONE stream, so that diagnostic is what the
        // invocation carries as its output — the same way the file utilities beside `ls`
        // report a path they could not read.
        ScriptCase {
            case: "a script that names a path the machine does not hold",
            commands: vec!["ls /tmp/not-there"],
            status: "Failed",
            response_code: 1,
            output: "ls: cannot access '/tmp/not-there': No such file or directory\n",
        },
    ];

    for row in rows {
        let command_id = send(&emulator, row.case, &instance, &row.commands).await;
        finish_due(&emulator, &command_id);
        let answered = finished(&emulator, row.case, &command_id, &instance).await;
        assert_eq!(
            answered["Status"].as_str(),
            Some(row.status),
            "case {:?}: expected status {:?}, got {answered}",
            row.case,
            row.status
        );
        assert_eq!(
            answered["StatusDetails"].as_str(),
            Some(row.status),
            "case {:?}: the detailed status is the same word, got {answered}",
            row.case
        );
        assert_eq!(
            answered["ResponseCode"].as_i64(),
            Some(row.response_code),
            "case {:?}: expected the exit code the guest's shell answered, got {answered}",
            row.case
        );
        assert_eq!(
            answered["StandardOutputContent"].as_str(),
            Some(row.output),
            "case {:?}: expected the guest's output, got {answered}",
            row.case
        );
        assert_eq!(
            answered["PluginName"].as_str(),
            Some(LINUX_STEP),
            "case {:?}: the step that ran is the document's own, got {answered}",
            row.case
        );
        // A finished invocation reports when the agent took it, when its run was owed to
        // have finished and the span between the two, rather than three empty strings.
        for member in [
            "ExecutionStartDateTime",
            "ExecutionEndDateTime",
            "ExecutionElapsedTime",
        ] {
            assert!(
                answered[member]
                    .as_str()
                    .is_some_and(|written| !written.is_empty()),
                "case {:?}: expected {member} on a finished invocation, got {answered}",
                row.case
            );
        }
    }
}

/// RULE: what one invocation wrote on the machine's storage is still there for the next one
/// — the tree a script runs in IS the instance's own, the same storage its boot script
/// wrote.
#[tokio::test]
async fn what_one_invocation_wrote_is_there_for_the_next() {
    let emulator = Emulator::new();
    let instance = node(&emulator, "the storage case", "storage-role").await;

    let wrote = send(
        &emulator,
        "the storage case",
        &instance,
        &["echo first-run > /var/tmp/kept"],
    )
    .await;
    finish_due(&emulator, &wrote);
    let answered = finished(&emulator, "the storage case", &wrote, &instance).await;
    assert_eq!(
        answered["Status"].as_str(),
        Some("Success"),
        "case \"the storage case\": the write must succeed, got {answered}"
    );

    let read = send(
        &emulator,
        "the storage case",
        &instance,
        &["cat /var/tmp/kept"],
    )
    .await;
    finish_due(&emulator, &read);
    let answered = finished(&emulator, "the storage case", &read, &instance).await;
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some("first-run\n"),
        "case \"the storage case\": the later invocation must read what the earlier wrote, \
         got {answered}"
    );
}

/// RULE: the command's own status is what its invocations reached, so a client watching the
/// command rather than the invocation sees it finish too.
///
/// The one case that waits on the REAL clock: nothing here moves the owed instant, so what
/// it shows is that the invocation finishes without anything asking it to.
#[tokio::test]
async fn the_commands_status_is_what_its_invocations_reached() {
    let emulator = Emulator::new();
    let instance = node(&emulator, "the command status case", "command-status-role").await;
    let command_id = send(
        &emulator,
        "the command status case",
        &instance,
        &["echo done"],
    )
    .await;
    finished(&emulator, "the command status case", &command_id, &instance).await;

    let (status, answered) = emulator
        .ssm(
            REGION,
            "ListCommands",
            &json!({"CommandId": command_id.clone()}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the command status case\": ListCommands must be served, got {status}: {answered}"
    );
    let command = answered["Commands"]
        .as_array()
        .and_then(|list| list.first().cloned())
        .unwrap_or_else(|| {
            panic!("case \"the command status case\": the command must be listed: {answered}")
        });
    assert_eq!(
        command["Status"].as_str(),
        Some("Success"),
        "case \"the command status case\": expected the command to report what its \
         invocation reached, got {command}"
    );
    assert_eq!(
        command["CompletedCount"].as_i64(),
        Some(1),
        "case \"the command status case\": the completed count counts the invocations that \
         reached a terminal state, got {command}"
    );
}
