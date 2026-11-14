// SPDX-License-Identifier: CC0-1.0

//! `ListWorkflowRuns`: the runs STORED under one workflow definition, ordered and paged.
//!
//! The rule these cases assert is not "this one request answers these bytes": the listing
//! reports the workflow runs held under the definition the path names, each summary member
//! read off the stored run (its ARN, its minted id, the status it currently holds, the
//! instant it started, the instant it ended — which only a finished run has — and the
//! location of its trace), ordered by the one instant the summary carries (`startedAt`) in
//! the requested `sortOrder` with the run id as the tie-break, in pages of at most
//! `maxResults`, each page resuming strictly after the last run the previous one emitted; a
//! definition the store does not hold is refused with 404.
//!
//! So the tables below vary what that rule generalises over — how many runs are stored and
//! under which definition, page size (every published bound and one past it), sort order,
//! which status each run holds and whether it has finished, whether a trace was recorded,
//! how a resumption token is damaged, and whether the definition exists at all — and the
//! recorded "list the runs after a status transition" case is one row among them.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_novaact_amazonnovaagentsdataplane::ops::types::TraceLocation;
use service_com_amazonaws_novaact_amazonnovaagentsdataplane::state::WorkflowRunStatus;

use crate::workflow_lifecycle::{text, Emulator};

/// The definition most cases list the runs of.
const DEFINITION: &str = "run-listing";

/// A second definition, so a case can prove the listing is scoped to the definition the
/// path names rather than to the store.
const OTHER_DEFINITION: &str = "other_runs-1";

/// `MaxResults`' modelled range is 1..100
/// (`smithy_query(query="shape",shape="MaxResults")`).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 100;

/// The byte that separates the fields inside a `nextToken` this operation issues.
const TOKEN_SEPARATOR: char = '\u{1f}';

/// A well-formed `UuidString`
/// (`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`) no run was minted for.
const ABSENT_UUID: &str = "00000000-0000-4000-8000-000000000000";

/// How long a case waits for a transition the clock owes before calling it a failure. Far
/// longer than any span this service schedules, so a loaded host cannot fail it.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One `ListWorkflowRuns` call: `maxResults` and `nextToken` are `@httpQuery` members and
/// `sortOrder` travels in the body
/// (`smithy_query(query="operation",operation="ListWorkflowRuns")`).
async fn list(
    emulator: &Emulator,
    definition: &str,
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
    let base = format!("/workflow-definitions/{definition}/workflow-runs");
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
    body["workflowRunSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("workflowRunSummaries must be a list in {body}"))
        .clone()
}

/// The `workflowRunId` of every summary in one page, in the order the page carries them.
fn page_run_ids(body: &Value) -> Vec<String> {
    summaries(body)
        .iter()
        .map(|summary| text(summary, "workflowRunId"))
        .collect()
}

/// A modelled timestamp member as epoch seconds, from either wire form a Smithy timestamp
/// can take on restJson1 (this model's `DateTimestamp` carries `@timestampFormat`
/// `date-time`, so it is the RFC 3339 string).
fn timestamp_seconds(summary: &Value, member: &str) -> f64 {
    match &summary[member] {
        Value::Number(seconds) => seconds.as_f64().expect("a finite epoch-second value"),
        Value::String(rendered) => ::aws_smithy_types::DateTime::from_str(
            rendered,
            ::aws_smithy_types::date_time::Format::DateTime,
        )
        .unwrap_or_else(|error| panic!("member {member} must be an RFC 3339 date-time: {error}"))
        .as_secs_f64(),
        other => panic!("member {member} must be a timestamp, got {other}"),
    }
}

/// Create `count` runs under `definition`, each through the real `CreateWorkflowRun` wire
/// call, and answer the ids in the order they were created.
async fn create_runs(emulator: &Emulator, definition: &str, count: usize) -> Vec<String> {
    let mut created: Vec<String> = Vec::new();
    for index in 0..count {
        let (status, body) = emulator.create_run(definition).await;
        assert_eq!(
            status, 201,
            "case create run {index} under {definition}: expected HTTP 201, got {status} {body}"
        );
        created.push(text(&body, "workflowRunId"));
    }
    created
}

/// Create `definition` through the real wire call.
async fn create_definition(emulator: &Emulator, definition: &str) {
    let (status, body) = emulator.create_definition(definition).await;
    assert_eq!(
        status, 201,
        "case create definition {definition}: expected HTTP 201, got {status} {body}"
    );
}

/// Page through the whole listing, asserting the paging invariants on the way: no page is
/// bigger than the page size, every page carrying a token is full, and the token satisfies
/// `NextToken`'s published constraints.
async fn drain(
    emulator: &Emulator,
    label: &str,
    sort_order: Option<&str>,
    max_results: Option<i64>,
    expected_page_size: usize,
    stored: usize,
) -> Vec<String> {
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One page more than the store can hold: a listing that does not terminate is a bug,
    // not something to loop forever on.
    for page_index in 0..=stored + 1 {
        let (status, body) = list(
            emulator,
            DEFINITION,
            sort_order,
            max_results,
            token.as_deref(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label}: page {page_index} expected HTTP 200, got {status} {body}"
        );
        let ids = page_run_ids(&body);
        assert!(
            ids.len() <= expected_page_size,
            "case {label}: page {page_index} must hold at most {expected_page_size} summaries, got {ids:?}"
        );
        collected.extend(ids.iter().cloned());
        match body["nextToken"].as_str() {
            Some(next) => {
                assert_eq!(
                    ids.len(),
                    expected_page_size,
                    "case {label}: page {page_index} carries a nextToken, so it must be full, got {ids:?}"
                );
                assert!(
                    !next.is_empty() && next.len() <= 2048 && !next.contains(char::is_whitespace),
                    "case {label}: page {page_index} nextToken must satisfy NextToken's length 1..2048 and ^\\S*$, got {next:?}"
                );
                token = Some(next.to_string());
            }
            None => return collected,
        }
    }
    panic!("case {label}: the listing never ended, collected {collected:?}");
}

/// Every page size and both orders answer the same runs, in the order the request asked
/// for, split into pages of the size it asked for.
#[tokio::test]
async fn page_size_and_sort_order_table() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    create_definition(&emulator, OTHER_DEFINITION).await;
    let created = create_runs(&emulator, DEFINITION, 8).await;
    // Runs of another definition are never part of this listing, whatever page it is.
    let elsewhere = create_runs(&emulator, OTHER_DEFINITION, 3).await;
    let stored = created.len();

    // The canonical order is the listing's own: the runs ordered by the instant each
    // summary reports with the id as the tie-break. It is asserted to BE that order below
    // rather than assumed, because two runs minted inside one clock tick are separated by
    // the tie-break and not by creation order.
    let (status, whole) = list(&emulator, DEFINITION, None, None, None).await;
    assert_eq!(
        status, 200,
        "the unpaged listing must answer 200, got {status} {whole}"
    );
    let ascending = page_run_ids(&whole);
    assert_eq!(
        ascending.iter().cloned().collect::<BTreeSet<String>>(),
        created.iter().cloned().collect::<BTreeSet<String>>(),
        "case the whole listing: exactly the runs created under this definition are reported, got {whole}"
    );
    for id in &elsewhere {
        assert!(
            !ascending.contains(id),
            "case scoping: run {id} belongs to {OTHER_DEFINITION} and must not be listed, got {whole}"
        );
    }
    let listed = summaries(&whole);
    for pair in listed.windows(2) {
        let (earlier, later) = (&pair[0], &pair[1]);
        let earlier_key = (
            timestamp_seconds(earlier, "startedAt"),
            text(earlier, "workflowRunId"),
        );
        let later_key = (
            timestamp_seconds(later, "startedAt"),
            text(later, "workflowRunId"),
        );
        assert!(
            earlier_key < later_key,
            "case ascending order: {earlier_key:?} must sort before {later_key:?}, got {whole}"
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
        ("ascending, one page per run", Some("Ascending"), Some(1), 1),
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
            "descending, one page per run",
            Some("Descending"),
            Some(1),
            1,
        ),
        ("descending, two per page", Some("Descending"), Some(2), 2),
        (
            "descending, a page size that does not divide the store",
            Some("Descending"),
            Some(5),
            5,
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
            label,
            sort_order,
            max_results,
            page_size.min(stored),
            stored,
        )
        .await;
        let expected = if sort_order == Some("Descending") {
            &descending
        } else {
            &ascending
        };
        assert_eq!(
            &collected, expected,
            "case {label}: the whole listing must be every stored run of this definition in the requested order, got {collected:?}"
        );
    }
}

/// A definition holding no runs answers the required list, empty, and invites no page that
/// does not exist.
#[tokio::test]
async fn a_definition_with_no_runs_answers_an_empty_page() {
    let emulator = Emulator::new("eu-west-1");
    create_definition(&emulator, DEFINITION).await;

    for (label, sort_order, max_results) in [
        ("no members", None, None),
        ("ascending", Some("Ascending"), Some(1)),
        ("descending", Some("Descending"), Some(MAX_RESULTS_MAX)),
    ] {
        let (status, body) = list(&emulator, DEFINITION, sort_order, max_results, None).await;
        assert_eq!(
            status, 200,
            "case {label}: expected HTTP 200, got {status} {body}"
        );
        assert_eq!(
            page_run_ids(&body),
            Vec::<String>::new(),
            "case {label}: a definition with no runs reports no summaries, got {body}"
        );
        assert_eq!(
            body["nextToken"],
            Value::Null,
            "case {label}: there is no further page to invite, got {body}"
        );
    }
}

/// Every summary member is the stored run's own: the same ARN, id, status and instants the
/// implemented reader answers with for that run.
#[tokio::test]
async fn each_summary_is_read_off_the_stored_run() {
    let emulator = Emulator::new("ap-southeast-2");
    create_definition(&emulator, DEFINITION).await;
    let created = create_runs(&emulator, DEFINITION, 3).await;

    let (status, listing) = list(&emulator, DEFINITION, None, None, None).await;
    assert_eq!(
        status, 200,
        "the listing must answer 200, got {status} {listing}"
    );
    let listed = summaries(&listing);
    assert_eq!(
        listed.len(),
        created.len(),
        "every created run must be listed, got {listing}"
    );

    for summary in &listed {
        let run_id = text(summary, "workflowRunId");
        let (status, read) = emulator
            .call(
                "GET",
                &format!("/workflow-definitions/{DEFINITION}/workflow-runs/{run_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {run_id}: the listed run must read back, got {status} {read}"
        );
        for member in ["workflowRunArn", "workflowRunId", "status", "startedAt"] {
            assert_eq!(
                summary[member], read[member],
                "case {run_id}: the summary's {member} is the stored one, got {summary} vs {read}"
            );
        }
        assert_eq!(
            summary["endedAt"], read["endedAt"],
            "case {run_id}: a run that has not finished reports no endedAt, got {summary} vs {read}"
        );
        assert_eq!(
            summary["workflowRunArn"],
            json!(format!(
                "arn:aws:nova-act:ap-southeast-2:{}:workflow-definition/{DEFINITION}/workflow-run/{run_id}",
                engine::EMULATOR_ACCOUNT_ID
            )),
            "case {run_id}: the ARN names this service, region, account and the run's place in the chain, got {summary}"
        );
        assert_eq!(
            summary["traceLocation"],
            Value::Null,
            "case {run_id}: nothing has recorded a trace for this run, got {summary}"
        );
    }
}

/// The listing reflects the status each run is in, with `endedAt` exactly on the runs that
/// have finished and `traceLocation` exactly on the runs a trace was recorded for.
///
/// The store is put into each status directly: a chosen end instant and a recorded trace
/// location are not something `UpdateWorkflowRun` (which ends a run at the instant it is
/// called) produces.
#[tokio::test]
async fn the_listing_reflects_each_run_status_transition() {
    let emulator = Emulator::new("us-west-2");
    create_definition(&emulator, DEFINITION).await;

    // Rows: every status `WorkflowRunStatus` declares
    // (`smithy_query(query="shape",shape="WorkflowRunStatus")`), how long after the start
    // the run's end instant was recorded, and whether a trace location was recorded. The
    // non-terminal rows carry an end instant too, so the rule "only a finished run reports
    // an `endedAt`" is tested where the stored record alone would not decide it.
    let rows: [(WorkflowRunStatus, Duration, Option<&str>); 5] = [
        (WorkflowRunStatus::Running, Duration::from_secs(1), None),
        (
            WorkflowRunStatus::Succeeded,
            Duration::from_secs(2),
            Some("s3://nova-act-traces/runs/succeeded"),
        ),
        (WorkflowRunStatus::Failed, Duration::from_millis(400), None),
        (
            WorkflowRunStatus::TimedOut,
            Duration::from_secs(900),
            Some("s3://nova-act-traces/runs/timed-out"),
        ),
        (
            WorkflowRunStatus::Deleting,
            Duration::from_secs(5),
            Some("s3://nova-act-traces/runs/deleting"),
        ),
    ];
    let created = create_runs(&emulator, DEFINITION, rows.len()).await;

    {
        let store = emulator.store();
        let mut store = store.lock();
        for (slot, (status, elapsed, location)) in rows.iter().enumerate() {
            let run = store
                .workflow_definitions
                .get_mut(DEFINITION)
                .and_then(|definition| definition.runs.get_mut(&created[slot]))
                .unwrap_or_else(|| panic!("case {slot}: the run must be stored"));
            run.status = *status;
            run.ended_at = Some(run.started_at + *elapsed);
            run.trace_location = location.map(|location| TraceLocation {
                location_type: "S3".to_string(),
                location: location.to_string(),
            });
        }
    }

    let (status, body) = list(&emulator, DEFINITION, None, None, None).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {body}");
    let listed = summaries(&body);

    for (slot, (run_status, _, location)) in rows.iter().enumerate() {
        let run_id = &created[slot];
        let summary = listed
            .iter()
            .find(|summary| &text(summary, "workflowRunId") == run_id)
            .unwrap_or_else(|| panic!("case {run_id}: the run must be listed, got {body}"));
        let wire = run_status.as_wire();
        assert_eq!(
            summary["status"], wire,
            "case {run_id}: expected the stored status {wire}, got {summary}"
        );
        if run_status.is_terminal() {
            assert!(
                timestamp_seconds(summary, "startedAt") < timestamp_seconds(summary, "endedAt"),
                "case {run_id}: a finished run reports the instant it ended, after its start, got {summary}"
            );
        } else {
            assert_eq!(
                summary["endedAt"],
                Value::Null,
                "case {run_id}: a run in {wire} has not finished, so it reports no endedAt, got {summary}"
            );
        }
        match location {
            Some(expected) => assert_eq!(
                summary["traceLocation"],
                json!({ "locationType": "S3", "location": expected }),
                "case {run_id}: the summary reports the recorded trace location, got {summary}"
            ),
            None => assert_eq!(
                summary["traceLocation"],
                Value::Null,
                "case {run_id}: no trace was recorded for this run, got {summary}"
            ),
        }

        // The reader of the same run agrees with the listing about the transition.
        let (status, read) = emulator
            .call(
                "GET",
                &format!("/workflow-definitions/{DEFINITION}/workflow-runs/{run_id}"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {run_id}: the run must read back, got {status} {read}"
        );
        assert_eq!(
            summary["status"], read["status"],
            "case {run_id}: the listing and the reader report one status, got {summary} vs {read}"
        );
        assert_eq!(
            summary["endedAt"], read["endedAt"],
            "case {run_id}: the listing and the reader report one end instant, got {summary} vs {read}"
        );
    }
}

/// A definition this account's store does not hold is refused, naming the resource that was
/// not found — never answered with an empty page. A definition whose deletion has fallen
/// due is in exactly that position.
#[tokio::test]
async fn a_definition_the_store_does_not_hold_is_refused() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    create_runs(&emulator, DEFINITION, 2).await;

    // Rows: names `WorkflowDefinitionName` admits that name no stored definition.
    for absent in ["never_created", "9", "A_0123456789_0123456789_0123456789_0123"] {
        let (status, body) = list(&emulator, absent, None, None, None).await;
        assert_eq!(
            status, 404,
            "case {absent}: expected HTTP 404, got {status} {body}"
        );
        assert_eq!(
            body["__type"], "ResourceNotFoundException",
            "case {absent}: expected ResourceNotFoundException, got {body}"
        );
        assert_eq!(
            body["resourceType"], "workflow-definition",
            "case {absent}: the missing resource is the definition the path named, got {body}"
        );
        assert_eq!(
            body["resourceId"],
            json!(format!(
                "arn:aws:nova-act:us-east-1:{}:workflow-definition/{absent}",
                engine::EMULATOR_ACCOUNT_ID
            )),
            "case {absent}: the refusal identifies the resource by its ARN, got {body}"
        );
    }

    // While the delete is still pending the definition is held, so its runs are still
    // listed; once the removal falls due the listing refuses like any other absent
    // definition.
    let (status, body) = emulator
        .call("DELETE", &format!("/workflow-definitions/{DEFINITION}"), None)
        .await;
    assert_eq!(status, 202, "the delete must answer 202, got {status} {body}");

    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = list(&emulator, DEFINITION, None, None, None).await;
        match status {
            404 => {
                assert_eq!(
                    body["__type"], "ResourceNotFoundException",
                    "case after the deleting span: expected ResourceNotFoundException, got {body}"
                );
                break;
            }
            200 => assert_eq!(
                page_run_ids(&body).len(),
                2,
                "case while deleting: the definition is still held, so its runs are still listed, got {body}"
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
    let created = create_runs(&emulator, DEFINITION, 1).await;

    for (label, max_results) in [
        ("one below the minimum", MAX_RESULTS_MIN - 1),
        ("one above the maximum", MAX_RESULTS_MAX + 1),
    ] {
        let (status, body) = list(&emulator, DEFINITION, None, Some(max_results), None).await;
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
        let (status, body) = list(&emulator, DEFINITION, None, Some(max_results), None).await;
        assert_eq!(
            status, 200,
            "case {label} ({max_results}): expected HTTP 200, got {status} {body}"
        );
        assert_eq!(
            page_run_ids(&body),
            created,
            "case {label} ({max_results}): the one stored run must be reported, got {body}"
        );
    }
}

/// A resumption carries a POSITION, not an offset: the page after a token holds the runs
/// that sort after it, including one created after the token was issued.
#[tokio::test]
async fn a_page_resumes_after_its_cursor() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    let created = create_runs(&emulator, DEFINITION, 4).await;

    let (status, first) = list(&emulator, DEFINITION, Some("Ascending"), Some(2), None).await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {first}");
    assert_eq!(
        page_run_ids(&first),
        created[..2].to_vec(),
        "case the first page: the two oldest runs, oldest first, got {first}"
    );
    let token = text(&first, "nextToken");

    // A brand-new run is the youngest, so it belongs on the LAST page of an ascending
    // listing — after the cursor, hence in the resumption.
    let later = create_runs(&emulator, DEFINITION, 1).await;

    let (status, second) = list(
        &emulator,
        DEFINITION,
        Some("Ascending"),
        Some(2),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {second}");
    assert_eq!(
        page_run_ids(&second),
        created[2..].to_vec(),
        "case resumed after the first page: got {second}"
    );
    let token = text(&second, "nextToken");

    let (status, third) = list(
        &emulator,
        DEFINITION,
        Some("Ascending"),
        Some(2),
        Some(&token),
    )
    .await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {third}");
    assert_eq!(
        page_run_ids(&third),
        later,
        "case the run created mid-listing: got {third}"
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
    let created = create_runs(&emulator, DEFINITION, 3).await;

    let (_, first) = list(&emulator, DEFINITION, Some("Ascending"), Some(1), None).await;
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
                "Ascending{TOKEN_SEPARATOR}1.000000000{TOKEN_SEPARATOR}{ABSENT_UUID}{TOKEN_SEPARATOR}x"
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
                "Descending{TOKEN_SEPARATOR}1.000000000{TOKEN_SEPARATOR}not-a-run-id"
            )),
        ),
        (
            "a key with an upper-case hex digit",
            hex::encode(format!(
                "Descending{TOKEN_SEPARATOR}1.000000000{TOKEN_SEPARATOR}00000000-0000-4000-8000-00000000000A"
            )),
        ),
    ];

    for (label, token) in cases {
        // Every row is sent on a Descending listing, so the Ascending token above is a
        // token for the other order.
        let (status, body) = list(
            &emulator,
            DEFINITION,
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
        Some("Ascending"),
        Some(1),
        Some(&ascending_token),
    )
    .await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {body}");
    assert_eq!(
        page_run_ids(&body),
        vec![created[1].clone()],
        "case the token it was issued for: got {body}"
    );
}

/// A `sortOrder` outside the model's enum is refused, never answered in some other order.
#[tokio::test]
async fn a_sort_order_outside_the_enum_is_refused() {
    let emulator = Emulator::new("us-east-1");
    create_definition(&emulator, DEFINITION).await;
    create_runs(&emulator, DEFINITION, 1).await;

    // `SortOrder`'s values are `Ascending | Descending`
    // (`smithy_query(query="shape",shape="SortOrder")`), so each of these is outside it —
    // including the spellings a caller might expect to be accepted.
    for label in ["ASCENDING", "ascending", "Newest", ""] {
        let (status, body) = list(&emulator, DEFINITION, Some(label), None, None).await;
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
