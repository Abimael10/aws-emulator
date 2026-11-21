// SPDX-License-Identifier: CC0-1.0

//! Athena's CALCULATION listing, driven end to end over the same router the emulator boots
//! (awsJson1_1, `x-amz-target: AmazonAthena.ListCalculationExecutions`), so every assertion
//! is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: `ListCalculationExecutions` reports the calculations submitted into
//! the session the request names — "in descending order. Newer calculations are listed first;
//! older calculations are listed later" — out of the records the submissions persisted, in
//! pages of at most the `MaxResults` asked for, each page but the last followed by a
//! `NextToken` this service minted for THAT session's listing and no other value, narrowed by
//! `StateFilter` to the calculations in one state. A `SessionId` this account holds no session
//! under is refused with the one refusal every session-addressed request this service will
//! not act on answers with — the class and the sentence the service pairs for that class — and
//! a listing is a READ: it reports what the submissions left and changes nothing a later
//! caller sees.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! description a submission carried (none, one line, text outside ASCII, the modelled maximum
//! length), how many calculations the session holds, the page size asked for, the state
//! filtered on (every value of the modelled enum), which session is addressed, the shape of an
//! id that names no session, and the form of a continuation token a caller sends back.
//!
//! Nothing here waits on the wall clock: a session is seeded with the instants that put it
//! past every span this emulator owes, and the calculations are submitted and read back over
//! the wire.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::ops::types::CALCULATION_EXECUTION_STATE_VALUES;
use service_com_amazonaws_athena_amazonathena::state::{
    self, page_token_malformed, SessionRecord, CALCULATION_STATE_CREATED, ERROR_CODE_INVALID_INPUT,
    ERROR_CODE_UNAUTHORIZED, PYSPARK_ENGINE_VERSION, SESSION_NOT_FOUND,
};
use service_common::page_tokens::AUTHENTICATOR_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, engine_and_router};

/// The operation under test.
const OPERATION: &str = "ListCalculationExecutions";

/// The Spark enabled workgroup the notebooks and sessions below live in.
const WORK_GROUP: &str = "spark-calculation-listing";

/// The execution role a Spark workgroup accesses user resources through, inside the modelled
/// `RoleArn` pattern.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/AthenaSparkExecution";

/// A span long enough that a session seeded as started that long ago is past every span this
/// emulator owes, for as long as any test run takes: so a seeded session is running however
/// fast or slow the host is.
const WELL_PAST_ANY_RUN: Duration = Duration::from_secs(3_600);

/// The ceiling of the modelled `MaxCalculationsCount` range (1..100) a request may ask for,
/// which is also the length of the list this operation answers with (`CalculationsList`,
/// len 0..100), from
/// `smithy_query(query="operation",operation="ListCalculationExecutions")`.
const MODELLED_MAX_RESULTS: usize = 100;

/// The longest description a submission may carry: the modelled `DescriptionString` length
/// (1..1024).
const MAX_DESCRIPTION_LENGTH: usize = 1_024;

/// Create the Spark enabled workgroup the notebooks live in and the sessions run in.
async fn given_spark_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": EXECUTION_ROLE,
                "ResultConfiguration": { "OutputLocation": "s3://athena-results/calculations/" }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {WORK_GROUP} must be created, got {status} with {created:?}"
    );
}

/// Create the notebook `name` in that workgroup and hand back the `NotebookId` it minted.
async fn given_notebook(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        router,
        "CreateNotebook",
        &json!({ "WorkGroup": WORK_GROUP, "Name": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be created, got {status} with {created:?}"
    );
    created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("a create must answer with a NotebookId, got {created:?}"))
        .to_string()
}

/// Seed one running session into `engine`'s store, bound to `notebook_id`, and hand back its
/// `SessionId`.
///
/// Seeding through this service's own [`SessionRecord`] is what puts the session past the span
/// its resources are due after without waiting for it; every call below is made over the wire.
fn given_session(engine: &engine::InMemoryEngine, notebook_id: &str) -> String {
    let id = state::new_session_id();
    let started_at = SystemTime::now()
        .checked_sub(WELL_PAST_ANY_RUN)
        .expect("an instant this far back exists");
    let record = SessionRecord::started(
        id.clone(),
        WORK_GROUP.to_string(),
        Some(notebook_id.to_string()),
        started_at,
    );
    // A seeded session must really be running, so a mis-seeded row cannot pass by accident.
    assert!(
        record.is_active_at(SystemTime::now()),
        "a seeded session must be running, got the state {:?}",
        record.state_at(SystemTime::now())
    );
    let store = state::service_state(engine).expect("this service's state");
    store.lock().sessions.insert(record);
    id
}

/// Submit one calculation into `session_id` with `code_block` and `description`, and hand back
/// the `CalculationExecutionId` it minted.
async fn given_calculation(
    router: &MultiServiceRouter,
    session_id: &str,
    code_block: &str,
    description: Option<&str>,
) -> String {
    let mut request = json!({ "SessionId": session_id, "CodeBlock": code_block });
    if let Some(description) = description {
        request["Description"] = json!(description);
    }
    let (status, answer) = call(router, "StartCalculationExecution", &request).await;
    assert_eq!(
        status, 200,
        "a submission into {session_id} must be accepted, got {status} with {answer:?}"
    );
    answer["CalculationExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("a submission must answer with an id, got {answer:?}"))
        .to_string()
}

/// One listing that must be accepted.
async fn list(router: &MultiServiceRouter, request: &Value) -> Value {
    let (status, listed) = call(router, OPERATION, request).await;
    assert_eq!(
        status, 200,
        "{OPERATION}({request}) must answer 200, got {status} with {listed:?}"
    );
    listed
}

/// The entries a listing answered with: the member is optional in the model, so a listing that
/// carries none at all is reported as the empty page it is.
fn summaries(listed: &Value) -> Vec<Value> {
    listed["Calculations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The calculation ids those entries carry, in the order the listing reported them.
fn calculation_ids(listed: &Value) -> Vec<String> {
    summaries(listed)
        .iter()
        .map(|entry| {
            entry["CalculationExecutionId"]
                .as_str()
                .unwrap_or_else(|| panic!("every entry names its calculation, got {entry:?}"))
                .to_string()
        })
        .collect()
}

/// The `NextToken` a page handed back, if it handed one back.
fn next_token(listed: &Value) -> Option<String> {
    listed["NextToken"].as_str().map(str::to_string)
}

/// How many calculations the store holds, which a READ must leave exactly as it found it.
fn held_calculations(engine: &engine::InMemoryEngine) -> usize {
    let store = state::service_state(engine).expect("this service's state");
    let held = store.lock();
    held.calculations.len()
}

/// RULE: the listing reports the session's calculations newest first, each as the summary of
/// the record its submission persisted — the id it was minted under, the description it
/// carried, and the state it is in with the instant it was submitted at.
#[tokio::test]
async fn a_listing_reports_a_sessions_calculations_newest_first() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "ordering").await;
    let session = given_session(&engine, &notebook);

    // The rows differ along the description the submission carried, which is the one member of
    // a summary that comes from the request.
    let longest = "d".repeat(MAX_DESCRIPTION_LENGTH);
    let rows: Vec<(&str, &str, Option<&str>)> = vec![
        ("no description", "1", None),
        ("one line of description", "2", Some("the second cell")),
        (
            "a description outside ASCII",
            "3",
            Some("der dritte Block ✓"),
        ),
        (
            "the longest description the model admits",
            "4",
            Some(longest.as_str()),
        ),
    ];

    let mut submitted: Vec<(&str, Option<&str>, String)> = Vec::new();
    for (case, code_block, description) in &rows {
        let id = given_calculation(&router, &session, code_block, *description).await;
        submitted.push((case, *description, id));
    }

    let listed = list(&router, &json!({ "SessionId": session })).await;
    let expected: Vec<String> = submitted
        .iter()
        .rev()
        .map(|(_, _, id)| id.clone())
        .collect();
    assert_eq!(
        calculation_ids(&listed),
        expected,
        "case \"every submission\": expected the calculations newest first: {expected:?}, got \
         {listed:?}"
    );
    assert_eq!(
        next_token(&listed),
        None,
        "case \"every submission\": one page holding them all hands back no token, got {listed:?}"
    );

    for (entry, (case, description, id)) in summaries(&listed).iter().zip(submitted.iter().rev()) {
        assert_eq!(
            entry["CalculationExecutionId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the entry of {id:?}, got {entry:?}"
        );
        assert_eq!(
            entry["Description"].as_str(),
            *description,
            "case {case:?}: expected the description the submission carried, got {entry:?}"
        );
        assert_eq!(
            entry["Status"]["State"].as_str(),
            Some(CALCULATION_STATE_CREATED),
            "case {case:?}: expected the state {CALCULATION_STATE_CREATED:?}, got {entry:?}"
        );
        let submitted_at = entry["Status"]["SubmissionDateTime"].as_f64();
        assert!(
            submitted_at.is_some_and(|instant| instant > 0.0),
            "case {case:?}: expected the instant the calculation was submitted at, got {entry:?}"
        );
        assert!(
            entry["Status"]["CompletionDateTime"].is_null(),
            "case {case:?}: nothing has run the calculation, so no completion instant is \
             reported, got {entry:?}"
        );
    }

    // A read: a second listing reports what the first one did, and the store still holds every
    // calculation the submissions persisted.
    let again = list(&router, &json!({ "SessionId": session })).await;
    assert_eq!(
        calculation_ids(&again),
        expected,
        "case \"a second listing\": expected the same answer, got {again:?}"
    );
    assert_eq!(
        held_calculations(&engine),
        rows.len(),
        "case \"a second listing\": expected {} calculations still held, got {}",
        rows.len(),
        held_calculations(&engine)
    );
}

/// RULE: a paged walk reports every calculation of the session exactly once, newest first, in
/// pages of at most the size asked for, and hands back a token exactly while calculations
/// remain behind the page.
#[tokio::test]
async fn a_paged_walk_reports_every_calculation_exactly_once() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "paged").await;
    let session = given_session(&engine, &notebook);

    let total = 5_usize;
    let mut expected: Vec<String> = Vec::new();
    for index in 0..total {
        let id = given_calculation(&router, &session, &format!("print({index})"), None).await;
        // The newest is listed first, so a submission goes to the FRONT of what a walk must
        // report.
        expected.insert(0, id);
    }

    // Each row is a page size: one at a time, sizes that divide the total and sizes that leave
    // a remainder, the total itself, the modelled maximum, and no size at all — the page a
    // request naming none gets.
    for case in [
        Some(1),
        Some(2),
        Some(3),
        Some(total),
        Some(MODELLED_MAX_RESULTS),
        None,
    ] {
        let page_size = case.unwrap_or(MODELLED_MAX_RESULTS);
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let mut request = json!({ "SessionId": session });
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

            let page = calculation_ids(&listed);
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
                "case {case:?}: expected a NextToken to be present={more} after {walked:?}, got \
                 {listed:?}"
            );
            if token.is_none() {
                break;
            }
        }

        assert_eq!(
            walked, expected,
            "case {case:?}: expected the walk to report every calculation once, newest first: \
             {expected:?}, got {walked:?}"
        );
        assert_eq!(
            pages,
            total.div_ceil(page_size),
            "case {case:?}: expected the walk to take {} pages, took {pages}",
            total.div_ceil(page_size)
        );
    }
}

/// RULE: `StateFilter` is "a filter for a specific calculation execution state", so a listing
/// naming one reports only the calculations in that state — nothing for a state none of them
/// is in.
///
/// The rows are EVERY value of the modelled `CalculationExecutionState` enum
/// (`CALCULATION_EXECUTION_STATE_VALUES` in this service's `ops::types`, generated from the
/// model), so no state of the catalog goes untested.
#[tokio::test]
async fn a_state_filter_reports_only_the_calculations_in_that_state() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "filtered").await;
    let session = given_session(&engine, &notebook);

    let mut expected: Vec<String> = Vec::new();
    for index in 0..3 {
        let id = given_calculation(&router, &session, &format!("{index}"), None).await;
        expected.insert(0, id);
    }

    assert!(
        CALCULATION_EXECUTION_STATE_VALUES.contains(&CALCULATION_STATE_CREATED),
        "the state a submitted calculation is in must be one of the modelled states, got \
         {CALCULATION_EXECUTION_STATE_VALUES:?}"
    );
    for state in CALCULATION_EXECUTION_STATE_VALUES {
        let listed = list(
            &router,
            &json!({ "SessionId": session, "StateFilter": state }),
        )
        .await;
        let reported = calculation_ids(&listed);
        let wanted: Vec<String> = if *state == CALCULATION_STATE_CREATED {
            expected.clone()
        } else {
            Vec::new()
        };
        assert_eq!(
            reported, wanted,
            "case {state:?}: expected the calculations in that state: {wanted:?}, got {listed:?}"
        );
        assert_eq!(
            next_token(&listed),
            None,
            "case {state:?}: a page with nothing behind it hands back no token, got {listed:?}"
        );
    }
}

/// RULE: the calculations reported are the ones submitted INTO THE SESSION the request names,
/// so a session reports neither another session's calculations nor, holding none itself, any
/// at all — and the history a session holds outlives the shutdown of that session, which
/// `GetSession` still reads.
#[tokio::test]
async fn a_listing_reports_only_the_session_it_names() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "scoped").await;
    let first = given_session(&engine, &notebook);
    let second = given_session(&engine, &notebook);
    let untouched = given_session(&engine, &notebook);

    let older = given_calculation(&router, &first, "print('first')", None).await;
    let newer = given_calculation(&router, &first, "print('second')", None).await;
    let elsewhere = given_calculation(&router, &second, "print('elsewhere')", None).await;

    // (case, the session listed, the calculations it must report)
    let rows: Vec<(&str, &String, Vec<String>)> = vec![
        (
            "the session two calculations were submitted into",
            &first,
            vec![newer.clone(), older.clone()],
        ),
        (
            "another session of the same notebook",
            &second,
            vec![elsewhere.clone()],
        ),
        ("a session holding none", &untouched, Vec::new()),
    ];
    for (case, session, wanted) in &rows {
        let listed = list(&router, &json!({ "SessionId": session })).await;
        assert_eq!(
            &calculation_ids(&listed),
            wanted,
            "case {case:?}: expected {wanted:?}, got {listed:?}"
        );
        assert_eq!(
            next_token(&listed),
            None,
            "case {case:?}: a page with nothing behind it hands back no token, got {listed:?}"
        );
    }

    let (status, terminated) = call(
        &router,
        "TerminateSession",
        &json!({ "SessionId": first }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the session must be terminated, got {status} with {terminated:?}"
    );
    let listed = list(&router, &json!({ "SessionId": first })).await;
    assert_eq!(
        calculation_ids(&listed),
        vec![newer, older],
        "case \"after the session was terminated\": the submitted history must still be \
         reported, got {listed:?}"
    );
}

/// RULE: a `SessionId` this account holds no session under names nothing to list, which is the
/// refusal every session-addressed request this service will not act on answers with rather
/// than an empty page.
#[tokio::test]
async fn a_session_this_account_holds_none_under_lists_nothing() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "unreachable").await;
    // One session with one calculation exists throughout, so each refusal below is about the id
    // the request carried rather than about the account holding nothing.
    let held = given_session(&engine, &notebook);
    let calculation = given_calculation(&router, &held, "print('held')", None).await;

    // The rows differ along the SHAPE of an id that names no session of this account.
    let rows: Vec<(&str, String)> = vec![
        ("an id nothing was started under", state::new_session_id()),
        (
            "an id in the short Athena form",
            "s-0123456789abcdef".to_string(),
        ),
        ("the notebook's own id", notebook.clone()),
        ("the calculation's own id", calculation.clone()),
        ("a single character", "a".to_string()),
        ("the longest id the model admits", "s".repeat(256)),
    ];
    for (case, session_id) in &rows {
        let (status, refused) = call(&router, OPERATION, &json!({ "SessionId": session_id })).await;
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_UNAUTHORIZED,
            SESSION_NOT_FOUND,
        );
    }

    let listed = list(&router, &json!({ "SessionId": held })).await;
    assert_eq!(
        calculation_ids(&listed),
        vec![calculation],
        "case \"the session this account holds\": its calculation must still be reported, got \
         {listed:?}"
    );
}

/// RULE: a `NextToken` is "a token generated by the Athena service", so a value this service
/// did not mint for THIS session's calculation listing names no page of it and is refused
/// rather than answered with one.
#[tokio::test]
async fn a_next_token_this_service_did_not_mint_names_no_page() {
    let (engine, router) = engine_and_router();
    given_spark_work_group(&router).await;
    let notebook = given_notebook(&router, "tokens").await;
    let session = given_session(&engine, &notebook);
    let elsewhere = given_session(&engine, &notebook);

    for index in 0..2 {
        given_calculation(&router, &session, &format!("{index}"), None).await;
        given_calculation(&router, &elsewhere, &format!("{index}"), None).await;
    }

    // A token this store DID mint, but for another session's calculation listing.
    let other_session_token = {
        let listed = list(
            &router,
            &json!({ "SessionId": elsewhere, "MaxResults": 1 }),
        )
        .await;
        next_token(&listed).expect("a first page of two calculations hands back a token")
    };
    // A token this store minted for another listing altogether: a page of the workgroup's
    // sessions.
    let session_listing_token = {
        let (status, listed) = call(
            &router,
            "ListSessions",
            &json!({ "WorkGroup": WORK_GROUP, "MaxResults": 1 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "the workgroup's session listing must be readable, got {status} with {listed:?}"
        );
        next_token(&listed).expect("a first page of two sessions hands back a token")
    };

    // (case, the token sent)
    let rows: Vec<(&str, String)> = vec![
        (
            "a value that is not a token at all",
            "made-up-token".to_string(),
        ),
        ("an empty-looking hex value", "00".to_string()),
        (
            "the shape of a minted token with an authenticator nobody computed",
            ::hex::encode(vec![0_u8; AUTHENTICATOR_BYTES + 16]),
        ),
        (
            "a token minted for another session's calculation listing",
            other_session_token,
        ),
        (
            "a token this store minted for another listing",
            session_listing_token,
        ),
    ];
    for (case, token) in &rows {
        let (status, refused) = call(
            &router,
            OPERATION,
            &json!({ "SessionId": session, "MaxResults": 1, "NextToken": token }),
        )
        .await;
        // The malformed-token class, which is the one every paginated read of this service
        // states for a `NextToken` it did not mint — not the session class the rows of
        // `a_session_this_account_holds_none_under_lists_nothing` assert, because the session
        // the request names is one this account holds.
        assert_refusal(
            case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &page_token_malformed(token),
        );
    }

    // The token this listing DID mint still names its page, so the rows above are about the
    // value sent rather than about tokens being refused altogether.
    let first = list(
        &router,
        &json!({ "SessionId": session, "MaxResults": 1 }),
    )
    .await;
    let token = next_token(&first).expect("a first page of two calculations hands back a token");
    let second = list(
        &router,
        &json!({ "SessionId": session, "MaxResults": 1, "NextToken": token }),
    )
    .await;
    assert_eq!(
        calculation_ids(&second).len(),
        1,
        "case \"the token this listing minted\": expected the next page of one, got {second:?}"
    );
    assert_ne!(
        calculation_ids(&second),
        calculation_ids(&first),
        "case \"the token this listing minted\": expected the page behind the first one, got \
         {second:?}"
    );
}
