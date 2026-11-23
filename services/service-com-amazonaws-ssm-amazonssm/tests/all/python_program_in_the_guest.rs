// SPDX-License-Identifier: CC0-1.0

//! The PYTHON PROGRAM an `AWS-RunShellScript` invocation hands the guest's interpreter, run in
//! the node's guest and reported on the invocation's own two streams — driven end to end with
//! Systems Manager, Amazon EC2 and IAM on one engine.
//!
//! RULE under test: A COMMAND NAMING A PROGRAM THE GUEST HAS RUNS THAT PROGRAM, AND
//! `StandardOutputContent` IS WHAT THE PROGRAM WROTE TO STANDARD OUTPUT AND NOTHING ELSE —
//! so a script that ends in an inline `python3` program reports the line that program printed
//! (the universal idiom: the last non-blank line of stdout is the JSON document the program
//! computed), a diagnostic or a traceback is `StandardErrorContent` beside a non-zero
//! `ResponseCode`, and the program is given the environment the lines before it exported.
//!
//! Every case goes through the front door: `AmazonSSM.SendCommand` and
//! `AmazonSSM.GetCommandInvocation` over `awsJson1_1`, with the instance launched over EC2's
//! `ec2Query` wire and its role and instance profile created over IAM's `awsQuery` wire. The
//! node is the managed node the launched instance became
//! (`tests/all/ec2_instance_managed_nodes.rs`), and the send and the read are the shared ones
//! the sibling guest suites drive (`tests/all/run_command_in_the_guest.rs`).
//!
//! The rows span what the rule generalises over: WHERE the program comes from (a
//! here-document, the command line, a file of the machine's own storage), which name the
//! interpreter was invoked under, what the program reads (its environment, its arguments,
//! nothing), what it computes, how many lines it writes, which stream each line goes to, and
//! the status it ends on — including the ones it cannot be run at all for.

use serde_json::Value;

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};
use super::run_command_in_the_guest::{finish_due, finished, send};

/// The status CPython exits with for a program that ended on an uncaught exception.
const RAISED: i64 = 1;

/// The status it exits with when it could not read the program it was given at all: a file it
/// cannot open, and an option it does not perform.
const USAGE_FAILURE: i64 = 2;

/// One row of the table: the script sent, and what the invocation reports for it.
struct ProgramCase {
    case: &'static str,
    commands: Vec<&'static str>,
    status: &'static str,
    response_code: i64,
    /// What the program wrote to standard output, exactly.
    output: &'static str,
    /// What the invocation reports on standard error: the whole text where the program's own
    /// is exact, else the markers it must carry (a traceback's wording is the interpreter's).
    errors: Stream,
}

/// What a row expects of a stream.
enum Stream {
    /// Exactly this text.
    Exactly(&'static str),
    /// Text carrying each of these.
    Carrying(&'static [&'static str]),
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

/// Send one script, wait for the invocation and check what it reports.
async fn check(emulator: &Emulator, instance: &str, row: &ProgramCase) {
    let command_id = send(emulator, row.case, instance, &row.commands).await;
    finish_due(emulator, &command_id);
    let answered = finished(emulator, row.case, &command_id, instance).await;
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
        "case {:?}: expected the status the program ended on, got {answered}",
        row.case
    );
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some(row.output),
        "case {:?}: expected what the program wrote to standard output, got {answered}",
        row.case
    );
    let errors = answered["StandardErrorContent"].as_str().unwrap_or_default();
    match row.errors {
        Stream::Exactly(expected) => assert_eq!(
            errors, expected,
            "case {:?}: expected {expected:?} on standard error, got {errors:?}",
            row.case
        ),
        Stream::Carrying(markers) => {
            for marker in markers {
                assert!(
                    errors.contains(marker),
                    "case {:?}: expected standard error to carry {marker:?}, got {errors:?}",
                    row.case
                );
            }
        }
    }
}

/// The last non-blank line of `written`, read the way a client that parses a command's output
/// reads it.
fn last_line(written: &str) -> &str {
    written
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or_default()
}

/// RULE: the invocation's standard output is what the guest's own program wrote there, so an
/// inline `python3` program's computed line is what a client reads back.
#[tokio::test]
async fn an_inline_program_reports_what_it_printed() {
    let emulator = Emulator::new();
    let instance = node(&emulator, "the program table", "python-program-role").await;

    let rows = [
        // The idiom itself: a here-document program ending in one JSON object, with the value
        // an earlier line of the script EXPORTED in it.
        ProgramCase {
            case: "a here-document program printing one JSON object from its environment",
            commands: vec![
                "export HITS=7",
                "python3 - <<'JEOF'",
                "import json, os",
                "print(json.dumps({\"hits\": os.environ.get(\"HITS\", \"\")}))",
                "JEOF",
            ],
            status: "Success",
            response_code: 0,
            output: "{\"hits\": \"7\"}\n",
            errors: Stream::Exactly(""),
        },
        // The same program after lines of the script's own output: the LAST non-blank line is
        // the program's, which is what the client parses.
        ProgramCase {
            case: "the program's line after output of the script's own",
            commands: vec![
                "echo preparing",
                "python3 - <<'EOF'",
                "import json",
                "print(json.dumps({\"ok\": True}))",
                "EOF",
            ],
            status: "Success",
            response_code: 0,
            output: "preparing\n{\"ok\": true}\n",
            errors: Stream::Exactly(""),
        },
        // A program on the command line, COMPUTING its answer rather than echoing one.
        ProgramCase {
            case: "a program given on the command line",
            commands: vec!["python3 -c 'print(6 * 7)'"],
            status: "Success",
            response_code: 0,
            output: "42\n",
            errors: Stream::Exactly(""),
        },
        // Its own ARGUMENTS: the words after the program are what it reads as `sys.argv`.
        ProgramCase {
            case: "a program computing over the arguments it was given",
            commands: vec![
                "python3 -c 'import sys; print(sum(int(word) for word in sys.argv[1:]))' 2 3 4",
            ],
            status: "Success",
            response_code: 0,
            output: "9\n",
            errors: Stream::Exactly(""),
        },
        // Several lines, in the order the program wrote them.
        ProgramCase {
            case: "a program writing several lines",
            commands: vec!["python3 -c 'for index in range(3): print(index)'"],
            status: "Success",
            response_code: 0,
            output: "0\n1\n2\n",
            errors: Stream::Exactly(""),
        },
        // The interpreter's other name is the same interpreter.
        ProgramCase {
            case: "the interpreter invoked under its other name",
            commands: vec!["python - <<'EOF'", "print('named')", "EOF"],
            status: "Success",
            response_code: 0,
            output: "named\n",
            errors: Stream::Exactly(""),
        },
        // A program in a FILE of the machine's own storage, written by an earlier line of the
        // same script.
        ProgramCase {
            case: "a program read from a file the script wrote",
            commands: vec![
                "cat > /tmp/probe.py <<'EOF'",
                "import json",
                "print(json.dumps({\"from\": \"file\"}))",
                "EOF",
                "python3 /tmp/probe.py",
            ],
            status: "Success",
            response_code: 0,
            output: "{\"from\": \"file\"}\n",
            errors: Stream::Exactly(""),
        },
        // What the program wrote is a stream of the shell's, so a command substitution
        // captures it and a later line of the script reads it.
        ProgramCase {
            case: "a value a later line of the script reads out of the program",
            commands: vec![
                "export LOCAL=nonce-value",
                "VALUE=$(python3 -c 'import os; print(os.environ[\"LOCAL\"])')",
                "echo got-$VALUE",
            ],
            status: "Success",
            response_code: 0,
            output: "got-nonce-value\n",
            errors: Stream::Exactly(""),
        },
        // The two streams are APART: what the program wrote on each goes where that stream
        // goes, and a program that wrote on standard error still ran to success.
        ProgramCase {
            case: "a program writing on both of its streams",
            commands: vec![
                "python3 -c 'import sys; sys.stderr.write(\"warned\\n\"); print(\"out\")'",
            ],
            status: "Success",
            response_code: 0,
            output: "out\n",
            errors: Stream::Exactly("warned\n"),
        },
        // A program that ENDS ITSELF: the status it named is the invocation's.
        ProgramCase {
            case: "a program that exits with a status of its own",
            commands: vec!["python3 -c 'print(\"first\"); import sys; sys.exit(3)'"],
            status: "Failed",
            response_code: 3,
            output: "first\n",
            errors: Stream::Exactly(""),
        },
        // An uncaught exception: the traceback is on standard ERROR, the lines the program
        // printed before it are still its output, and the status is the one CPython exits
        // with.
        ProgramCase {
            case: "a program that raises",
            commands: vec!["python3 -c 'print(\"before\"); raise ValueError(\"boom\")'"],
            status: "Failed",
            response_code: RAISED,
            output: "before\n",
            errors: Stream::Carrying(&["Traceback (most recent call last):", "ValueError: boom"]),
        },
        // A file the machine does not hold is the interpreter's own usage failure, in its own
        // wording, with nothing on standard output.
        ProgramCase {
            case: "a program file the machine does not hold",
            commands: vec!["python3 /tmp/not-there.py"],
            status: "Failed",
            response_code: USAGE_FAILURE,
            output: "",
            errors: Stream::Carrying(&["can't open file '/tmp/not-there.py'"]),
        },
        // An option the interpreter performs nothing of is its usage failure too, and not a
        // program that ran.
        ProgramCase {
            case: "an option the interpreter does not perform",
            commands: vec!["python3 -Z"],
            status: "Failed",
            response_code: USAGE_FAILURE,
            output: "",
            errors: Stream::Carrying(&["Unknown option: -Z"]),
        },
    ];

    for row in &rows {
        check(&emulator, &instance, row).await;
    }
}

/// RULE: the last non-blank line of the invocation's standard output is the one-line JSON
/// document the program printed — the reading every client that uses a managed node as an
/// observation point performs — whatever the script wrote around it.
///
/// The rows differ in what surrounds the program's own line: output of the script's own
/// before it, a diagnostic of the script's on the other stream, and a blank line after it.
#[tokio::test]
async fn the_last_line_of_the_output_is_the_document_the_program_printed() {
    let emulator = Emulator::new();
    let instance = node(&emulator, "the last-line table", "python-last-line-role").await;

    let rows: &[(&str, Vec<&str>, &str)] = &[
        (
            "the program's line alone",
            vec![
                "python3 - <<'EOF'",
                "import json",
                "print(json.dumps({\"rc\": 0, \"code\": \"200\"}))",
                "EOF",
            ],
            "{\"rc\": 0, \"code\": \"200\"}",
        ),
        (
            "a line of the script's own before it",
            vec![
                "echo starting the probe",
                "export CODE=204",
                "python3 - <<'EOF'",
                "import json, os",
                "print(json.dumps({\"code\": os.environ[\"CODE\"]}))",
                "EOF",
            ],
            "{\"code\": \"204\"}",
        ),
        (
            "a diagnostic of the script's on the other stream",
            vec![
                "echo not-a-program-at-all 1>&2",
                "python3 -c 'import json; print(json.dumps({\"hits\": 2}))'",
            ],
            "{\"hits\": 2}",
        ),
        (
            "a blank line written after it",
            vec![
                "python3 -c 'import json; print(json.dumps([1, 2]))'",
                "echo ''",
            ],
            "[1, 2]",
        ),
    ];

    for (case, commands, expected) in rows {
        let command_id = send(&emulator, case, &instance, commands).await;
        finish_due(&emulator, &command_id);
        let answered = finished(&emulator, case, &command_id, &instance).await;
        let written = answered["StandardOutputContent"]
            .as_str()
            .unwrap_or_default();
        let line = last_line(written);
        assert_eq!(
            line, *expected,
            "case {case:?}: expected the program's own line last, got {written:?}"
        );
        // ... and it is a JSON document, which is what the client parses it as.
        let parsed: Result<Value, _> = serde_json::from_str(line);
        assert!(
            parsed.is_ok(),
            "case {case:?}: expected the last line to parse as JSON, got {line:?}"
        );
    }
}

/// RULE: the bounds one guest program runs under are the emulator's, held in host code the
/// program cannot reach — so a program that computes without end is STOPPED, with the bound it
/// ran past reported on standard error and a non-zero status, rather than holding the request
/// for as long as it likes.
///
/// WHICH bound stops it is the host's own reading — its step budget, its own clock, or the host
/// time one run may take — so what this pins is that one of them does, and that nothing of the
/// program's own is reported for a run nothing of which finished.
#[tokio::test]
async fn a_program_that_computes_without_end_is_stopped() {
    let case = "the step budget";
    let emulator = Emulator::new();
    let instance = node(&emulator, case, "python-bound-role").await;

    let command_id = send(
        &emulator,
        case,
        &instance,
        &["python3 -c 'index = 0\nwhile True: index += 1'"],
    )
    .await;
    finish_due(&emulator, &command_id);
    let answered = finished(&emulator, case, &command_id, &instance).await;
    assert_eq!(
        answered["Status"].as_str(),
        Some("Failed"),
        "case {case:?}: expected the stopped program to fail the invocation, got {answered}"
    );
    assert_eq!(
        answered["ResponseCode"].as_i64(),
        Some(RAISED),
        "case {case:?}: expected a non-zero status for a program that was stopped, got \
         {answered}"
    );
    let errors = answered["StandardErrorContent"].as_str().unwrap_or_default();
    assert!(
        errors.contains("was stopped"),
        "case {case:?}: expected the bound it ran past on standard error, got {errors:?}"
    );
    assert_eq!(
        answered["StandardOutputContent"].as_str(),
        Some(""),
        "case {case:?}: expected no output for a program nothing of which finished, got \
         {answered}"
    );
}

/// RULE: how many programs ONE REQUEST runs is bounded too — the product of the commands its
/// script carries and the nodes it is delivered to is what the allowance caps — so a script
/// naming more programs than that runs the ones the allowance has room for and reports that it
/// did not run the rest, on standard error and with a non-zero status, instead of running
/// however many the caller wrote.
#[tokio::test]
async fn a_script_naming_more_programs_than_one_request_may_run_reports_the_ones_it_did_not() {
    let case = "the request allowance";
    let emulator = Emulator::new();
    let instance = node(&emulator, case, "python-allowance-role").await;

    // Far more inline programs than one request runs, each printing which one it is, so what
    // ran and what did not is visible in the two streams.
    let lines: Vec<String> = (0..24)
        .map(|index| format!("python3 -c 'print(\"ran-{index}\")'"))
        .collect();
    let script: Vec<&str> = lines.iter().map(String::as_str).collect();
    let command_id = send(&emulator, case, &instance, &script).await;
    finish_due(&emulator, &command_id);
    let answered = finished(&emulator, case, &command_id, &instance).await;

    let written = answered["StandardOutputContent"]
        .as_str()
        .unwrap_or_default();
    let ran = written.lines().filter(|line| !line.is_empty()).count();
    assert!(
        ran > 0,
        "case {case:?}: expected the programs the allowance had room for to have run, got \
         {written:?}"
    );
    assert!(
        ran < lines.len(),
        "case {case:?}: expected the allowance to stop short of the {} programs the script \
         names, got {ran} lines: {written:?}",
        lines.len()
    );
    let errors = answered["StandardErrorContent"].as_str().unwrap_or_default();
    assert!(
        errors.contains("one request may run"),
        "case {case:?}: expected the programs past the allowance to report that they did not \
         run, got {errors:?}"
    );
    assert_eq!(
        answered["Status"].as_str(),
        Some("Failed"),
        "case {case:?}: expected the script to end on the status of a program that did not \
         run, got {answered}"
    );
}
