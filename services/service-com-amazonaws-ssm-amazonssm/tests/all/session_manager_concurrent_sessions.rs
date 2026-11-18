// SPDX-License-Identifier: CC0-1.0

//! Several Session Manager sessions at once: every start is a connection of its OWN.
//!
//! RULE under test: `StartSession` "initiates a connection to a target (for example, a
//! managed node)", so each served call records a NEW session — its own `SessionId`, its own
//! token and its own data-channel URL — holding the input that call supplied, and leaves
//! every session already recorded exactly as it was. Every one of them is reported together
//! under `Active`, narrowed by a filter to the sessions that match it, and `TerminateSession`
//! moves the ONE session it names into `History`, never a sibling that shares its target or
//! its document.
//!
//! The rows differ along what the rule generalises over: which managed node the session
//! connects to, whether the request names a document and how (not at all, by name, by ARN),
//! whether it carries `Parameters` and a `Reason`, and whether an identical request has
//! already been served — two identical starts are two sessions, not one.
//!
//! Every call goes through the wire a client uses, and the managed nodes the sessions connect
//! to are ones an Amazon EC2 launch registered — the helpers the sibling session suite drives
//! ([`crate::session_manager_sessions`]), rather than a second copy of them.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{create_profile, emulator, image_id, AL2023_PARAMETER};
use crate::parameter_store::{call_in_region, REGION};
use crate::session_manager_sessions::{
    create_session_document, registered_node, registering_profile, session_in, sessions,
    start_session, SESSION_DOCUMENT,
};

/// How one row names the document its session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Document {
    /// The request names none: "a shell to the managed node is launched by default".
    None,
    /// `DocumentName`, carrying the name the account's own session document is held under.
    ByName,
    /// `DocumentName`, carrying the ARN that same document is addressable by.
    ByArn,
}

/// Which of the two registered managed nodes a row connects to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
    First,
    Second,
}

/// One start: the target it names, how it names the document, and the optional input it
/// carries.
#[derive(Debug, Clone, Copy)]
struct Start {
    case: &'static str,
    node: Node,
    document: Document,
    /// Whether the request carries `Parameters`, "the values you want to specify for the
    /// parameters defined in the Session document" — input the `Session` record reports no
    /// member for, so it must be accepted and change nothing about what is reported.
    parameters: bool,
    /// The `Reason` the request carries, when it carries one.
    reason: Option<&'static str>,
}

/// The table every case of this suite walks: six starts over two nodes, three document
/// spellings, with and without `Parameters` and a `Reason`, ending in a repeat of an earlier
/// row's request.
const STARTS: &[Start] = &[
    Start {
        case: "the first node, no document, no reason",
        node: Node::First,
        document: Document::None,
        parameters: false,
        reason: None,
    },
    Start {
        case: "the first node, the account's own document by name, with parameters and a reason",
        node: Node::First,
        document: Document::ByName,
        parameters: true,
        reason: Some("connecting to read the logs"),
    },
    Start {
        case: "the first node, no document, with parameters",
        node: Node::First,
        document: Document::None,
        parameters: true,
        reason: None,
    },
    Start {
        case: "the second node, that same document by ARN, with a reason",
        node: Node::Second,
        document: Document::ByArn,
        parameters: false,
        reason: Some("connecting to restart the agent"),
    },
    Start {
        case: "the second node, no document, no reason",
        node: Node::Second,
        document: Document::None,
        parameters: false,
        reason: None,
    },
    Start {
        case: "the first node again, the same document by name — the same request twice",
        node: Node::First,
        document: Document::ByName,
        parameters: false,
        reason: None,
    },
];

/// The two nodes and the document ARN every row of the table is built from.
struct Fixtures {
    router: MultiServiceRouter,
    first: String,
    second: String,
    document_arn: String,
}

/// Register the two managed nodes and the account's own session document the table needs.
async fn fixtures() -> Fixtures {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let document_arn = create_session_document(&router).await;
    let first = registered_node(&router, "the first node", &image, &registering_profile()).await;
    let second = registered_node(&router, "the second node", &image, &registering_profile()).await;
    Fixtures {
        router,
        first,
        second,
        document_arn,
    }
}

impl Fixtures {
    /// The node id `row` connects to.
    fn target(&self, row: &Start) -> &str {
        match row.node {
            Node::First => &self.first,
            Node::Second => &self.second,
        }
    }

    /// The request `row` describes, as a client sends it.
    fn request(&self, row: &Start) -> Value {
        let mut request = json!({ "Target": self.target(row) });
        match row.document {
            Document::None => {}
            Document::ByName => request["DocumentName"] = json!(SESSION_DOCUMENT),
            Document::ByArn => request["DocumentName"] = json!(self.document_arn),
        }
        if row.parameters {
            request["Parameters"] = json!({ "idleSessionTimeout": ["20"] });
        }
        if let Some(reason) = row.reason {
            request["Reason"] = json!(reason);
        }
        request
    }
}

/// What a served start reported, kept so a later assertion can hold it against what the
/// endpoint reports.
#[derive(Debug)]
struct Opened {
    row: Start,
    session_id: String,
}

/// The `DocumentName` the session of `row` reports: the document's own name whichever
/// spelling named it, and nothing when the request named none.
fn expected_document(row: &Start) -> Value {
    match row.document {
        Document::None => Value::Null,
        Document::ByName | Document::ByArn => json!(SESSION_DOCUMENT),
    }
}

/// Serve the whole table against one endpoint, asserting each start is its own connection,
/// and answer what each one opened in the order it was opened.
async fn open_every_session(fixtures: &Fixtures) -> Vec<Opened> {
    let mut opened: Vec<Opened> = Vec::new();
    let mut session_ids: BTreeSet<String> = BTreeSet::new();
    let mut tokens: BTreeSet<String> = BTreeSet::new();
    let mut stream_urls: BTreeSet<String> = BTreeSet::new();

    for row in STARTS {
        let (status, body) = start_session(&fixtures.router, REGION, &fixtures.request(row)).await;
        assert_eq!(
            status, 200,
            "case {:?}: StartSession must be served, got {status} with {body}",
            row.case
        );
        let session_id = body["SessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: a start reports a SessionId, got {body}", row.case))
            .to_string();
        // A start "initiates a connection", so what it mints belongs to THIS session: no
        // id, token or URL an earlier start was handed is handed out again.
        assert!(
            session_ids.insert(session_id.clone()),
            "case {:?}: expected a SessionId of its own, got {session_id} which an earlier \
             start was handed",
            row.case
        );
        let token = body["TokenValue"].as_str().unwrap_or_default().to_string();
        assert!(
            !token.is_empty(),
            "case {:?}: expected a TokenValue on the start, got {body}",
            row.case
        );
        assert!(
            tokens.insert(token),
            "case {:?}: expected a TokenValue of its own, got one an earlier start was handed \
             ({body})",
            row.case
        );
        let stream_url = body["StreamUrl"].as_str().unwrap_or_default().to_string();
        assert!(
            stream_url.starts_with("wss://")
                && stream_url.contains(&format!("/v1/data-channel/{session_id}"))
                && !stream_url.contains("amazonaws.com"),
            "case {:?}: expected a data-channel URL back to this emulator carrying \
             {session_id}, got {stream_url:?}",
            row.case
        );
        assert!(
            stream_urls.insert(stream_url),
            "case {:?}: expected a StreamUrl of its own, got one an earlier start was handed \
             ({body})",
            row.case
        );

        opened.push(Opened {
            row: *row,
            session_id,
        });
    }
    opened
}

/// Every start of the table is recorded as a session of its own, holding the input that call
/// supplied, and a filter narrows the report to the sessions that match it.
#[tokio::test]
async fn every_start_records_a_session_of_its_own() {
    let fixtures = fixtures().await;
    let opened = open_every_session(&fixtures).await;

    // Each session reports what ITS start supplied — no record was overwritten by a later
    // start, not even by the one that repeated an earlier request verbatim.
    for Opened { row, session_id } in &opened {
        let reported = session_in(&fixtures.router, REGION, "Active", session_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected {session_id} to be reported as an active session",
                    row.case
                )
            });
        for (member, expected) in [
            ("SessionId", json!(session_id)),
            ("Target", json!(fixtures.target(row))),
            ("Status", json!("Connected")),
            ("DocumentName", expected_document(row)),
            (
                "Reason",
                row.reason.map_or(Value::Null, |reason| json!(reason)),
            ),
            ("EndDate", Value::Null),
        ] {
            assert_eq!(
                reported[member], expected,
                "case {:?}: expected {member} {expected}, got {reported}",
                row.case
            );
        }
    }

    // Every one of them is active at the same time: a start ends no session.
    let active: BTreeSet<String> = sessions(&fixtures.router, REGION, "Active", json!([]))
        .await
        .iter()
        .filter_map(|session| session["SessionId"].as_str().map(str::to_string))
        .collect();
    let expected: BTreeSet<String> = opened
        .iter()
        .map(|session| session.session_id.clone())
        .collect();
    assert_eq!(
        active, expected,
        "expected every started session to be active together, got {active:?} against \
         {expected:?}"
    );
    assert!(
        sessions(&fixtures.router, REGION, "History", json!([]))
            .await
            .is_empty(),
        "expected no session in the history before any was ended"
    );

    // A filter narrows the report to the sessions that match it, over the two dimensions the
    // table varies: which node the session connects to, and which document it runs.
    for (case, filters, expected) in [
        (
            "the first node",
            json!([{ "key": "Target", "value": fixtures.first }]),
            ids_where(&opened, |row| row.node == Node::First),
        ),
        (
            "the second node",
            json!([{ "key": "Target", "value": fixtures.second }]),
            ids_where(&opened, |row| row.node == Node::Second),
        ),
    ] {
        let reported: BTreeSet<String> = sessions(&fixtures.router, REGION, "Active", filters)
            .await
            .iter()
            .filter_map(|session| session["SessionId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the sessions {expected:?}, got {reported:?}"
        );
    }
}

/// The ids of the opened sessions whose start matches `wanted`.
fn ids_where(opened: &[Opened], wanted: impl Fn(&Start) -> bool) -> BTreeSet<String> {
    opened
        .iter()
        .filter(|session| wanted(&session.row))
        .map(|session| session.session_id.clone())
        .collect()
}

/// Ending one of several sessions moves that session alone into the history, and a start
/// served afterwards is another session again.
#[tokio::test]
async fn ending_one_session_leaves_every_sibling_as_it_was() {
    let fixtures = fixtures().await;
    let opened = open_every_session(&fixtures).await;

    // The session that is ended shares its target AND its document with another row, so
    // nothing but the id it was named by can select it.
    let ended = opened
        .iter()
        .find(|session| {
            session.row.node == Node::First && session.row.document == Document::ByName
        })
        .expect("the table opens a session of the first node running the account's document");

    // What every other session reports before the end, to hold against it afterwards.
    let mut before: Vec<(&Opened, Value)> = Vec::new();
    for session in &opened {
        if session.session_id == ended.session_id {
            continue;
        }
        let reported = session_in(&fixtures.router, REGION, "Active", &session.session_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected {} to be active before the end",
                    session.row.case, session.session_id
                )
            });
        before.push((session, reported));
    }

    let (status, body) = call_in_region(
        &fixtures.router,
        REGION,
        "TerminateSession",
        &json!({ "SessionId": ended.session_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: TerminateSession must be served, got {status} with {body}",
        ended.row.case
    );
    assert_eq!(
        body["SessionId"].as_str(),
        Some(ended.session_id.as_str()),
        "case {:?}: the end reports the session it ended, got {body}",
        ended.row.case
    );

    // The one session named is in the history, terminated and with the instant it ended.
    let in_history = session_in(&fixtures.router, REGION, "History", &ended.session_id)
        .await
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected {} to be reported in the history",
                ended.row.case, ended.session_id
            )
        });
    assert_eq!(
        in_history["Status"],
        json!("Terminated"),
        "case {:?}: expected a terminated session, got {in_history}",
        ended.row.case
    );
    assert!(
        !in_history["EndDate"].is_null(),
        "case {:?}: expected an EndDate on the ended session, got {in_history}",
        ended.row.case
    );
    assert!(
        session_in(&fixtures.router, REGION, "Active", &ended.session_id)
            .await
            .is_none(),
        "case {:?}: an ended session is no longer active",
        ended.row.case
    );

    // Every sibling is untouched — including the one that shares the ended session's target
    // and document.
    for (session, was) in &before {
        let now = session_in(&fixtures.router, REGION, "Active", &session.session_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected {} to still be active after another session ended",
                    session.row.case, session.session_id
                )
            });
        assert_eq!(
            &now, was,
            "case {:?}: ending another session must leave {} as it was, got {now} against {was}",
            session.row.case, session.session_id
        );
        assert!(
            session_in(&fixtures.router, REGION, "History", &session.session_id)
                .await
                .is_none(),
            "case {:?}: expected {} to stay out of the history",
            session.row.case,
            session.session_id
        );
    }

    // A start served after an end is another session again, against the same node the ended
    // session used.
    let (status, body) = start_session(
        &fixtures.router,
        REGION,
        &json!({ "Target": fixtures.first, "DocumentName": SESSION_DOCUMENT }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a start after an end\": StartSession must be served, got {status} with {body}"
    );
    let reopened = body["SessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"a start after an end\": expected a SessionId, got {body}"))
        .to_string();
    assert_ne!(
        reopened, ended.session_id,
        "case \"a start after an end\": expected a session of its own rather than the ended \
         session's id"
    );
    assert!(
        session_in(&fixtures.router, REGION, "Active", &reopened)
            .await
            .is_some(),
        "case \"a start after an end\": expected {reopened} to be reported as an active session"
    );
    let in_history = session_in(&fixtures.router, REGION, "History", &ended.session_id)
        .await
        .unwrap_or_else(|| {
            panic!(
                "case \"a start after an end\": expected {} to stay in the history",
                ended.session_id
            )
        });
    assert_eq!(
        in_history["Status"],
        json!("Terminated"),
        "case \"a start after an end\": expected the ended session to stay terminated, got \
         {in_history}"
    );
}
