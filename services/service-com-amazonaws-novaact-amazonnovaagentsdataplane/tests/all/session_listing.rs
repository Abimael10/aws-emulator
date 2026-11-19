// SPDX-License-Identifier: CC0-1.0

//! `ListSessions`: the sessions STORED under one workflow run, ordered and paged.
//!
//! The rule these cases assert is not "this one request answers these bytes": the listing
//! reports the sessions held under the workflow run the path names — each summary carrying
//! that session's own stored id — ordered by the instant the session was created with its
//! id as the tie-break, in pages of at most `maxResults`, each page resuming strictly
//! after the last session the previous one emitted; a definition or a run this store does
//! not hold is refused with 404 rather than answered with an empty page.
//!
//! So the tables below vary what that rule generalises over — how many sessions are stored
//! and under which run and definition, page size (every published bound and one past it),
//! sort order, which status the parent run holds, how a resumption token is damaged, and
//! which parent is missing — and the recorded "create a session and list it under its
//! workflow run" case is one row among them.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_novaact_amazonnovaagentsdataplane::state::WorkflowRunStatus;

use crate::workflow_lifecycle::{client_token, is_lowercase_uuid, text, Emulator};

/// The definition most cases list the sessions under.
const DEFINITION: &str = "session-listing";

/// A second definition, so a case can prove the listing is scoped to the run the path
/// names rather than to the store.
const OTHER_DEFINITION: &str = "other_sessions-1";

/// `MaxResults`' modelled range is 1..100
/// (`smithy_query(query="shape",shape="MaxResults")`).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 100;

/// The byte that separates the fields inside a `nextToken` this operation issues.
const TOKEN_SEPARATOR: char = '\u{1f}';

/// A well-formed `UuidString`
/// (`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`) nothing was minted for.
const ABSENT_UUID: &str = "00000000-0000-4000-8000-000000000000";

/// How long a case waits for a transition the clock owes before calling it a failure. Far
/// longer than any span this service schedules, so a loaded host cannot fail it.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One `ListSessions` call: `maxResults` and `nextToken` are `@httpQuery` members and
/// `sortOrder` travels in the body, on `POST` against the run's own path
/// (`smithy_query(query="operation",operation="ListSessions")`).
async fn list(
    emulator: &Emulator,
    definition: &str,
    run_id: &str,
    sort_order: Option<&str>,
    max_results: Option<i64>,
    next_token: Option<&str>,
) -> (u16, Value) {
    let mut query: Vec<String> = Vec::new();
    if let Some(max_results) = max_results {
        query.push(format!("maxResults={max_results}"));
    }
    if let Some(token) = next_token {
        query.push(format!("nextToken={token}"));
    }
    let base = format!("/workflow-definitions/{definition}/workflow-runs/{run_id}");
    let path = if query.is_empty() {
        base
    } else {
        format!("{base}?{}", query.join("&"))
    };
    let body = match sort_order {
        Some(order) => json!({ "sortOrder": order }),
        None => json!({}),
    };
    emulator.call("POST", &path, Some(body)).await
}

/// The summaries of one page, refused when the required member is missing or not a list.
fn summaries(body: &Value) -> Vec<Value> {
    body["sessionSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("sessionSummaries must be a list in {body}"))
        .clone()
}

/// The `sessionId` of every summary in one page, in the order the page carries them.
fn page_session_ids(body: &Value) -> Vec<String> {
    summaries(body)
        .iter()
        .map(|summary| text(summary, "sessionId"))
        .collect()
}

/// Create `definition` through the real wire call.
async fn create_definition(emulator: &Emulator, definition: &str) {
    let (status, body) = emulator.create_definition(definition).await;
    assert_eq!(
        status, 201,
        "case create definition {definition}: expected HTTP 201, got {status} {body}"
    );
}

/// Create one run under `definition` through the real wire call.
async fn create_run(emulator: &Emulator, definition: &str) -> String {
    let (status, body) = emulator.create_run(definition).await;
    assert_eq!(
        status, 201,
        "case create run under {definition}: expected HTTP 201, got {status} {body}"
    );
    text(&body, "workflowRunId")
}

/// Create `count` sessions under one run, each through the real `CreateSession` wire call,
/// and answer the ids it minted in the order they were created.
async fn create_sessions(
    emulator: &Emulator,
    definition: &str,
    run_id: &str,
    count: usize,
) -> Vec<String> {
    let mut created: Vec<String> = Vec::new();
    for index in 0..count {
        let (status, body) = emulator.create_session(definition, run_id).await;
        assert_eq!(
            status, 201,
            "case create session {index} under {definition}/{run_id}: expected HTTP 201, got \
             {status} {body}"
        );
        created.push(text(&body, "sessionId"));
    }
    created
}

/// The instant the store holds for one session, which is what the listing orders by.
fn stored_created_at(
    emulator: &Emulator,
    definition: &str,
    run_id: &str,
    session_id: &str,
) -> std::time::SystemTime {
    let store = emulator.store();
    let store = store.lock();
    store
        .definition(definition)
        .and_then(|definition| definition.run(run_id))
        .and_then(|run| run.session(session_id))
        .unwrap_or_else(|missing| {
            panic!("session {session_id} must be stored, got {missing:?}");
        })
        .created_at
}

/// One whole listing to page through: which run's sessions, in which order and page size,
/// how big a page that request is answered in, and how many sessions the run holds.
struct Drain<'a> {
    definition: &'a str,
    run_id: &'a str,
    label: &'a str,
    sort_order: Option<&'a str>,
    max_results: Option<i64>,
    expected_page_size: usize,
    stored: usize,
}

/// Page through the whole listing, asserting the paging invariants on the way: no page is
/// bigger than the page size, every page carrying a token is full, and the token satisfies
/// `NextToken`'s published constraints.
async fn drain(emulator: &Emulator, request: Drain<'_>) -> Vec<String> {
    let Drain {
        definition,
        run_id,
        label,
        sort_order,
        max_results,
        expected_page_size,
        stored,
    } = request;
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One page more than the store can hold: a listing that does not terminate is a bug,
    // not something to loop forever on.
    for page_index in 0..=stored + 1 {
        let (status, body) = list(
            emulator,
            definition,
            run_id,
            sort_order,
            max_results,
            token.as_deref(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label}: page {page_index} expected HTTP 200, got {status} {body}"
        );
        let ids = page_session_ids(&body);
        assert!(
            ids.len() <= expected_page_size,
            "case {label}: page {page_index} must hold at most {expected_page_size} summaries, \
             got {ids:?}"
        );
        collected.extend(ids.iter().cloned());
        match body["nextToken"].as_str() {
            Some(next) => {
                assert_eq!(
                    ids.len(),
                    expected_page_size,
                    "case {label}: page {page_index} carries a nextToken, so it must be full, \
                     got {ids:?}"
                );
                assert!(
                    !next.is_empty() && next.len() <= 2048 && !next.contains(char::is_whitespace),
                    "case {label}: page {page_index} nextToken must satisfy NextToken's length \
                     1..2048 and ^\\S*$, got {next:?}"
                );
                token = Some(next.to_string());
            }
            None => return collected,
        }
    }
    panic!("case {label}: the listing never ended, collected {collected:?}");
}

/// Every page size and both orders answer the same sessions of the run the path names, in
/// the order the request asked for, split into pages of the size it asked for.
#[tokio::test]
async fn page_size_and_sort_order_table() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    create_definition(&emulator, OTHER_DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;
    let sibling_run = create_run(&emulator, DEFINITION).await;
    let foreign_run = create_run(&emulator, OTHER_DEFINITION).await;

    let created = create_sessions(&emulator, DEFINITION, &run, 7).await;
    // Sessions of another run — of this definition and of another one — are never part of
    // this listing, whatever page it is.
    let elsewhere = {
        let mut elsewhere = create_sessions(&emulator, DEFINITION, &sibling_run, 3).await;
        elsewhere.extend(create_sessions(&emulator, OTHER_DEFINITION, &foreign_run, 2).await);
        elsewhere
    };
    let stored = created.len();

    // The canonical order is the listing's own: the sessions ordered by the instant each
    // was created with the id as the tie-break. It is asserted to BE that order below
    // rather than assumed, because two sessions minted inside one clock tick are separated
    // by the tie-break and not by creation order.
    let (status, whole) = list(&emulator, DEFINITION, &run, None, None, None).await;
    assert_eq!(
        status, 200,
        "the unpaged listing must answer 200, got {status} {whole}"
    );
    let ascending = page_session_ids(&whole);
    assert_eq!(
        ascending.iter().cloned().collect::<BTreeSet<String>>(),
        created.iter().cloned().collect::<BTreeSet<String>>(),
        "case the whole listing: exactly the sessions created under this run are reported, got \
         {whole}"
    );
    for id in &elsewhere {
        assert!(
            !ascending.contains(id),
            "case scoping: session {id} belongs to another run and must not be listed, got {whole}"
        );
    }
    let keys: Vec<(std::time::SystemTime, String)> = ascending
        .iter()
        .map(|id| (stored_created_at(&emulator, DEFINITION, &run, id), id.clone()))
        .collect();
    for pair in keys.windows(2) {
        assert!(
            pair[0] < pair[1],
            "case ascending order: {:?} must sort before {:?}, got {whole}",
            pair[0],
            pair[1]
        );
    }
    let descending: Vec<String> = ascending.iter().rev().cloned().collect();

    // (label, sortOrder, maxResults, the page size that request is answered in)
    let cases: Vec<(&str, Option<&str>, Option<i64>, usize)> = vec![
        ("no sortOrder, no maxResults", None, None, stored),
        (
            "no sortOrder, the smallest page",
            None,
            Some(MAX_RESULTS_MIN),
            1,
        ),
        (
            "ascending, one page per session",
            Some("Ascending"),
            Some(1),
            1,
        ),
        ("ascending, two per page", Some("Ascending"), Some(2), 2),
        (
            "ascending, a page size that does not divide the store",
            Some("Ascending"),
            Some(3),
            3,
        ),
        (
            "ascending, exactly the store",
            Some("Ascending"),
            Some(stored as i64),
            stored,
        ),
        (
            "ascending, one page larger than the store",
            Some("Ascending"),
            Some(stored as i64 + 1),
            stored,
        ),
        (
            "ascending, the largest page",
            Some("Ascending"),
            Some(MAX_RESULTS_MAX),
            stored,
        ),
        (
            "descending, one page per session",
            Some("Descending"),
            Some(1),
            1,
        ),
        ("descending, two per page", Some("Descending"), Some(2), 2),
        (
            "descending, a page size that does not divide the store",
            Some("Descending"),
            Some(4),
            4,
        ),
        (
            "descending, the largest page",
            Some("Descending"),
            Some(MAX_RESULTS_MAX),
            stored,
        ),
        ("descending, no maxResults", Some("Descending"), None, stored),
    ];

    for (label, sort_order, max_results, page_size) in cases {
        let collected = drain(
            &emulator,
            Drain {
                definition: DEFINITION,
                run_id: &run,
                label,
                sort_order,
                max_results,
                expected_page_size: page_size.min(stored),
                stored,
            },
        )
        .await;
        let expected = if sort_order == Some("Descending") {
            &descending
        } else {
            &ascending
        };
        assert_eq!(
            &collected, expected,
            "case {label}: the whole listing must be every stored session of this run in the \
             requested order, got {collected:?}"
        );
    }

    // And each sibling run reports its own sessions, so the scoping is a rule about the
    // run the path names and not about which run happens to be listed first.
    for (definition, run_id, expected) in [
        (DEFINITION, &sibling_run, &elsewhere[..3]),
        (OTHER_DEFINITION, &foreign_run, &elsewhere[3..]),
    ] {
        let collected = drain(
            &emulator,
            Drain {
                definition,
                run_id,
                label: "a sibling run reports its own sessions",
                sort_order: Some("Ascending"),
                max_results: Some(1),
                expected_page_size: 1,
                stored: expected.len(),
            },
        )
        .await;
        assert_eq!(
            collected.iter().cloned().collect::<BTreeSet<String>>(),
            expected.iter().cloned().collect::<BTreeSet<String>>(),
            "case {definition}/{run_id}: exactly that run's sessions are reported, got \
             {collected:?}"
        );
    }
}

/// A run holding no sessions answers the required list, empty, and invites no page that
/// does not exist.
#[tokio::test]
async fn a_run_with_no_sessions_answers_an_empty_page() {
    let emulator = Emulator::new("eu-west-1");
    create_definition(&emulator, DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;

    for (label, sort_order, max_results) in [
        ("no members", None, None),
        ("ascending", Some("Ascending"), Some(1)),
        ("descending", Some("Descending"), Some(MAX_RESULTS_MAX)),
    ] {
        let (status, body) = list(&emulator, DEFINITION, &run, sort_order, max_results, None).await;
        assert_eq!(
            status, 200,
            "case {label}: expected HTTP 200, got {status} {body}"
        );
        assert_eq!(
            page_session_ids(&body),
            Vec::<String>::new(),
            "case {label}: a run with no sessions reports no summaries, got {body}"
        );
        assert_eq!(
            body["nextToken"],
            Value::Null,
            "case {label}: there is no further page to invite, got {body}"
        );
    }
}

/// Every listed id is a session this service minted and still holds: it has `UuidString`'s
/// form, it is the id `CreateSession` answered with, and it resolves as a parent — the act
/// created under it is accepted and read back under that same session.
#[tokio::test]
async fn each_listed_session_is_the_stored_one() {
    let emulator = Emulator::new("ap-southeast-2");
    create_definition(&emulator, DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;
    let created = create_sessions(&emulator, DEFINITION, &run, 3).await;

    let (status, listing) = list(&emulator, DEFINITION, &run, None, None, None).await;
    assert_eq!(
        status, 200,
        "the listing must answer 200, got {status} {listing}"
    );
    let listed = page_session_ids(&listing);
    assert_eq!(
        listed.iter().cloned().collect::<BTreeSet<String>>(),
        created.iter().cloned().collect::<BTreeSet<String>>(),
        "every created session must be listed exactly once, got {listing}"
    );
    assert_eq!(
        summaries(&listing)
            .iter()
            .filter(|summary| summary.as_object().is_some_and(|members| members.len() == 1))
            .count(),
        listed.len(),
        "each summary carries the one member SessionSummary models, got {listing}"
    );

    for session_id in &listed {
        assert!(
            is_lowercase_uuid(session_id),
            "case {session_id}: a listed session id has UuidString's form, got {listing}"
        );
        let (status, act) = emulator
            .create_act(DEFINITION, &run, session_id, "read the listed session back")
            .await;
        assert_eq!(
            status, 201,
            "case {session_id}: the listed session must resolve as a parent, got {status} {act}"
        );
        let act_id = text(&act, "actId");
        // `ListActs` selects by `workflowRunId` and `sessionId` as `@httpQuery` members
        // (`smithy_query(query="operation",operation="ListActs")`), so this reads back the
        // acts held under the listed session itself.
        let (status, acts) = emulator
            .call(
                "POST",
                &format!(
                    "/workflow-definitions/{DEFINITION}/acts\
                     ?workflowRunId={run}&sessionId={session_id}"
                ),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {session_id}: the acts of the listed session must read back, got {status} {acts}"
        );
        let under_session: Vec<String> = acts["actSummaries"]
            .as_array()
            .unwrap_or_else(|| panic!("actSummaries must be a list in {acts}"))
            .iter()
            .map(|summary| text(summary, "actId"))
            .collect();
        assert_eq!(
            under_session,
            vec![act_id.clone()],
            "case {session_id}: the act created under the listed session is held there, got {acts}"
        );
    }
}

/// The sessions of a run are reported whatever status the run holds: the listing reports
/// what is STORED under the run, and a run's status is not what makes its sessions
/// readable.
#[tokio::test]
async fn the_listing_reports_sessions_under_a_run_in_any_status() {
    let emulator = Emulator::new("us-west-2");
    create_definition(&emulator, DEFINITION).await;

    // Rows: every status `WorkflowRunStatus` declares
    // (`smithy_query(query="shape",shape="WorkflowRunStatus")`).
    let rows: [WorkflowRunStatus; 5] = [
        WorkflowRunStatus::Running,
        WorkflowRunStatus::Succeeded,
        WorkflowRunStatus::Failed,
        WorkflowRunStatus::TimedOut,
        WorkflowRunStatus::Deleting,
    ];

    let mut runs: Vec<(WorkflowRunStatus, String, Vec<String>)> = Vec::new();
    for (slot, status) in rows.iter().enumerate() {
        let run = create_run(&emulator, DEFINITION).await;
        // Sessions are created while the run is still running, because a run that is not
        // running refuses a new session; the status is then set on the stored run, which is
        // the state a listing of a finished run reads.
        let sessions = create_sessions(&emulator, DEFINITION, &run, slot + 1).await;
        {
            let store = emulator.store();
            let mut store = store.lock();
            let stored = store
                .workflow_definitions
                .get_mut(DEFINITION)
                .and_then(|definition| definition.runs.get_mut(&run))
                .unwrap_or_else(|| panic!("case {slot}: the run must be stored"));
            stored.status = *status;
        }
        runs.push((*status, run, sessions));
    }

    for (status, run, sessions) in &runs {
        let wire = status.as_wire();
        let (code, body) = list(&emulator, DEFINITION, run, None, None, None).await;
        assert_eq!(
            code, 200,
            "case a run in {wire}: expected HTTP 200, got {code} {body}"
        );
        assert_eq!(
            page_session_ids(&body)
                .iter()
                .cloned()
                .collect::<BTreeSet<String>>(),
            sessions.iter().cloned().collect::<BTreeSet<String>>(),
            "case a run in {wire}: its stored sessions are reported, got {body}"
        );
    }
}

/// A parent this account's store does not hold is refused, naming the resource that was not
/// found — never answered with an empty page. A run of another definition, and a definition
/// whose deletion has fallen due, are in exactly that position.
#[tokio::test]
async fn a_parent_the_store_does_not_hold_is_refused() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    create_definition(&emulator, OTHER_DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;
    create_sessions(&emulator, DEFINITION, &run, 2).await;
    let foreign_run = create_run(&emulator, OTHER_DEFINITION).await;
    create_sessions(&emulator, OTHER_DEFINITION, &foreign_run, 1).await;

    let account = engine::EMULATOR_ACCOUNT_ID;
    // Rows: (label, the definition named, the run named, the resource the refusal reports,
    // its ARN path). Each row names a parent the store does not hold under the path given.
    let cases: Vec<(&str, String, String, &str, String)> = vec![
        (
            "a definition that was never created",
            "never_created".to_string(),
            run.clone(),
            "workflow-definition",
            "workflow-definition/never_created".to_string(),
        ),
        (
            "the shortest name no definition holds",
            "9".to_string(),
            run.clone(),
            "workflow-definition",
            "workflow-definition/9".to_string(),
        ),
        (
            "the longest name no definition holds",
            "A_0123456789_0123456789_0123456789_0123".to_string(),
            run.clone(),
            "workflow-definition",
            "workflow-definition/A_0123456789_0123456789_0123456789_0123".to_string(),
        ),
        (
            "a run id nothing was minted for",
            DEFINITION.to_string(),
            ABSENT_UUID.to_string(),
            "workflow-run",
            format!("workflow-definition/{DEFINITION}/workflow-run/{ABSENT_UUID}"),
        ),
        (
            "a run of another definition",
            DEFINITION.to_string(),
            foreign_run.clone(),
            "workflow-run",
            format!("workflow-definition/{DEFINITION}/workflow-run/{foreign_run}"),
        ),
        (
            "this definition's run under the other definition",
            OTHER_DEFINITION.to_string(),
            run.clone(),
            "workflow-run",
            format!("workflow-definition/{OTHER_DEFINITION}/workflow-run/{run}"),
        ),
    ];

    for (label, definition, run_id, resource_type, resource_path) in cases {
        let (status, body) = list(&emulator, &definition, &run_id, None, None, None).await;
        assert_eq!(
            status, 404,
            "case {label}: expected HTTP 404, got {status} {body}"
        );
        assert_eq!(
            body["__type"], "ResourceNotFoundException",
            "case {label}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["resourceType"],
            json!(resource_type),
            "case {label}: the missing resource is the parent the path named, got {body}"
        );
        assert_eq!(
            body["resourceId"],
            json!(format!("arn:aws:nova-act:us-east-1:{account}:{resource_path}")),
            "case {label}: the refusal identifies the resource by its ARN, got {body}"
        );
    }

    // While the delete is still pending the definition is held, so its run's sessions are
    // still listed; once the removal falls due the listing refuses like any other absent
    // definition.
    let (status, body) = emulator
        .call("DELETE", &format!("/workflow-definitions/{DEFINITION}"), None)
        .await;
    assert_eq!(status, 202, "the delete must answer 202, got {status} {body}");

    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = list(&emulator, DEFINITION, &run, None, None, None).await;
        match status {
            404 => {
                assert_eq!(
                    body["__type"], "ResourceNotFoundException",
                    "case after the deleting span: expected ResourceNotFoundException, got {body}"
                );
                break;
            }
            200 => assert_eq!(
                page_session_ids(&body).len(),
                2,
                "case while deleting: the definition is still held, so its sessions are still \
                 listed, got {body}"
            ),
            other => panic!("case deleting: expected HTTP 200 or 404, got {other} {body}"),
        }
        assert!(
            Instant::now() < deadline,
            "case after the deleting span: the definition's removal never fell due"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A page size outside `MaxResults`' published range is refused by the modelled constraint,
/// never clamped into an answer.
#[tokio::test]
async fn page_size_outside_the_modeled_range_is_refused() {
    let emulator = Emulator::new("us-west-2");
    create_definition(&emulator, DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;
    let created = create_sessions(&emulator, DEFINITION, &run, 1).await;

    for (label, max_results) in [
        ("one below the minimum", MAX_RESULTS_MIN - 1),
        ("one above the maximum", MAX_RESULTS_MAX + 1),
    ] {
        let (status, body) = list(
            &emulator,
            DEFINITION,
            &run,
            None,
            Some(max_results),
            None,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label} ({max_results}): expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            body["__type"], "ValidationException",
            "case {label} ({max_results}): expected ValidationException, got {body}"
        );
    }

    for (label, max_results) in [
        ("the published minimum", MAX_RESULTS_MIN),
        ("the published maximum", MAX_RESULTS_MAX),
    ] {
        let (status, body) = list(
            &emulator,
            DEFINITION,
            &run,
            None,
            Some(max_results),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label} ({max_results}): expected HTTP 200, got {status} {body}"
        );
        assert_eq!(
            page_session_ids(&body),
            created,
            "case {label} ({max_results}): the one stored session must be reported, got {body}"
        );
    }
}

/// A resumption carries a POSITION, not an offset: the page after a token holds the
/// sessions that sort after it, including one created after the token was issued.
#[tokio::test]
async fn a_page_resumes_after_its_cursor() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;
    let created = create_sessions(&emulator, DEFINITION, &run, 4).await;

    let (status, first) = list(
        &emulator,
        DEFINITION,
        &run,
        Some("Ascending"),
        Some(2),
        None,
    )
    .await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {first}");
    assert_eq!(
        page_session_ids(&first),
        created[..2].to_vec(),
        "case the first page: the two oldest sessions, oldest first, got {first}"
    );
    let token = text(&first, "nextToken");

    // A brand-new session is the youngest, so it belongs on the LAST page of an ascending
    // listing — after the cursor, hence in the resumption.
    let later = create_sessions(&emulator, DEFINITION, &run, 1).await;

    let (status, second) = list(
        &emulator,
        DEFINITION,
        &run,
        Some("Ascending"),
        Some(2),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {second}");
    assert_eq!(
        page_session_ids(&second),
        created[2..].to_vec(),
        "case resumed after the first page: got {second}"
    );
    let token = text(&second, "nextToken");

    let (status, third) = list(
        &emulator,
        DEFINITION,
        &run,
        Some("Ascending"),
        Some(2),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {third}");
    assert_eq!(
        page_session_ids(&third),
        later,
        "case the session created mid-listing: got {third}"
    );
    assert_eq!(
        third["nextToken"],
        Value::Null,
        "case the last page: no further page is invited, got {third}"
    );
}

/// A `nextToken` this operation did not issue for this listing is refused with the modelled
/// `ValidationException`, naming the member at fault — never answered with a page from some
/// other position.
#[tokio::test]
async fn a_token_this_listing_did_not_issue_is_refused() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;
    let created = create_sessions(&emulator, DEFINITION, &run, 3).await;

    let (_, first) = list(
        &emulator,
        DEFINITION,
        &run,
        Some("Ascending"),
        Some(1),
        None,
    )
    .await;
    let ascending_token = text(&first, "nextToken");

    let cases: Vec<(&str, String)> = vec![
        ("not hex at all", "not-a-token".to_string()),
        (
            "hex of nothing this operation encodes",
            hex::encode(ABSENT_UUID),
        ),
        (
            "a field missing",
            hex::encode(format!("Ascending{TOKEN_SEPARATOR}1.000000000")),
        ),
        (
            "a field too many",
            hex::encode(format!(
                "Ascending{TOKEN_SEPARATOR}1.000000000{TOKEN_SEPARATOR}{ABSENT_UUID}\
                 {TOKEN_SEPARATOR}x"
            )),
        ),
        ("an order it was not issued for", ascending_token.clone()),
        (
            "an unreadable instant",
            hex::encode(format!(
                "Descending{TOKEN_SEPARATOR}not-an-instant{TOKEN_SEPARATOR}{ABSENT_UUID}"
            )),
        ),
        (
            "a nanosecond field out of range",
            hex::encode(format!(
                "Descending{TOKEN_SEPARATOR}1.1000000000{TOKEN_SEPARATOR}{ABSENT_UUID}"
            )),
        ),
        (
            "an instant past what a clock can name",
            hex::encode(format!(
                "Descending{TOKEN_SEPARATOR}{}.000000000{TOKEN_SEPARATOR}{ABSENT_UUID}",
                u64::MAX
            )),
        ),
        (
            "a key outside UuidString's pattern",
            hex::encode(format!(
                "Descending{TOKEN_SEPARATOR}1.000000000{TOKEN_SEPARATOR}not-a-session-id"
            )),
        ),
        (
            "a key with an upper-case hex digit",
            hex::encode(format!(
                "Descending{TOKEN_SEPARATOR}1.000000000{TOKEN_SEPARATOR}\
                 00000000-0000-4000-8000-00000000000A"
            )),
        ),
    ];

    for (label, token) in cases {
        // Every row is sent on a Descending listing, so the Ascending token above is a
        // token for the other order.
        let (status, body) = list(
            &emulator,
            DEFINITION,
            &run,
            Some("Descending"),
            Some(1),
            Some(&token),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            body["__type"], "ValidationException",
            "case {label:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            body["reason"], "FieldValidationFailed",
            "case {label:?}: a broken field rule is reported as FieldValidationFailed, got {body}"
        );
        assert_eq!(
            body["fieldList"][0]["name"], "nextToken",
            "case {label:?}: the refusal names the member at fault, got {body}"
        );
        assert!(
            !text(&body, "message").is_empty(),
            "case {label:?}: the refusal says what was wrong, got {body}"
        );
    }

    // And the token it WAS issued for still pages that listing.
    let (status, body) = list(
        &emulator,
        DEFINITION,
        &run,
        Some("Ascending"),
        Some(1),
        Some(&ascending_token),
    )
    .await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {body}");
    assert_eq!(
        page_session_ids(&body),
        vec![created[1].clone()],
        "case the token it was issued for: got {body}"
    );
}

/// A `sortOrder` outside the model's enum is refused, never answered in some other order.
#[tokio::test]
async fn a_sort_order_outside_the_enum_is_refused() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;
    create_sessions(&emulator, DEFINITION, &run, 1).await;

    // `SortOrder`'s values are `Ascending | Descending`
    // (`smithy_query(query="shape",shape="SortOrder")`), so each of these is outside it —
    // including the spellings a caller might expect to be accepted.
    for label in ["ASCENDING", "ascending", "Newest", ""] {
        let (status, body) = list(&emulator, DEFINITION, &run, Some(label), None, None).await;
        assert_eq!(
            status, 400,
            "case sortOrder {label:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            body["__type"], "ValidationException",
            "case sortOrder {label:?}: expected ValidationException, got {body}"
        );
    }
}

/// A session created with a `clientToken` already seen is the SAME session, so replaying the
/// create adds no second entry to the listing.
#[tokio::test]
async fn an_idempotent_create_adds_one_entry_to_the_listing() {
    let emulator = Emulator::new("eu-central-1");
    create_definition(&emulator, DEFINITION).await;
    let run = create_run(&emulator, DEFINITION).await;
    let path = format!("/workflow-definitions/{DEFINITION}/workflow-runs/{run}/sessions");

    // Rows: distinct `clientToken`s, each sent twice.
    let tokens = [client_token("listing-a"), client_token("listing-b")];
    let mut minted: Vec<String> = Vec::new();
    for token in &tokens {
        let mut answered: Vec<String> = Vec::new();
        for attempt in 0..2 {
            let (status, body) = emulator
                .call("PUT", &path, Some(json!({ "clientToken": token })))
                .await;
            assert_eq!(
                status, 201,
                "case {token} attempt {attempt}: expected HTTP 201, got {status} {body}"
            );
            answered.push(text(&body, "sessionId"));
        }
        assert_eq!(
            answered[0], answered[1],
            "case {token}: the same token replays the session it created, got {answered:?}"
        );
        minted.push(answered[0].clone());
    }

    let (status, body) = list(&emulator, DEFINITION, &run, Some("Ascending"), None, None).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {body}");
    assert_eq!(
        page_session_ids(&body)
            .iter()
            .cloned()
            .collect::<BTreeSet<String>>(),
        minted.iter().cloned().collect::<BTreeSet<String>>(),
        "each distinct clientToken contributes exactly one session to the listing, got {body}"
    );
}
