// SPDX-License-Identifier: CC0-1.0

//! Athena's NOTEBOOK SESSION listing, driven end to end over the same router the emulator
//! boots (awsJson1_1, `x-amz-target: AmazonAthena.ListNotebookSessions`), so every
//! assertion is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: `ListNotebookSessions` reports, newest first, exactly those sessions
//! this account holds for the notebook the request names that are in an ACTIVE state
//! (CREATING, CREATED, IDLE or BUSY), in pages of at most the ten entries the modelled
//! `NotebookSessionsList` admits, each page but the last followed by a `NextToken` this
//! service minted for THAT notebook's listing and no other value. A `NotebookId` no notebook
//! was minted under names no notebook whose sessions could be listed and is refused with
//! `InvalidRequestException`.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! point of its lifecycle each session is at, which notebook (or none) it was started in,
//! how many sessions the notebook holds, the page size asked for, the shape of the id that
//! names no notebook, and the form of a continuation token a caller sends back.
//!
//! Nothing here waits on the wall clock: a session that must be at a given point of its
//! lifecycle is seeded with the instants that put it there, and every row's expectation
//! holds whether or not the emulator's own creation and shutdown spans elapse mid-run.
//! The sessions are written straight into this service's store through its own
//! [`SessionRecord`] — seeding is what lets a row place a session at an exact point of its
//! lifecycle, which starting one over the wire cannot — and the listing is read back over
//! the wire.

use std::cmp::Reverse;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    self, page_token_malformed, SessionRecord, ERROR_CODE_INVALID_INPUT,
    ERROR_CODE_NOTEBOOK_NOT_FOUND, NOTEBOOK_TYPE_IPYNB,
};
use service_common::page_tokens::AUTHENTICATOR_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, engine_and_router, error_code, message};

/// The Spark enabled workgroup the notebooks below live in, and the one the seeded sessions
/// run in.
const WORK_GROUP: &str = "spark-notebooks";

/// The PySpark engine version a Spark enabled workgroup selects, which is what makes the
/// workgroup one a notebook can be imported into.
const PYSPARK_ENGINE_VERSION: &str = "PySpark engine version 3";

/// A span long enough that a session seeded as started long ago stays past every span this
/// emulator owes, for as long as any test run takes: no assertion here depends on how fast
/// the host runs.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// The longest list `ListNotebookSessions` can answer with: the modelled
/// `NotebookSessionsList` length (0..10), which bounds a page whatever `MaxResults` asks
/// for.
const LIST_CEILING: usize = 10;

/// The ceiling of the modelled `MaxSessionsCount` range (1..100) a request may ask for.
const MODELLED_MAX_RESULTS: usize = 100;

/// Create the Spark enabled workgroup the notebooks are imported into.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {WORK_GROUP} must be created, got {status} with {created:?}"
    );
}

/// Import an ipynb notebook called `name` and hand back the `NotebookId` it minted.
async fn given_notebook(router: &MultiServiceRouter, name: &str) -> String {
    let (status, imported) = call(
        router,
        "ImportNotebook",
        &json!({
            "WorkGroup": WORK_GROUP,
            "Name": name,
            "Type": NOTEBOOK_TYPE_IPYNB,
            "Payload": "{\"cells\": []}",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be imported, got {status} with {imported:?}"
    );
    imported["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("an import must answer with a NotebookId, got {imported:?}"))
        .to_string()
}

/// Where in its lifecycle a seeded session sits.
#[derive(Clone, Copy, Debug)]
enum Lifecycle {
    /// Started at the seeded instant and nothing else: a session started NOW is still
    /// acquiring its resources (CREATING), one started long ago has them (IDLE). Both are
    /// ACTIVE, so a row using it expects the same answer however much of the emulator's
    /// creation span has elapsed.
    Started,
    /// Asked to shut down just now: TERMINATING until the shutdown is complete and
    /// TERMINATED after it, neither of which is an active state.
    ShuttingDown,
    /// Asked to shut down at the seeded instant, long enough ago that the shutdown is
    /// complete: TERMINATED.
    ShutDown,
}

/// Whether a session at this point of its lifecycle is in one of the ACTIVE states a
/// notebook's listing reports.
impl Lifecycle {
    fn active(self) -> bool {
        matches!(self, Self::Started)
    }
}

/// Seed one session into `engine`'s store, started in `notebook_id` at `started_at` and
/// moved to the point of its lifecycle `lifecycle` names, and hand back its `SessionId`.
fn given_session(
    engine: &engine::InMemoryEngine,
    notebook_id: Option<&str>,
    started_at: SystemTime,
    lifecycle: Lifecycle,
) -> String {
    let id = state::new_session_id();
    let mut record = SessionRecord::started(
        id.clone(),
        WORK_GROUP.to_string(),
        notebook_id.map(str::to_string),
        started_at,
    );
    match lifecycle {
        Lifecycle::Started => {}
        Lifecycle::ShuttingDown => record.terminate(SystemTime::now()),
        Lifecycle::ShutDown => record.terminate(started_at),
    }
    // The seeded instants must really put the session where the row claims, so a
    // mis-seeded row cannot pass by accident.
    assert_eq!(
        record.is_active_at(SystemTime::now()),
        lifecycle.active(),
        "a session seeded as {lifecycle:?} must be active={}, got the state {:?}",
        lifecycle.active(),
        record.state_at(SystemTime::now())
    );
    let store = state::service_state(engine).expect("this service's state");
    store.lock().sessions.insert(record);
    id
}

/// One `ListNotebookSessions` call that must be accepted.
async fn list(router: &MultiServiceRouter, request: &Value) -> Value {
    let (status, listed) = call(router, "ListNotebookSessions", request).await;
    assert_eq!(
        status, 200,
        "ListNotebookSessions({request}) must answer 200, got {status} with {listed:?}"
    );
    listed
}

/// The entries a listing answered with.
fn summaries(listed: &Value) -> Vec<Value> {
    listed["NotebookSessionsList"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a listing must carry NotebookSessionsList, got {listed:?}"))
}

/// The session ids those entries carry, in the order the listing reported them.
fn session_ids(listed: &Value) -> Vec<String> {
    summaries(listed)
        .iter()
        .map(|entry| {
            entry["SessionId"]
                .as_str()
                .unwrap_or_else(|| panic!("every entry names its session, got {entry:?}"))
                .to_string()
        })
        .collect()
}

/// The `NextToken` a page handed back, if it handed one back.
fn next_token(listed: &Value) -> Option<String> {
    listed["NextToken"].as_str().map(str::to_string)
}

/// How many sessions the store holds, which a READ must leave exactly as it found it.
fn held_sessions(engine: &Arc<engine::InMemoryEngine>) -> usize {
    let store = state::service_state(&**engine).expect("this service's state");
    let held = store.lock();
    held.sessions.len()
}

/// Which notebook a seeded session was started in.
#[derive(Clone, Copy, Debug)]
enum StartedIn {
    /// The notebook under test.
    This,
    /// Another notebook of the same workgroup.
    Another,
    /// No notebook at all — a session started for programmatic access.
    None,
}

/// One seeded session and what the notebook's listing must then report for it.
struct Row {
    case: &'static str,
    started_in: StartedIn,
    /// How long before this run's start instant the session was started.
    started_ago: Duration,
    lifecycle: Lifecycle,
    /// Whether the notebook's own listing reports it.
    listed: bool,
}

/// A notebook's listing reports its ACTIVE sessions, newest first, with the creation instant
/// each record was written with — and reports neither a session that has left those states
/// nor an active session of another notebook or of none. Listing is a READ: a second listing
/// reports what the first one did, and the store still holds every session.
#[tokio::test]
async fn a_listing_reports_the_notebooks_active_sessions_newest_first() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "ordering.ipynb").await;
    let elsewhere = given_notebook(&router, "elsewhere.ipynb").await;

    // Rows differ along the point of the lifecycle the session is at, how long ago it was
    // started, and which notebook it belongs to.
    let rows = [
        Row {
            case: "a session still acquiring its resources",
            started_in: StartedIn::This,
            started_ago: Duration::ZERO,
            lifecycle: Lifecycle::Started,
            listed: true,
        },
        Row {
            case: "a session the clock made idle",
            started_in: StartedIn::This,
            started_ago: WELL_PAST_ANY_RUN,
            lifecycle: Lifecycle::Started,
            listed: true,
        },
        Row {
            case: "an older idle session",
            started_in: StartedIn::This,
            started_ago: WELL_PAST_ANY_RUN * 2,
            lifecycle: Lifecycle::Started,
            listed: true,
        },
        Row {
            case: "the oldest idle session",
            started_in: StartedIn::This,
            started_ago: WELL_PAST_ANY_RUN * 3,
            lifecycle: Lifecycle::Started,
            listed: true,
        },
        Row {
            case: "a session shutting down",
            started_in: StartedIn::This,
            started_ago: WELL_PAST_ANY_RUN,
            lifecycle: Lifecycle::ShuttingDown,
            listed: false,
        },
        Row {
            case: "a session whose resources are gone",
            started_in: StartedIn::This,
            started_ago: WELL_PAST_ANY_RUN * 4,
            lifecycle: Lifecycle::ShutDown,
            listed: false,
        },
        Row {
            case: "an active session of another notebook",
            started_in: StartedIn::Another,
            started_ago: Duration::ZERO,
            lifecycle: Lifecycle::Started,
            listed: false,
        },
        Row {
            case: "an active session bound to no notebook",
            started_in: StartedIn::None,
            started_ago: Duration::ZERO,
            lifecycle: Lifecycle::Started,
            listed: false,
        },
    ];

    let run_started = SystemTime::now();
    let mut seeded: Vec<(&Row, SystemTime, String)> = Vec::new();
    for row in &rows {
        let started_at = run_started - row.started_ago;
        let in_notebook = match row.started_in {
            StartedIn::This => Some(notebook.as_str()),
            StartedIn::Another => Some(elsewhere.as_str()),
            StartedIn::None => None,
        };
        let id = given_session(&engine, in_notebook, started_at, row.lifecycle);
        seeded.push((row, started_at, id));
    }

    // Newest first: the order the seeded instants imply for the rows the listing reports.
    let mut expected: Vec<(SystemTime, String)> = seeded
        .iter()
        .filter(|(row, _, _)| row.listed)
        .map(|(_, started_at, id)| (*started_at, id.clone()))
        .collect();
    expected.sort_by_key(|(started_at, _)| Reverse(*started_at));
    let expected_ids: Vec<String> = expected.into_iter().map(|(_, id)| id).collect();

    let listed = list(&router, &json!({ "NotebookId": notebook })).await;
    assert_eq!(
        session_ids(&listed),
        expected_ids,
        "expected the notebook's active sessions newest first: {expected_ids:?}, got {listed:?}"
    );

    for (row, started_at, id) in &seeded {
        let reported = session_ids(&listed).contains(id);
        assert_eq!(
            reported, row.listed,
            "case {:?}: expected the listing to report it={}, got {listed:?}",
            row.case, row.listed
        );
        if !row.listed {
            continue;
        }
        let entry = summaries(&listed)
            .into_iter()
            .find(|entry| entry["SessionId"].as_str() == Some(id.as_str()))
            .unwrap_or_else(|| panic!("case {:?}: {id} must be listed, got {listed:?}", row.case));
        // The instant the record was written with, as the wire renders a timestamp: epoch
        // seconds, compared to the millisecond rather than bit for bit.
        let want = ::aws_smithy_types::DateTime::from(*started_at).as_secs_f64();
        let got = entry["CreationTime"].as_f64().unwrap_or_else(|| {
            panic!("case {:?}: every entry carries a CreationTime, got {entry:?}", row.case)
        });
        assert!(
            (got - want).abs() < 1e-3,
            "case {:?}: expected the creation instant {want}, got {got} in {entry:?}",
            row.case
        );
    }

    // A listing REPORTS: it moved no session on and removed none.
    let again = list(&router, &json!({ "NotebookId": notebook })).await;
    assert_eq!(
        again, listed,
        "two listings of one notebook must report the same sessions, got {again:?} after \
         {listed:?}"
    );
    assert_eq!(
        held_sessions(&engine),
        rows.len(),
        "a listing must leave every seeded session in the store, got {} of {}",
        held_sessions(&engine),
        rows.len()
    );
}

/// A paged walk reports every active session of the notebook exactly once, in pages of at
/// most the size asked for and never longer than the modelled list, and hands back a token
/// exactly while sessions remain behind the page.
#[tokio::test]
async fn a_paged_walk_reports_every_active_session_exactly_once() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "paged.ipynb").await;

    // More active sessions than one page can carry, each with its own creation instant so
    // the newest-first order is total and no entry can change between pages.
    let run_started = SystemTime::now();
    let total = 12_usize;
    let mut expected: Vec<String> = Vec::new();
    for index in 0..total {
        // Index 0 is the newest, so the ids are already in the order a walk must report.
        let started_at = run_started - WELL_PAST_ANY_RUN - Duration::from_secs(index as u64);
        expected.push(given_session(
            &engine,
            Some(&notebook),
            started_at,
            Lifecycle::Started,
        ));
    }

    // Each row is a page size: one at a time, sizes that divide the total and sizes that
    // leave a remainder, the modelled list's own ceiling, the total itself, the modelled
    // maximum, and no size at all — the page a request naming none gets.
    for case in [
        Some(1),
        Some(2),
        Some(5),
        Some(LIST_CEILING),
        Some(total),
        Some(MODELLED_MAX_RESULTS),
        None,
    ] {
        let page_size = case.unwrap_or(LIST_CEILING).min(LIST_CEILING);
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let mut request = json!({ "NotebookId": notebook });
            if let Some(size) = case {
                request["MaxResults"] = json!(size);
            }
            if let Some(token) = token.as_deref() {
                request["NextToken"] = json!(token);
            }
            let listed = list(&router, &request).await;
            pages += 1;
            assert!(
                pages <= total + 1,
                "case {case:?}: a token walk must terminate, walked {walked:?}"
            );

            let page = session_ids(&listed);
            let remaining = total - walked.len();
            assert_eq!(
                page.len(),
                page_size.min(remaining),
                "case {case:?}: expected a page of {} after {walked:?}, got {page:?}",
                page_size.min(remaining)
            );
            walked.extend(page);

            token = next_token(&listed);
            let more = walked.len() < total;
            assert_eq!(
                token.is_some(),
                more,
                "case {case:?}: expected a NextToken to be present={more} after {walked:?}, \
                 got {listed:?}"
            );
            if token.is_none() {
                break;
            }
        }

        assert_eq!(
            walked, expected,
            "case {case:?}: expected the walk to report every active session once, newest \
             first: {expected:?}, got {walked:?}"
        );
        assert_eq!(
            pages,
            total.div_ceil(page_size),
            "case {case:?}: expected the walk to take {} pages, took {pages}",
            total.div_ceil(page_size)
        );
    }
}

/// `MaxResults` is "the maximum number of notebook sessions to return" within the modelled
/// `MaxSessionsCount` range (1..100), and no page is longer than the modelled
/// `NotebookSessionsList`: each published bound is a page size, and one step past either
/// bound is refused rather than answered with a page.
#[tokio::test]
async fn the_page_size_is_bounded_by_the_modelled_range_and_the_lists_own_ceiling() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "bounded.ipynb").await;

    // More active sessions than the modelled list can carry, so a page size above that
    // ceiling is bounded by the list rather than by how many sessions there are.
    let run_started = SystemTime::now();
    for index in 0..(LIST_CEILING + 2) {
        let started_at = run_started - WELL_PAST_ANY_RUN - Duration::from_secs(index as u64);
        given_session(&engine, Some(&notebook), started_at, Lifecycle::Started);
    }

    // (the page size asked for, how many entries it may report)
    for (size, reported) in [
        (1, 1),
        (2, 2),
        (LIST_CEILING, LIST_CEILING),
        (LIST_CEILING + 1, LIST_CEILING),
        (MODELLED_MAX_RESULTS, LIST_CEILING),
    ] {
        let listed = list(&router, &json!({ "NotebookId": notebook, "MaxResults": size })).await;
        assert_eq!(
            summaries(&listed).len(),
            reported,
            "case {size}: expected {reported} entries, got {listed:?}"
        );
    }

    // (case, the page size asked for, the clause the refusal must state)
    for (case, size, clause) in [
        (
            "one below the modelled floor",
            0,
            "Member must have value greater than or equal to 1",
        ),
        (
            "one past the modelled ceiling",
            MODELLED_MAX_RESULTS + 1,
            "Member must have value less than or equal to 100",
        ),
    ] {
        let (status, refused) = call(
            &router,
            "ListNotebookSessions",
            &json!({ "NotebookId": notebook, "MaxResults": size }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for MaxResults {size}, got {status} with \
             {refused:?}"
        );
        let expected = format!(
            "1 validation error detected: Value at 'maxResults' failed to satisfy constraint: \
             {clause}"
        );
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got {refused:?}"
        );
    }
}

/// A notebook whose sessions are all gone — and one nothing ever started a session in —
/// reports the empty list the required member takes, and no continuation token.
#[tokio::test]
async fn a_notebook_with_no_active_session_lists_none() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let untouched = given_notebook(&router, "untouched.ipynb").await;
    let finished = given_notebook(&router, "finished.ipynb").await;

    let run_started = SystemTime::now();
    given_session(
        &engine,
        Some(&finished),
        run_started - WELL_PAST_ANY_RUN,
        Lifecycle::ShutDown,
    );
    given_session(
        &engine,
        Some(&finished),
        run_started - WELL_PAST_ANY_RUN,
        Lifecycle::ShuttingDown,
    );

    // Rows differ along why the notebook has no active session, and along the page a client
    // asks for.
    for (case, notebook) in [
        ("a notebook nothing started a session in", &untouched),
        ("a notebook whose sessions have all shut down", &finished),
    ] {
        for request in [
            json!({ "NotebookId": notebook }),
            json!({ "NotebookId": notebook, "MaxResults": 1 }),
            json!({ "NotebookId": notebook, "MaxResults": MODELLED_MAX_RESULTS }),
        ] {
            let listed = list(&router, &request).await;
            assert_eq!(
                summaries(&listed),
                Vec::<Value>::new(),
                "case {case:?}: expected the empty list for {request}, got {listed:?}"
            );
            assert_eq!(
                next_token(&listed),
                None,
                "case {case:?}: a page with nothing behind it hands back no token, got \
                 {listed:?}"
            );
        }
    }
}

/// A `NotebookId` of the modelled form that names no notebook this account holds is refused
/// as a bad request, under the code for an absent notebook and the sentence this service
/// words that miss with — the same miss any other read of that notebook reports.
#[tokio::test]
async fn a_notebook_id_no_notebook_was_minted_under_is_refused() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let held = given_notebook(&router, "held.ipynb").await;
    given_session(
        &engine,
        Some(&held),
        SystemTime::now() - WELL_PAST_ANY_RUN,
        Lifecycle::Started,
    );

    // Rows differ along the shape of the id that names no notebook: every nibble zero,
    // every nibble f, and one that differs from the held id in a single character.
    let flipped = format!(
        "{}{}",
        if held.starts_with('0') { "1" } else { "0" },
        &held[1..]
    );
    let absent = [
        ("every nibble zero", "00000000-0000-4000-8000-000000000000".to_string()),
        ("every nibble f", "ffffffff-ffff-4fff-8fff-ffffffffffff".to_string()),
        ("one character from a held id", flipped),
    ];

    for (case, notebook_id) in &absent {
        let (status, refused) = call(
            &router,
            "ListNotebookSessions",
            &json!({ "NotebookId": notebook_id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {notebook_id}, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        let expected = state::notebook_not_found(notebook_id);
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected the sentence {expected:?}, got {refused:?}"
        );
        for member in ["AthenaErrorCode", "ErrorCode"] {
            assert_eq!(
                refused[member].as_str(),
                Some(ERROR_CODE_NOTEBOOK_NOT_FOUND),
                "case {case:?}: expected {member} {ERROR_CODE_NOTEBOOK_NOT_FOUND:?}, got \
                 {refused:?}"
            );
        }
    }

    // The refusals left the notebook that IS held listing exactly as before.
    let listed = list(&router, &json!({ "NotebookId": held })).await;
    assert_eq!(
        session_ids(&listed).len(),
        1,
        "the held notebook must still report its one active session, got {listed:?}"
    );
}

/// A `NextToken` is "a token generated by the Athena service": a value this service did not
/// mint for THIS notebook's listing names no page of it, and is refused rather than answered
/// with one.
#[tokio::test]
async fn a_next_token_this_service_did_not_mint_names_no_page() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "tokens.ipynb").await;
    let elsewhere = given_notebook(&router, "tokens-elsewhere.ipynb").await;

    let run_started = SystemTime::now();
    for index in 0..3 {
        let started_at = run_started - WELL_PAST_ANY_RUN - Duration::from_secs(index);
        given_session(&engine, Some(&notebook), started_at, Lifecycle::Started);
        given_session(&engine, Some(&elsewhere), started_at, Lifecycle::Started);
    }

    // A token this store minted for ANOTHER notebook's listing: it resumes at a position in
    // another sequence of sessions entirely.
    let other_notebook_token = next_token(
        &list(&router, &json!({ "NotebookId": elsewhere, "MaxResults": 1 })).await,
    )
    .expect("a page of one of three sessions hands back a token");

    // A token ANOTHER store minted for this same listing: another account's registry, so
    // another secret.
    let (other_engine, other_router) = engine_and_router();
    given_spark_work_group(&other_router).await;
    let other_store_notebook = given_notebook(&other_router, "tokens.ipynb").await;
    for index in 0..3 {
        let started_at = run_started - WELL_PAST_ANY_RUN - Duration::from_secs(index);
        given_session(
            &other_engine,
            Some(&other_store_notebook),
            started_at,
            Lifecycle::Started,
        );
    }
    let other_store_token = next_token(
        &list(
            &other_router,
            &json!({ "NotebookId": other_store_notebook, "MaxResults": 1 }),
        )
        .await,
    )
    .expect("a page of one of three sessions hands back a token");

    // A token THIS store minted for a DIFFERENT listing: the account's workgroups.
    let work_group_token = {
        crate::harness::create_work_group(&router, "token-case").await;
        let (status, listed) = call(&router, "ListWorkGroups", &json!({ "MaxResults": 1 })).await;
        assert_eq!(
            status, 200,
            "the workgroup listing must be readable, got {status} with {listed:?}"
        );
        next_token(&listed).expect("a page of one of the account's workgroups hands back a token")
    };

    // (case, the token sent)
    for (case, token) in [
        ("a value that is not a token at all", "made-up-token".to_string()),
        ("an empty-looking hex value", "00".to_string()),
        (
            "the shape of a minted token with an authenticator nobody computed",
            ::hex::encode(vec![0_u8; AUTHENTICATOR_BYTES + 16]),
        ),
        ("a token minted for another notebook's listing", other_notebook_token),
        ("a token another store minted for this listing", other_store_token),
        ("a token this store minted for another listing", work_group_token),
    ] {
        let (status, refused) = call(
            &router,
            "ListNotebookSessions",
            &json!({ "NotebookId": notebook, "MaxResults": 2, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            message(&refused),
            page_token_malformed(&token),
            "case {case:?}: expected the sentence {:?}, got {refused:?}",
            page_token_malformed(&token)
        );
        assert_eq!(
            refused["AthenaErrorCode"].as_str(),
            Some(ERROR_CODE_INVALID_INPUT),
            "case {case:?}: expected AthenaErrorCode {ERROR_CODE_INVALID_INPUT:?}, got \
             {refused:?}"
        );
    }

    // The refusals left the listing itself alone.
    let listed = list(&router, &json!({ "NotebookId": notebook, "MaxResults": 10 })).await;
    assert_eq!(
        session_ids(&listed).len(),
        3,
        "expected the three active sessions the seeding wrote, got {listed:?}"
    );
}
