// SPDX-License-Identifier: CC0-1.0

//! A `command-runner.jar` step whose command line RUNS a Python program, end to end over the
//! wire, with Amazon S3 mounted beside Amazon EMR on one engine (the harness
//! `tests/all/spark_step_application.rs` owns).
//!
//! THE RULE: "command-runner.jar … lets you run commands on your cluster" (Amazon EMR
//! documentation) and an Amazon EMR release ships the Python interpreter on every node, so a
//! command line that invokes `python3` runs the program it names and the step ends in the
//! status that program exited with — zero is the step's completion, as `AddJobFlowSteps`
//! documents, and any other status is the modelled failure. The words a `sh -c` command line
//! writes after its program are that program's own positional parameters, and the arguments a
//! step configured reach the process it starts in order and byte for byte, except an argument
//! of zero length, which the command runner drops.
//!
//! The rows span what the rule generalises over: where the program comes from (a file the
//! command line fetched into the node's own storage, and a program written on the command
//! line itself), what it exits with (its end, and a status it names), and a file the node
//! holds nothing under. The argument row is the one that observes the delivery: an ordinary
//! word, a word holding a space, a word that LOOKS like a shell parameter (which an argument
//! never is), and a word of zero length.

use serde_json::{json, Value};

use crate::spark_step_application::{
    drained, emr_ok, emulator, object, put_object, s3, BUCKET,
};

/// The roles a launch names, as the harness names them.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The helper one row's command line fetches out of Amazon S3 and runs: it writes its own
/// argument vector as JSON, which is how the delivery of a step's arguments is observed.
const HELPER: &str = "import json, sys\nprint(json.dumps(sys.argv[1:]))\n";

/// The key the helper is written under.
const HELPER_KEY: &str = "argv/dump_argv.py";

/// One row: the command line a step submits, the state it must end in, and the object its
/// command must have left behind.
struct Row {
    case: &'static str,
    /// The `HadoopJarStep.Args` the step is configured with.
    args: Vec<String>,
    state: &'static str,
    /// `(key, contents)` the command must have written, where it writes one.
    wrote: Option<(String, String)>,
}

/// The arguments the argv row configures, the zero-length one among them.
fn configured_arguments() -> Vec<String> {
    vec![
        "argv-report".to_string(),
        "plain".to_string(),
        "two words".to_string(),
        String::new(),
        "$NOT_EXPANDED".to_string(),
    ]
}

/// What the program must report as its own argument vector: the configured arguments after
/// the name, byte for byte, with the zero-length one dropped — and the parameter the command
/// line names past them expanding to nothing, as an unset parameter does.
///
/// Rendered as the helper's own encoder writes a list of strings: each word quoted, the words
/// separated by a comma and a space.
fn delivered_argv() -> String {
    let words: Vec<String> = ["plain", "two words", "$NOT_EXPANDED", ""]
        .into_iter()
        .map(|word| Value::String(word.to_string()).to_string())
        .collect();
    format!("[{}]", words.join(", "))
}

fn rows() -> Vec<Row> {
    let fetched = format!(
        "set -eu; aws s3 cp s3://{BUCKET}/{HELPER_KEY} /tmp/dump_argv.py; \
         python3 /tmp/dump_argv.py \"$1\" \"$2\" \"$3\" \"$4\" > /tmp/out; \
         aws s3 cp /tmp/out s3://{BUCKET}/argv/out.json"
    );
    let mut args = vec!["bash".to_string(), "-c".to_string(), fetched];
    args.extend(configured_arguments());
    vec![
        Row {
            case: "a program the command line fetched, given the step's own arguments",
            args,
            state: "COMPLETED",
            wrote: Some((
                "argv/out.json".to_string(),
                format!("{}\n", delivered_argv()),
            )),
        },
        Row {
            case: "a program written on the command line itself",
            args: vec![
                "bash".to_string(),
                "-c".to_string(),
                format!(
                    "set -eu; python3 -c 'print(sum(range(5)))' > /tmp/inline; \
                     aws s3 cp /tmp/inline s3://{BUCKET}/argv/inline.txt"
                ),
            ],
            state: "COMPLETED",
            // The sum of 0..5, computed here rather than read back.
            wrote: Some(("argv/inline.txt".to_string(), "10\n".to_string())),
        },
        Row {
            case: "a program that names the status it exits with",
            args: vec![
                "bash".to_string(),
                "-c".to_string(),
                "python3 -c 'import sys; sys.exit(3)'".to_string(),
            ],
            state: "FAILED",
            wrote: None,
        },
        Row {
            case: "a file the node holds nothing under",
            args: vec![
                "bash".to_string(),
                "-c".to_string(),
                "python3 /tmp/absent.py".to_string(),
            ],
            state: "FAILED",
            wrote: None,
        },
    ]
}

/// A kept-alive cluster carrying one step with `args`.
async fn launch(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    args: &[String],
) -> String {
    let launched = emr_ok(
        router,
        "RunJobFlow",
        json!({
            "Name": name,
            "ReleaseLabel": "emr-6.15.0",
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "Applications": [{ "Name": "Spark" }, { "Name": "Hadoop" }],
            "Steps": [{
                "Name": name,
                "ActionOnFailure": "CONTINUE",
                "HadoopJarStep": { "Jar": "command-runner.jar", "Args": args },
            }],
            "Instances": {
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            },
        }),
    )
    .await;
    launched["JobFlowId"]
        .as_str()
        .expect("RunJobFlow answers a JobFlowId")
        .to_string()
}

/// Every row: the step ends in what its command's program exited with, the object the command
/// line carried is really there, and `DescribeStep` still echoes every configured argument —
/// the zero-length one included.
#[tokio::test]
async fn a_command_line_runs_a_python_program_and_its_step_ends_in_what_it_exited_with() {
    let router = emulator();
    let (status, answered) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert!(
        (200..300).contains(&status),
        "CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&answered)
    );
    put_object(&router, "the helper", HELPER_KEY, HELPER.as_bytes()).await;

    let rows = rows();
    // Every cluster is launched before any is waited on, so the rows run beside each other.
    let mut launched: Vec<String> = Vec::new();
    for row in &rows {
        launched.push(launch(&router, row.case, &row.args).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let steps = drained(case, &router, cluster_id).await;
        let step = steps
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected the submitted step back"));
        assert_eq!(
            step["Status"]["State"].as_str(),
            Some(row.state),
            "case {case:?}: expected the state its program exited with, got {step}"
        );
        // The configured arguments are the step's own, whatever the command made of them.
        assert_eq!(
            step["Config"]["Args"],
            Value::Array(
                row.args
                    .iter()
                    .map(|word| Value::String(word.clone()))
                    .collect()
            ),
            "case {case:?}: DescribeStep echoes every configured argument, got {step}"
        );
        if let Some((key, expected)) = row.wrote.as_ref() {
            let held = object(&router, key).await;
            assert_eq!(
                held.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected {key} to hold {expected:?}, got {held:?}"
            );
        }
    }
}
