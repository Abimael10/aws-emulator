// SPDX-License-Identifier: CC0-1.0

//! What a notebook instance's LIFECYCLE HOOK really DOES — driven end to end over the real
//! wire path (SageMaker, Amazon S3 and IAM mounted on ONE shared engine, as the production
//! router mounts every service), so what is asserted is what a client sees.
//!
//! THE RULE UNDER TEST: a lifecycle configuration's hook is a shell script the platform RUNS
//! for the instance that names the configuration, so the AWS calls its command lines make are
//! really made — as the instance's execution role (`CreateNotebookInstance$RoleArn`: "when you
//! send any requests to Amazon Web Services resources from the notebook instance, SageMaker AI
//! assumes this role to perform tasks on your behalf") — and their effects are at the
//! destination by the time the instance reports `InService`. A hook whose script carries a
//! file into a bucket has that file carried; one whose role is not granted the call carries
//! nothing.
//!
//! The first table spans what the rule generalises over: the COMMAND FORM and the direction of
//! the transfer — an upload from the command's own standard input, an upload of a file the
//! script staged, a download re-uploaded as what it read, a copy inside the store, the CLI's
//! low-level command group, a removal, and a command form this program performs nothing of.
//! The second spans what decides whether a call is made at all: what the permissions in force
//! on the instance's role say about the action.
//!
//! Nothing here waits for a span: the hook runs inside the create, so a case reads the
//! destination after the create answered, and the status the instance settles in is polled for
//! as a CONDITION with a deadline generous enough to bound only a broken emulator.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::model_artifact_admission::{role_arn, trust_for, Emulator};

/// The service principal a SageMaker execution role's trust document admits.
const SERVICE_PRINCIPAL: &str = service_com_amazonaws_sagemaker_sagemaker::state::SERVICE_PRINCIPAL;

/// The bucket every hook below reaches.
const BUCKET: &str = "notebook-bootstrap";

/// The object the rows that READ something read, and what it holds.
///
/// Its text ENDS IN A NEWLINE, which is what a script writes: a line a program wrote carries
/// its terminator, so the text a download hands the next stage of a pipeline is the text the
/// object holds.
const SEED_KEY: &str = "bootstrap/seed.txt";
const SEED_TEXT: &str = "seeded-data\n";

/// The status an instance that came up reports (`NotebookInstanceStatus`).
const IN_SERVICE: &str = "InService";

/// How long a case polls for the status the clock owes the instance, and how often.
///
/// GENEROUS on purpose: what the rule is about is the hook's EFFECT, which is already at the
/// destination when the create answers; this bounds only a broken emulator.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const SETTLE_POLL: Duration = Duration::from_millis(10);

/// `script` as the base64 a hook's `Content` carries ("a base64-encoded string that contains a
/// shell script", `NotebookInstanceLifecycleHook$Content`).
fn encoded(script: &str) -> String {
    ::base64::Engine::encode(&::base64::engine::general_purpose::STANDARD, script)
}

/// A policy document allowing `actions` on this bucket and the objects in it.
fn every_s3_action() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject", "s3:ListBucket"],
            "Resource": [
                format!("arn:aws:s3:::{BUCKET}"),
                format!("arn:aws:s3:::{BUCKET}/*"),
            ],
        }],
    })
}

/// Create the lifecycle configuration `config` with `on_start` as its `OnStart` script, then
/// the notebook instance `instance` naming it and running as `role`.
async fn given_notebook_with_hook(
    emulator: &Emulator,
    case: &str,
    instance: &str,
    config: &str,
    role: &str,
    on_start: &str,
) {
    emulator
        .sagemaker_ok(
            case,
            "CreateNotebookInstanceLifecycleConfig",
            &json!({
                "NotebookInstanceLifecycleConfigName": config,
                "OnStart": [{ "Content": encoded(on_start) }],
            }),
        )
        .await;
    emulator
        .sagemaker_ok(
            case,
            "CreateNotebookInstance",
            &json!({
                "NotebookInstanceName": instance,
                "InstanceType": "ml.t3.medium",
                "RoleArn": role,
                "LifecycleConfigName": config,
            }),
        )
        .await;
}

/// Read the instance until `DescribeNotebookInstance` reports `wanted`, failing the case with
/// the last answer when it never does.
async fn settles_in(emulator: &Emulator, case: &str, instance: &str, wanted: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = emulator
            .sagemaker_ok(
                case,
                "DescribeNotebookInstance",
                &json!({ "NotebookInstanceName": instance }),
            )
            .await;
        let status = described
            .get("NotebookInstanceStatus")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if status == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the instance to report {wanted:?} within \
             {SETTLE_DEADLINE:?}, last read {described}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// What Amazon S3 holds at `key`: `Some(text)` for an object it serves, and `None` for one it
/// answers is not there.
async fn held_object(emulator: &Emulator, case: &str, key: &str) -> Option<String> {
    let (status, body) = emulator.s3("GET", &format!("/{BUCKET}/{key}"), &[]).await;
    let text = String::from_utf8_lossy(&body).to_string();
    match status {
        200 => Some(text),
        404 => None,
        other => panic!("case {case:?}: reading {key} answered {other} with {text}"),
    }
}

/// One row of the first table: the `OnStart` script, and what the store must hold afterwards.
struct Row {
    case: &'static str,
    /// The script the row's hook carries, built from the key it writes.
    script: fn(&str) -> String,
    /// What the object at the row's own key must hold, and `None` for a row whose script
    /// writes no object at all.
    written: Option<&'static str>,
}

/// The rows: one per command form the CLI's object groups take.
const ROWS: &[Row] = &[
    Row {
        case: "an upload from the command's own standard input",
        script: |key| format!("echo marker-one | aws s3 cp - s3://{BUCKET}/{key}\n"),
        written: Some("marker-one\n"),
    },
    Row {
        case: "an upload of a file the script staged",
        script: |key| {
            format!(
                "echo staged-two > /tmp/payload.txt\naws s3 cp /tmp/payload.txt \
                 s3://{BUCKET}/{key}\n"
            )
        },
        written: Some("staged-two\n"),
    },
    Row {
        // The instance has storage of its own, so a script stages the bytes it means to
        // carry — with no line terminator of its own, which `printf` is what writes — and
        // the object holds exactly those bytes.
        case: "an upload of bytes the script formatted into a file",
        script: |key| {
            format!(
                "PAYLOAD=staged-exactly\nprintf '%s' \"$PAYLOAD\" > /tmp/exact.txt\naws s3 cp \
                 /tmp/exact.txt s3://{BUCKET}/{key}\n"
            )
        },
        written: Some("staged-exactly"),
    },
    Row {
        case: "a download, re-uploaded as what it read",
        script: |key| {
            format!("aws s3 cp s3://{BUCKET}/{SEED_KEY} - | aws s3 cp - s3://{BUCKET}/{key}\n")
        },
        written: Some(SEED_TEXT),
    },
    Row {
        case: "a copy inside the store",
        script: |key| format!("aws s3 cp s3://{BUCKET}/{SEED_KEY} s3://{BUCKET}/{key}\n"),
        written: Some(SEED_TEXT),
    },
    Row {
        case: "the low-level command group",
        script: |key| {
            format!(
                "echo api-five > /tmp/body.txt\naws s3api put-object --bucket {BUCKET} --key \
                 {key} --body /tmp/body.txt\n"
            )
        },
        written: Some("api-five\n"),
    },
    Row {
        case: "a command form this program performs nothing of",
        script: |key| format!("aws s3 sync /tmp s3://{BUCKET}/{key}\n"),
        written: None,
    },
    Row {
        case: "a script whose shebang and comments carry no command at all",
        script: |key| format!("#!/bin/bash\n# nothing is carried to s3://{BUCKET}/{key}\n"),
        written: None,
    },
];

/// RULE: the AWS calls a hook's script makes are really made, so what the script wrote is in
/// the bucket by the time the instance reports `InService`.
#[tokio::test]
async fn a_hooks_script_really_carries_its_objects() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    emulator.given_object(BUCKET, SEED_KEY, SEED_TEXT).await;
    let role = emulator
        .given_role("notebook-bootstrap-role", &trust_for(SERVICE_PRINCIPAL))
        .await;
    emulator
        .given_role_policy("notebook-bootstrap-role", &every_s3_action())
        .await;

    for (index, row) in ROWS.iter().enumerate() {
        let instance = format!("bootstrap-notebook-{index}");
        let key = format!("bootstrap/written-{index}.txt");
        given_notebook_with_hook(
            &emulator,
            row.case,
            &instance,
            &format!("bootstrap-config-{index}"),
            &role,
            &(row.script)(&key),
        )
        .await;
        settles_in(&emulator, row.case, &instance, IN_SERVICE).await;

        let held = held_object(&emulator, row.case, &key).await;
        let expected = row.written.map(ToString::to_string);
        assert_eq!(
            held, expected,
            "case {:?}: expected s3://{BUCKET}/{key} to hold {expected:?}, got {held:?}",
            row.case
        );
    }
}

/// RULE: a hook's command deletes the object it names, so a script that takes a staged object
/// away leaves the store without it.
#[tokio::test]
async fn a_hooks_script_really_removes_what_it_deletes() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let role = emulator
        .given_role("notebook-removal-role", &trust_for(SERVICE_PRINCIPAL))
        .await;
    emulator
        .given_role_policy("notebook-removal-role", &every_s3_action())
        .await;

    // One row per spelling of the removal: the file-oriented command and the low-level one.
    let rows: [(&str, &str, &str); 2] = [
        (
            "the file-oriented command",
            "bootstrap/doomed-high.txt",
            "aws s3 rm",
        ),
        (
            "the low-level command",
            "bootstrap/doomed-low.txt",
            "aws s3api delete-object",
        ),
    ];
    for (index, (case, key, command)) in rows.into_iter().enumerate() {
        emulator.given_object(BUCKET, key, "to be removed").await;
        let script = if command.contains("s3api") {
            format!("{command} --bucket {BUCKET} --key {key}\n")
        } else {
            format!("{command} s3://{BUCKET}/{key}\n")
        };
        let instance = format!("removal-notebook-{index}");
        given_notebook_with_hook(
            &emulator,
            case,
            &instance,
            &format!("removal-config-{index}"),
            &role,
            &script,
        )
        .await;
        settles_in(&emulator, case, &instance, IN_SERVICE).await;

        let held = held_object(&emulator, case, key).await;
        assert!(
            held.is_none(),
            "case {case:?}: expected s3://{BUCKET}/{key} to be gone, got {held:?}"
        );
    }
}

/// RULE: the calls are made AS the instance's execution role, so what the permissions in force
/// on that role say about the action decides whether the object is written at all — and only
/// what was ANSWERED withholds anything.
#[tokio::test]
async fn the_execution_role_decides_what_a_hook_carries() {
    /// One row: how the row's role is held, and whether the hook's write lands.
    struct RoleRow {
        case: &'static str,
        /// The grant the role carries, and `None` for a role that carries none.
        grant: Option<fn() -> Value>,
        /// Whether IAM holds the role at all.
        held: bool,
        /// Whether the object the script writes is in the bucket afterwards.
        carried: bool,
        /// The status the instance settles in.
        settles_in: &'static str,
    }

    // Rows differ along what the role's permissions ANSWER about the write: a grant that
    // allows it, a role whose policies say nothing about it, a policy that denies it
    // explicitly, and a role this account's IAM holds nothing under — whose instance never
    // comes up at all, so no hook of its configuration runs.
    let rows: &[RoleRow] = &[
        RoleRow {
            case: "a role granted the write",
            grant: Some(every_s3_action),
            held: true,
            carried: true,
            settles_in: IN_SERVICE,
        },
        RoleRow {
            case: "a role whose policies say nothing about the write",
            grant: Some(|| {
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": "sagemaker:DescribeNotebookInstance",
                        "Resource": "*",
                    }],
                })
            }),
            held: true,
            carried: false,
            settles_in: IN_SERVICE,
        },
        RoleRow {
            case: "a role whose policy denies the write explicitly",
            grant: Some(|| {
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        {
                            "Effect": "Allow",
                            "Action": "s3:*",
                            "Resource": "*",
                        },
                        {
                            "Effect": "Deny",
                            "Action": "s3:PutObject",
                            "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
                        },
                    ],
                })
            }),
            held: true,
            carried: false,
            settles_in: IN_SERVICE,
        },
        RoleRow {
            case: "a role this account's IAM holds nothing under",
            grant: None,
            held: false,
            carried: false,
            settles_in: "Failed",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let name = format!("role-row-{index}");
        let role = if row.held {
            let arn = emulator
                .given_role(&name, &trust_for(SERVICE_PRINCIPAL))
                .await;
            if let Some(grant) = row.grant {
                emulator.given_role_policy(&name, &grant()).await;
            }
            arn
        } else {
            role_arn(&name)
        };
        let key = format!("bootstrap/role-{index}.txt");
        given_notebook_with_hook(
            &emulator,
            row.case,
            "role-notebook",
            "role-config",
            &role,
            &format!("echo carried | aws s3 cp - s3://{BUCKET}/{key}\n"),
        )
        .await;
        settles_in(&emulator, row.case, "role-notebook", row.settles_in).await;

        let held = held_object(&emulator, row.case, &key).await;
        assert_eq!(
            held.is_some(),
            row.carried,
            "case {:?}: expected s3://{BUCKET}/{key} to be carried: {}, got {held:?}",
            row.case,
            row.carried
        );
    }
}
