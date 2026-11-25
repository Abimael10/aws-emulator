// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the character constraint this service applies to the `SessionId` a
//! request names, driven over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`).
//!
//! THE RULE UNDER TEST: a `SessionId` holding a character the session-id charset does not
//! admit is refused as the constraint it broke — HTTP 400 `InvalidRequestException` of the
//! class a refusal the caller's own input caused carries, stating AWS's validation wording for
//! that member and expression — and it is refused BEFORE the id is resolved against anything
//! the account holds, so the refusal is the same for every operation that takes the member and
//! is not the authorization refusal a WELL-FORMED id the account holds no session under keeps.
//!
//! The rows differ along what the rule generalises over: which character of the value sits
//! outside the charset (a space, punctuation, an upper-case letter, a dot, a slash, a
//! non-ASCII letter), the length of the value, which operation carried the member, and —
//! for the admitted half — which form inside the charset the id takes.
//!
//! Nothing here waits for a fixed span: the one test that needs a transition the clock owes
//! POLLS for the state it is waiting on, under a deadline far above the span that transition
//! is due after.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    ERROR_CODE_INVALID_INPUT, ERROR_CODE_UNAUTHORIZED, PYSPARK_ENGINE_VERSION, SESSION_NOT_FOUND,
    SESSION_STATE_IDLE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, router};

/// The Spark enabled workgroup the session below is started in.
const WORK_GROUP: &str = "spark-session-id-constraint";

/// The sentence a `SessionId` outside the charset is refused with, written out here rather
/// than read off the implementation so a message that lost either half fails the row: AWS's
/// validation envelope over one clause naming the member and quoting the expression the
/// service publishes for it, with the rejected value nowhere in it.
const EXPECTED_CONSTRAINT_MESSAGE: &str = "1 validation error detected: Value at 'sessionId' \
     failed to satisfy constraint: Member must satisfy regular expression pattern: \
     ^[0-9a-z-_]+$";

/// How long a poll for a transition the clock owes keeps looking, and how long it waits
/// between looks. The deadline is far above the span a session's resources are due after, so
/// the CONDITION is what is waited on rather than a span of the host's own speed.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLL_LOOKS: u32 = 1_200;

/// The values outside the session-id charset every operation below is driven with: each row
/// names what sits outside it, and the rows span whitespace, punctuation, case, a path
/// separator, a non-ASCII letter and a value at the modelled length bound.
fn malformed_ids() -> Vec<(&'static str, String)> {
    vec![
        (
            "spaces and punctuation",
            "!! not-a-session-id ??".to_string(),
        ),
        ("a single space", " ".to_string()),
        ("an upper-case letter", "Session-1".to_string()),
        ("a dot", "session.1".to_string()),
        ("a path separator", "sessions/1".to_string()),
        ("a non-ASCII letter", "sessión".to_string()),
        (
            "the longest id the model admits, one character outside the charset",
            format!("{}!", "a".repeat(255)),
        ),
    ]
}

/// The values INSIDE the charset the admitted rows are driven with: each is a well-formed id,
/// and the rows span the forms the charset allows and the modelled length bounds.
fn well_formed_ids() -> Vec<(&'static str, String)> {
    vec![
        (
            "a UUID nothing minted",
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        ("a single character", "s".to_string()),
        ("digits only", "0123456789".to_string()),
        ("underscores and hyphens", "a_b-c_d".to_string()),
        ("the longest id the model admits", "s".repeat(256)),
    ]
}

/// One operation that takes a `SessionId`, as this suite drives it: the request members
/// beside the id, and how the id is placed into the request body.
struct SessionAddressed {
    /// The operation's name, which is also the case name of every row it is driven in.
    operation: &'static str,
    /// The members the operation requires beside the session id.
    beside: Value,
}

/// Every operation of this service whose input carries a `SessionId` member, with the members
/// each requires beside it — the whole surface the rule holds over.
///
/// `UpdateNotebook` is the one whose member is optional ("The active notebook session ID.
/// Required if the notebook has an active session"), and it is driven with a notebook this
/// account holds so that nothing but the session id can be what the refusal is about.
fn session_addressed(notebook_id: &str) -> Vec<SessionAddressed> {
    vec![
        SessionAddressed {
            operation: "GetSessionEndpoint",
            beside: json!({}),
        },
        SessionAddressed {
            operation: "GetSession",
            beside: json!({}),
        },
        SessionAddressed {
            operation: "GetSessionStatus",
            beside: json!({}),
        },
        SessionAddressed {
            operation: "TerminateSession",
            beside: json!({}),
        },
        SessionAddressed {
            operation: "ListExecutors",
            beside: json!({}),
        },
        SessionAddressed {
            operation: "ListCalculationExecutions",
            beside: json!({}),
        },
        SessionAddressed {
            operation: "CreatePresignedNotebookUrl",
            beside: json!({}),
        },
        SessionAddressed {
            operation: "StartCalculationExecution",
            beside: json!({ "CodeBlock": "print(1)" }),
        },
        SessionAddressed {
            operation: "UpdateNotebook",
            beside: json!({
                "NotebookId": notebook_id,
                "Payload": "{\"cells\":[]}",
                "Type": "IPYNB"
            }),
        },
    ]
}

/// The request body `addressed` is driven with for the session id `session_id`.
fn request(addressed: &SessionAddressed, session_id: &str) -> Value {
    let mut body = addressed.beside.clone();
    body["SessionId"] = json!(session_id);
    body
}

/// Create the Spark enabled workgroup the sessions below run in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution",
                "ResultConfiguration": { "OutputLocation": "s3://athena-results/sessions/" }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup must be created, got {status} with {created:?}"
    );
}

/// Create one notebook in [`WORK_GROUP`] and hand back its `NotebookId`.
async fn given_notebook(router: &MultiServiceRouter) -> String {
    let (status, created) = call(
        router,
        "CreateNotebook",
        &json!({ "WorkGroup": WORK_GROUP, "Name": "constraint.ipynb" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook must be created, got {status} with {created:?}"
    );
    created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNotebook must answer with an id, got {created:?}"))
        .to_string()
}

/// RULE: a session id outside the charset is refused as the constraint it broke, by every
/// operation that takes the member, whatever character of the value sits outside it.
#[tokio::test]
async fn a_session_id_outside_the_charset_is_refused_as_the_constraint_it_broke() {
    let router = router();
    given_spark_work_group(&router).await;
    let notebook_id = given_notebook(&router).await;

    for addressed in session_addressed(&notebook_id) {
        for (outside, id) in malformed_ids() {
            let case = format!("{} with {outside}", addressed.operation);
            let (status, refused) =
                call(&router, addressed.operation, &request(&addressed, &id)).await;
            // The sentence asserted here names the member and the requirement and never the
            // bytes the request carried, so a client cannot read the refused value back out
            // of a refusal either.
            assert_refusal(
                &case,
                status,
                &refused,
                ERROR_CODE_INVALID_INPUT,
                EXPECTED_CONSTRAINT_MESSAGE,
            );
        }
    }
}

/// RULE: a WELL-FORMED id the account holds no session under is a different class — it keeps
/// the authorization refusal every session-addressed call of this service states for that miss
/// — so a client can tell an argument it must repair from a session it may not address.
#[tokio::test]
async fn a_well_formed_id_no_session_was_issued_under_keeps_the_authorization_refusal() {
    let router = router();
    given_spark_work_group(&router).await;
    let notebook_id = given_notebook(&router).await;

    for addressed in session_addressed(&notebook_id) {
        for (form, id) in well_formed_ids() {
            let case = format!("{} with {form}", addressed.operation);
            let (status, refused) =
                call(&router, addressed.operation, &request(&addressed, &id)).await;
            assert_refusal(
                &case,
                status,
                &refused,
                ERROR_CODE_UNAUTHORIZED,
                SESSION_NOT_FOUND,
            );
        }
    }

    // The two classes read differently, which is the whole point of measuring the charset
    // before resolving the id.
    assert_ne!(
        SESSION_NOT_FOUND, EXPECTED_CONSTRAINT_MESSAGE,
        "the unknown-session and the malformed-id refusals must not read alike"
    );
}

/// RULE: the constraint admits every id this service issues, so a session a start really
/// minted is served its connection endpoint — the charset gate refuses input, never the
/// account's own sessions.
#[tokio::test]
async fn the_session_a_start_issued_is_admitted_and_served() {
    let router = router();
    given_spark_work_group(&router).await;

    let (status, started) = call(
        &router,
        "StartSession",
        &json!({
            "WorkGroup": WORK_GROUP,
            "EngineConfiguration": { "MaxConcurrentDpus": 2 },
            "SessionIdleTimeoutInMinutes": 60
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartSession must be accepted, got {status} with {started:?}"
    );
    let session_id = started["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSession must answer with a SessionId, got {started:?}"))
        .to_string();

    // The id a start issues is inside the charset, so it is not refused as malformed at any
    // point of the session's lifecycle: while the resources are still being acquired the call
    // is refused for the STATE, in the sentence that class is worded in.
    let (status, refused) = call(
        &router,
        "GetSessionEndpoint",
        &json!({ "SessionId": session_id }),
    )
    .await;
    if status != 200 {
        assert_ne!(
            crate::harness::message(&refused),
            EXPECTED_CONSTRAINT_MESSAGE,
            "an id this service issued must never be refused as malformed, got {refused:?}"
        );
    }

    // Once the resources are up — waited on by POLLING for the state rather than by sleeping
    // out the span it is due after — the grant is served.
    for look in 0..=POLL_LOOKS {
        let (status, body) = call(
            &router,
            "GetSessionStatus",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "GetSessionStatus must answer 200 while polling, got {status} with {body:?}"
        );
        if body["Status"]["State"].as_str() == Some(SESSION_STATE_IDLE) {
            break;
        }
        assert!(
            look < POLL_LOOKS,
            "the session {session_id} never reached {SESSION_STATE_IDLE}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }

    let (status, granted) = call(
        &router,
        "GetSessionEndpoint",
        &json!({ "SessionId": session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the session a start issued must be served, got {status} with {granted:?}"
    );
    assert!(
        granted["EndpointUrl"].as_str().is_some_and(|url| url.ends_with(&session_id)),
        "the grant must name the session, got {granted:?}"
    );
}
