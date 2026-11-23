// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what a SANDBOX does with the commands submitted to it, driven
//! over the wire the router serves: `POST /` with
//! `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1.
//!
//! The rule under test: a sandbox is a live session of its project's environment, and a
//! command execution submitted to it is RUN in that session — so the execution's status
//! follows from the status the command exited with (`SUCCEEDED` for zero, `FAILED` for
//! anything else), `exitCode` is that status, and `standardOutputContent` /
//! `standardErrContent` are the text the command wrote on each stream
//! (`CommandExecution$standardOutputContent` is "The text written by the command to
//! stdout", `standardErrContent` "The text written by the command to stderr", `exitCode`
//! "The exit code to return upon completion"). The commands of ONE sandbox run one after
//! another in the SAME session, so what one of them set or wrote is what the next one
//! finds, and a command whose sandbox has no session left never ran at all.
//!
//! Each suite in `tests/all/` drives the wire itself, so the harness below (the router,
//! `call`, the project body) is this suite's own.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::ops::types::{CommandExecution, Sandbox};
use service_com_amazonaws_codebuild_codebuild_20161006::state::{
    self, ComAmazonawsCodebuildCodebuild20161006State as CodeBuildState, StoredCommandExecution,
    StoredSandbox,
};
use service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The access key id every request below is signed with.
const ACCESS_KEY_ID: &str = "AKIDCODEBUILDSANDBOXRUN";

/// A plaintext environment variable the project under test declares, so a command of its
/// sandbox can resolve one ("A set of environment variables to make available to builds
/// for this build project").
const PROJECT_VARIABLE: (&str, &str) = ("SANDBOX_MARKER", "from-the-project");

/// How long a poll below waits for a condition the emulator's own clock brings about
/// before giving up.
///
/// Deliberately far longer than any span this service runs on
/// ([`state::COMMAND_EXECUTION_SETTLE_MILLIS`] and
/// [`state::SANDBOX_SETTLE_MILLIS`] are sub-second), so a loaded or slow host still
/// reaches the condition rather than the deadline.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How often a poll below looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One router carrying only this service, over one shared engine — the same assembly
/// `core/router` builds, minus the other services.
fn emulator_on(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Call one operation and return its HTTP status and decoded body.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CodeBuild_20161006.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/codebuild/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0f0f"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// A minimal valid `CreateProject` request for `name`, declaring one environment variable.
fn create_project_body(name: &str) -> Value {
    json!({
        "name": name,
        "source": {"type": "NO_SOURCE", "buildspec": "version: 0.2"},
        "artifacts": {"type": "NO_ARTIFACTS"},
        "environment": {
            "type": "LINUX_CONTAINER",
            "image": "aws/codebuild/standard:7.0",
            "computeType": "BUILD_GENERAL1_SMALL",
            "environmentVariables": [
                {"name": PROJECT_VARIABLE.0, "value": PROJECT_VARIABLE.1, "type": "PLAINTEXT"},
            ],
        },
        "serviceRole": "arn:aws:iam::000000000000:role/service-role/codebuild",
    })
}

/// Create `project` and start a sandbox of it over the wire, asserting both succeeded, and
/// hand back the sandbox id.
async fn start_sandbox(router: &MultiServiceRouter, project: &str) -> String {
    let (status, body) = call(router, "CreateProject", create_project_body(project)).await;
    assert_eq!(
        status, 200,
        "CreateProject {project:?}: expected 200, got {status} with {body}"
    );
    let (status, body) = call(router, "StartSandbox", json!({"projectName": project})).await;
    assert_eq!(
        status, 200,
        "StartSandbox {project:?}: expected 200, got {status} with {body}"
    );
    body["sandbox"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Submit `command` to `sandbox_id`, asserting it was accepted, and hand back its id.
async fn submit(router: &MultiServiceRouter, sandbox_id: &str, command: &str) -> String {
    let (status, body) = call(
        router,
        "StartCommandExecution",
        json!({"sandboxId": sandbox_id, "command": command, "type": "SHELL"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartCommandExecution {command:?}: expected 200, got {status} with {body}"
    );
    body["commandExecution"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Read one command execution back under the sandbox it belongs to.
async fn read_execution(router: &MultiServiceRouter, sandbox_id: &str, id: &str) -> Value {
    let (status, body) = call(
        router,
        "BatchGetCommandExecutions",
        json!({"sandboxId": sandbox_id, "commandExecutionIds": [id]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "BatchGetCommandExecutions {id:?}: expected 200, got {status} with {body}"
    );
    body["commandExecutions"][0].clone()
}

/// Submit `command` and look again until the execution has left `IN_PROGRESS`, which the
/// sandbox's session brings about on its own clock.
///
/// Waits for the CONDITION rather than for a span: it looks again every
/// [`POLL_INTERVAL`] until the record is terminal or [`POLL_DEADLINE`] has passed.
async fn run_command(router: &MultiServiceRouter, sandbox_id: &str, command: &str) -> Value {
    let id = submit(router, sandbox_id, command).await;
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = read_execution(router, sandbox_id, &id).await;
        if last["status"] != json!(state::COMMAND_EXECUTION_STATUS_IN_PROGRESS) {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("command {command:?} never left IN_PROGRESS, last read {last}");
}

/// A member as a string, for an assertion message that shows what was actually there.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// What one row of the table below expects of the execution: its status, the exit code it
/// reports, the text it wrote on standard output, and what its standard error carries.
struct Expected {
    /// The `CommandExecution$status` the execution must settle on.
    status: &'static str,
    /// The `exitCode` it must report, as the decimal string the member carries.
    exit_code: &'static str,
    /// The whole of `standardOutputContent`, or `None` when the command wrote nothing
    /// there and the member must be absent.
    standard_output: Option<String>,
    /// Text `standardErrContent` must CONTAIN, or `None` when the member must be absent.
    err_contains: Option<&'static str>,
}

/// RULE: the execution's status, exit code and two output members are what running the
/// command answered.
///
/// The rows span the command forms a caller can submit and the outcomes they produce: a
/// command that succeeds, one that ends itself non-zero, a utility that answers failure, a
/// script of several lines, a command that writes on standard error, one the environment
/// has no program for, a quoted argument, a command reading an environment variable of the
/// project, one that substitutes another command's output, a compound command spanning
/// lines, and an and-or list.
#[tokio::test]
async fn a_sandbox_runs_each_command_and_reports_what_it_answered() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    let sandbox_id = start_sandbox(&router, "sandbox-runs").await;

    let cases: Vec<(&str, &str, Expected)> = vec![
        (
            "a command that succeeds",
            "echo marker",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_SUCCEEDED,
                exit_code: "0",
                standard_output: Some("marker\n".to_string()),
                err_contains: None,
            },
        ),
        (
            "a command that ends itself non-zero",
            "exit 7",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_FAILED,
                exit_code: "7",
                standard_output: None,
                err_contains: None,
            },
        ),
        (
            "a utility that answers failure",
            "false",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_FAILED,
                exit_code: "1",
                standard_output: None,
                err_contains: None,
            },
        ),
        (
            "a script of several lines",
            "echo one\necho two",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_SUCCEEDED,
                exit_code: "0",
                standard_output: Some("one\ntwo\n".to_string()),
                err_contains: None,
            },
        ),
        (
            "a command that writes on standard error",
            "echo oops 1>&2",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_SUCCEEDED,
                exit_code: "0",
                standard_output: None,
                err_contains: Some("oops"),
            },
        ),
        (
            "a command the environment has no program for",
            "definitely-not-a-program",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_FAILED,
                // The status a shell answers for a utility it could not find.
                exit_code: "127",
                standard_output: None,
                err_contains: Some("definitely-not-a-program"),
            },
        ),
        (
            "a quoted argument",
            "echo 'hello  world'",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_SUCCEEDED,
                exit_code: "0",
                standard_output: Some("hello  world\n".to_string()),
                err_contains: None,
            },
        ),
        (
            "a command reading an environment variable of the project",
            "echo ${SANDBOX_MARKER}",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_SUCCEEDED,
                exit_code: "0",
                standard_output: Some(format!("{}\n", PROJECT_VARIABLE.1)),
                err_contains: None,
            },
        ),
        (
            "a command substituting another command's output",
            "echo \"[$(echo inner)]\"",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_SUCCEEDED,
                exit_code: "0",
                standard_output: Some("[inner]\n".to_string()),
                err_contains: None,
            },
        ),
        (
            "a compound command spanning lines",
            "for name in one two\ndo\n  echo ${name}\ndone",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_SUCCEEDED,
                exit_code: "0",
                standard_output: Some("one\ntwo\n".to_string()),
                err_contains: None,
            },
        ),
        (
            "an and-or list whose first command fails",
            "false || echo recovered",
            Expected {
                status: state::COMMAND_EXECUTION_STATUS_SUCCEEDED,
                exit_code: "0",
                standard_output: Some("recovered\n".to_string()),
                err_contains: None,
            },
        ),
    ];

    for (case, command, expected) in cases {
        let record = run_command(&router, &sandbox_id, command).await;
        assert_eq!(
            record["status"],
            json!(expected.status),
            "case {case:?}: expected status {:?}, got {record}",
            expected.status
        );
        assert_eq!(
            record["exitCode"],
            json!(expected.exit_code),
            "case {case:?}: expected exitCode {:?}, got {record}",
            expected.exit_code
        );
        match expected.standard_output {
            Some(written) => assert_eq!(
                record["standardOutputContent"],
                json!(written),
                "case {case:?}: expected stdout {written:?}, got {record}"
            ),
            None => assert!(
                record["standardOutputContent"].is_null(),
                "case {case:?}: the command wrote nothing to stdout, got {record}"
            ),
        }
        match expected.err_contains {
            Some(fragment) => assert!(
                text(&record, "standardErrContent").contains(fragment),
                "case {case:?}: expected stderr carrying {fragment:?}, got {record}"
            ),
            None => assert!(
                record["standardErrContent"].is_null(),
                "case {case:?}: the command wrote nothing to stderr, got {record}"
            ),
        }
        assert!(
            record["startTime"].is_number() && record["endTime"].is_number(),
            "case {case:?}: a command that ran reports when it started and ended, got \
             {record}"
        );
    }
}

/// RULE: the commands of ONE sandbox run one after another in the SAME session, so what one
/// of them sets or writes is what the next one of that sandbox finds — and nothing of
/// another sandbox's session reaches it.
#[tokio::test]
async fn a_sandboxs_commands_run_in_one_session_and_never_in_anothers() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    let sandbox_id = start_sandbox(&router, "sandbox-session").await;

    // (case, the command, what the execution must report for stdout)
    let steps: [(&str, &str, Option<&str>); 4] = [
        ("a command that sets a variable", "export KEPT=by-the-session", None),
        ("the next command reads that variable", "echo ${KEPT}", Some("by-the-session\n")),
        ("a command that writes a file", "echo saved > kept.txt", None),
        ("the next command reads that file", "cat kept.txt", Some("saved\n")),
    ];
    for (case, command, expected) in steps {
        let record = run_command(&router, &sandbox_id, command).await;
        assert_eq!(
            record["status"],
            json!(state::COMMAND_EXECUTION_STATUS_SUCCEEDED),
            "case {case:?}: expected the command to succeed, got {record}"
        );
        match expected {
            Some(written) => assert_eq!(
                record["standardOutputContent"],
                json!(written),
                "case {case:?}: expected stdout {written:?}, got {record}"
            ),
            None => assert!(
                record["standardOutputContent"].is_null(),
                "case {case:?}: the command wrote nothing to stdout, got {record}"
            ),
        }
    }

    // Another sandbox is another session: the variable the first one's command set is not
    // in it, so the expansion is empty and the command writes the empty line `echo` gives
    // it.
    let other_sandbox = start_sandbox(&router, "sandbox-session-other").await;
    let record = run_command(&router, &other_sandbox, "echo ${KEPT}").await;
    assert_eq!(
        record["standardOutputContent"],
        json!("\n"),
        "another sandbox's session must not hold this one's variables, got {record}"
    );
    let record = run_command(&router, &other_sandbox, "cat kept.txt").await;
    assert_eq!(
        record["status"],
        json!(state::COMMAND_EXECUTION_STATUS_FAILED),
        "another sandbox's session must not hold this one's files, got {record}"
    );
}

/// RULE: the window an execution reports is the time the command itself took — the session
/// performs it in process and spends none of the host's clock on it, so a command that
/// states it took time reports exactly that span between `startTime` and `endTime`.
#[tokio::test]
async fn an_execution_reports_the_time_its_own_command_stated() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    let sandbox_id = start_sandbox(&router, "sandbox-window").await;

    // (case, the command, the seconds it states it takes)
    for (case, command, stated) in [
        ("a command that states no time", "echo quick", 0.0_f64),
        ("a command that states two seconds", "sleep 2", 2.0),
        ("two sleeps of one second each", "sleep 1\nsleep 1", 2.0),
    ] {
        let record = run_command(&router, &sandbox_id, command).await;
        let started = record["startTime"].as_f64().unwrap_or_default();
        let ended = record["endTime"].as_f64().unwrap_or_default();
        assert!(
            (ended - started - stated).abs() < 0.002,
            "case {case:?}: expected a reported window of {stated}s, got {} from {record}",
            ended - started
        );
    }
}

/// RULE: a command whose sandbox has no session left when its turn comes never ran — it
/// ends in the status that session took, with no exit code and no output, because nothing
/// executed it.
///
/// The records are seeded so the instants are the subject rather than a race: the sandbox
/// ended before the submitted command's turn came round.
#[tokio::test]
async fn a_command_whose_sandbox_session_ended_first_never_ran() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    let sandbox_id = "sandbox-ended:0001";
    let now = state::now_millis();
    let ended_millis = now - 1_000;
    {
        let handle = engine.state::<CodeBuildState>();
        let mut guard = handle.lock();
        guard.insert_sandbox(
            sandbox_id.to_string(),
            StoredSandbox {
                sandbox: Sandbox {
                    id: Some(sandbox_id.to_string()),
                    arn: Some(state::sandbox_arn(REGION, sandbox_id)),
                    project_name: Some("sandbox-ended".to_string()),
                    request_time: Some(state::datetime(now - 2_000)),
                    status: Some(state::SANDBOX_STATUS_STOPPED.to_string()),
                    end_time: Some(state::datetime(ended_millis)),
                    ..Sandbox::default()
                },
                request_millis: now - 2_000,
                settle_due_millis: now - 2_000,
            },
        );
        guard.insert_command_execution(
            "sandbox-ended-command".to_string(),
            StoredCommandExecution {
                command_execution: CommandExecution {
                    id: Some("sandbox-ended-command".to_string()),
                    sandbox_id: Some(sandbox_id.to_string()),
                    command: Some("echo never".to_string()),
                    status: Some(state::COMMAND_EXECUTION_STATUS_IN_PROGRESS.to_string()),
                    submit_time: Some(state::datetime(ended_millis)),
                    ..CommandExecution::default()
                },
                submit_millis: ended_millis,
                // Its turn would have come after the session had already ended.
                settle_due_millis: ended_millis + state::COMMAND_EXECUTION_SETTLE_MILLIS,
            },
        );
    }

    let record = read_execution(&router, sandbox_id, "sandbox-ended-command").await;
    assert_eq!(
        record["status"],
        json!(state::SANDBOX_STATUS_STOPPED),
        "a command no session was left to run must end where that session did, got {record}"
    );
    assert!(
        record["exitCode"].is_null()
            && record["standardOutputContent"].is_null()
            && record["standardErrContent"].is_null(),
        "nothing ran the command, so it reports no result and no output, got {record}"
    );
    assert!(
        record["endTime"].is_number(),
        "the execution ended, so it reports when, got {record}"
    );
}
