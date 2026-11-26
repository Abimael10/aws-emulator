// SPDX-License-Identifier: CC0-1.0

//! The interactive-session flow a notebook client drives, end to end, on the ASSEMBLED
//! emulator: an IAM role Glue can assume, a session that reaches `READY`, a cell that runs
//! on the session's runtime, and then every statement ADDRESS that session answers.
//!
//! It lives here rather than in the Glue crate because the role the session runs as is IAM's:
//! only a suite holding both services on one engine drives `CreateSession` the way a client
//! does, with a role this account's own IAM really holds and whose trust policy really admits
//! the Glue service principal.
//!
//! The rule under test, which is about a session's own statement ID SPACE rather than about
//! any one id: a session mints the `Id` of each statement run on it, counting up, so for
//! `GetStatement(SessionId, Id)`
//!
//! * an `Id` the session MINTED reads back 200 with that statement — its code, its terminal
//!   state and the output its code itself wrote;
//! * an `Id` INSIDE the session's own space that the session does not hold — the one it would
//!   mint next — names a statement slot of this session's runtime, so the read is refused 400
//!   `IllegalSessionStateException` with the service's fixed sentence, which has no variable
//!   part at all;
//! * an `Id` OUTSIDE that space names no statement of any session, so the read is refused 400
//!   `EntityNotFoundException` naming the id the request asked for and nothing else;
//! * and once the session is deleted, its id and every statement of it are misses.
//!
//! The rows span what the rule generalises over: where the id sits relative to the session's
//! space (minted, the next one, one past it, far past it, the modelled default `0`, a negative
//! id) and HOW MANY statements the session has minted — the table is driven twice, once after
//! one statement and once after a second, because the space MOVES when the session mints
//! another id, and a row that is the session-state refusal in the first pass is a 200 in the
//! second.
//!
//! Every span this flow waits on is POLLED for the condition — the session reaching `READY`,
//! the statement reaching a terminal state — against a generous deadline, so nothing here
//! depends on how fast the host runs.
//!
//! Assembling the router costs stack, so the check runs on an explicitly sized thread for the
//! same reason `boot_serve.rs` does.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one percent-encoder of this test crate: IAM's `awsQuery` parameters travel in a form
// body, and `query_wire` is where that encoding and the XML element readers live.
use crate::query_wire::{element, form_encode};

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call here is signed for, and so the Region the session is served in.
const REGION: &str = "us-east-1";

/// The role the session runs as, created in this account's own IAM.
const ROLE_NAME: &str = "sim2real-glue-session-role";

/// Its trust policy: the Glue service principal may assume it, which is what lets Glue stand
/// up the session's runtime as that role.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"glue.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The session this flow drives.
const SESSION_ID: &str = "sim2real-glue-sess-statement-space";

/// The `SessionStatus` values this suite observes, of the modelled enum (`PROVISIONING`,
/// `READY`, `FAILED`, `TIMEOUT`, `STOPPING`, `STOPPED`, from
/// `smithy_query(query="shape",shape="SessionStatus")`).
const STATUS_PROVISIONING: &str = "PROVISIONING";
const STATUS_READY: &str = "READY";

/// The `StatementState` a cell that ran to its end comes to rest in, of the modelled enum
/// (`WAITING`, `RUNNING`, `AVAILABLE`, `CANCELLING`, `CANCELLED`, `ERROR`, from
/// `smithy_query(query="shape",shape="StatementState")`).
const STATE_AVAILABLE: &str = "AVAILABLE";

/// The `StatementState` of a cell that failed, which this flow's cells must never reach.
const STATE_ERROR: &str = "ERROR";

/// How long a poll waits for a condition this flow waits on. GENEROUS: the spans are the
/// service's own and the cell's own run is real work, so the deadline bounds nothing this
/// suite asserts about — it only keeps a broken emulator from hanging the suite.
const CONDITION_DEADLINE: Duration = Duration::from_secs(180);

/// The gap between two reads of a client that polls — not a span anything is expected to
/// finish inside.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The access key id every request presents. Request authentication is off on this router, so
/// only the credential scope is read: its service field routes the request and its Region
/// field is the Region the service serves it in.
const ACCESS_KEY: &str = "AKIDGLUESESSIONFLOW";

/// A SigV4 `Authorization` header whose credential scope names `signing_name` in [`REGION`].
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCESS_KEY}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// flow spans IAM and Glue, and both are reached through the router the binary serves.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// One IAM action over the awsQuery wire.
async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Glue operation over the awsJson1_1 wire, answering its HTTP status and parsed body.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("glue.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header("authorization", authorization("glue"))
        .body(Bytes::from(body.to_string()))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Glue operation that must answer 200, and its body.
async fn glue_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, answer) = glue(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// The awsJson error code of a failure body (`__type`), which may carry a namespace.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `Message` a modelled Glue failure carries.
fn error_message(body: &Value) -> String {
    body.get("Message")
        .and_then(Value::as_str)
        .unwrap_or("<no Message>")
        .to_string()
}

/// What a `GetStatement` address must be answered with.
#[derive(Debug)]
enum Answered {
    /// The session holds that statement: 200, carrying the code submitted under that id.
    Held { code: &'static str },
    /// The id is inside the session's own space but the session holds no statement under it:
    /// the session-state refusal, whose sentence has no variable part.
    IllegalSessionStatus,
    /// The id is outside the session's space: the statement miss, naming the id asked for.
    Miss,
}

/// Read the statement `id` of the session and assert it is answered as `expected`.
async fn assert_statement_address(
    router: &MultiServiceRouter,
    case: &str,
    id: i64,
    expected: &Answered,
) {
    let (status, body) = glue(
        router,
        "GetStatement",
        &json!({ "SessionId": SESSION_ID, "Id": id }),
    )
    .await;
    match expected {
        Answered::Held { code } => {
            assert_eq!(
                status, 200,
                "case {case}: an id the session minted must read back, got {status} with {body}"
            );
            assert_eq!(
                body.pointer("/Statement/Id").and_then(Value::as_i64),
                Some(id),
                "case {case}: the read must echo the id it was asked for, got {body}"
            );
            assert_eq!(
                body.pointer("/Statement/Code").and_then(Value::as_str),
                Some(*code),
                "case {case}: the statement must carry the code submitted under that id, got \
                 {body}"
            );
        }
        Answered::IllegalSessionStatus => {
            assert_eq!(
                status, 400,
                "case {case}: an id inside the session's space that it does not hold must be \
                 refused, got {status} with {body}"
            );
            assert!(
                error_code(&body).ends_with("IllegalSessionStateException"),
                "case {case}: expected IllegalSessionStateException, got {} in {body}",
                error_code(&body)
            );
            // The service's fixed sentence for this refusal: no session id and no statement
            // id appear in it.
            assert_eq!(
                error_message(&body),
                "Illegal Session Status",
                "case {case}: unexpected wording, got {body}"
            );
        }
        Answered::Miss => {
            assert_eq!(
                status, 400,
                "case {case}: an id outside the session's space must be refused, got {status} \
                 with {body}"
            );
            assert!(
                error_code(&body).ends_with("EntityNotFoundException"),
                "case {case}: expected EntityNotFoundException, got {} in {body}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                format!("Statement {id} not found."),
                "case {case}: the miss must name the id asked for and nothing else, got {body}"
            );
        }
    }
}

/// Poll the session until it reports `STATUS_READY`, and answer the record it then reports.
async fn session_at_ready(router: &MultiServiceRouter) -> Value {
    let deadline = SystemTime::now() + CONDITION_DEADLINE;
    loop {
        let read = glue_ok(router, "GetSession", &json!({ "Id": SESSION_ID })).await;
        if read.pointer("/Session/Status").and_then(Value::as_str) == Some(STATUS_READY) {
            return read;
        }
        assert!(
            SystemTime::now() < deadline,
            "the session must reach {STATUS_READY}, and it reports {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `code` on the session and answer the `Id` the session minted for it.
async fn run_cell(router: &MultiServiceRouter, code: &str) -> i64 {
    let answer = glue_ok(
        router,
        "RunStatement",
        &json!({ "SessionId": SESSION_ID, "Code": code }),
    )
    .await;
    answer["Id"]
        .as_i64()
        .unwrap_or_else(|| panic!("RunStatement must answer the Id it minted, got {answer}"))
}

/// Poll the statement `id` until it has come to rest, and answer the record it then reports.
async fn statement_at_rest(router: &MultiServiceRouter, id: i64) -> Value {
    let deadline = SystemTime::now() + CONDITION_DEADLINE;
    loop {
        let read = glue_ok(
            router,
            "GetStatement",
            &json!({ "SessionId": SESSION_ID, "Id": id }),
        )
        .await;
        let state = read
            .pointer("/Statement/State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if state == STATE_AVAILABLE || state == STATE_ERROR {
            return read;
        }
        assert!(
            SystemTime::now() < deadline,
            "statement {id} must come to rest, and it reports {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The text one statement's output carries, with the surrounding whitespace trimmed: what is
/// pinned is what the cell WROTE, not the line break a write ends with.
fn output_text(statement: &Value) -> Option<String> {
    statement
        .pointer("/Statement/Output/Data/TextPlain")
        .and_then(Value::as_str)
        .map(|text| text.trim().to_string())
}

#[test]
fn a_glue_session_answers_each_statement_address_by_its_own_id_space() {
    std::thread::Builder::new()
        .name("glue-session-statement-space".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build a runtime");
            runtime.block_on(drive_the_session_flow());
        })
        .expect("spawn the check thread")
        .join()
        .expect("the check must pass");
}

async fn drive_the_session_flow() {
    let router = build_router();

    // The role the session runs as: one this account's own IAM holds, whose trust policy
    // admits the Glue service principal.
    let (status, created) = iam(
        &router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME.to_string()),
            ("AssumeRolePolicyDocument", TRUST_POLICY.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole must answer 200, got {status} with {created}"
    );
    let role_arn = element(&created, "Arn");

    // The create answers with the record it wrote, and that record is NOT ready yet: the
    // remote Spark runtime is still being provisioned.
    let created_session = glue_ok(
        &router,
        "CreateSession",
        &json!({
            "Id": SESSION_ID,
            "Role": role_arn,
            "Command": { "Name": "glueetl", "PythonVersion": "3" },
            "GlueVersion": "5.0",
            "Timeout": 15,
            "IdleTimeout": 15,
        }),
    )
    .await;
    assert_eq!(
        created_session.pointer("/Session/Id").and_then(Value::as_str),
        Some(SESSION_ID),
        "the create must answer with the session it wrote, got {created_session}"
    );
    assert_eq!(
        created_session
            .pointer("/Session/Status")
            .and_then(Value::as_str),
        Some(STATUS_PROVISIONING),
        "a session is still being provisioned when its create returns, got {created_session}"
    );

    // And the session then leaves PROVISIONING for READY on the service's own clock.
    let ready = session_at_ready(&router).await;
    assert_eq!(
        ready.pointer("/Session/Role").and_then(Value::as_str),
        Some(role_arn.as_str()),
        "the session must report the role it runs as, got {ready}"
    );

    // A cell whose answer cannot be echoed from the request: what the statement reports is
    // what the code COMPUTED on the session's runtime.
    let first_code = "1 + 1";
    let first = run_cell(&router, first_code).await;
    let settled = statement_at_rest(&router, first).await;
    assert_eq!(
        settled.pointer("/Statement/State").and_then(Value::as_str),
        Some(STATE_AVAILABLE),
        "a cell that ran to its end must come to rest in {STATE_AVAILABLE}, got {settled}"
    );
    assert_eq!(
        output_text(&settled).as_deref(),
        Some("2"),
        "the statement must report the value its own code computed, got {settled}"
    );

    // The id space of a session that has minted exactly one statement.
    //
    // (case, the id asked for, how it must be answered)
    let after_one: Vec<(String, i64, Answered)> = vec![
        (
            "the id the session minted".to_string(),
            first,
            Answered::Held { code: first_code },
        ),
        (
            "the id the session would mint next".to_string(),
            first + 1,
            Answered::IllegalSessionStatus,
        ),
        (
            "one past the id space".to_string(),
            first + 2,
            Answered::Miss,
        ),
        (
            "far above the id space".to_string(),
            first + 987_654,
            Answered::Miss,
        ),
        (
            // "default 0" (`IntegerValue`): the id a request that named none carries, which
            // is below the first id a session mints.
            "the modelled default id".to_string(),
            0,
            Answered::Miss,
        ),
        (
            "an id below the id space".to_string(),
            -1,
            Answered::Miss,
        ),
    ];
    for (case, id, expected) in &after_one {
        assert_statement_address(&router, case, *id, expected).await;
    }

    // A second cell MOVES the space: the session mints the next id, so the address that was
    // the session-state refusal is now a statement of its own and the refusal moves up with
    // the session's own counter.
    let second_code = "print(6 * 7)";
    let second = run_cell(&router, second_code).await;
    assert_eq!(
        second,
        first + 1,
        "a session mints its statement ids counting up, got {second} after {first}"
    );
    let settled = statement_at_rest(&router, second).await;
    assert_eq!(
        settled.pointer("/Statement/State").and_then(Value::as_str),
        Some(STATE_AVAILABLE),
        "the second cell must run to its end too, got {settled}"
    );
    assert_eq!(
        output_text(&settled).as_deref(),
        Some("42"),
        "the second statement must report what its own code wrote, got {settled}"
    );

    let after_two: Vec<(String, i64, Answered)> = vec![
        (
            "the first id, still held".to_string(),
            first,
            Answered::Held { code: first_code },
        ),
        (
            "the id the second run minted".to_string(),
            second,
            Answered::Held { code: second_code },
        ),
        (
            "the id the session would mint next".to_string(),
            second + 1,
            Answered::IllegalSessionStatus,
        ),
        (
            "one past the moved id space".to_string(),
            second + 2,
            Answered::Miss,
        ),
        (
            "far above the moved id space".to_string(),
            second + 987_654,
            Answered::Miss,
        ),
    ];
    for (case, id, expected) in &after_two {
        assert_statement_address(&router, case, *id, expected).await;
    }

    // The delete takes the whole record, statements and all: the session id and every
    // statement address of it are then misses.
    let deleted = glue_ok(&router, "DeleteSession", &json!({ "Id": SESSION_ID })).await;
    assert_eq!(
        deleted.get("Id").and_then(Value::as_str),
        Some(SESSION_ID),
        "DeleteSession must report the id it deleted, got {deleted}"
    );
    for (operation, request, expected_message) in [
        (
            "GetSession",
            json!({ "Id": SESSION_ID }),
            format!("Session {SESSION_ID} not found."),
        ),
        (
            "GetStatement",
            json!({ "SessionId": SESSION_ID, "Id": first }),
            format!("Session ID {SESSION_ID} not found"),
        ),
    ] {
        let (status, body) = glue(&router, operation, &request).await;
        assert_eq!(
            status, 400,
            "case {operation}: a deleted session must be a miss, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {operation}: expected EntityNotFoundException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            expected_message,
            "case {operation}: unexpected wording for the miss, got {body}"
        );
    }
}
