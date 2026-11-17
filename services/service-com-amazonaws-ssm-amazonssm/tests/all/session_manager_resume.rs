// SPDX-License-Identifier: CC0-1.0

//! Reconnecting a Session Manager session end to end: what `ResumeSession` serves, what it
//! refuses, and that a reconnect continues the session it was asked for.
//!
//! RULE under test: `ResumeSession` "reconnects a session to a managed node after it has
//! been disconnected", so it is served for every `SessionId` this endpoint holds a session
//! under that has NOT ended — handing the caller that same session's id, a fresh token and a
//! URL back to this emulator, and leaving the session itself as it was: same id, same start
//! instant, connected, and still the only session the endpoint holds for it. "Connections
//! can be resumed for disconnected sessions, but not terminated sessions", so every
//! `SessionId` with no live session behind it is refused `ValidationException` and nothing
//! is written.
//!
//! The rows differ along what the rule generalises over: for a served reconnect, how the
//! session was started (with no document, with the account's own session document, with a
//! reason) and how many times the client reconnects; for a refused one, the FORM of the id
//! that names no live session (a value nothing was started under, the shortest and the
//! longest id the model allows, an id of the form this service mints, the id of a session
//! that has been terminated, and the id of a live session asked of another Region's
//! endpoint).
//!
//! Every call goes through the wire a client uses, and the managed node every session is
//! started against is one an Amazon EC2 launch registered — the helpers the sibling session
//! suite drives, rather than a second copy of them.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{create_profile, emulator, image_id, AL2023_PARAMETER};
use crate::parameter_store::{call_in_region, error_code, error_message, REGION};
use crate::session_manager_sessions::{
    create_session_document, registered_node, registering_profile, session_in, sessions,
    start_session, OTHER_REGION, SESSION_DOCUMENT,
};

/// The modelled `SessionId` length (1..96), which is what the shortest and the longest id a
/// refused row can carry are drawn from.
const MIN_SESSION_ID_CHARS: usize = 1;

/// See [`MIN_SESSION_ID_CHARS`].
const MAX_SESSION_ID_CHARS: usize = 96;

/// The largest a `TokenValue` may be (the modelled length 0..300).
const MAX_TOKEN_VALUE_CHARS: usize = 300;

/// Reconnect one session and answer the status and body, whatever it was.
async fn resume_session(
    router: &MultiServiceRouter,
    region: &str,
    session_id: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "ResumeSession",
        &json!({ "SessionId": session_id }),
    )
    .await
}

/// Start one session against `node` the way `row` says, as the id it was recorded under.
async fn started(
    router: &MultiServiceRouter,
    row: &Served,
    node: &str,
    document_arn: &str,
) -> String {
    let mut request = json!({ "Target": node });
    match row.document {
        Document::None => {}
        Document::ByName => request["DocumentName"] = json!(SESSION_DOCUMENT),
        Document::ByArn => request["DocumentName"] = json!(document_arn),
    }
    if let Some(reason) = row.reason {
        request["Reason"] = json!(reason);
    }
    let (status, body) = start_session(router, REGION, &request).await;
    assert_eq!(
        status, 200,
        "case {:?}: StartSession must be served, got {status} with {body}",
        row.case
    );
    body["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {:?}: a start reports a SessionId, got {body}", row.case))
        .to_string()
}

/// How a served row names the document the session it reconnects runs.
#[derive(Debug, Clone, Copy)]
enum Document {
    /// The request names none: "a shell to the managed node is launched by default".
    None,
    /// `DocumentName`, carrying the name the account's own document is held under.
    ByName,
    /// `DocumentName`, carrying the ARN that same document is addressable by.
    ByArn,
}

/// One served row: how the session was started, and how often its client reconnects.
#[derive(Debug, Clone, Copy)]
struct Served {
    case: &'static str,
    document: Document,
    /// The `Reason` the start carried, when it carried one.
    reason: Option<&'static str>,
    /// How many times the client reconnects — "automatically reconnect during intermittent
    /// network issues" is not once only.
    reconnects: usize,
}

/// A reconnect of a session this endpoint holds is served, hands the caller that session and
/// a connection to it, and leaves the session itself as it was.
#[tokio::test]
async fn a_live_session_is_reconnected_as_often_as_its_client_asks() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let document_arn = create_session_document(&router).await;
    let node = registered_node(&router, "a registered node", &image, &registering_profile()).await;

    let rows = [
        Served {
            case: "a session started with no document, reconnected once",
            document: Document::None,
            reason: None,
            reconnects: 1,
        },
        Served {
            case: "a session started with the account's own document and a reason, \
                   reconnected three times",
            document: Document::ByName,
            reason: Some("the client's network dropped"),
            reconnects: 3,
        },
        Served {
            case: "a session started with that document by ARN, reconnected twice",
            document: Document::ByArn,
            reason: None,
            reconnects: 2,
        },
    ];

    // Every token this endpoint hands out over the whole table, so no reconnect can be
    // served the token another call was handed.
    let mut tokens = BTreeSet::new();

    for row in rows {
        let session_id = started(&router, &row, &node, &document_arn).await;
        let before = session_in(&router, REGION, "Active", &session_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected {session_id} to be reported as an active session",
                    row.case
                )
            });

        for attempt in 1..=row.reconnects {
            let (status, body) = resume_session(&router, REGION, &session_id).await;
            assert_eq!(
                status, 200,
                "case {:?}: reconnect {attempt} must be served, got {status} with {body}",
                row.case
            );
            // "The ID of the session": the one the request named, because a reconnect
            // continues that session rather than starting another.
            assert_eq!(
                body["SessionId"].as_str(),
                Some(session_id.as_str()),
                "case {:?}: reconnect {attempt} reports the session it reconnected, got {body}",
                row.case
            );
            let token = body["TokenValue"].as_str().unwrap_or_default().to_string();
            assert!(
                !token.is_empty() && token.chars().count() <= MAX_TOKEN_VALUE_CHARS,
                "case {:?}: reconnect {attempt} expected a TokenValue inside the modelled \
                 0..{MAX_TOKEN_VALUE_CHARS} length, got {body}",
                row.case
            );
            // The token authenticates ONE connection, so each reconnect is handed a token of
            // its own rather than the one an earlier call minted.
            assert!(
                tokens.insert(token.clone()),
                "case {:?}: reconnect {attempt} expected a token of its own, got one an \
                 earlier call was handed",
                row.case
            );
            // "A URL back to SSM Agent on the managed node that the Session Manager client
            // uses": the data-channel path of this session, at THIS emulator rather than at
            // any Systems Manager hostname.
            let stream_url = body["StreamUrl"].as_str().unwrap_or_default();
            assert!(
                stream_url.starts_with("wss://")
                    && stream_url.contains(&format!("/v1/data-channel/{session_id}"))
                    && !stream_url.contains("amazonaws.com"),
                "case {:?}: reconnect {attempt} expected a data-channel URL back to this \
                 emulator carrying {session_id}, got {stream_url:?}",
                row.case
            );

            // The session itself: the same record, still connected, and still the only one
            // this endpoint holds under that id.
            let after = session_in(&router, REGION, "Active", &session_id)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: expected {session_id} to still be active after reconnect \
                         {attempt}",
                        row.case
                    )
                });
            assert_eq!(
                after, before,
                "case {:?}: reconnect {attempt} must leave the session as it was, got {after} \
                 against {before}",
                row.case
            );
            assert_eq!(
                after["Status"], json!("Connected"),
                "case {:?}: reconnect {attempt} expected a connected session, got {after}",
                row.case
            );
            assert!(
                session_in(&router, REGION, "History", &session_id)
                    .await
                    .is_none(),
                "case {:?}: reconnect {attempt} must not put the session in the history",
                row.case
            );
            let held = sessions(
                &router,
                REGION,
                "Active",
                json!([{ "key": "SessionId", "value": session_id }]),
            )
            .await;
            assert_eq!(
                held.len(),
                1,
                "case {:?}: reconnect {attempt} expected the one session {session_id}, got \
                 {held:?}",
                row.case
            );
        }
    }
}

/// Why one refused row has no live session behind the id it names.
#[derive(Debug, Clone, Copy)]
enum Missing {
    /// No session was ever started under the id.
    NeverStarted,
    /// The session the id names was terminated: "a terminated session can't be resumed".
    Terminated,
    /// The id names a live session of this Region, asked of another Region's endpoint.
    OfAnotherRegion,
}

/// One refused row: the form of the id named, and why no live session is behind it.
#[derive(Debug, Clone, Copy)]
struct Refused {
    case: &'static str,
    missing: Missing,
}

/// A reconnect is refused for every `SessionId` with no live session behind it, and changes
/// nothing about the sessions this endpoint holds.
#[tokio::test]
async fn a_resume_with_no_live_session_behind_it_is_refused() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let node = registered_node(&router, "a registered node", &image, &registering_profile()).await;

    // One live session, to be asked for through the wrong endpoint, and one that is ended.
    let live = started(
        &router,
        &Served {
            case: "the live session",
            document: Document::None,
            reason: None,
            reconnects: 0,
        },
        &node,
        "",
    )
    .await;
    let ended = started(
        &router,
        &Served {
            case: "the session that is ended",
            document: Document::None,
            reason: None,
            reconnects: 0,
        },
        &node,
        "",
    )
    .await;
    let (status, body) = call_in_region(
        &router,
        REGION,
        "TerminateSession",
        &json!({ "SessionId": ended }),
    )
    .await;
    assert_eq!(
        status, 200,
        "TerminateSession({ended}) must be served, got {status} with {body}"
    );

    let rows = [
        (
            Refused {
                case: "a value no session was started under",
                missing: Missing::NeverStarted,
            },
            "never-started-here".to_string(),
        ),
        (
            Refused {
                case: "the shortest id the model allows",
                missing: Missing::NeverStarted,
            },
            "s".repeat(MIN_SESSION_ID_CHARS),
        ),
        (
            Refused {
                case: "the longest id the model allows",
                missing: Missing::NeverStarted,
            },
            "s".repeat(MAX_SESSION_ID_CHARS),
        ),
        (
            Refused {
                case: "an id of the form this service mints, under which nothing was started",
                missing: Missing::NeverStarted,
            },
            "AKIDTEST-11111111-2222-3333-4444-555555555555".to_string(),
        ),
        (
            Refused {
                case: "the id of a session that has been terminated",
                missing: Missing::Terminated,
            },
            ended.clone(),
        ),
        (
            Refused {
                case: "the id of a live session, asked of another Region's endpoint",
                missing: Missing::OfAnotherRegion,
            },
            live.clone(),
        ),
    ];

    for (row, session_id) in &rows {
        let region = match row.missing {
            Missing::OfAnotherRegion => OTHER_REGION,
            Missing::NeverStarted | Missing::Terminated => REGION,
        };
        let (status, body) = resume_session(&router, region, session_id).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the reconnect to be refused, got {status} with {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {:?}: expected ValidationException, got {body}",
            row.case
        );
        // Every such request is refused with the one sentence: the refusal is about the
        // state a reconnect needs, and says nothing else of the request.
        let expected = "Session is not in a valid state";
        assert_eq!(
            error_message(&body),
            expected,
            "case {:?}: expected the message {expected:?}, got {body}",
            row.case
        );
    }

    // Nothing the refusals touched changed: the live session is still active, the ended one
    // is still in the history and still terminated, and no session was invented.
    let still_live = session_in(&router, REGION, "Active", &live)
        .await
        .unwrap_or_else(|| panic!("expected {live} to still be reported as an active session"));
    assert_eq!(
        still_live["Status"], json!("Connected"),
        "expected the live session to stay connected, got {still_live}"
    );
    let still_ended = session_in(&router, REGION, "History", &ended)
        .await
        .unwrap_or_else(|| panic!("expected {ended} to still be reported in the history"));
    assert_eq!(
        still_ended["Status"], json!("Terminated"),
        "expected a refused reconnect to leave the ended session terminated, got {still_ended}"
    );
    assert!(
        session_in(&router, REGION, "Active", &ended).await.is_none(),
        "expected a refused reconnect not to make the ended session active again"
    );
    let active = sessions(&router, REGION, "Active", json!([])).await;
    assert_eq!(
        active.len(),
        1,
        "expected the one live session to be the only active one, got {active:?}"
    );
}
