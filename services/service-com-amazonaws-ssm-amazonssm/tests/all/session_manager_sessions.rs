// SPDX-License-Identifier: CC0-1.0

//! Session Manager sessions end to end: a session exists exactly when a caller started one
//! against a node this endpoint reports as managed, and it is reported until it is ended.
//!
//! RULE under test: `StartSession` "initiates a connection to a target (for example, a
//! managed node)", so it is served only for a target this endpoint holds as a managed node
//! whose agent is responding — every other target is `TargetNotConnected`, and a
//! `DocumentName` this endpoint resolves no document for is `InvalidDocument`. A served
//! start hands the caller the session's id, a token and a URL back to this emulator, and
//! records the session: `DescribeSessions` reports it under `Active` with what the caller
//! supplied, `TerminateSession` ends it, and from then on it is reported under `History`
//! instead — a second end changes nothing.
//!
//! The rows differ along what the rule generalises over: which kind of target is named (a
//! node id no node is held under, an instance that never registered, one whose role
//! withholds the registration call, a hybrid node id, a value that is no node id at all, and
//! a registered node asked for through another Region's endpoint), whether the request names
//! a document and how (not at all, by name, by ARN, and a name nothing resolves), and
//! whether it carries a reason.
//!
//! Every call goes through the wire a client uses, and the managed node every served row
//! connects to is one an Amazon EC2 launch registered — the registration this service
//! derives from the three components that own its evidence
//! ([`crate::ec2_managed_node_registration`]), whose helpers this suite drives rather than
//! keeping a second copy of them.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_node_registration::{
    create_profile, emulator, image_id, launch, node_when_registered, Grant, Profile,
    AL2023_PARAMETER, DEADLINE, POLL_PAUSE,
};
use crate::parameter_store::{call_in_region, error_code, error_message, ACCOUNT, REGION};

/// Another Region's endpoint, for the row that asks it about a session and a node this
/// Region holds.
pub(crate) const OTHER_REGION: &str = "eu-central-1";

/// The `AccessType` every session a caller starts directly carries: "`Standard` access type
/// is the default for Session Manager sessions" (`Session$AccessType`).
const STANDARD: &str = "Standard";

/// A `Session` document of the account's own — the kind a client creates to define "the
/// parameters and plugin settings for the session" — written in the format
/// `CreateDocument` takes.
const SESSION_DOCUMENT_CONTENT: &str = r#"{
  "schemaVersion": "1.0",
  "description": "Start an interactive shell",
  "sessionType": "Standard_Stream",
  "inputs": { "idleSessionTimeout": "20" }
}"#;

/// The name that document is created under.
pub(crate) const SESSION_DOCUMENT: &str = "SSM-SessionManagerRunShell";

/// A document name no case creates, so nothing resolves it.
const ABSENT_DOCUMENT: &str = "SSM-NeverCreated";

/// The instance profile every served row's instance runs as: its role carries the
/// AWS-managed policy a client attaches so the agent may register the node.
pub(crate) fn registering_profile() -> Profile {
    Profile {
        name: "ssm-core",
        trusts: "ec2.amazonaws.com",
        grant: Grant::AwsManaged,
    }
}

/// An instance profile whose role is granted nothing, so the instance running as it never
/// becomes a managed node.
fn silent_profile() -> Profile {
    Profile {
        name: "no-grant",
        trusts: "ec2.amazonaws.com",
        grant: Grant::None,
    }
}

/// Create the `Session` document of the account's own and answer the ARN it is also
/// addressable by.
pub(crate) async fn create_session_document(router: &MultiServiceRouter) -> String {
    let (status, body) = call_in_region(
        router,
        REGION,
        "CreateDocument",
        &json!({
            "Name": SESSION_DOCUMENT,
            "DocumentType": "Session",
            "Content": SESSION_DOCUMENT_CONTENT,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateDocument({SESSION_DOCUMENT}) must succeed, got {status} with {body}"
    );
    format!("arn:aws:ssm:{REGION}:{ACCOUNT}:document/{SESSION_DOCUMENT}")
}

/// Start one session and answer the status and body, whatever it was.
pub(crate) async fn start_session(
    router: &MultiServiceRouter,
    region: &str,
    request: &Value,
) -> (u16, Value) {
    call_in_region(router, region, "StartSession", request).await
}

/// The sessions this endpoint reports in `state`, narrowed by `filters`.
///
/// `Filters` is modelled with a minimum length of one, so a case that narrows by nothing
/// leaves the member out rather than sending an empty list.
pub(crate) async fn sessions(
    router: &MultiServiceRouter,
    region: &str,
    state: &str,
    filters: Value,
) -> Vec<Value> {
    let mut request = json!({ "State": state });
    if filters.as_array().is_some_and(|given| !given.is_empty()) {
        request["Filters"] = filters;
    }
    let (status, body) = call_in_region(router, region, "DescribeSessions", &request).await;
    assert_eq!(
        status, 200,
        "DescribeSessions({state}) must answer, got {status} with {body}"
    );
    body["Sessions"].as_array().cloned().unwrap_or_default()
}

/// The ids of the sessions this endpoint reports in `state`, with no filter at all.
async fn session_ids(router: &MultiServiceRouter, region: &str, state: &str) -> Vec<String> {
    let (status, body) =
        call_in_region(router, region, "DescribeSessions", &json!({ "State": state })).await;
    assert_eq!(
        status, 200,
        "DescribeSessions({state}) must answer, got {status} with {body}"
    );
    body["Sessions"]
        .as_array()
        .map(|reported| {
            reported
                .iter()
                .filter_map(|session| session["SessionId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The one session `session_id` names in `state`, or `None` when that set holds none.
pub(crate) async fn session_in(
    router: &MultiServiceRouter,
    region: &str,
    state: &str,
    session_id: &str,
) -> Option<Value> {
    sessions(
        router,
        region,
        state,
        json!([{ "key": "SessionId", "value": session_id }]),
    )
    .await
    .first()
    .cloned()
}

/// Launch one instance with `profile` and look again until this endpoint reports it as a
/// managed node, as the node id a session can be started against.
pub(crate) async fn registered_node(
    router: &MultiServiceRouter,
    case: &str,
    image: &str,
    profile: &Profile,
) -> String {
    let instance_id = launch(router, case, REGION, image, Some(profile.name)).await;
    node_when_registered(router, case, REGION, &instance_id).await;
    instance_id
}

/// How one served row names the document the session runs.
#[derive(Debug, Clone, Copy)]
enum Document {
    /// The request names none: "a shell to the managed node is launched by default".
    None,
    /// `DocumentName`, carrying the name the account's own document is held under.
    ByName,
    /// `DocumentName`, carrying the ARN that same document is addressable by — "the name
    /// can be an Amazon Resource Name (ARN)".
    ByArn,
}

/// One served row: how the start is made, and what the session it records must report.
#[derive(Debug, Clone, Copy)]
struct Served {
    case: &'static str,
    document: Document,
    /// The `Reason` the request carries, when it carries one.
    reason: Option<&'static str>,
}

/// A served start records a session this endpoint then reports, and ending it moves the
/// session from the `Active` set to the `History` one.
#[tokio::test]
async fn a_started_session_is_reported_until_it_is_terminated() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let document_arn = create_session_document(&router).await;
    let node = registered_node(&router, "a registered node", &image, &registering_profile()).await;

    let rows = [
        Served {
            case: "no document and no reason",
            document: Document::None,
            reason: None,
        },
        Served {
            case: "the account's own session document by name, with a reason",
            document: Document::ByName,
            reason: Some("connecting to debug the host"),
        },
        Served {
            case: "that same document by ARN",
            document: Document::ByArn,
            reason: None,
        },
    ];

    for row in rows {
        let mut request = json!({ "Target": node });
        match row.document {
            Document::None => {}
            Document::ByName => request["DocumentName"] = json!(SESSION_DOCUMENT),
            Document::ByArn => request["DocumentName"] = json!(document_arn),
        }
        if let Some(reason) = row.reason {
            request["Reason"] = json!(reason);
        }

        let (status, body) = start_session(&router, REGION, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: StartSession must be served, got {status} with {body}",
            row.case
        );
        let session_id = body["SessionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: a start reports a SessionId, got {body}", row.case))
            .to_string();
        assert!(
            (1..=96).contains(&session_id.chars().count()),
            "case {:?}: expected a SessionId inside the modelled 1..96 length, got \
             {session_id:?}",
            row.case
        );
        let token = body["TokenValue"].as_str().unwrap_or_default();
        assert!(
            !token.is_empty() && token.len() <= 300,
            "case {:?}: expected a TokenValue inside the modelled 0..300 length, got {body}",
            row.case
        );
        // "A URL back to SSM Agent on the managed node that the Session Manager client
        // uses": the data-channel path of the session, at THIS emulator rather than at any
        // Systems Manager hostname.
        let stream_url = body["StreamUrl"].as_str().unwrap_or_default();
        assert!(
            stream_url.starts_with("wss://")
                && stream_url.contains(&format!("/v1/data-channel/{session_id}"))
                && !stream_url.contains("amazonaws.com"),
            "case {:?}: expected a data-channel URL back to this emulator carrying \
             {session_id}, got {stream_url:?}",
            row.case
        );

        // The session this endpoint now holds, in the `Active` set and not in the other.
        let reported = session_in(&router, REGION, "Active", &session_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected {session_id} to be reported as an active session",
                    row.case
                )
            });
        let expected_document = match row.document {
            Document::None => Value::Null,
            // `Session$DocumentName` is the document's own name, whichever spelling the
            // request named it by.
            Document::ByName | Document::ByArn => json!(SESSION_DOCUMENT),
        };
        for (member, expected) in [
            ("SessionId", json!(session_id)),
            ("Target", json!(node)),
            ("Status", json!("Connected")),
            ("AccessType", json!(STANDARD)),
            ("DocumentName", expected_document),
            ("Reason", row.reason.map_or(Value::Null, |reason| json!(reason))),
            ("EndDate", Value::Null),
        ] {
            assert_eq!(
                reported[member], expected,
                "case {:?}: expected {member} {expected}, got {reported}",
                row.case
            );
        }
        for member in ["StartDate", "Owner"] {
            assert!(
                !reported[member].is_null(),
                "case {:?}: expected a {member} on the session, got {reported}",
                row.case
            );
        }
        assert!(
            session_in(&router, REGION, "History", &session_id)
                .await
                .is_none(),
            "case {:?}: a connected session is in no history, got it in History",
            row.case
        );
        // A session is reached through the endpoint it was started at.
        assert!(
            session_in(&router, OTHER_REGION, "Active", &session_id)
                .await
                .is_none(),
            "case {:?}: a session of one Region must not be reported by another's endpoint",
            row.case
        );

        // Ending it: the id is reported back, and the session moves to the other set with
        // the instant it ended.
        let (status, body) = call_in_region(
            &router,
            REGION,
            "TerminateSession",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the first end must be served, got {status} with {body}",
            row.case
        );
        assert_eq!(
            body["SessionId"].as_str(),
            Some(session_id.as_str()),
            "case {:?}: the first end reports the session it ended, got {body}",
            row.case
        );
        // And ending it AGAIN is refused: the session is in no state to be ended twice.
        let (status, refused) = call_in_region(
            &router,
            REGION,
            "TerminateSession",
            &json!({ "SessionId": session_id }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str(), error_message(&refused).as_str()),
            (400, "ValidationException", "Session is not in a valid state"),
            "case {:?}: the same end again must be refused, got {status} with {refused}",
            row.case
        );
        let ended = session_in(&router, REGION, "History", &session_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected {session_id} to be reported in the history",
                    row.case
                )
            });
        assert_eq!(
            ended["Status"], json!("Terminated"),
            "case {:?}: expected a terminated session, got {ended}",
            row.case
        );
        assert!(
            !ended["EndDate"].is_null(),
            "case {:?}: expected an EndDate on the ended session, got {ended}",
            row.case
        );
        assert!(
            session_in(&router, REGION, "Active", &session_id)
                .await
                .is_none(),
            "case {:?}: an ended session is no longer active",
            row.case
        );
    }

    // A session id this endpoint holds none of ends nothing, so the answer names no
    // session.
    let (status, body) = call_in_region(
        &router,
        REGION,
        "TerminateSession",
        &json!({ "SessionId": "never-started-here" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"an id nothing is held under\": TerminateSession must answer, got {status} with \
         {body}"
    );
    assert!(
        body["SessionId"].is_null(),
        "case \"an id nothing is held under\": expected no session to be reported, got {body}"
    );
}

/// One refused row: the target named, and whether the document is the reason.
#[derive(Debug, Clone, Copy)]
enum Target {
    /// A node id this Region holds no node under.
    UnheldNodeId,
    /// A hybrid node id form nothing registered.
    UnheldHybridId,
    /// A value that is no node id at all.
    NotANodeId,
    /// An instance of this Region running as no instance profile, so its agent never
    /// registered it.
    InstanceWithoutProfile,
    /// An instance whose instance-profile role withholds the call the agent registers
    /// with — the sibling of a node that does register.
    InstanceWithoutGrant,
    /// A node this Region holds, asked for through another Region's endpoint.
    NodeOfAnotherRegion,
}

/// `StartSession` is served for a connected managed node of the endpoint it was asked of,
/// and for nothing else.
#[tokio::test]
async fn a_start_against_an_unmanaged_target_is_refused() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    create_profile(&router, &silent_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    // The instances that must NOT be managed are launched BEFORE the one that must, so by
    // the time the registration below has been observed their own spans have passed too —
    // which is what makes "it is not managed" an answer rather than a race.
    let without_profile = launch(&router, "no instance profile", REGION, &image, None).await;
    let without_grant = launch(
        &router,
        "a role granted nothing",
        REGION,
        &image,
        Some(silent_profile().name),
    )
    .await;
    let node = registered_node(&router, "a registered node", &image, &registering_profile()).await;

    let rows = [
        (
            "a node id this Region holds no node under",
            Target::UnheldNodeId,
        ),
        ("a hybrid node id nothing registered", Target::UnheldHybridId),
        ("a value that is no node id at all", Target::NotANodeId),
        (
            "an instance running as no instance profile",
            Target::InstanceWithoutProfile,
        ),
        (
            "an instance whose role withholds the registration call",
            Target::InstanceWithoutGrant,
        ),
        (
            "a managed node of another Region's endpoint",
            Target::NodeOfAnotherRegion,
        ),
    ];

    for (case, target) in rows {
        let (region, named) = match target {
            Target::UnheldNodeId => (REGION, "i-0dead999".to_string()),
            Target::UnheldHybridId => (REGION, "mi-0123456789abcdef0".to_string()),
            Target::NotANodeId => (REGION, "not-a-managed-node".to_string()),
            Target::InstanceWithoutProfile => (REGION, without_profile.clone()),
            Target::InstanceWithoutGrant => (REGION, without_grant.clone()),
            Target::NodeOfAnotherRegion => (OTHER_REGION, node.clone()),
        };
        let (status, body) = start_session(&router, region, &json!({ "Target": named })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "TargetNotConnected",
            "case {case:?}: expected TargetNotConnected, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("{named} is not connected."),
            "case {case:?}: expected the refusal to name the target, got {body}"
        );
        // Nothing was recorded for a refused start.
        assert!(
            session_ids(&router, region, "Active").await.is_empty(),
            "case {case:?}: a refused start records no session"
        );
    }

    // A document this endpoint resolves nothing for: the target is a connected node, so
    // what is refused is the document.
    let (status, body) = start_session(
        &router,
        REGION,
        &json!({ "Target": node, "DocumentName": ABSENT_DOCUMENT }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a document nothing resolves\": expected a refusal, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidDocument",
        "case \"a document nothing resolves\": expected InvalidDocument, got {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("Document with name {ABSENT_DOCUMENT} does not exist."),
        "case \"a document nothing resolves\": expected the refusal to name the document, got \
         {body}"
    );
    assert!(
        session_ids(&router, REGION, "Active").await.is_empty(),
        "case \"a document nothing resolves\": a refused start records no session"
    );
}

/// Every filter key `SessionFilter` declares selects the sessions it says it does, a page
/// holds what the request asked for, and a token no session id could be is refused.
#[tokio::test]
async fn a_session_report_filters_and_pages_what_it_holds() {
    let router = emulator();
    create_profile(&router, &registering_profile()).await;
    let image = image_id(&router, AL2023_PARAMETER).await;
    let first = registered_node(&router, "the first node", &image, &registering_profile()).await;
    let second = registered_node(&router, "the second node", &image, &registering_profile()).await;

    // Three sessions: two against one node and one against the other, so a target filter
    // selects a subset rather than everything.
    let mut started: Vec<(String, String)> = Vec::new();
    for target in [&first, &first, &second] {
        let (status, body) = start_session(&router, REGION, &json!({ "Target": target })).await;
        assert_eq!(
            status, 200,
            "StartSession against {target} must be served, got {status} with {body}"
        );
        let session_id = body["SessionId"]
            .as_str()
            .expect("a start reports a SessionId")
            .to_string();
        started.push((session_id, target.clone()));
    }
    let owner = session_in(&router, REGION, "Active", &started[0].0)
        .await
        .and_then(|session| session["Owner"].as_str().map(str::to_string))
        .expect("an active session reports its Owner");
    let every_id: Vec<String> = {
        let mut ids: Vec<String> = started.iter().map(|(id, _)| id.clone()).collect();
        ids.sort();
        ids
    };

    // (case, the filters, the session ids that must be reported)
    let of_first: Vec<String> = {
        let mut ids: Vec<String> = started
            .iter()
            .filter(|(_, target)| target == &first)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    };
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "no filter at all",
            json!([]),
            every_id.clone(),
        ),
        (
            "the target two of them connect to",
            json!([{ "key": "Target", "value": first }]),
            of_first.clone(),
        ),
        (
            "a target no session connects to",
            json!([{ "key": "Target", "value": "i-0dead999" }]),
            Vec::new(),
        ),
        (
            "the id of one session",
            json!([{ "key": "SessionId", "value": started[2].0 }]),
            vec![started[2].0.clone()],
        ),
        (
            "the owner that started them",
            json!([{ "key": "Owner", "value": owner }]),
            every_id.clone(),
        ),
        (
            "an owner that started none",
            json!([{ "key": "Owner", "value": "arn:aws:iam::123456789012:user/nobody" }]),
            Vec::new(),
        ),
        (
            "the status a connected session reports",
            json!([{ "key": "Status", "value": "Connected" }]),
            every_id.clone(),
        ),
        (
            "a status none of them is in",
            json!([{ "key": "Status", "value": "Disconnected" }]),
            Vec::new(),
        ),
        (
            "the access type of a session a caller started",
            json!([{ "key": "AccessType", "value": STANDARD }]),
            every_id.clone(),
        ),
        (
            "another access type",
            json!([{ "key": "AccessType", "value": "JustInTime" }]),
            Vec::new(),
        ),
        (
            "started after an instant long past",
            json!([{ "key": "InvokedAfter", "value": "2000-01-01T00:00:00Z" }]),
            every_id.clone(),
        ),
        (
            "started after an instant far ahead",
            json!([{ "key": "InvokedAfter", "value": "2999-01-01T00:00:00Z" }]),
            Vec::new(),
        ),
        (
            "started before an instant far ahead",
            json!([{ "key": "InvokedBefore", "value": "2999-01-01T00:00:00Z" }]),
            every_id.clone(),
        ),
        (
            "started before an instant long past",
            json!([{ "key": "InvokedBefore", "value": "2000-01-01T00:00:00Z" }]),
            Vec::new(),
        ),
        (
            "two filters that intersect",
            json!([
                { "key": "Target", "value": first },
                { "key": "Status", "value": "Connected" },
            ]),
            of_first.clone(),
        ),
        (
            "two filters that cannot both hold",
            json!([
                { "key": "Target", "value": first },
                { "key": "Target", "value": second },
            ]),
            Vec::new(),
        ),
    ];

    for (case, filters, expected) in cases {
        let reported: Vec<String> = sessions(&router, REGION, "Active", filters)
            .await
            .iter()
            .filter_map(|session| session["SessionId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the sessions {expected:?}, got {reported:?}"
        );
    }

    // Paging: every page size from one session to past the holding, and each published
    // bound of the modelled range, walk the same sessions in the same order.
    for size in [1, 2, 3, 4, 200] {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = json!({ "State": "Active", "MaxResults": size });
            if let Some(held) = token.clone() {
                request["NextToken"] = json!(held);
            }
            let (status, body) =
                call_in_region(&router, REGION, "DescribeSessions", &request).await;
            assert_eq!(
                status, 200,
                "case page size {size}: DescribeSessions must answer, got {status} with {body}"
            );
            let page = body["Sessions"].as_array().cloned().unwrap_or_default();
            assert!(
                page.len() <= usize::try_from(size).expect("a page size is a count"),
                "case page size {size}: a page must hold at most {size}, got {body}"
            );
            walked.extend(
                page.iter()
                    .filter_map(|session| session["SessionId"].as_str().map(str::to_string)),
            );
            token = body["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, every_id,
            "case page size {size}: paging must walk every session once, got {walked:?}"
        );
    }

    // A token no session id could be was not one this service issued.
    let (status, body) = call_in_region(
        &router,
        REGION,
        "DescribeSessions",
        &json!({ "State": "Active", "NextToken": "t".repeat(97) }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a token longer than any session id\": expected a refusal, got {status} with \
         {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidNextToken",
        "case \"a token longer than any session id\": expected InvalidNextToken, got {body}"
    );

    // A filter value that could not be a value of the attribute its key filters.
    for (case, filters) in [
        (
            "an instant that is not an instant",
            json!([{ "key": "InvokedAfter", "value": "the day before yesterday" }]),
        ),
        (
            "a status outside the set the member lists",
            json!([{ "key": "Status", "value": "Sleeping" }]),
        ),
    ] {
        let (status, body) = call_in_region(
            &router,
            REGION,
            "DescribeSessions",
            &json!({ "State": "Active", "Filters": filters }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {body}"
        );
    }

    // Ending one of them moves exactly that session to the other set, and the report of
    // each set stays in session-id order.
    let (ended, _) = started[1].clone();
    let (status, body) = call_in_region(
        &router,
        REGION,
        "TerminateSession",
        &json!({ "SessionId": ended }),
    )
    .await;
    assert_eq!(
        status, 200,
        "TerminateSession must be served, got {status} with {body}"
    );
    let until = Instant::now() + DEADLINE;
    loop {
        let active = session_ids(&router, REGION, "Active").await;
        let history = session_ids(&router, REGION, "History").await;
        let expected_active: Vec<String> = every_id
            .iter()
            .filter(|id| *id != &ended)
            .cloned()
            .collect();
        if active == expected_active && history == vec![ended.clone()] {
            break;
        }
        assert!(
            Instant::now() < until,
            "expected {ended} to be reported in the history and the rest active, got active \
             {active:?} and history {history:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}
