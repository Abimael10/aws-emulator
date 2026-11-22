// SPDX-License-Identifier: CC0-1.0

//! `TerminateSession` as a STATE CHANGE this call performs: "permanently ends a session and
//! closes the data connection between the Session Manager client and SSM Agent on the managed
//! node", so the end happens once and a session that has already ended is in no state to be
//! ended again.
//!
//! The RULE under test: the first end of a live session is served 200 reporting the session it
//! ended and moves it into `History` with the instant it ended; every further end of that same
//! session is refused HTTP 400 `ValidationException` with the fixed sentence Session Manager
//! words this condition with, and leaves the ONE record the first end wrote exactly as it was —
//! the status it carries, the instant it ended at, and the single entry the history holds for
//! it. A caller therefore learns from the refusal that the session was already closed instead
//! of being told it closed one that nothing was open.
//!
//! Every call goes through the front door a client uses — `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router — and the sessions
//! are started against managed nodes an Amazon EC2 launch registered, through the helpers the
//! sibling session suites drive ([`crate::session_manager_sessions`]) rather than a second copy
//! of them.
//!
//! The rows differ along what the rule generalises over: whether the session names a `Session`
//! document, which registered node it connects to, whether the session was RESUMED before it
//! was ended, and how many ends follow the first — none of which changes the answer.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{create_profile, emulator, image_id, AL2023_PARAMETER};
use crate::parameter_store::{call_in_region, error_code, error_message, REGION};
use crate::session_manager_sessions::{
    create_session_document, registered_node, registering_profile, session_in, sessions,
    start_session, SESSION_DOCUMENT,
};

/// The sentence a session that is in no state for the change a request asks of it is refused
/// with (`crate::state::sessions::SESSION_NOT_IN_A_VALID_STATE`).
const NOT_IN_A_VALID_STATE: &str = "Session is not in a valid state";

/// How many ends follow the first for the rows that repeat it: more than one, because the rule
/// is about the STATE the session is in and not about the second call alone.
const FURTHER_ENDS: usize = 3;

/// Which of the two registered nodes a row's session connects to.
#[derive(Debug, Clone, Copy)]
enum Node {
    First,
    Second,
}

/// One row: how the session is started, whether it is resumed before it is ended, and how many
/// ends follow the first.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    node: Node,
    /// Whether the start names the account's own `Session` document.
    document: bool,
    /// Whether the session is reconnected through `ResumeSession` before it is ended.
    resumed: bool,
    further_ends: usize,
}

/// `TerminateSession` for `session_id`.
async fn terminate(router: &MultiServiceRouter, session_id: &str) -> (u16, Value) {
    call_in_region(
        router,
        REGION,
        "TerminateSession",
        &json!({ "SessionId": session_id }),
    )
    .await
}

/// RULE: the first end of a live session is served and every end after it is refused, with the
/// one history record the first end wrote left exactly as it was.
#[tokio::test]
async fn ending_a_session_that_has_already_ended_is_refused() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    create_session_document(&router).await;
    let first = registered_node(&router, "the first node", &image, &registering_profile()).await;
    let second = registered_node(&router, "the second node", &image, &registering_profile()).await;

    let rows = [
        Row {
            case: "a session that names no document",
            node: Node::First,
            document: false,
            resumed: false,
            further_ends: 1,
        },
        Row {
            case: "a session running the account's own document",
            node: Node::First,
            document: true,
            resumed: false,
            further_ends: FURTHER_ENDS,
        },
        Row {
            case: "a session of the other node, reconnected before it was ended",
            node: Node::Second,
            document: false,
            resumed: true,
            further_ends: FURTHER_ENDS,
        },
    ];

    for row in rows {
        let Row {
            case,
            node,
            document,
            resumed,
            further_ends,
        } = row;
        let target = match node {
            Node::First => &first,
            Node::Second => &second,
        };
        let mut request = json!({ "Target": target });
        if document {
            request["DocumentName"] = json!(SESSION_DOCUMENT);
        }
        let (status, started) = start_session(&router, REGION, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the session must be started first, got {status} with {started}"
        );
        let session_id = started["SessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a start reports a SessionId, got {started}"))
            .to_string();
        if resumed {
            let (status, body) = call_in_region(
                &router,
                REGION,
                "ResumeSession",
                &json!({ "SessionId": &session_id }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the reconnect must be served, got {status} with {body}"
            );
        }

        // The FIRST end is the one that happens: it reports the session it ended.
        let (status, ended) = terminate(&router, &session_id).await;
        assert_eq!(
            (status, ended["SessionId"].as_str()),
            (200, Some(session_id.as_str())),
            "case {case:?}: the first end must report the session it ended, got {status} with \
             {ended}"
        );
        let recorded = session_in(&router, REGION, "History", &session_id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the ended session must be in the history"));
        assert_eq!(
            recorded["Status"],
            json!("Terminated"),
            "case {case:?}: expected the ended session to be Terminated, got {recorded}"
        );
        assert!(
            !recorded["EndDate"].is_null(),
            "case {case:?}: expected the ended session to carry the instant it ended, got \
             {recorded}"
        );

        // Every end after it is refused, and none of them touches the record.
        for attempt in 1..=further_ends {
            let (status, refused) = terminate(&router, &session_id).await;
            assert_eq!(
                (
                    status,
                    error_code(&refused).as_str(),
                    error_message(&refused).as_str(),
                ),
                (400, "ValidationException", NOT_IN_A_VALID_STATE),
                "case {case:?}: end {attempt} of {further_ends} after the first must be \
                 refused, got {status} with {refused}"
            );
            let again = session_in(&router, REGION, "History", &session_id)
                .await
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the refused end must leave the history record")
                });
            assert_eq!(
                again, recorded,
                "case {case:?}: expected the refused end to change nothing of the record, got \
                 {again}"
            );
        }

        // Exactly ONE record of it, and it is no longer active.
        let history = sessions(&router, REGION, "History", json!([])).await;
        let mine = history
            .iter()
            .filter(|session| session["SessionId"].as_str() == Some(session_id.as_str()))
            .count();
        assert_eq!(
            mine, 1,
            "case {case:?}: expected exactly one history record of {session_id}, got {mine} in \
             {history:?}"
        );
        assert!(
            session_in(&router, REGION, "Active", &session_id)
                .await
                .is_none(),
            "case {case:?}: an ended session is no longer active"
        );
    }
}

/// RULE, the other half: an id this endpoint holds no session under ends nothing, so it is
/// answered rather than refused and the answer names no session — the refusal above is about
/// the STATE a session this endpoint holds is in, not about an id it resolves nothing for.
#[tokio::test]
async fn an_id_this_endpoint_holds_no_session_under_ends_nothing() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let node = registered_node(&router, "the node", &image, &registering_profile()).await;
    let (status, started) = start_session(&router, REGION, &json!({ "Target": node })).await;
    assert_eq!(
        status, 200,
        "the live session must be started first, got {status} with {started}"
    );
    let live = started["SessionId"].as_str().unwrap_or_default().to_string();

    // (case, the id the end names)
    for (case, named) in [
        ("a value no session was started under", "never-started-here"),
        ("the shortest id the model admits", "s"),
        ("an id of the form this service mints", "emulator-not-a-session"),
    ] {
        let (status, body) = terminate(&router, named).await;
        assert_eq!(
            (status, body["SessionId"].as_str()),
            (200, None),
            "case {case:?}: expected the end to name no session, got {status} with {body}"
        );
    }

    // The live session is untouched by any of it, and its own end is still served.
    assert!(
        session_in(&router, REGION, "Active", &live).await.is_some(),
        "expected the live session to still be active"
    );
    let (status, ended) = terminate(&router, &live).await;
    assert_eq!(
        (status, ended["SessionId"].as_str()),
        (200, Some(live.as_str())),
        "expected the live session's own end to be served, got {status} with {ended}"
    );
}
