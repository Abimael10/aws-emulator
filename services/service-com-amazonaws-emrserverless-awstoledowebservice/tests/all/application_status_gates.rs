// SPDX-License-Identifier: CC0-1.0

//! The EMR Serverless application-lifecycle STATUS GATES, end to end over HTTP.
//!
//! Every case goes through the real router (restJson1, SigV4 signing name
//! `emr-serverless`), so each one exercises routing, the handler's gate, the persisted
//! record and serialization — the same path an SDK client takes.
//!
//! The rule under test is asserted over a TABLE spanning the whole `ApplicationState`
//! enum rather than over one recorded request: `StartApplication` admits a start from ONE
//! set of statuses and refuses every other status with the sentence that states that very
//! set; the sentence names neither the application nor the status it is in, and a refused
//! start changes nothing.
//!
//! The rows differ along the status the gate finds the application in — including the
//! statuses a transition is still in flight in, whose expectation is the same whether or
//! not the clock has brought the transition due, so no row races the clock. One table
//! walks the statuses a client can reach over HTTP; the other parks an application on a
//! SETTLED status no call leaves behind, through the store the handlers share, because the
//! gate has to judge every value of the enum and not only the terminal one.
//!
//! The sibling gate on `DeleteApplication` is pinned elsewhere and not re-tabled here:
//! `application_lifecycle.rs` walks that gate over every `ApplicationState` value, and
//! `refusal_wording.rs` asserts its sentence — the application the request asked for, the
//! allowed-status list and the current status.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_emrserverless_awstoledowebservice::state::ComAmazonawsEmrserverlessAwstoledowebserviceState;

use super::application_lifecycle::{
    create_application, delete_application, emulator, emulator_with_engine, get_application,
    start_application, stop_application, Answer,
};

/// How long a row waits for a status it polls for before it gives up. Generous: every
/// status this suite waits for is owed within one clock-owed transition span, so this
/// budget is only ever spent on a host that has stopped making progress.
const BUDGET: Duration = Duration::from_secs(30);

/// How often a row looks again while it waits for a status.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The statuses a start is admitted from, as the refused start's own sentence renders
/// them. Written out here rather than read from `crate::state`, so a wrong set in `src/`
/// cannot make this suite pass. Source: the recorded refusal of a start on a deleted
/// application.
const STARTABLE: [&str; 2] = ["CREATED", "STOPPED"];

/// The sentence a start refused on its status carries: fixed text plus the bracketed list
/// of the statuses that would admit it, in either order.
fn not_startable_sentences() -> Vec<String> {
    bracketed_orders(&STARTABLE)
        .into_iter()
        .map(|statuses| {
            format!(
                "Application cannot be STARTED. Applications must be in one of the following \
                 statuses: {statuses}"
            )
        })
        .collect()
}

/// Both renderings of an allowed-status list: the service renders the pair in either
/// order, so a row accepts either.
fn bracketed_orders(states: &[&str; 2]) -> Vec<String> {
    vec![
        format!("[{}, {}]", states[0], states[1]),
        format!("[{}, {}]", states[1], states[0]),
    ]
}

/// The `message` an error body carries, or the empty string when there is none.
fn message(answer: &Answer) -> &str {
    answer.body["message"].as_str().unwrap_or_default()
}

/// The statuses named by a `&[&str]` catalog, as a set.
fn status_set(states: &[&str]) -> BTreeSet<String> {
    states.iter().map(|status| status.to_string()).collect()
}

/// The statuses a sentence names between its brackets, as a set.
fn statuses_named_by(sentence: &str) -> BTreeSet<String> {
    sentence
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(list, _)| {
            list.split(',')
                .map(|status| status.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Where a row starts: one value of the model's `ApplicationState` enum. Source:
/// `smithy_query(query="shape", shape="ApplicationState")` → `CREATING`, `CREATED`,
/// `STARTING`, `STARTED`, `STOPPING`, `STOPPED`, `TERMINATED`.
#[derive(Clone, Copy, Debug)]
enum Status {
    Creating,
    Created,
    Starting,
    Started,
    Stopping,
    Stopped,
    Terminated,
}

/// A `CreateApplication` body whose application never stops itself, so a row's
/// expectation is about the status the row put the application in and nothing else.
/// `AutoStopConfig$enabled` — "Enables the application to automatically stop after a
/// certain amount of time being idle" — is what turns that off.
fn create_body(token: &str) -> Value {
    json!({
        "clientToken": token,
        "releaseLabel": "emr-7.1.0",
        "type": "SPARK",
        "autoStopConfiguration": { "enabled": false },
    })
}

/// Poll `GetApplication` until the application reports `want`, and answer that read.
///
/// The wait is on the CONDITION, not on a span: it looks again every [`POLL_INTERVAL`]
/// until the status is there or the generous [`BUDGET`] passes.
async fn poll_until_state(
    router: &MultiServiceRouter,
    case: &str,
    application_id: &str,
    want: &str,
) -> Answer {
    let deadline = Instant::now() + BUDGET;
    loop {
        let answer = get_application(router, application_id).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: GetApplication expected 200, got {} with {:?}",
            answer.status, answer.body
        );
        let state = answer
            .at("/application/state")
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == want {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the application must reach {want:?}, last saw {state:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Create one application and answer its id.
async fn create(router: &MultiServiceRouter, case: &str, token: &str) -> String {
    let created = create_application(router, create_body(token)).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateApplication expected 200, got {} with {:?}",
        created.status, created.body
    );
    let id = created
        .at("/applicationId")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !id.is_empty(),
        "case {case:?}: CreateApplication must answer an application id, got {:?}",
        created.body
    );
    id
}

/// Bring a NEW application to `status` and answer its id.
///
/// A decoy application is minted first, so the id under test is not the first id the
/// store hands out: an id inside a refusal sentence can then only have come from the
/// request that carried it.
async fn application_in(router: &MultiServiceRouter, case: &str, status: Status) -> String {
    let decoy = create(router, case, "tok-decoy").await;
    let id = create(router, case, "tok-subject").await;
    assert_ne!(
        decoy, id,
        "case {case:?}: two creates must mint two applications"
    );

    match status {
        // The create has returned but the clock has not brought the application up yet.
        Status::Creating => {}
        Status::Created => {
            poll_until_state(router, case, &id, "CREATED").await;
        }
        // The start has returned but the clock has not finished it yet.
        Status::Starting => {
            poll_until_state(router, case, &id, "CREATED").await;
            let started = start_application(router, &id).await;
            assert_eq!(
                started.status, 200,
                "case {case:?}: StartApplication expected 200, got {} with {:?}",
                started.status, started.body
            );
        }
        Status::Started => {
            poll_until_state(router, case, &id, "CREATED").await;
            start_application(router, &id).await;
            poll_until_state(router, case, &id, "STARTED").await;
        }
        // The stop has returned but the clock has not finished it yet.
        Status::Stopping => {
            poll_until_state(router, case, &id, "CREATED").await;
            start_application(router, &id).await;
            poll_until_state(router, case, &id, "STARTED").await;
            let stopped = stop_application(router, &id).await;
            assert_eq!(
                stopped.status, 200,
                "case {case:?}: StopApplication expected 200, got {} with {:?}",
                stopped.status, stopped.body
            );
        }
        Status::Stopped => {
            poll_until_state(router, case, &id, "CREATED").await;
            start_application(router, &id).await;
            poll_until_state(router, case, &id, "STARTED").await;
            stop_application(router, &id).await;
            poll_until_state(router, case, &id, "STOPPED").await;
        }
        Status::Terminated => {
            poll_until_state(router, case, &id, "CREATED").await;
            let deleted = delete_application(router, &id).await;
            assert_eq!(
                deleted.status, 200,
                "case {case:?}: DeleteApplication expected 200, got {} with {:?}",
                deleted.status, deleted.body
            );
            poll_until_state(router, case, &id, "TERMINATED").await;
        }
    }
    id
}

/// Rule 1: a start is admitted from the statuses the gate's allowed set names — and from
/// the statuses `@idempotent` makes the call a no-op in, where the application is already
/// running or on its way there — and is refused from every other status with the sentence
/// that states that set.
#[tokio::test]
async fn a_start_is_admitted_only_from_the_statuses_its_refusal_names() {
    // Every value of `ApplicationState`: the status the gate finds the application in,
    // and — for a status a start is refused from — the status the refused application is
    // left in. A transition still in flight is judged against the status the clock owes
    // it, so a create on its way to `CREATED` and a stop on its way to `STOPPED` admit a
    // start the settled application admits.
    for (case, status, refused_in) in [
        ("a create still in flight", Status::Creating, None),
        ("a created application", Status::Created, None),
        ("a start still in flight", Status::Starting, None),
        ("a started application", Status::Started, None),
        ("a stop still in flight", Status::Stopping, None),
        ("a stopped application", Status::Stopped, None),
        (
            "a terminated application",
            Status::Terminated,
            Some("TERMINATED"),
        ),
    ] {
        let router = emulator();
        let id = application_in(&router, case, status).await;

        let answer = start_application(&router, &id).await;
        let Some(left_in) = refused_in else {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: StartApplication expected 200, got {} with {:?}",
                answer.status, answer.body
            );
            // The accepted start is BACKED: the application the gate admitted reaches
            // the running status, read back through `GetApplication`.
            let running = poll_until_state(&router, case, &id, "STARTED").await;
            assert_eq!(
                running.at("/application/state"),
                json!("STARTED"),
                "case {case:?}: an admitted start must leave the application running, got {:?}",
                running.at("/application/state")
            );
            continue;
        };

        assert_refused_start(case, &id, &answer);
        // A refused start is not a mutation: the application is left in the status the
        // gate found it in.
        let after = poll_until_state(&router, case, &id, left_in).await;
        assert_eq!(
            after.at("/application/state"),
            json!(left_in),
            "case {case:?}: a refused start must change nothing, got {:?}",
            after.at("/application/state")
        );
    }
}

/// What a start refused on its status answers: the modelled client failure, the sentence
/// rendered from the statuses that WOULD admit it, and nothing about this application.
fn assert_refused_start(case: &str, application_id: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected 400, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ValidationException",
        "case {case:?}: expected ValidationException, got {:?}",
        answer.body
    );
    let expected = not_startable_sentences();
    assert!(
        expected.iter().any(|sentence| sentence == message(answer)),
        "case {case:?}: expected one of {expected:?}, got {:?}",
        message(answer)
    );
    assert_eq!(
        statuses_named_by(message(answer)),
        status_set(&STARTABLE),
        "case {case:?}: the sentence must name the statuses a start is admitted from, got {:?}",
        message(answer)
    );
    assert!(
        !message(answer).contains(application_id),
        "case {case:?}: the sentence must name no application, got {:?}",
        message(answer)
    );
}

/// Park `application_id` on `status` with nothing owed, through the store the handlers
/// share — the seam the idle suite rewinds a record's own clock through.
///
/// It is how a row reaches a SETTLED status that no sequence of calls leaves behind: the
/// clock owes a `STOPPING` application a settled status, and a create leaves none standing
/// on `CREATING` at all, so over HTTP the gate never finds one standing on either. The
/// status itself is an ordinary value of the model's `ApplicationState` enum, and the gate
/// has to judge it like any other.
pub(crate) fn park_on(engine: &engine::InMemoryEngine, application_id: &str, status: &str) {
    let state = engine.state::<ComAmazonawsEmrserverlessAwstoledowebserviceState>();
    let mut store = state.lock();
    let record = store
        .applications
        .get_mut(application_id)
        .expect("the application the test created is in the store");
    record.pending = None;
    record.application.state = status.to_string();
}

/// Rule 1 from the other side: EVERY status the allowed set does not name is refused, not
/// just the terminal one — the gate is the allowed set, not a list of refused statuses.
#[tokio::test]
async fn a_start_is_refused_from_every_settled_status_the_allowed_set_does_not_name() {
    // The `ApplicationState` values that are neither in the allowed set nor ones the start
    // is already satisfied by (`STARTING`, `STARTED`, which `@idempotent` makes a no-op).
    // Source: the enum (`CREATING`, `CREATED`, `STARTING`, `STARTED`, `STOPPING`,
    // `STOPPED`, `TERMINATED`) minus those four.
    for (case, settled_status) in [
        ("an application standing on CREATING", "CREATING"),
        ("an application standing on STOPPING", "STOPPING"),
        ("an application standing on TERMINATED", "TERMINATED"),
    ] {
        let (router, engine) = emulator_with_engine();
        let id = create(&router, case, "tok-parked").await;
        park_on(&engine, &id, settled_status);

        let before = get_application(&router, &id).await;
        assert_eq!(
            before.at("/application/state"),
            json!(settled_status),
            "case {case:?}: the application must stand on that status before the start, got {:?}",
            before.at("/application/state")
        );

        let answer = start_application(&router, &id).await;
        assert_refused_start(case, &id, &answer);

        let after = get_application(&router, &id).await;
        assert_eq!(
            after.at("/application/state"),
            json!(settled_status),
            "case {case:?}: a refused start must change nothing, got {:?}",
            after.at("/application/state")
        );
    }
}

