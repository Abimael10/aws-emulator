// SPDX-License-Identifier: CC0-1.0

//! `GenerateCredentialReport`, end to end over the awsQuery wire.
//!
//! Every test drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is asserted is the
//! HTTP behaviour (status, awsQuery error code, XML members) a client on the socket sees.
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other
//! suites in this crate share.
//!
//! The rules being asserted:
//!
//! * the action is ROUTED — a bound operation of this service answers it rather than the
//!   dispatcher's `InvalidAction`;
//! * what a call reports is the state of the account's ONE credential-report generation,
//!   read off that record and the clock: a call with none on record starts one
//!   (`STARTED`), a call while it runs reports `INPROGRESS`, the call that first finds it
//!   due reports `COMPLETE`, a request for another report inside the published
//!   once-every-four-hours rate generates none and reports that same `COMPLETE`, and from
//!   the end of that window a new generation starts;
//! * a caller whose identity-based policies do not allow the action is refused, and a
//!   refused call starts nothing;
//! * the published rate governs whether a NEW report is generated, never whether the call
//!   is served: an authorized caller repeating the call inside that window is answered the
//!   state of the generation the account holds, however many times it asks, wherever the
//!   clock stands inside the window and whenever its permission arrived — so the documented
//!   "call until the state is COMPLETE" loop can be completed, and the report the account
//!   holds stays readable and unchanged while it is.
//!
//! Each suite is a TABLE whose rows differ along what those rules generalise over: where
//! the clock stands relative to the generation's due instant and to the published rate —
//! including the instant on each side of both — which identity drives the call, and what
//! that caller is permitted.
//!
//! A row stands the clock where it needs it through the clock the EMULATOR holds
//! ([`router_with_a_standing_clock`], [`pass_seconds`]): the emulator's clock is stood
//! still for the whole suite and then moved on by a known span, so where the clock stands
//! when a call is served is the span the row named and nothing else. No row depends on how
//! long the host took to serve the calls before it, and none waits for real time to pass.

use std::sync::Arc;
use std::time::Duration;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::REPORT_STATE_TYPE_VALUES;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::credential_report_reads::get;
use crate::identity_policy_enforcement::{grant_action, principal, statement};
use crate::user_lifecycle::{
    account_id, assert_access_denied_message, call_signed_by, element, elements, error_code,
    router_with_engine,
};

/// The three values `ReportStateType` declares, named here so a row reads as the state it
/// is about. Source: `ops::types::REPORT_STATE_TYPE_VALUES`, the model's own value set.
const STARTED: &str = "STARTED";
const INPROGRESS: &str = "INPROGRESS";
const COMPLETE: &str = "COMPLETE";

/// One `GenerateCredentialReport` call, presented as signed by `akid` when a row names
/// one. The operation takes no input, so the call carries no form parameter beyond the
/// action.
///
/// `pub(crate)` because the suite about READING the report
/// (`tests/all/credential_report_reads.rs`) starts its generations through this same call.
pub(crate) async fn generate(router: &MultiServiceRouter, akid: Option<&str>) -> (u16, String) {
    call_signed_by(router, akid, "GenerateCredentialReport", &[]).await
}

/// A router and the engine behind it, with this emulator's clock stood STILL.
///
/// Every span the service owes is then held open until [`pass_seconds`] brings the clock to
/// it, so a row that asks for a generation in flight sees one however long the host takes,
/// and a row that asks for one past its due instant stands exactly the span it named past it
/// rather than that span plus whatever real time went by.
///
/// `pub(crate)` because the suite about READING the report stands its clock the same way:
/// one clock, so the two suites cannot disagree about where it is.
pub(crate) fn router_with_a_standing_clock() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = router_with_engine();
    ::service_common::clock::stop(&*engine).expect("the emulator's clock");
    (router, engine)
}

/// Move this emulator's clock `seconds` on, which is how a row crosses a span the service
/// owes without sleeping for it: the generation record holds the instant the generation was
/// asked for and the instant it is due, and every state a call reports is read off those two
/// against this same clock.
///
/// `pub(crate)` for the same reason [`router_with_a_standing_clock`] is.
pub(crate) fn pass_seconds(engine: &engine::InMemoryEngine, seconds: i64) {
    let span = Duration::from_secs(u64::try_from(seconds).expect("a span the clock moves on by"));
    ::service_common::clock::advance(engine, span).expect("the emulator's clock");
}

/// Whether the account holds a credential-report generation at all: what a refused call
/// must leave untouched.
fn holds_a_generation(engine: &engine::InMemoryEngine) -> bool {
    let store = state::service_state(engine).expect("the IAM store");
    let held = store.lock().credential_report.is_some();
    held
}

/// Assert the call answered 200 with `state` and a non-empty description, and that the
/// state is one the model declares.
fn assert_state(case: &str, status: u16, body: &str, expected: &str) {
    assert_eq!(
        status, 200,
        "case {case:?}: expected the generation to be served, got {status}: {body}"
    );
    let reported = element(body, "State");
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the state {expected}, got {reported:?}: {body}"
    );
    assert!(
        REPORT_STATE_TYPE_VALUES.contains(&reported.as_str()),
        "case {case:?}: expected a state ReportStateType declares, got {reported:?}"
    );
    assert!(
        !element(body, "Description").is_empty(),
        "case {case:?}: expected the response to describe the report, got {body}"
    );
}

/// Assert the call was SERVED rather than refused, and reports `state`.
///
/// A poll carries no error document at all: the published rate decides whether a new report
/// is GENERATED, and the one client-side failure the operation declares is about the
/// account's resource limits, so neither of them belongs on the answer to a call the service
/// authorized. A row therefore asserts the response is not a failure envelope before reading
/// the state off it.
fn assert_served(case: &str, status: u16, body: &str, state: &str) {
    let codes = elements(body, "Code");
    assert!(
        codes.is_empty(),
        "case {case:?}: expected a served state rather than the refusal {codes:?}: {body}"
    );
    assert_state(case, status, body, state);
}

/// The instant a served read reports the account's report was generated at, which is the
/// generation that produced it: it changes only when a NEW generation is started, so it is
/// what a row asserts a poll started none by.
async fn generated_time(case: &str, router: &MultiServiceRouter) -> String {
    let (status, body) = get(router, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the account's report to be readable, got {status}: {body}"
    );
    assert!(
        !element(&body, "Content").is_empty(),
        "case {case:?}: expected the readable report to carry content, got {body}"
    );
    element(&body, "GeneratedTime")
}

/// RULE: the action is a bound operation of this service, so a call reaches a handler and
/// is answered with the operation's own response rather than with the dispatcher's
/// `InvalidAction` for an action the service does not have.
#[tokio::test]
async fn the_action_is_routed_rather_than_answered_as_an_unknown_action() {
    let (router, _engine) = router_with_engine();
    let (status, body) = generate(&router, None).await;
    assert!(
        !body.contains("<Code>"),
        "expected the action to be routed rather than answered with an error document, \
         got {body}"
    );
    assert_state("the first call of an account", status, &body, STARTED);
    assert_eq!(
        element(&body, "Description"),
        "No report exists. Starting a new report generation task",
        "expected the first call to describe the task it started, got {:?}",
        element(&body, "Description")
    );
}

/// RULE: what a call reports is the state of the account's one credential-report
/// generation, read off that record and the clock — so the rows are where the clock
/// stands: before the generation is due, at it, and on each side of the published rate
/// that governs generating another report. Every value `ReportStateType` declares is
/// reached.
#[tokio::test]
async fn the_clock_and_the_held_generation_are_what_a_call_reports() {
    let (router, engine) = router_with_a_standing_clock();

    // No generation on record: one is started.
    let (status, body) = generate(&router, None).await;
    assert_state("no generation on record", status, &body, STARTED);
    assert!(
        holds_a_generation(&engine),
        "the started generation must be recorded"
    );

    // Still running, because the span is stored rather than slept for: a caller that calls
    // again straight away sees the generation in flight, and sees it however many times it
    // asks.
    for attempt in 0..2 {
        let case = format!("a generation still running, attempt {attempt}");
        let (status, body) = generate(&router, None).await;
        assert_state(&case, status, &body, INPROGRESS);
    }

    // One second short of due: still running. The span is the emulator's own
    // (`state::CREDENTIAL_REPORT_GENERATION_SECONDS`), so the row is written against it
    // rather than against a literal, and the clock stands exactly there because it is the
    // emulator's clock the row moves.
    pass_seconds(&engine, state::CREDENTIAL_REPORT_GENERATION_SECONDS - 1);
    let (status, body) = generate(&router, None).await;
    assert_state("one second short of due", status, &body, INPROGRESS);

    // Due: the completion the clock already made due is reported.
    pass_seconds(&engine, 1);
    let (status, body) = generate(&router, None).await;
    assert_state("the generation has become due", status, &body, COMPLETE);

    // The account now HAS a report, so a further call is asking for a new one — which the
    // published rate does not generate yet: the call is served and reports the generation
    // the account holds, twice over and whatever the caller.
    for attempt in 0..2 {
        let case = format!("a report asked for again inside the rate, attempt {attempt}");
        let (status, body) = generate(&router, None).await;
        assert_state(&case, status, &body, COMPLETE);
    }

    // One second short of the rate's end: still the held generation.
    pass_seconds(
        &engine,
        state::CREDENTIAL_REPORT_REGENERATION_SECONDS - state::CREDENTIAL_REPORT_GENERATION_SECONDS
            - 1,
    );
    let (status, body) = generate(&router, None).await;
    assert_state("one second short of the rate's end", status, &body, COMPLETE);

    // From the rate's end: a new generation starts, so the account can get a fresh report.
    pass_seconds(&engine, 1);
    let (status, body) = generate(&router, None).await;
    assert_state("the rate now allows another report", status, &body, STARTED);
    let (status, body) = generate(&router, None).await;
    assert_state("the new generation is running", status, &body, INPROGRESS);
}

/// RULE: the state a call reports is a fact about the account's generation and the clock,
/// never about who asked — so two different callers polling one generation see the same
/// state, and the completion is reported to whichever call first finds it due.
#[tokio::test]
async fn the_reported_state_follows_the_generation_rather_than_the_caller() {
    let (router, engine) = router_with_a_standing_clock();
    let grant = vec![(
        "generate".to_string(),
        statement("Allow", "iam:GenerateCredentialReport", "*"),
    )];
    let first = principal(&router, "first poller", "report-reader-one", &grant).await;
    let second = principal(&router, "second poller", "report-reader-two", &grant).await;

    let (status, body) = generate(&router, Some(&first)).await;
    assert_state("the first caller starts the generation", status, &body, STARTED);
    let (status, body) = generate(&router, Some(&second)).await;
    assert_state("another caller polls the same generation", status, &body, INPROGRESS);

    pass_seconds(&engine, state::CREDENTIAL_REPORT_GENERATION_SECONDS);
    let (status, body) = generate(&router, Some(&second)).await;
    assert_state("the other caller finds it due", status, &body, COMPLETE);
    let (status, body) = generate(&router, Some(&first)).await;
    assert_state(
        "the caller that started it reads the same completion",
        status,
        &body,
        COMPLETE,
    );
}

/// RULE: IAM decides the call against the caller's identity-based policies before it
/// records anything, so a caller no policy allows the action for is refused with HTTP 403
/// `AccessDenied` naming the principal, the action and the account-wide resource — and the
/// refused call starts no generation.
#[tokio::test]
async fn a_caller_whose_policies_do_not_allow_the_call_is_refused() {
    // Rows: a caller whose only grant is for ANOTHER action, and one explicitly denied this
    // action by a second statement.
    let rows = vec![
        (
            "no allow at all",
            vec![(
                "unrelated".to_string(),
                statement("Allow", "iam:ListUsers", "*"),
            )],
            "because no identity-based policy allows the iam:GenerateCredentialReport \
             action"
                .to_string(),
        ),
        (
            "an explicit deny over the allow",
            vec![
                (
                    "allow".to_string(),
                    statement("Allow", "iam:GenerateCredentialReport", "*"),
                ),
                (
                    "deny".to_string(),
                    statement("Deny", "iam:GenerateCredentialReport", "*"),
                ),
            ],
            "with an explicit deny in an identity-based policy".to_string(),
        ),
    ];

    for (row, (case, policies, reason)) in rows.into_iter().enumerate() {
        let (router, engine) = router_with_engine();
        let user_name = format!("reporter{row}");
        let access_key_id = principal(&router, case, &user_name, &policies).await;
        let (status, body) = generate(&router, Some(&access_key_id)).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected 403, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "AccessDenied",
            "case {case:?}: expected AccessDenied, got {:?}: {body}",
            error_code(&body)
        );
        assert_access_denied_message(
            case,
            &element(&body, "Message"),
            &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
            "iam:GenerateCredentialReport",
            state::authorization::ACCOUNT_WIDE_RESOURCE,
            &reason,
        );
        assert!(
            !holds_a_generation(&engine),
            "case {case:?}: a refused call must start no generation"
        );
    }

    // The same call under a statement that names its action is served, so the gate decides
    // it from the policies rather than refusing everything.
    let (router, engine) = router_with_engine();
    let access_key_id = principal(
        &router,
        "a granted caller",
        "reporter-granted",
        &[(
            "generate".to_string(),
            statement("Allow", "iam:GenerateCredentialReport", "*"),
        )],
    )
    .await;
    let (status, body) = generate(&router, Some(&access_key_id)).await;
    assert_state("a granted caller", status, &body, STARTED);
    assert!(
        holds_a_generation(&engine),
        "a served call must record the generation it started"
    );
}

/// RULE: the response carries exactly the two members the output shape declares — one
/// `State` and one `Description` — so a client reading either finds a single value.
#[tokio::test]
async fn the_response_carries_one_state_and_one_description() {
    let (router, _engine) = router_with_engine();
    let (status, body) = generate(&router, None).await;
    assert_eq!(status, 200, "expected 200, got {status}: {body}");
    assert_eq!(
        elements(&body, "State").len(),
        1,
        "expected exactly one <State>, got {:?}: {body}",
        elements(&body, "State")
    );
    assert_eq!(
        elements(&body, "Description").len(),
        1,
        "expected exactly one <Description>, got {:?}: {body}",
        elements(&body, "Description")
    );
}

/// RULE: the span a generation takes and the rate that governs generating another report
/// are crossed by the EMULATOR's clock — the generation records its due instant from that
/// clock and every later call compares it against that same clock — so where the clock
/// stands when a call is served is the only thing that decides the state, and a call served
/// at a given span past the generation reports that span's state whatever the host was doing
/// in between.
///
/// The rows are the spans a call can be served at, each measured from the generation the
/// row's own emulator started: inside the generation span (at its start and the instant
/// before its end), at the instant it falls due, inside the rate (the instant before its
/// end), at the rate's end, and far past it.
#[tokio::test]
async fn the_emulators_own_clock_is_what_crosses_the_generation_span_and_the_rate() {
    let generation = state::CREDENTIAL_REPORT_GENERATION_SECONDS;
    let rate = state::CREDENTIAL_REPORT_REGENERATION_SECONDS;
    let rows = [
        ("no time at all past the generation", 0, INPROGRESS),
        ("the instant before the generation is due", generation - 1, INPROGRESS),
        ("the instant the generation falls due", generation, COMPLETE),
        ("the instant before the rate's end", rate - 1, COMPLETE),
        ("the instant the rate ends", rate, STARTED),
        // 86_400 seconds: a day, so the row stands far past the rate rather than at its edge.
        ("a whole day past the rate's end", rate + 86_400, STARTED),
    ];

    for (case, span, expected) in rows {
        // One emulator per row, so the span the row names is the only time that has passed
        // for the generation it is about.
        let (router, engine) = router_with_a_standing_clock();
        let (status, body) = generate(&router, None).await;
        assert_state(&format!("{case}: the generation is started"), status, &body, STARTED);

        pass_seconds(&engine, span);
        let (status, body) = generate(&router, None).await;
        assert_state(case, status, &body, expected);

        // Asked again with the clock standing still, the state is still read off the record:
        // the generation a row's call STARTED is now the one in flight, and every other state
        // is reported again rather than consumed by having been asked for once.
        let (status, body) = generate(&router, None).await;
        let again = if expected == STARTED { INPROGRESS } else { expected };
        assert_state(&format!("{case}, asked again"), status, &body, again);
    }
}

/// RULE: the published rate governs whether a NEW report is generated, never whether the
/// call is served — so a caller the service authorized is answered the state of the
/// generation the account holds for every repeat of the call inside that window, and the
/// report that generation produced stays readable and unchanged, with no new generation
/// started.
///
/// The rows are where inside the window the repeat is served, measured from the call that
/// reported the completion: at that very instant, a second later, halfway through the
/// window, and the instant before it ends. Each row then repeats the call, because the
/// documented way to use the operation is to call it until the state is `COMPLETE`, so no
/// repeat may be answered differently from the one before it.
#[tokio::test]
async fn an_authorized_repeat_inside_the_rate_is_served_wherever_the_clock_stands() {
    let generation = state::CREDENTIAL_REPORT_GENERATION_SECONDS;
    let rate = state::CREDENTIAL_REPORT_REGENERATION_SECONDS;
    let rows = [
        ("the instant the completion was reported", 0),
        ("a second past the completion", 1),
        ("halfway through the published rate", rate / 2),
        // The window is measured from the call that ASKED for the report, which stands
        // `generation` seconds before the completion this span is measured from.
        ("the instant before the published rate ends", rate - generation - 1),
    ];

    for (case, span) in rows {
        // One emulator per row, so the span the row names is the only time that has passed
        // since the account's report was generated.
        let (router, engine) = router_with_a_standing_clock();
        let (status, body) = generate(&router, None).await;
        assert_served(&format!("{case}: the generation is started"), status, &body, STARTED);
        pass_seconds(&engine, generation);
        let (status, body) = generate(&router, None).await;
        assert_served(&format!("{case}: the generation falls due"), status, &body, COMPLETE);

        let before = generated_time(case, &router).await;
        pass_seconds(&engine, span);

        // The documented loop: the call is made again, and again, and each repeat is served
        // the state of the generation the account holds.
        for attempt in 0..3 {
            let repeat = format!("{case}, repeat {attempt}");
            let (status, body) = generate(&router, None).await;
            assert_served(&repeat, status, &body, COMPLETE);
        }

        // No repeat started a generation of its own: the account's report is still the one
        // the first call produced, still readable, and reports the same instant.
        let after = generated_time(&format!("{case}, after the repeats"), &router).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the repeats to start no new generation, \
             so the report stays the one generated at {before:?}, got {after:?}"
        );
    }
}

/// RULE: the state a repeat is answered with is a fact about the account's generation and
/// the clock, so a caller the service authorizes for the action is served it whenever that
/// permission arrived — a grant put on a principal that was just refused makes the very
/// same call a served state rather than any other refusal.
///
/// This is the documented sequence end to end: the account polls its generation to
/// `COMPLETE`, reads the report, a principal holding no policy is refused, that principal is
/// granted the action, and its retry is served.
#[tokio::test]
async fn a_grant_that_arrives_inside_the_rate_makes_the_same_call_a_served_state() {
    let (router, engine) = router_with_a_standing_clock();

    // The account polls its own generation until it is complete, and reads the report.
    let (status, body) = generate(&router, None).await;
    assert_served("the account's first call", status, &body, STARTED);
    pass_seconds(&engine, state::CREDENTIAL_REPORT_GENERATION_SECONDS);
    let (status, body) = generate(&router, None).await;
    assert_served("the account's generation falls due", status, &body, COMPLETE);
    let before = generated_time("the report the account holds", &router).await;

    // A principal holding no identity-based policy at all: refused, as the authorization
    // leg decides and whatever the account's report is.
    let case = "a principal granted the action after being refused";
    let user_name = "report-poller";
    let access_key_id = principal(&router, case, user_name, &[]).await;
    let (status, body) = generate(&router, Some(&access_key_id)).await;
    assert_eq!(
        status, 403,
        "case {case:?}: expected the unpermitted call to be refused 403, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "AccessDenied",
        "case {case:?}: expected AccessDenied, got {:?}: {body}",
        error_code(&body)
    );

    // The grant arrives, and the SAME call by the SAME principal is served the state of the
    // generation the account holds — for every repeat of it, because the published rate is
    // about generating another report and not about serving the call.
    grant_action(&router, case, user_name, "iam:GenerateCredentialReport").await;
    for attempt in 0..3 {
        let repeat = format!("{case}, repeat {attempt}");
        let (status, body) = generate(&router, Some(&access_key_id)).await;
        assert_served(&repeat, status, &body, COMPLETE);
    }

    // The account's report is untouched by those polls: still readable, still the one the
    // first generation produced.
    let after = generated_time("the report after the granted polls", &router).await;
    assert_eq!(
        after, before,
        "case {case:?}: expected the granted polls to start no new generation, \
         so the report stays the one generated at {before:?}, got {after:?}"
    );
}
