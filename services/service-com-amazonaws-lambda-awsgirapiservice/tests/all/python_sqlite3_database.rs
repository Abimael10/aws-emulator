// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DATABASE module of the standard library a `python3.x` function's
//! run is given: `CreateFunction` then `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a `python3.x` runtime presents the standard library of the runtime it
//! advertises, so `import sqlite3` imports cleanly and every statement the handler then runs is
//! COMPILED AND RUN by a real SQLite engine — what a query answers is what the handler's own
//! writes put in the database, a statement the engine refuses raises the DB-API exception class
//! the standard library maps that failure to, and a transaction the handler rolls back leaves
//! nothing behind.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: which storage class a value is stored and read back in, which
//! parameter style binds it, whether the answer is stored or COMPUTED by the engine, how a
//! transaction ends, which failure the engine reports, which database the connection names and
//! which surface of the module the program reads.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Nothing here is about a
/// deadline, so it is given generous room: how long the standard library's own import work
/// takes on a loaded host is not what any row asserts.
const FUNCTION_TIMEOUT_SECONDS: i32 = 600;

/// How many times a case re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// Create a `python3.12` function whose package holds `source` as `lambda_function.py`, and
/// wait out the provisioning window the create documents.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", source)])),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// `Invoke` on `name` with `event`, re-driven while the answer reports a bound this whole test
/// binary shares rather than a run.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    let mut answered = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        answered = invoke_once(router, name, event).await;
    }
    answered
}

/// One `Invoke` request on the function `name` names.
async fn invoke_once(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(event).expect("the event serializes"),
    )
    .await
}

/// What one case of the probe answered, or `None` when the run was ended by a bound this whole
/// test binary shares rather than by anything the case asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
) -> Option<Value> {
    let answer = invoke(router, name, event).await;
    if is_host_bound(answer.status, &answer.text()) {
        return None;
    }
    assert_eq!(
        answer.status,
        200,
        "case {case}: expected 200, got {} {}",
        answer.status,
        answer.text()
    );
    let failure = answer.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {case}: the handler's module imports cleanly and the handler catches each refusal \
         itself, so the invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END: ONE real `CreateFunction` whose handler module BODY
/// imports `sqlite3` — which is where the recorded symptom failed, an absent module failing the
/// whole package's import — and real `Invoke` requests over the router, which the tables below
/// read.
///
/// One test, one function and one stream of invocations, the way the sibling runtime suites are
/// built: the memory bound a run is stopped on is read from the WHOLE process
/// (`state::execution::python`) and this binary runs its suites in parallel, so fewer
/// interpreters in flight at once is fewer runs stopped for memory belonging to another suite.
/// A case whose run was stopped by one of those shared bounds says nothing about the rule and is
/// left out of the reading — but not every one of them can be: each table answers how many of
/// its cases a RUN answered, and a test that read none of them at all is a test that asserted
/// nothing, so it fails rather than passing silently.
#[tokio::test]
async fn the_python_runtime_runs_a_handlers_statements_on_a_real_database() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let probe = "probe-sqlite3-database";
    create_function(&router, probe, PROBE_SOURCE).await;
    let mut read = the_module_states_its_own_surface(&router, probe).await;
    read += every_storage_class_round_trips_through_the_engine(&router, probe).await;
    read += a_query_is_computed_by_the_engine(&router, probe).await;
    read += a_transaction_holds_or_discards_what_it_carried(&router, probe).await;
    read += a_statement_the_engine_refuses_raises_its_own_class(&router, probe).await;
    read += a_named_database_is_the_environments_own(&router, probe).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the rule \
         was read"
    );
}

/// The module's own surface: the DB-API constants a program reads off it, and the version of the
/// engine its statements run on.
///
/// Source of each expected value: the DB-API 2.0 specification's module globals, as the standard
/// library's `sqlite3` publishes them (`apilevel`, `threadsafety`, `paramstyle`), and the
/// engine's own version, which is read as a dotted release number rather than pinned to one
/// build.
///
/// Answers how many cases a run answered.
async fn the_module_states_its_own_surface(router: &MultiServiceRouter, probe: &str) -> usize {
    let case = "the module's own surface";
    let Some(document) = answered(router, probe, case, &json!({"case": "surface"})).await else {
        return 0;
    };
    for (member, expected) in [
        ("apilevel", json!("2.0")),
        ("paramstyle", json!("qmark")),
        ("threadsafety", json!(1)),
        ("imported", json!(true)),
    ] {
        let actual = document.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case} {member}: expected {expected}, got {actual}"
        );
    }
    let version = document
        .get("sqlite_version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let parts: Vec<&str> = version.split('.').collect();
    assert!(
        parts.len() >= 3 && parts.iter().all(|part| part.chars().all(char::is_numeric)),
        "case {case} sqlite_version: expected the engine's dotted release number, got \
         {version:?}"
    );
    let info = document.get("sqlite_version_info").cloned().unwrap_or_default();
    assert_eq!(
        info,
        json!(parts
            .iter()
            .map(|part| part.parse::<i64>().unwrap_or_default())
            .collect::<Vec<i64>>()),
        "case {case} sqlite_version_info: expected the same release as numbers, got {info}"
    );
    1
}

/// A value of each STORAGE CLASS the engine has, written by the handler and read back: the
/// round trip is the invariant, so the rows differ along the class of the value, the parameter
/// style that bound it and the shape the row is answered in.
///
/// Answers how many cases a run answered.
async fn every_storage_class_round_trips_through_the_engine(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    // (the case, the value the handler stores, what a read answers for it)
    let cases: &[(&str, Value, Value)] = &[
        ("an integer", json!(42), json!(42)),
        ("a negative integer", json!(-7), json!(-7)),
        ("a large integer", json!(9_007_199_254_740_993i64), json!(9_007_199_254_740_993i64)),
        ("a real", json!(1.5), json!(1.5)),
        ("text", json!("rows"), json!("rows")),
        ("text with a quote in it", json!("O'Hara"), json!("O'Hara")),
        ("the empty text", json!(""), json!("")),
        ("null", Value::Null, Value::Null),
        // A blob travels to the handler as the base64 of its bytes and is answered as the hex
        // of the bytes the engine gave back, so the row reads the BYTES rather than text.
        ("a blob", json!({"blob": "AP8B"}), json!("00ff01")),
    ];
    let mut read = 0;
    for style in ["qmark", "named"] {
        for (case, stored, expected) in cases {
            let case = format!("{case} bound by the {style} parameter style");
            let event = json!({"case": "round_trip", "style": style, "value": stored});
            let Some(document) = answered(router, probe, &case, &event).await else {
                continue;
            };
            read += 1;
            let actual = document.get("read").cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, *expected,
                "case {case}: expected the value the handler stored, got {actual} in {document}"
            );
            let rowcount = document.get("rowcount").cloned().unwrap_or_default();
            assert_eq!(
                rowcount,
                json!(1),
                "case {case}: an insert of one row reports one row changed, got {rowcount}"
            );
            let lastrowid = document.get("lastrowid").and_then(Value::as_i64);
            assert_eq!(
                lastrowid,
                Some(1),
                "case {case}: the insert's own row id is reported, got {lastrowid:?}"
            );
            // The same row, read through the row factory the module publishes: by name.
            let by_name = document.get("by_name").cloned().unwrap_or(Value::Null);
            assert_eq!(
                by_name, *expected,
                "case {case}: the same value is read by column name off a Row, got {by_name}"
            );
        }
    }
    read
}

/// What the ENGINE computes, which is the half of the rule no stored value can answer: an
/// aggregate over the rows the handler wrote, a grouping of them, and an expression evaluated
/// over them.
///
/// Answers how many cases a run answered.
async fn a_query_is_computed_by_the_engine(router: &MultiServiceRouter, probe: &str) -> usize {
    let case = "a computed query";
    let Some(document) = answered(router, probe, case, &json!({"case": "computed"})).await else {
        return 0;
    };
    // The handler wrote (1, 'a', 10), (2, 'b', 20), (3, 'a', 12): every expected value below is
    // computed from those three rows by the engine.
    for (member, expected) in [
        ("count", json!(3)),
        ("total", json!(42)),
        ("highest", json!(20)),
        ("average", json!(14.0)),
        ("grouped", json!([["a", 2, 22], ["b", 1, 20]])),
        ("ordered", json!([20, 12, 10])),
        ("computed", json!([11, 13, 21])),
        ("matched", json!(["a", "a"])),
    ] {
        let actual = document.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case} {member}: expected {expected}, got {actual} in {document}"
        );
    }
    1
}

/// A TRANSACTION: what a commit holds and what a rollback discards, and what the connection
/// reports about the one it has open. The rows differ along how the transaction ended.
///
/// Answers how many cases a run answered.
async fn a_transaction_holds_or_discards_what_it_carried(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    // (how the transaction ended, the rows that survive it)
    let cases: &[(&str, Value)] = &[
        ("commit", json!(["held", "kept"])),
        ("rollback", json!(["held"])),
        ("context", json!(["held", "kept"])),
    ];
    let mut read = 0;
    for (ending, expected) in cases {
        let case = format!("a transaction that ended with {ending}");
        let event = json!({"case": "transaction", "ending": ending});
        let Some(document) = answered(router, probe, &case, &event).await else {
            continue;
        };
        read += 1;
        let rows = document.get("rows").cloned().unwrap_or(Value::Null);
        assert_eq!(
            rows, *expected,
            "case {case}: expected the rows the ending leaves, got {rows} in {document}"
        );
        // A connection with a statement of its own open reports it, and one that has ended its
        // transaction reports none.
        for (member, expected) in [("inside", json!(true)), ("afterwards", json!(false))] {
            let actual = document.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case} {member}: expected {expected}, got {actual} in {document}"
            );
        }
    }
    read
}

/// A statement the ENGINE refuses, raised as the DB-API class the standard library maps that
/// failure to and carrying the engine's own sentence. The rows differ along which failure the
/// statement provokes.
///
/// Source of each expected class: the standard library's own mapping of SQLite result codes to
/// DB-API exceptions — a constraint violation is an `IntegrityError`, a statement the engine
/// cannot compile or resolve is an `OperationalError`, and a call that supplies the wrong
/// bindings is a `ProgrammingError`.
///
/// Answers how many cases a run answered.
async fn a_statement_the_engine_refuses_raises_its_own_class(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    // (the case, the class the module raises, the text the message holds)
    let cases: &[(&str, &str, &str)] = &[
        ("a unique constraint", "IntegrityError", "UNIQUE constraint failed"),
        ("a not-null constraint", "IntegrityError", "NOT NULL constraint failed"),
        ("a table that is not there", "OperationalError", "no such table"),
        ("a column that is not there", "OperationalError", "no such column"),
        ("text that is not a statement", "OperationalError", "syntax error"),
        (
            "a statement given too few bindings",
            "ProgrammingError",
            "Incorrect number of bindings supplied",
        ),
        (
            "a named binding that was not supplied",
            "ProgrammingError",
            "You did not supply a value for binding parameter",
        ),
        (
            "a function this engine does not run",
            "OperationalError",
            "not authorized",
        ),
    ];
    let mut read = 0;
    for (case, class, carries) in cases {
        let event = json!({"case": "refused", "which": case});
        let Some(document) = answered(router, probe, case, &event).await else {
            continue;
        };
        read += 1;
        let raised = document
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            raised, *class,
            "case {case}: expected the statement to raise {class}, got {raised:?} in {document}"
        );
        let message = document
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains(carries),
            "case {case}: expected the message to carry {carries:?}, got {message:?}"
        );
        // Every one of these is an `sqlite3.Error`, which is what a handler catches.
        let is_error = document.get("is_error").cloned().unwrap_or(Value::Null);
        assert_eq!(
            is_error,
            json!(true),
            "case {case}: every one of these is an sqlite3.Error, got {is_error}"
        );
    }
    read
}

/// WHICH database a connection names: the private in-memory one is this connection's own, and
/// a database named by a path is a FILE of the function's ephemeral storage — it holds the
/// database itself, it outlives every connection opened on it, and removing it is the database
/// gone.
///
/// Answers how many cases a run answered.
async fn a_named_database_is_the_environments_own(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "which database a connection names";
    let Some(document) = answered(router, probe, case, &json!({"case": "databases"})).await else {
        return 0;
    };
    for (member, expected) in [
        // What one connection wrote is read back by another opened after the first one closed,
        // which is what a database named by a path has to survive.
        ("shared", json!(["kept"])),
        // ... and the path is a file of the function's own ephemeral storage, holding the
        // database's own bytes.
        ("exists", json!(true)),
        ("sized", json!(true)),
        // A file the handler removed is the database gone: the next connection to that path
        // opens a fresh one.
        ("fresh", json!("no such table: held")),
        // ... and a private in-memory database is the connection's own, so the table the
        // named database holds is not there for it.
        ("private", json!("no such table: held")),
        // A database named anywhere but the function's own WRITABLE storage is one the engine
        // cannot open, whichever way the name reaches outside it — a read-only directory of
        // the environment, a relative name, or a path that climbs out.
        (
            "refused",
            json!({
                "/var/task/held.db": "unable to open database file",
                "/var/lang/held.db": "unable to open database file",
                "held.db": "unable to open database file",
                "../held.db": "unable to open database file",
                "/tmp/../var/task/held.db": "unable to open database file",
            }),
        ),
    ] {
        let actual = document.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case} {member}: expected {expected}, got {actual} in {document}"
        );
    }
    1
}

/// The probe every table drives: its module BODY imports `sqlite3` — the recorded symptom is
/// that import failing — and its handler answers the surface the event names.
const PROBE_SOURCE: &str = r#"
import base64
import os
import sqlite3

# The module above was imported by this module's BODY, so a handler that runs at all is a clean
# import of it.
_IMPORTED = True


def _value(given):
    """The value an event names, as the Python object the handler stores."""
    if isinstance(given, dict) and "blob" in given:
        return base64.b64decode(given["blob"])
    return given


def _read(value):
    """One answered value, as JSON carries it: bytes as their hexadecimal."""
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value).hex()
    return value


def _surface(event):
    return {
        "imported": _IMPORTED,
        "apilevel": sqlite3.apilevel,
        "paramstyle": sqlite3.paramstyle,
        "threadsafety": sqlite3.threadsafety,
        "sqlite_version": sqlite3.sqlite_version,
        "sqlite_version_info": list(sqlite3.sqlite_version_info),
    }


def _round_trip(event):
    value = _value(event.get("value"))
    connection = sqlite3.connect(":memory:")
    try:
        connection.execute("CREATE TABLE held (id INTEGER PRIMARY KEY, value)")
        if event.get("style") == "named":
            cursor = connection.execute(
                "INSERT INTO held (value) VALUES (:value)", {"value": value}
            )
        else:
            cursor = connection.execute("INSERT INTO held (value) VALUES (?)", (value,))
        connection.commit()
        read = connection.execute("SELECT value FROM held WHERE id = ?", (1,)).fetchone()[0]
        connection.row_factory = sqlite3.Row
        row = connection.execute("SELECT value FROM held").fetchone()
        return {
            "read": _read(read),
            "by_name": _read(row["value"]),
            "keys": row.keys(),
            "rowcount": cursor.rowcount,
            "lastrowid": cursor.lastrowid,
        }
    finally:
        connection.close()


def _computed(event):
    connection = sqlite3.connect(":memory:")
    try:
        connection.execute("CREATE TABLE totals (id INTEGER, label TEXT, amount INTEGER)")
        connection.executemany(
            "INSERT INTO totals (id, label, amount) VALUES (?, ?, ?)",
            [(1, "a", 10), (2, "b", 20), (3, "a", 12)],
        )
        connection.commit()
        count, total, highest, average = connection.execute(
            "SELECT COUNT(*), SUM(amount), MAX(amount), AVG(amount) FROM totals"
        ).fetchone()
        grouped = [
            list(row)
            for row in connection.execute(
                "SELECT label, COUNT(*), SUM(amount) FROM totals GROUP BY label ORDER BY label"
            ).fetchall()
        ]
        ordered = [
            row[0]
            for row in connection.execute(
                "SELECT amount FROM totals ORDER BY amount DESC"
            ).fetchall()
        ]
        computed = [
            row[0]
            for row in connection.execute(
                "SELECT amount + 1 AS raised FROM totals ORDER BY raised"
            ).fetchall()
        ]
        matched = [
            row[0]
            for row in connection.execute(
                "SELECT label FROM totals WHERE amount < ? ORDER BY id", (15,)
            ).fetchall()
        ]
        return {
            "count": count,
            "total": total,
            "highest": highest,
            "average": average,
            "grouped": grouped,
            "ordered": ordered,
            "computed": computed,
            "matched": matched,
        }
    finally:
        connection.close()


def _transaction(event):
    ending = event.get("ending")
    connection = sqlite3.connect(":memory:")
    try:
        connection.execute("CREATE TABLE held (label TEXT)")
        connection.execute("INSERT INTO held (label) VALUES ('held')")
        connection.commit()
        if ending == "context":
            with connection:
                connection.execute("INSERT INTO held (label) VALUES ('kept')")
                inside = connection.in_transaction
        else:
            connection.execute("INSERT INTO held (label) VALUES ('kept')")
            inside = connection.in_transaction
            if ending == "commit":
                connection.commit()
            else:
                connection.rollback()
        rows = [
            row[0]
            for row in connection.execute("SELECT label FROM held ORDER BY rowid").fetchall()
        ]
        return {
            "rows": rows,
            "inside": inside,
            "afterwards": connection.in_transaction,
        }
    finally:
        connection.close()


def _refused(event):
    which = event.get("which")
    connection = sqlite3.connect(":memory:")
    try:
        connection.execute("CREATE TABLE held (id INTEGER PRIMARY KEY, label TEXT NOT NULL UNIQUE)")
        connection.execute("INSERT INTO held (label) VALUES ('one')")
        connection.commit()
        try:
            if which == "a unique constraint":
                connection.execute("INSERT INTO held (label) VALUES ('one')")
            elif which == "a not-null constraint":
                connection.execute("INSERT INTO held (label) VALUES (NULL)")
            elif which == "a table that is not there":
                connection.execute("SELECT * FROM absent")
            elif which == "a column that is not there":
                connection.execute("SELECT absent FROM held")
            elif which == "text that is not a statement":
                connection.execute("SELEC 1")
            elif which == "a statement given too few bindings":
                connection.execute("SELECT * FROM held WHERE id = ? AND label = ?", (1,))
            elif which == "a named binding that was not supplied":
                connection.execute("SELECT * FROM held WHERE id = :id", {"other": 1})
            elif which == "a function this engine does not run":
                connection.execute("SELECT load_extension('held')")
            else:
                raise AssertionError("the event named no refusal: %r" % (which,))
        except BaseException as error:
            return {
                "class": type(error).__name__,
                "message": str(error),
                "is_error": isinstance(error, sqlite3.Error),
            }
        return {"class": "", "message": "", "is_error": False}
    finally:
        connection.close()


def _databases(event):
    path = "/tmp/probe.db"
    if os.path.exists(path):
        os.remove(path)
    first = sqlite3.connect(path)
    try:
        first.execute("CREATE TABLE held (label TEXT)")
        first.execute("INSERT INTO held (label) VALUES ('kept')")
        first.commit()
        sized = os.path.getsize(path) > 0
    finally:
        # Every connection to the database is closed before it is opened again, which is what
        # a database named by a path has to survive.
        first.close()
    second = sqlite3.connect(path)
    try:
        shared = [row[0] for row in second.execute("SELECT label FROM held").fetchall()]
    finally:
        second.close()
    private = sqlite3.connect(":memory:")
    try:
        private.execute("SELECT label FROM held")
        held = ""
    except sqlite3.OperationalError as error:
        held = str(error)
    finally:
        private.close()
    os.remove(path)
    third = sqlite3.connect(path)
    try:
        third.execute("SELECT label FROM held")
        fresh = ""
    except sqlite3.OperationalError as error:
        fresh = str(error)
    finally:
        third.close()
    return {
        "shared": shared,
        "exists": os.path.exists(path),
        "sized": sized,
        "private": held,
        "fresh": fresh,
        "refused": _refused_names(),
    }


def _refused_names():
    """What a database named outside the function's own writable storage answers."""
    answers = {}
    for named in (
        "/var/task/held.db",
        "/var/lang/held.db",
        "held.db",
        "../held.db",
        "/tmp/../var/task/held.db",
    ):
        try:
            sqlite3.connect(named).close()
            answers[named] = ""
        except sqlite3.OperationalError as error:
            answers[named] = str(error)
    return answers


_CASES = {
    "surface": _surface,
    "round_trip": _round_trip,
    "computed": _computed,
    "transaction": _transaction,
    "refused": _refused,
    "databases": _databases,
}


def handler(event, context):
    case = (event or {}).get("case")
    answer = _CASES.get(case)
    if answer is None:
        raise AssertionError("the event named no case: %r" % (case,))
    return answer(event or {})
"#;
