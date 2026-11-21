// SPDX-License-Identifier: CC0-1.0

//! The lines an `aws:runShellScript` step carries performed as ONE SCRIPT by one shell in the
//! node's guest, driven end to end with Systems Manager, Amazon EC2 and IAM on one engine.
//!
//! RULE under test: THE `commands` OF A SHELL-SCRIPT STEP ARE ONE SCRIPT, NOT ONE COMMAND
//! PER LINE — the agent writes the lines it was delivered into a script and runs that script
//! with the guest's shell, so a construct that SPANS lines is the script's own (a
//! here-document opened on one line carries the lines under it and its redirection writes the
//! file; a compound command is read to its end), the shell OPTIONS one line sets hold for the
//! lines after it, and what the invocation reports is that script's own exit status and its
//! TWO streams: `StandardOutputContent` is what the script wrote and
//! `StandardErrorContent` what it reported it could not do.
//!
//! Every case goes through the front door: `AmazonSSM.SendCommand` and
//! `AmazonSSM.GetCommandInvocation` over `awsJson1_1`, with the instance launched over EC2's
//! `ec2Query` wire and its role and instance profile created over IAM's `awsQuery` wire. The
//! node is the managed node the launched instance became
//! (`tests/all/ec2_instance_managed_nodes.rs`), and the send and the read are the shared ones
//! the sibling guest suite drives (`tests/all/run_command_in_the_guest.rs`).
//!
//! The rows span what the rule generalises over: which construct spans the lines (a quoted
//! here-document, an unquoted one, a tab-stripped one, a compound command), what carries
//! across them (a variable, a file, a shell option), which option the script sets
//! (`errexit`, `nounset`, `pipefail`, and one this session performs nothing of), and which of
//! the two streams a line writes on.

use serde_json::json;

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use super::run_command_in_the_guest::{finish_due, finished, send};

/// The status a shell answers for a utility it could not find, and for a command FORM the
/// guest's session performs nothing of (POSIX, and
/// `service_common::posix_shell_session::Statuses::could_not_run`).
const COULD_NOT_RUN: i64 = 127;

/// One row: the lines the step carries, the status the invocation ends in, the exit code the
/// script ended on, and what it wrote on each of its two streams.
struct ScriptCase {
    case: &'static str,
    commands: Vec<&'static str>,
    status: &'static str,
    response_code: i64,
    output: &'static str,
    errors: &'static str,
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

/// RULE: the delivered lines are one script, so a construct spanning them is the script's own
/// and what the invocation reports is that script's status and its two streams.
#[tokio::test]
async fn the_delivered_lines_are_one_script() {
    let emulator = Emulator::new();
    let instance = node(&emulator, "the one-script table", "one-script-role").await;

    let rows = [
        // A here-document whose delimiter is QUOTED is taken literally, so the lines under
        // the opener are the document and the opener's redirection is what writes it.
        ScriptCase {
            case: "a quoted here-document written to a file and read back",
            commands: vec![
                "cat > /tmp/one-script-quoted <<'DOC'",
                "alpha",
                "$NOT_EXPANDED",
                "DOC",
                "cat /tmp/one-script-quoted",
            ],
            status: "Success",
            response_code: 0,
            output: "alpha\n$NOT_EXPANDED\n",
            errors: "",
        },
        // An UNQUOTED delimiter expands the document, so a variable an earlier line of the
        // same script set reaches the file.
        ScriptCase {
            case: "an unquoted here-document expands what an earlier line set",
            commands: vec![
                "MARK=carried",
                "cat > /tmp/one-script-expanded <<DOC",
                "mark=$MARK",
                "DOC",
                "cat /tmp/one-script-expanded",
            ],
            status: "Success",
            response_code: 0,
            output: "mark=carried\n",
            errors: "",
        },
        // `<<-` strips the leading tabs off each line of the document, which is how a
        // document written inside an indented block is spelled.
        ScriptCase {
            case: "a tab-stripped here-document",
            commands: vec!["cat <<-DOC", "\tindented", "\tDOC"],
            status: "Success",
            response_code: 0,
            output: "indented\n",
            errors: "",
        },
        // A compound command is read to its end across the lines it is written on.
        ScriptCase {
            case: "a compound command spanning several lines",
            commands: vec![
                "if cat /tmp/one-script-quoted > /dev/null; then",
                "echo inside",
                "else",
                "echo otherwise",
                "fi",
            ],
            status: "Success",
            response_code: 0,
            output: "inside\n",
            errors: "",
        },
        // The two streams are reported apart: what the script wrote, and what it reported it
        // could not do.
        ScriptCase {
            case: "a script writing on both streams",
            commands: vec!["echo written", "definitely-not-a-program"],
            status: "Failed",
            response_code: COULD_NOT_RUN,
            output: "written\n",
            errors: "sh: definitely-not-a-program: command not found\n",
        },
        // `set -e`: "when any command fails … the shell immediately shall exit", so the line
        // after the failing one is not performed and the script's status is that failure.
        ScriptCase {
            case: "errexit ends the script at the failing line",
            commands: vec!["set -e", "echo before", "false", "echo after"],
            status: "Failed",
            response_code: 1,
            output: "before\n",
            errors: "",
        },
        // The same option passes over a status the script itself is READING: the condition of
        // an `if`, and a pipeline written with `!`.
        ScriptCase {
            case: "errexit passes over a status the script reads",
            commands: vec![
                "set -e",
                "if false; then",
                "echo taken",
                "fi",
                "! false",
                "echo after",
            ],
            status: "Success",
            response_code: 0,
            output: "after\n",
            errors: "",
        },
        // `set -u`: the expansion of a variable the session holds none of is an error, with
        // the shell's own diagnostic on standard error and a non-zero status.
        ScriptCase {
            case: "nounset refuses the expansion of a variable nothing set",
            commands: vec!["set -u", "echo ${ONE_SCRIPT_MISSING}", "echo after"],
            status: "Failed",
            response_code: 1,
            output: "",
            errors: "sh: ONE_SCRIPT_MISSING: parameter not set\n",
        },
        // Without `pipefail` a pipeline answers its LAST stage; with it, the rightmost stage
        // that failed.
        ScriptCase {
            case: "a pipeline answers its last stage",
            commands: vec!["false | cat"],
            status: "Success",
            response_code: 0,
            output: "",
            errors: "",
        },
        ScriptCase {
            case: "pipefail answers the stage that failed",
            commands: vec!["set -o pipefail", "false | cat"],
            status: "Failed",
            response_code: 1,
            output: "",
            errors: "",
        },
        // The option line a real script opens with sets BOTH the options its letters name and
        // the one the word after the group names: `-u` refuses an unset expansion and
        // `-o pipefail` makes a pipeline worth its failing stage.
        ScriptCase {
            case: "the combined option line a script opens with, nounset half",
            commands: vec![
                "set -uo pipefail",
                "echo ${ONE_SCRIPT_COMBINED}",
                "echo after",
            ],
            status: "Failed",
            response_code: 1,
            output: "",
            errors: "sh: ONE_SCRIPT_COMBINED: parameter not set\n",
        },
        ScriptCase {
            case: "the combined option line a script opens with, pipefail half",
            commands: vec!["set -uo pipefail", "false | cat"],
            status: "Failed",
            response_code: 1,
            output: "",
            errors: "",
        },
        ScriptCase {
            case: "every option in one word, with errexit among them",
            commands: vec!["set -euo pipefail", "echo before", "false | cat", "echo after"],
            status: "Failed",
            response_code: 1,
            output: "before\n",
            errors: "",
        },
        // An option whose effect the guest's session performs nothing of fails closed, with
        // nothing changed: the line after it still runs, as it does for every other command
        // the session has nothing for.
        ScriptCase {
            case: "an option this session performs nothing of",
            commands: vec!["set -o noclobber", "echo after"],
            status: "Success",
            response_code: 0,
            output: "after\n",
            errors: "",
        },
        // `set` with no word at all writes the names and values of the variables the session
        // holds, which is how a script reports its own environment; the one this script set
        // is among them.
        ScriptCase {
            case: "set with no word writes the variables",
            commands: vec!["ONE_SCRIPT_SHOWN=here", "set | grep ONE_SCRIPT_SHOWN"],
            status: "Success",
            response_code: 0,
            output: "ONE_SCRIPT_SHOWN=here\n",
            errors: "",
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
            answered["ResponseCode"].as_i64(),
            Some(row.response_code),
            "case {:?}: expected the exit code the script ended on, got {answered}",
            row.case
        );
        assert_eq!(
            answered["StandardOutputContent"].as_str(),
            Some(row.output),
            "case {:?}: expected what the script wrote, got {answered}",
            row.case
        );
        assert_eq!(
            answered["StandardErrorContent"].as_str(),
            Some(row.errors),
            "case {:?}: expected what the script reported it could not do, got {answered}",
            row.case
        );
    }
}

/// RULE: a file one line of the script wrote is a file of the instance's own storage, so a
/// LATER invocation reads what the here-document of an earlier one left — the round trip the
/// script's own `cat` cannot establish on its own.
#[tokio::test]
async fn a_here_document_leaves_the_file_on_the_machine() {
    let case = "the here-document round trip";
    let emulator = Emulator::new();
    let instance = node(&emulator, case, "here-document-role").await;

    let wrote = send(
        &emulator,
        case,
        &instance,
        &[
            "cat > /var/tmp/one-script-kept <<'DOC'",
            "first",
            "second",
            "DOC",
        ],
    )
    .await;
    finish_due(&emulator, &wrote);
    let answered = finished(&emulator, case, &wrote, &instance).await;
    assert_eq!(
        answered["Status"].as_str(),
        Some("Success"),
        "case {case:?}: the here-document's redirection must write the file, got {answered}"
    );

    let read = send(&emulator, case, &instance, &["cat /var/tmp/one-script-kept"]).await;
    finish_due(&emulator, &read);
    let answered = finished(&emulator, case, &read, &instance).await;
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some("first\nsecond\n"),
        "case {case:?}: the later invocation must read the document the earlier one wrote, \
         got {answered}"
    );
}

/// RULE: the step's own `commands` parameter is what carries the script, so a document sent
/// with a here-document in it reports the script's output whichever way the send spells the
/// parameter — the lines are one script either way.
#[tokio::test]
async fn the_script_is_one_program_however_many_lines_the_send_carries() {
    let case = "the parameter form";
    let emulator = Emulator::new();
    let instance = node(&emulator, case, "parameter-form-role").await;

    // One list element carrying the WHOLE script, newlines and all, which is the other way a
    // client spells it.
    let whole = send(
        &emulator,
        case,
        &instance,
        &["cat <<'DOC'\nwhole\nDOC\necho done"],
    )
    .await;
    finish_due(&emulator, &whole);
    let answered = finished(&emulator, case, &whole, &instance).await;
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some("whole\ndone\n"),
        "case {case:?}: expected the script's own output, got {answered}"
    );
    assert_eq!(
        answered["Status"].as_str(),
        Some("Success"),
        "case {case:?}: expected the script to run to its end, got {answered}"
    );

    let (status, listed) = emulator
        .ssm(
            REGION,
            "ListCommandInvocations",
            &json!({"CommandId": whole, "Details": true}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation listing must be served, got {status}: {listed}"
    );
    let plugin = listed["CommandInvocations"][0]["CommandPlugins"][0].clone();
    assert_eq!(
        plugin["ResponseCode"].as_i64(),
        Some(0),
        "case {case:?}: the step the agent ran reports the script's own exit code, got {listed}"
    );
    assert_eq!(
        plugin["Output"].as_str(),
        Some("whole\ndone\n"),
        "case {case:?}: the step's output is the script's own, got {listed}"
    );
}

/// RULE: a script is read whatever the NUMBER of groups its lines carry between them — the
/// bound a parse is given is stated for a whole script — so a long script whose every line
/// opens a command substitution of its own runs to its end, while one carrying more groups
/// than that bound runs nothing at all and says so with the status a shell answers for a line
/// it could not read.
#[tokio::test]
async fn a_long_script_of_many_groups_runs_while_a_deeply_nested_one_does_not() {
    let case = "the script's groups";
    let emulator = Emulator::new();
    let instance = node(&emulator, case, "script-groups-role").await;

    // Far more groups than any ONE line of a program carries, each on its own line.
    let lines: Vec<String> = (0..120)
        .map(|index| format!("echo $(echo line-{index})"))
        .collect();
    let script: Vec<&str> = lines.iter().map(String::as_str).collect();
    let many = send(&emulator, case, &instance, &script).await;
    finish_due(&emulator, &many);
    let answered = finished(&emulator, case, &many, &instance).await;
    assert_eq!(
        answered["Status"].as_str(),
        Some("Success"),
        "case {case:?}: expected a script of many groups to run to its end, got {answered}"
    );
    let written = answered["StandardOutputContent"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        written.lines().count(),
        lines.len(),
        "case {case:?}: expected one line of output per line of script, got {written:?}"
    );
    assert!(
        written.contains("line-119"),
        "case {case:?}: expected the last line of the script to have run, got {written:?}"
    );

    // One line nesting a substitution inside another far past the groups a parse is given.
    let deep = format!("echo {}{}", "$(echo ".repeat(600), ")".repeat(600));
    let nested = send(&emulator, case, &instance, &[deep.as_str()]).await;
    finish_due(&emulator, &nested);
    let answered = finished(&emulator, case, &nested, &instance).await;
    assert_eq!(
        (
            answered["Status"].as_str(),
            answered["ResponseCode"].as_i64(),
            answered["StandardOutputContent"].as_str(),
        ),
        (Some("Failed"), Some(COULD_NOT_RUN), Some("")),
        "case {case:?}: expected a script nested past the bound to perform nothing, got \
         {answered}"
    );
}
