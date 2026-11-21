// SPDX-License-Identifier: CC0-1.0

//! The code interpreter's `executeCommand` shell performs the ordinary TEXT UTILITIES a
//! sandbox's coreutils provide, and says so for a command name it cannot resolve.
//!
//! RULE: a command line of the code interpreter's shell names the standard text utilities an
//! ordinary Linux sandbox has installed, and each of them writes what it computed over its ONE
//! input — the file it names, or the stage before it in the pipeline — so a pipeline of them
//! comes to what those computations come to, with the pipeline's status its last stage's. A
//! command NAME nothing resolves is the shell's own command search failing, which a shell
//! reports with a diagnostic naming that command on standard error beside the status for a
//! command it could not run; a status with nothing written for it would leave a client unable
//! to tell a utility the sandbox does not have from one that ran and wrote nothing.
//!
//! The rows differ along what the rule generalises over: which utility a stage names, which
//! option it carries, whether its input is a file or the stage before it, how many stages the
//! pipeline has, and whether the utility is one the sandbox's own program catalog provides or
//! one the shell's shared text filters do.
//!
//! This suite lives here, in the assembled router's own test crate, because that is where a
//! suite of this project may be added: the AgentCore service crate still declares its suites by
//! hand. Every case drives real HTTP through the same `MultiServiceRouter` the emulator serves
//! with, so what is asserted is the wire behaviour a client sees — the `result` event of an
//! `application/vnd.amazon.eventstream` answer and the `structuredContent` it carries.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::bedrock_agentcore_data_plane_parent_resources::emulator;

/// The system code interpreter AWS ships, whose sessions need no control-plane create.
const SYSTEM_INTERPRETER: &str = "aws.codeinterpreter.v1";

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The header `sessionId` binds on an `InvokeCodeInterpreter` request (source: smithy_query
/// query="operation" operation="InvokeCodeInterpreter").
const SESSION_HEADER: &str = "x-amzn-code-interpreter-session-id";

/// One restJson1 call, answering the raw body: a tool invocation's answer is an event stream
/// rather than JSON, so the bytes are what a client decodes.
async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (u16, Vec<u8>) {
    let payload = body
        .map(|value| bytes::Bytes::from(value.to_string()))
        .unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://bedrock-agentcore.{REGION}.amazonaws.com{uri}"
        ))
        .header("host", format!("bedrock-agentcore.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
                 aws4_request, SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(request.body(payload).expect("a well-formed request"))
        .await;
    let status = response.status().as_u16();
    (status, response.into_body().to_vec())
}

/// `StartCodeInterpreterSession`, answering the `sessionId` it minted.
async fn start_session(router: &MultiServiceRouter) -> String {
    let (status, body) = send(
        router,
        "PUT",
        &format!("/code-interpreters/{SYSTEM_INTERPRETER}/sessions/start"),
        &[],
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status,
        200,
        "StartCodeInterpreterSession must answer 200, got {status} with {:?}",
        String::from_utf8_lossy(&body)
    );
    let started: Value = serde_json::from_slice(&body).expect("a JSON answer");
    started["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a started session carries a sessionId, got {started}"))
        .to_string()
}

/// `InvokeCodeInterpreter`, answering the ONE `result` event its stream carries.
async fn tool(router: &MultiServiceRouter, case: &str, session: &str, name: &str, arguments: Value) -> Value {
    let (status, body) = send(
        router,
        "POST",
        &format!("/code-interpreters/{SYSTEM_INTERPRETER}/tools/invoke"),
        &[(SESSION_HEADER, session)],
        Some(json!({"name": name, "arguments": arguments})),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: expected HTTP 200 from InvokeCodeInterpreter, got {status} with {:?}",
        String::from_utf8_lossy(&body)
    );
    first_event(case, &body)
}

/// The payload of the FIRST event of an `application/vnd.amazon.eventstream` body, as the JSON
/// it carries.
///
/// A frame is its total length, its headers length, a prelude checksum, its headers, its
/// payload and a message checksum, each length a big-endian `u32` — so the payload is the span
/// between the headers and that trailing checksum.
fn first_event(case: &str, body: &[u8]) -> Value {
    let prelude: [u8; 4] = body
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .unwrap_or_else(|| panic!("case {case:?}: an event-stream body carries a frame prelude"));
    let headers: [u8; 4] = body
        .get(4..8)
        .and_then(|bytes| bytes.try_into().ok())
        .unwrap_or_else(|| panic!("case {case:?}: an event-stream frame carries a headers length"));
    let total = usize::try_from(u32::from_be_bytes(prelude)).expect("a frame length");
    let headers = usize::try_from(u32::from_be_bytes(headers)).expect("a headers length");
    assert_eq!(
        total,
        body.len(),
        "case {case:?}: expected exactly one event frame in {} bytes",
        body.len()
    );
    let start = headers.saturating_add(12);
    let end = total.saturating_sub(4);
    let named = body
        .get(12..start)
        .unwrap_or_else(|| panic!("case {case:?}: a frame carries {headers} bytes of headers"));
    assert!(
        named.windows(6).any(|window| window == b"result"),
        "case {case:?}: expected the `result` member of the union, got the headers {:?}",
        String::from_utf8_lossy(named)
    );
    let payload = body
        .get(start..end)
        .unwrap_or_else(|| panic!("case {case:?}: a frame of {total} bytes carries a payload"));
    serde_json::from_slice(payload).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the event payload must be JSON ({error}), got {:?}",
            String::from_utf8_lossy(payload)
        )
    })
}

/// The text of `member` in a result's `structuredContent`.
fn structured_text(result: &Value, member: &str) -> String {
    result
        .get("structuredContent")
        .and_then(|structured| structured.get(member))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `exitCode` a result reports, which every command run carries.
fn exit_code(case: &str, result: &Value) -> i64 {
    result
        .get("structuredContent")
        .and_then(|structured| structured.get("exitCode"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: a command reports an exit status, got {result}"))
}

/// Whether the result is flagged as a tool failure.
fn is_error(result: &Value) -> bool {
    result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `executeCommand` with one command line.
async fn command(router: &MultiServiceRouter, case: &str, session: &str, line: &str) -> Value {
    tool(router, case, session, "executeCommand", json!({"command": line})).await
}

/// The text utilities of a sandbox's coreutils compute over the one input the command line
/// gives them, whether that is a file of the session's workspace or the stage before them.
#[tokio::test]
async fn the_sandbox_shell_performs_the_standard_text_utilities() {
    let router = emulator();
    let session = start_session(&router).await;
    let written = tool(
        &router,
        "seed-the-workspace",
        &session,
        "writeFiles",
        json!({"content": [
            {"path": "lines.txt", "text": "pear\napple\npear\nfig\n"},
            {"path": "numbers.txt", "text": "10\n9\n100\n"},
        ]}),
    )
    .await;
    assert!(!is_error(&written), "the workspace must be seeded, got {written}");

    // (case, the command line, what it writes to standard output, the status it ends with)
    let rows: Vec<(&str, &str, &str, i64)> = vec![
        (
            "sort writes the lines of a file in order",
            "sort lines.txt",
            "apple\nfig\npear\npear\n",
            0,
        ),
        (
            "sort -u keeps one of an equal run",
            "sort -u lines.txt",
            "apple\nfig\npear\n",
            0,
        ),
        (
            "sort -n compares according to numerical value",
            "sort -n numbers.txt",
            "9\n10\n100\n",
            0,
        ),
        (
            "sort -r reverses the comparison",
            "sort -r lines.txt",
            "pear\npear\nfig\napple\n",
            0,
        ),
        (
            "sort reads the stage before it in a pipeline",
            "cat lines.txt | sort -u",
            "apple\nfig\npear\n",
            0,
        ),
        (
            "uniq -c counts each run of adjacent equal lines",
            "sort lines.txt | uniq -c",
            "      1 apple\n      1 fig\n      2 pear\n",
            0,
        ),
        (
            "uniq -d writes the runs that repeat",
            "sort lines.txt | uniq -d",
            "pear\n",
            0,
        ),
        (
            "tr translates one character class into the other",
            "cat lines.txt | tr '[:lower:]' '[:upper:]' | sort -u",
            "APPLE\nFIG\nPEAR\n",
            0,
        ),
        (
            "tr -d deletes the characters its set names",
            "cat numbers.txt | tr -d '0'",
            "1\n9\n1\n",
            0,
        ),
        (
            "a utility of the sandbox's own catalog reads the stage before it",
            "cat lines.txt | wc -l",
            "4\n",
            0,
        ),
        (
            "tail reads the stage before it too",
            "cat lines.txt | tail -n 2",
            "pear\nfig\n",
            0,
        ),
        (
            "head of a file the sandbox holds",
            "head -n 1 lines.txt",
            "pear\n",
            0,
        ),
        (
            // The utility's own diagnostic goes to standard error, so standard output
            // carries nothing at all and the status is the utility's failure.
            "sort of a path the workspace holds nothing at",
            "sort absent.txt",
            "",
            1,
        ),
        (
            // The line the behavioural check runs: `ls` writes one diagnostic per missing
            // operand, the duplication sends them into the pipe, and `sort -u` writes them
            // — so the pipeline's status is the one its LAST stage answered.
            "a pipeline whose first stage failed and whose last stage succeeded",
            "ls -1 /tmp/absent-one.txt ../absent-two.txt 2>&1 | sort -u",
            "ls: cannot access '../absent-two.txt': No such file or directory\nls: cannot \
             access '/tmp/absent-one.txt': No such file or directory\n",
            0,
        ),
    ];

    for (case, line, expected, status) in rows {
        let result = command(&router, case, &session, line).await;
        assert_eq!(
            structured_text(&result, "stdout"),
            expected,
            "case {case:?}: expected what the utilities computed, got {result}"
        );
        assert_eq!(
            exit_code(case, &result),
            status,
            "case {case:?}: expected the status the line ends with, got {result}"
        );
    }

    // What a pipeline wrote into a file is in the session's workspace: the shell, the file
    // tools and a later command are three views of one filesystem.
    let kept = command(
        &router,
        "keep-what-the-pipeline-wrote",
        &session,
        "sort -u lines.txt > sorted.txt",
    )
    .await;
    assert_eq!(exit_code("keep-what-the-pipeline-wrote", &kept), 0, "got {kept}");
    let read = tool(
        &router,
        "read-what-the-pipeline-wrote",
        &session,
        "readFiles",
        json!({"paths": ["sorted.txt"]}),
    )
    .await;
    let blocks = read
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text: String = blocks
        .iter()
        .filter_map(|block| {
            // A read answers a resource block, whose own `text` is the file's contents.
            block
                .get("resource")
                .and_then(|resource| resource.get("text"))
                .or_else(|| block.get("text"))
                .and_then(Value::as_str)
        })
        .collect();
    assert!(
        text.contains("apple\nfig\npear\n"),
        "the file tool must read what the pipeline wrote, got {read}"
    );
}

/// A command NAME nothing in the sandbox resolves is reported the way a shell reports one: the
/// status for a command it could not run, AND the diagnostic naming that command.
#[tokio::test]
async fn a_command_the_sandbox_cannot_resolve_reports_the_shells_diagnostic() {
    let router = emulator();
    let session = start_session(&router).await;

    // (case, the command line, the name the diagnostic must carry)
    let rows: Vec<(&str, &str, &str)> = vec![
        ("a utility the sandbox has none of", "jq -r .", "jq"),
        ("a name with no arguments at all", "definitely-not-a-program", "definitely-not-a-program"),
        ("a name reached through a wrapper", "nohup cowsay hello", "cowsay"),
    ];

    for (case, line, name) in rows {
        let result = command(&router, case, &session, line).await;
        // "The utility to be executed could not be found": the status POSIX gives that
        // search, which is what the result reports.
        assert_eq!(
            exit_code(case, &result),
            127,
            "case {case:?}: expected the status for a command that could not be run, got {result}"
        );
        let said = structured_text(&result, "stderr");
        assert!(
            said.contains(name) && said.contains("not found"),
            "case {case:?}: expected a diagnostic naming {name:?} on standard error, got {result}"
        );
        assert!(
            structured_text(&result, "stdout").is_empty(),
            "case {case:?}: a command that did not run wrote nothing to standard output, got \
             {result}"
        );
        assert!(
            is_error(&result),
            "case {case:?}: a command that could not run is a failed tool call, got {result}"
        );
    }

    // `2>&1` sends the diagnostic where the command's output goes, which is what a client
    // reading one merged stream sees — and the pipeline's own status is its last stage's, so
    // the diagnostic is the only signal the first stage never ran.
    let merged = command(
        &router,
        "a-diagnostic-sent-into-the-pipe",
        &session,
        "jq . 2>&1 | cat",
    )
    .await;
    let written = structured_text(&merged, "stdout");
    assert!(
        written.contains("jq") && written.contains("not found"),
        "case \"a diagnostic sent into the pipe\": expected it on standard output, got {merged}"
    );
    assert_eq!(
        exit_code("a-diagnostic-sent-into-the-pipe", &merged),
        0,
        "case \"a diagnostic sent into the pipe\": the pipeline is worth its last stage, got \
         {merged}"
    );

    // A command the search DID resolve says nothing on standard error: the diagnostic is the
    // search's own failure, not something every command writes.
    let performed = command(&router, "a-command-that-ran", &session, "echo kept").await;
    assert_eq!(
        structured_text(&performed, "stdout"),
        "kept\n",
        "case \"a command that ran\": got {performed}"
    );
    assert_eq!(
        structured_text(&performed, "stderr"),
        "",
        "case \"a command that ran\": expected nothing on standard error, got {performed}"
    );
}
