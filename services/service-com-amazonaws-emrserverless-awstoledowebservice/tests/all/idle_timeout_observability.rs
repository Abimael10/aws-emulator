// SPDX-License-Identifier: CC0-1.0

//! The idle timeouts this service is configured with, as a POLLING client sees them.
//!
//! An idle timeout is the one configuration here whose effect nothing but the clock
//! produces: no call moves an application to `STOPPED` for having been idle, and no call
//! terminates a session for it either. It is also a wait the CALLER asks for, stated in
//! MINUTES by both configurations that carry one — `AutoStopConfig$idleTimeoutMinutes`
//! and `StartSessionRequest$idleTimeoutMinutes` — so the rule under test is that the
//! emulator serves the minutes the request named:
//!
//! * a resource stays in the state it is idle IN for the whole configured span, so a
//!   client observes that state however long after the call that started it the client's
//!   first read lands — an application settling into `STARTED` is never stopped for
//!   idleness before any read can report it;
//! * the change the idle timeout owes falls due one configured span after the resource
//!   became idle, and not before;
//! * the configured value itself is reported back verbatim on every read, whatever span
//!   the emulator serves it as.
//!
//! Every observation goes through the router over HTTP (`GetApplication`, `GetSession`),
//! the way a polling client makes it. Where a case needs a client's read to land some
//! way INTO the configured span, the span itself is moved into the past through the
//! shared store — the same synthetic clock
//! `idempotency_and_idle_lifecycle::an_idle_application_stops_itself_once_its_idle_timeout_has_passed`
//! uses — rather than waited for, so each row asserts the rule at an instant no test
//! could sit through and the suite still runs in seconds.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_emrserverless_awstoledowebservice::state::{
    idle_span, LIFECYCLE_TRANSITION, MAX_TRANSIENT_SPAN,
};

use super::application_lifecycle::{
    create_application, emulator, emulator_with_engine, get_application, minimal_create,
    start_application,
};
// The synthetic clock these cases read through: the sibling suite's helpers move an
// application's owed state and a session's owed state into the past, which is how a read
// lands some way INTO a configured idle span without a test sitting through it.
use super::idempotency_and_idle_lifecycle::{rewind_last_activity, rewind_session_idle_since};
use super::session_lifecycle::{get_session, minimal_session, session_application, start_session};

/// How often the poll loop below looks, and how many times it looks before giving up: it
/// waits for the CONDITION it is after, with a deadline (interval times looks) far longer
/// than any transition this service owes.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_LOOKS: usize = 600;

/// The interval a client polls a lifecycle edge at. A client waiting for a resource to
/// come up polls on the order of seconds — the recorded clients driving this service poll
/// `GetApplication` every ten — and the state such a client is told the resource settled
/// on must not depend on how coarsely it looks.
const CLIENT_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How far from the boundary a row's read is placed: a row reads either this long before
/// the configured idle span has run out or this long after it. It is deliberately
/// generous — the case under test is which side of the configured span the read falls on,
/// not how fast the host runs, so a loaded machine that stalls between the call and the
/// read still reads the side the row means.
const BOUNDARY_SLACK: Duration = Duration::from_secs(30);

/// The earliest a row's read lands: past the transition that brings a resource up, so
/// every row reads a resource that has settled into the state it is idle in.
const MIN_READ_OFFSET: Duration = Duration::from_secs(1);

/// The bounds `AutoStopConfig$idleTimeoutMinutes` publishes, and the default its
/// documentation states, written out here rather than read from `crate::state` so a wrong
/// value in `src/` cannot make this test pass. Source:
/// `smithy_query(query="shape",shape="AutoStopConfig")` — `@range(min: 1, max: 10080)`,
/// "The amount of idle time in minutes after which your application will automatically
/// stop. Defaults to 15 minutes."
const MIN_IDLE_MINUTES: i64 = 1;
const MAX_IDLE_MINUTES: i64 = 10_080;
const DEFAULT_IDLE_MINUTES: i64 = 15;

/// The bounds `StartSessionRequest$idleTimeoutMinutes` publishes, from
/// `smithy_query(query="operation",operation="StartSession")` — `@range(min: 0, max:
/// 1000000)`. Its minimum, zero, names no timeout at all.
const MIN_SESSION_IDLE_MINUTES: i64 = 0;
const MAX_SESSION_IDLE_MINUTES: i64 = 1_000_000;

/// Seconds in a minute: both configurations state their timeout in minutes, so this is
/// the only conversion between the value a request names and the span it is served as.
const SECONDS_PER_MINUTE: u64 = 60;

/// One row of the application table: what the create configured, the idle minutes that
/// configuration leaves in effect, and whether it owes the application a stop at all.
struct AutoStopRow {
    case: &'static str,
    /// The `autoStopConfiguration` the create carries, or `None` to name none at all.
    configuration: Option<Value>,
    /// The `idleTimeoutMinutes` every read must report for it.
    effective_minutes: i64,
    /// Whether the configuration owes the application an automatic stop.
    stops: bool,
}

/// The rows: they differ along the configured idle timeout (both published bounds, the
/// documented default taken by naming no configuration, and a value between them) and
/// along whether the stop is switched on, which is what decides whether the edge is owed
/// at all.
fn auto_stop_rows() -> Vec<AutoStopRow> {
    vec![
        AutoStopRow {
            case: "the published minimum, enabled explicitly",
            configuration: Some(json!({ "enabled": true, "idleTimeoutMinutes": MIN_IDLE_MINUTES })),
            effective_minutes: MIN_IDLE_MINUTES,
            stops: true,
        },
        AutoStopRow {
            case: "twice the minimum, with enabled left to its documented default",
            configuration: Some(json!({ "idleTimeoutMinutes": MIN_IDLE_MINUTES * 2 })),
            effective_minutes: MIN_IDLE_MINUTES * 2,
            stops: true,
        },
        AutoStopRow {
            case: "the documented default, with no configuration at all",
            configuration: None,
            effective_minutes: DEFAULT_IDLE_MINUTES,
            stops: true,
        },
        AutoStopRow {
            case: "the published maximum",
            configuration: Some(json!({ "enabled": true, "idleTimeoutMinutes": MAX_IDLE_MINUTES })),
            effective_minutes: MAX_IDLE_MINUTES,
            stops: true,
        },
        AutoStopRow {
            case: "the minimum idle timeout with the stop switched off",
            configuration: Some(
                json!({ "enabled": false, "idleTimeoutMinutes": MIN_IDLE_MINUTES }),
            ),
            effective_minutes: MIN_IDLE_MINUTES,
            stops: false,
        },
    ]
}

/// A created and started application carrying `row`'s configuration.
async fn started_application(
    router: &MultiServiceRouter,
    row: &AutoStopRow,
    token: &str,
) -> String {
    let mut request = minimal_create(token);
    if let Some(configuration) = row.configuration.clone() {
        request["autoStopConfiguration"] = configuration;
    }
    let created = create_application(router, request).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: CreateApplication expected 200, got {} with {:?}",
        row.case, created.status, created.body
    );
    let application_id = created
        .at("/applicationId")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let started = start_application(router, &application_id).await;
    assert_eq!(
        started.status, 200,
        "case {:?}: StartApplication expected 200, got {} with {:?}",
        row.case, started.status, started.body
    );
    application_id
}

/// The span the minutes a request configured name, computed from that value and from the
/// seconds in a minute rather than from the emulator's own helper, so a wrong span in
/// `src/` cannot make the rows below pass.
fn configured_span(minutes: i64) -> Duration {
    Duration::from_secs(u64::try_from(minutes).unwrap_or(0) * SECONDS_PER_MINUTE)
}

/// How long after the call that started it a client's first read lands, for one
/// configured idle span: close behind the call, at the interval a recorded client polls
/// at, a good deal later, just inside the configured span, just past it, and long past
/// it.
fn read_offsets(span: Duration) -> Vec<(&'static str, Duration)> {
    vec![
        ("one second after the call", MIN_READ_OFFSET),
        ("one client poll interval after it", CLIENT_POLL_INTERVAL),
        ("three client poll intervals after it", CLIENT_POLL_INTERVAL * 3),
        (
            "just inside the configured idle span",
            span.saturating_sub(BOUNDARY_SLACK).max(MIN_READ_OFFSET),
        ),
        (
            "just past the configured idle span",
            span + BOUNDARY_SLACK,
        ),
        (
            "long past the configured idle span",
            span + Duration::from_secs(10 * SECONDS_PER_MINUTE),
        ),
    ]
}

/// Rule: an application is `STARTED` for the whole idle span its `autoStopConfiguration`
/// configures, and `STOPPED` once that span has run out — so the state a client is told
/// the start settled on does not depend on how long after `StartApplication` the client's
/// first read lands, and an application given idle minutes is never stopped before they
/// have run. No `StopApplication` is called anywhere in this case.
///
/// The rows span the configured timeout (both published bounds, the documented default,
/// and a value between them, with the stop switched on and off) crossed with how far into
/// the configured span the client's first read lands.
#[tokio::test]
async fn an_application_is_started_until_the_idle_minutes_it_was_given_have_run() {
    let rows = auto_stop_rows();
    for row in &rows {
        let span = configured_span(row.effective_minutes);
        for (index, (offset_case, offset)) in read_offsets(span).into_iter().enumerate() {
            let (router, engine) = emulator_with_engine();
            let application_id =
                started_application(&router, row, &format!("tok-idle-{index}")).await;
            rewind_last_activity(&engine, &application_id, offset);

            let read = get_application(&router, &application_id).await.at("/application");
            // Whatever span the emulator serves it as, the configured value is what
            // every read reports.
            assert_eq!(
                read.pointer("/autoStopConfiguration/idleTimeoutMinutes")
                    .and_then(Value::as_i64),
                Some(row.effective_minutes),
                "case {:?}/{offset_case:?}: every read must report the configured \
                 idleTimeoutMinutes {}, got {:?}",
                row.case,
                row.effective_minutes,
                read.pointer("/autoStopConfiguration/idleTimeoutMinutes")
            );
            let expected = if row.stops && offset > span {
                "STOPPED"
            } else {
                "STARTED"
            };
            let state = read.get("state").and_then(Value::as_str).unwrap_or_default();
            assert_eq!(
                state, expected,
                "case {:?}/{offset_case:?}: {} configured idle minute(s) is {span:?} of \
                 idleness, so a read {offset:?} after the start must be {expected}, got \
                 {state:?} with stateDetails {:?}",
                row.case,
                row.effective_minutes,
                read.get("stateDetails")
            );
            if expected == "STOPPED" {
                let details = read
                    .get("stateDetails")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                assert!(
                    details.contains("idle"),
                    "case {:?}/{offset_case:?}: the automatic stop must say it was idle, \
                     got {:?}",
                    row.case,
                    read.get("stateDetails")
                );
            }
        }
    }
}

/// Rule, at the HTTP edge with no synthetic clock: a client that polls `GetApplication`
/// until the application leaves `STARTING` is told it settled on `STARTED`, for every
/// configured idle timeout — including the smallest the model admits.
#[tokio::test]
async fn an_application_leaves_starting_as_started_for_every_configured_idle_timeout() {
    let router = emulator();
    let rows = auto_stop_rows();
    for (index, row) in rows.iter().enumerate() {
        let application_id =
            started_application(&router, row, &format!("tok-starting-{index}")).await;
        let mut settled = None;
        for _ in 0..POLL_LOOKS {
            let state = get_application(&router, &application_id)
                .await
                .at("/application/state")
                .as_str()
                .unwrap_or_default()
                .to_string();
            if state != "STARTING" {
                settled = Some(state);
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        assert_eq!(
            settled.as_deref(),
            Some("STARTED"),
            "case {:?}: a start must settle on STARTED for a polling client, got {:?}",
            row.case,
            settled
        );
    }
}

/// The same rule on the other idle timeout this service is configured with: a session is
/// `STARTED` for the whole span the `idleTimeoutMinutes` its start asked for configures,
/// and `TERMINATED` once that span has run out — while the zero the range's minimum names
/// asks for no timeout and is owed no termination however late the read lands.
#[tokio::test]
async fn a_session_is_started_until_the_idle_minutes_it_asked_for_have_run() {
    // (case, the idleTimeoutMinutes the start asks for, whether a termination is owed)
    let rows: Vec<(&str, i64, bool)> = vec![
        ("the smallest timeout past the range's minimum", 1, true),
        ("twice that timeout", 2, true),
        (
            "the range's minimum, which names no timeout",
            MIN_SESSION_IDLE_MINUTES,
            false,
        ),
    ];

    for (case, minutes, ends) in rows {
        let span = configured_span(minutes);
        // One application per row, with a session per read offset on it: a session's own
        // idle timeout is the session's, so the sessions of one application are
        // independent of each other.
        let (router, engine) = emulator_with_engine();
        let (application_id, _) = session_application(&router, "tok-idle-session").await;
        for (index, (offset_case, offset)) in read_offsets(span).into_iter().enumerate() {
            let mut body = minimal_session(&format!("ses-idle-{index}"));
            body["idleTimeoutMinutes"] = json!(minutes);
            let started = start_session(&router, &application_id, body).await;
            assert_eq!(
                started.status, 200,
                "case {case:?}: StartSession expected 200, got {} with {:?}",
                started.status, started.body
            );
            let session_id = started
                .at("/sessionId")
                .as_str()
                .unwrap_or_default()
                .to_string();
            rewind_session_idle_since(&engine, &application_id, &session_id, offset);

            let session = get_session(&router, &application_id, &session_id)
                .await
                .at("/session");
            assert_eq!(
                session.get("idleTimeoutMinutes").and_then(Value::as_i64),
                Some(minutes),
                "case {case:?}/{offset_case:?}: every read must report the configured \
                 idleTimeoutMinutes {minutes}, got {:?}",
                session.get("idleTimeoutMinutes")
            );
            let expected = if ends && offset > span {
                "TERMINATED"
            } else {
                "STARTED"
            };
            let state = session
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                state, expected,
                "case {case:?}/{offset_case:?}: {minutes} configured idle minute(s) is \
                 {span:?} of idleness, so a read {offset:?} after the start must be \
                 {expected}, got {state:?} with stateDetails {:?}",
                session.get("stateDetails")
            );
        }
    }
}

/// The span a configured idle minute is served as, over the WHOLE published range of both
/// configurations: it is the minute the request named, so no configured timeout can fall
/// due inside the window a client polls a lifecycle edge in, the span grows strictly with
/// the minutes configured, zero minutes is no span at all, and the largest value either
/// range admits stays inside the longest transient span this service ever waits out.
///
/// Exhaustive over `AutoStopConfig$idleTimeoutMinutes`'s `@range(min: 1, max: 10080)`,
/// and over the bounds of `StartSessionRequest$idleTimeoutMinutes`'s
/// `@range(min: 0, max: 1000000)` — plus one value past the application range's maximum,
/// which the same rule serves rather than a case of its own.
#[test]
fn the_span_a_configured_idle_minute_is_served_as_is_that_minute() {
    // (case, the minutes configured, the span they are served as)
    let cases: Vec<(&str, i64, Duration)> = vec![
        (
            "the session range's minimum names no timeout",
            MIN_SESSION_IDLE_MINUTES,
            Duration::ZERO,
        ),
        (
            "the published minimum",
            MIN_IDLE_MINUTES,
            Duration::from_secs(SECONDS_PER_MINUTE),
        ),
        (
            "twice the published minimum",
            MIN_IDLE_MINUTES * 2,
            Duration::from_secs(2 * SECONDS_PER_MINUTE),
        ),
        (
            "the documented default",
            DEFAULT_IDLE_MINUTES,
            Duration::from_secs(15 * SECONDS_PER_MINUTE),
        ),
        (
            "a value between the bounds",
            120,
            Duration::from_secs(120 * SECONDS_PER_MINUTE),
        ),
        (
            "the published maximum",
            MAX_IDLE_MINUTES,
            Duration::from_secs(10_080 * SECONDS_PER_MINUTE),
        ),
        (
            "one minute past the published maximum",
            MAX_IDLE_MINUTES + 1,
            Duration::from_secs(10_081 * SECONDS_PER_MINUTE),
        ),
        (
            "the session range's maximum",
            MAX_SESSION_IDLE_MINUTES,
            Duration::from_secs(1_000_000 * SECONDS_PER_MINUTE),
        ),
    ];

    for (case, minutes, expected) in cases {
        let span = idle_span(u64::try_from(minutes).unwrap_or(0));
        assert_eq!(
            span, expected,
            "case {case:?}: {minutes} configured idle minute(s) must be served as \
             {expected:?}, got {span:?}"
        );
        assert!(
            span <= MAX_TRANSIENT_SPAN,
            "case {case:?}: the span must stay inside {MAX_TRANSIENT_SPAN:?}, got {span:?}"
        );
    }

    let smallest = idle_span(u64::try_from(MIN_IDLE_MINUTES).unwrap_or(1));
    assert!(
        smallest > LIFECYCLE_TRANSITION + CLIENT_POLL_INTERVAL,
        "case {MIN_IDLE_MINUTES}: the smallest configured idle timeout must outlast the \
         transition that brings a resource up plus the interval a client polls that edge \
         at, got {smallest:?} against {LIFECYCLE_TRANSITION:?} and \
         {CLIENT_POLL_INTERVAL:?}"
    );

    let mut previous = smallest;
    for minutes in (MIN_IDLE_MINUTES + 1)..=MAX_IDLE_MINUTES {
        let span = idle_span(u64::try_from(minutes).unwrap_or(1));
        assert!(
            span > previous,
            "case {minutes}: more configured idle minutes must be a longer span, got \
             {span:?} after {previous:?}"
        );
        previous = span;
    }
}
