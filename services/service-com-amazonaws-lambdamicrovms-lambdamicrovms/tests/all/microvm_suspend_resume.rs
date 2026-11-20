// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the caller-requested MicroVM run-state transitions —
//! `SuspendMicrovm` and `ResumeMicrovm` — driven over the wire through the same router
//! the emulator boots.
//!
//! Each test states a RULE and exercises it over a SET of inputs: every run state
//! `MicrovmState` publishes is a row, and the values any one row happens to carry are
//! examples, not the specification. The harness (`emulator`, `call`, `built_image`,
//! `run_microvm`, …) is the one the lifecycle suite beside this file already defines.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    ComAmazonawsLambdamicrovmsLambdamicrovmsState as MicrovmStore, SUSPENDED_EXPIRY_REASON,
};
use smithy_server_runtime::MultiServiceRouter;

use super::microvm_lifecycle::{
    as_uri_value, built_image, call, emulator, emulator_with_engine, expected_microvm_arn,
    poll_until_microvm_state, run_microvm, PAST_THE_SPAN, REGION,
};

/// The run states a MicroVM of this store can be observed in. Source:
/// `smithy_query(query="shape",shape="MicrovmState")` — `PENDING`, `RUNNING`,
/// `SUSPENDING`, `SUSPENDED`, `TERMINATING`, `TERMINATED`.
const PENDING: &str = "PENDING";
const RUNNING: &str = "RUNNING";
const SUSPENDING: &str = "SUSPENDING";
const SUSPENDED: &str = "SUSPENDED";
const TERMINATING: &str = "TERMINATING";
const TERMINATED: &str = "TERMINATED";

/// The two transitions a caller can ask for, as the URI suffix each is bound to:
/// `POST /2025-09-09/microvms/{microvmIdentifier}/suspend` and `…/resume`
/// (`smithy_query(query="operation",operation="SuspendMicrovm,ResumeMicrovm")`).
const SUSPEND: &str = "suspend";
const RESUME: &str = "resume";

/// Ask `asked` of the MicroVM `identifier` names.
async fn transition(router: &MultiServiceRouter, asked: &str, identifier: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/2025-09-09/microvms/{}/{asked}", as_uri_value(identifier)),
        None,
    )
    .await
}

/// Read the stored MicroVM back through `GetMicrovm`.
async fn get_microvm(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/2025-09-09/microvms/{}", as_uri_value(identifier)),
        None,
    )
    .await
}

/// Terminate the MicroVM `identifier` names.
async fn terminate(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/2025-09-09/microvms/{}", as_uri_value(identifier)),
        None,
    )
    .await
}

/// A fresh MicroVM of `image`, driven to `state` and read back to prove it is there.
///
/// Each state is reached the way a client reaches it: by the call that starts the
/// lifecycle, plus the wait for the move the clock owes afterwards.
async fn microvm_in_state(router: &MultiServiceRouter, image: &str, state: &str) -> String {
    let (status, run) = run_microvm(router, json!({"imageIdentifier": image})).await;
    assert_eq!(
        status, 200,
        "seed {state:?}: RunMicrovm: expected 200, got {status} {run}"
    );
    let id = run["microvmId"].as_str().unwrap_or_default().to_string();

    match state {
        // A MicroVM is `PENDING` until the provisioning the clock owes is due, so this
        // row is the one that must NOT wait.
        PENDING => {}
        RUNNING => tokio::time::sleep(PAST_THE_SPAN).await,
        SUSPENDING | SUSPENDED => {
            tokio::time::sleep(PAST_THE_SPAN).await;
            let (status, body) = transition(router, SUSPEND, &id).await;
            assert_eq!(
                status, 200,
                "seed {state:?}: SuspendMicrovm: expected 200, got {status} {body}"
            );
            if state == SUSPENDED {
                tokio::time::sleep(PAST_THE_SPAN).await;
            }
        }
        TERMINATING | TERMINATED => {
            tokio::time::sleep(PAST_THE_SPAN).await;
            let (status, body) = terminate(router, &id).await;
            assert_eq!(
                status, 200,
                "seed {state:?}: TerminateMicrovm: expected 200, got {status} {body}"
            );
            if state == TERMINATED {
                tokio::time::sleep(PAST_THE_SPAN).await;
            }
        }
        other => panic!("seed state {other:?} is not a MicrovmState this harness drives to"),
    }

    let (status, got) = get_microvm(router, &id).await;
    assert_eq!(
        status, 200,
        "seed {state:?}: GetMicrovm: expected 200, got {status} {got}"
    );
    assert_eq!(
        got["state"].as_str(),
        Some(state),
        "seed {state:?}: the MicroVM must really be in that state, got {:?}",
        got["state"]
    );
    id
}

/// The stored record without its run state, so two reads can be compared on everything
/// a run-state transition must leave alone.
fn without_run_state(body: &Value) -> Value {
    let mut copy = body.clone();
    if let Some(object) = copy.as_object_mut() {
        object.remove("state");
    }
    copy
}

/// RULE: `SuspendMicrovm` moves a running MicroVM through `SUSPENDING` to `SUSPENDED`,
/// the second move owed by the clock, and `ResumeMicrovm` restores it to `RUNNING` —
/// each transition writing the stored record and nothing else about it, so the MicroVM
/// comes back with the memory-and-disk record it was suspended with.
#[tokio::test]
async fn suspend_and_resume_move_the_stored_microvm_between_its_run_states() {
    let router = emulator();
    built_image(&router, "suspendable").await;
    let id = microvm_in_state(&router, "suspendable", RUNNING).await;
    let (_, running) = get_microvm(&router, &id).await;

    let (status, body) = transition(&router, SUSPEND, &id).await;
    assert_eq!(status, 200, "suspend: expected 200, got {status} {body}");

    let (_, suspending) = get_microvm(&router, &id).await;
    assert_eq!(
        suspending["state"].as_str(),
        Some(SUSPENDING),
        "a read straight after the suspend observes the transition in flight, got {:?}",
        suspending["state"]
    );

    tokio::time::sleep(PAST_THE_SPAN).await;
    let (_, suspended) = get_microvm(&router, &id).await;
    assert_eq!(
        suspended["state"].as_str(),
        Some(SUSPENDED),
        "the SUSPENDING -> SUSPENDED move the clock made due is materialised, got {:?}",
        suspended["state"]
    );
    assert_eq!(
        without_run_state(&suspended),
        without_run_state(&running),
        "the suspend preserves the whole record apart from its run state: {suspended} vs {running}"
    );

    let (status, body) = transition(&router, RESUME, &id).await;
    assert_eq!(status, 200, "resume: expected 200, got {status} {body}");
    let (_, resumed) = get_microvm(&router, &id).await;
    assert_eq!(
        resumed, running,
        "the resume restores the record the MicroVM was suspended with, got {resumed}"
    );

    // The run-state transitions leave the MicroVM terminable, and the record the
    // termination then writes is the one a read reports.
    let (status, body) = terminate(&router, &id).await;
    assert_eq!(
        status, 200,
        "terminate after a resume: expected 200, got {status} {body}"
    );
    let (_, ending) = get_microvm(&router, &id).await;
    assert_eq!(
        ending["state"].as_str(),
        Some(TERMINATING),
        "a resumed MicroVM still terminates, got {:?}",
        ending["state"]
    );
}

/// RULE: both transitions are `@idempotent` — asking a MicroVM for the run state it
/// already has (or is already coming to rest in) succeeds and changes no part of the
/// stored record.
#[tokio::test]
async fn asking_for_the_run_state_a_microvm_already_has_changes_nothing() {
    let router = emulator();
    built_image(&router, "idempotent").await;

    // Rows span both transitions and every state each one is already satisfied by.
    let rows: Vec<(&str, &str, &str)> = vec![
        ("a suspend of a MicroVM still coming to rest", SUSPEND, SUSPENDING),
        ("a suspend of an already suspended MicroVM", SUSPEND, SUSPENDED),
        ("a resume of an already running MicroVM", RESUME, RUNNING),
    ];

    for (case, asked, state) in rows {
        let id = microvm_in_state(&router, "idempotent", state).await;
        let (_, before) = get_microvm(&router, &id).await;

        let (status, body) = transition(&router, asked, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body}"
        );

        let (_, after) = get_microvm(&router, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: the record must be left exactly as it was, got {after}"
        );
    }
}

/// RULE: a transition starts only from the run state it leads out of — a suspend from a
/// running MicroVM, a resume from a suspended one (including one whose suspend has not
/// settled yet) — and reaches the run state it leads to at once or on the clock.
#[tokio::test]
async fn a_transition_starts_from_every_state_it_leads_out_of() {
    let router = emulator();
    built_image(&router, "startable").await;

    // Rows span each (transition, start state) pair the rule serves, and the state the
    // MicroVM is in once the transition's own clock-owed move is due.
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        ("a suspend of a running MicroVM", SUSPEND, RUNNING, SUSPENDED),
        (
            "a resume of a suspended MicroVM",
            RESUME,
            SUSPENDED,
            RUNNING,
        ),
        (
            "a resume of a MicroVM still coming to rest",
            RESUME,
            SUSPENDING,
            RUNNING,
        ),
    ];

    for (case, asked, from, settled) in rows {
        let id = microvm_in_state(&router, "startable", from).await;

        let (status, body) = transition(&router, asked, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {body}"
        );

        tokio::time::sleep(PAST_THE_SPAN).await;
        let (_, got) = get_microvm(&router, &id).await;
        assert_eq!(
            got["state"].as_str(),
            Some(settled),
            "case {case:?}: expected the MicroVM to settle in {settled:?}, got {:?}",
            got["state"]
        );
    }
}

/// RULE: a run-state transition asked of a MicroVM whose state has no such transition is
/// refused with the modelled 400 `ValidationException` and changes nothing: a MicroVM
/// that has entered its terminal lifecycle is told its state can be changed no further,
/// and any other state that no transition starts from is told the transition cannot be
/// made from the state the record is in.
#[tokio::test]
async fn a_transition_no_run_state_leads_out_of_is_refused() {
    let router = emulator();
    built_image(&router, "refusable").await;

    // Rows span both transitions across every state neither leads out of, and which of
    // the two refusals the state calls for.
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "suspending a MicroVM that is still provisioning",
            SUSPEND,
            PENDING,
        ),
        (
            "resuming a MicroVM that is still provisioning",
            RESUME,
            PENDING,
        ),
        ("suspending a MicroVM on its way out", SUSPEND, TERMINATING),
        ("resuming a MicroVM on its way out", RESUME, TERMINATING),
        ("suspending a terminated MicroVM", SUSPEND, TERMINATED),
        ("resuming a terminated MicroVM", RESUME, TERMINATED),
    ];

    for (case, asked, state) in rows {
        let id = microvm_in_state(&router, "refusable", state).await;
        let (_, before) = get_microvm(&router, &id).await;

        let (status, refusal) = transition(&router, asked, &id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {refusal}"
        );
        let expected_message = if state == TERMINATING || state == TERMINATED {
            format!("The MicroVM {id} has been terminated and its state cannot be changed.")
        } else {
            format!(
                "Cannot {asked} MicroVM in its current state: {}",
                expected_microvm_arn(&id)
            )
        };
        assert_eq!(
            refusal["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: the refusal must state the condition and name the MicroVM, got {refusal}"
        );

        let (_, after) = get_microvm(&router, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused transition must change nothing, got {after}"
        );
    }
}

/// How long is left of a deadline the store holds for the MicroVM `microvm_id`, read
/// through the shared engine — `None` where the record owes no such deadline.
///
/// `which` picks the deadline: the automatic suspend an `idlePolicy` owes a RUNNING
/// MicroVM, or the automatic termination it owes a resting one.
pub(crate) fn deadline_left(
    engine: &Arc<engine::InMemoryEngine>,
    microvm_id: &str,
    which: Deadline,
) -> Option<Duration> {
    let state = engine.state::<MicrovmStore>();
    let store = state.lock();
    let vm = store.microvm(REGION, microvm_id)?;
    let due = match which {
        Deadline::Idle => vm.idle_due_at,
        Deadline::Rest => vm.suspended_expiry_due_at,
    }?;
    Some(due.saturating_duration_since(Instant::now()))
}

/// Move one of those deadlines to `left` from now, and into the PAST where the caller
/// names no time left — which is the same thing as waiting it out: the store holds the
/// INSTANT each clock-owed move becomes due, so a test drives THAT clock rather than the
/// wall clock. The durations an `idlePolicy` carries are the caller's own and keep their
/// value, which is why no test waits them out.
pub(crate) fn set_deadline(
    engine: &Arc<engine::InMemoryEngine>,
    microvm_id: &str,
    which: Deadline,
    left: Option<Duration>,
) {
    let state = engine.state::<MicrovmStore>();
    let mut store = state.lock();
    let vm = store
        .microvm_mut(REGION, microvm_id)
        .unwrap_or_else(|| panic!("the MicroVM {microvm_id} is in the store"));
    let now = Instant::now();
    let due = match left {
        Some(left) => now.checked_add(left).unwrap_or(now),
        None => now
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
    };
    match which {
        Deadline::Idle => vm.idle_due_at = Some(due),
        Deadline::Rest => vm.suspended_expiry_due_at = Some(due),
    }
}

/// Which clock-owed deadline of an `idlePolicy` a test is driving.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Deadline {
    /// `maxIdleDurationSeconds`: the automatic suspend.
    Idle,
    /// `suspendedDurationSeconds`: the automatic termination of a resting MicroVM.
    Rest,
}

/// Launch a MicroVM of `image` carrying an `idlePolicy` of the three durations `case`
/// names, and hand back its id once it reports `RUNNING` — the state from which the
/// policy's own idle span is measured.
async fn microvm_with_idle_policy(
    router: &MultiServiceRouter,
    image: &str,
    case: &str,
    max_idle: u64,
    rest_for: u64,
    auto_resume: bool,
) -> String {
    let (status, run) = run_microvm(
        router,
        json!({
            "imageIdentifier": image,
            "idlePolicy": {
                "maxIdleDurationSeconds": max_idle,
                "suspendedDurationSeconds": rest_for,
                "autoResumeEnabled": auto_resume,
            },
        }),
    )
    .await;
    assert_eq!(status, 200, "case {case:?}: run got {status} {run}");
    let id = run["microvmId"].as_str().unwrap_or_default().to_string();
    poll_until_microvm_state(router, case, &id, RUNNING).await;
    id
}

/// How far a deadline the store holds may sit from the duration the REQUEST gave it: a
/// deadline is armed at the instant the MicroVM entered the state it is measured from, so
/// what a case reads back is that duration less however long the emulator took to get
/// there — generous room for a loaded host, and far too tight to be satisfied by any other
/// duration a row carries.
const DEADLINE_SLACK: Duration = Duration::from_secs(30);

/// Assert that the `which` deadline of the MicroVM `microvm_id` is owed, `expected_seconds`
/// away — the duration the request gave it rather than a span of this emulator's choosing.
fn assert_deadline_owed(
    engine: &Arc<engine::InMemoryEngine>,
    microvm_id: &str,
    which: Deadline,
    expected_seconds: u64,
    case: &str,
    what: &str,
) {
    let expected = Duration::from_secs(expected_seconds);
    let left = deadline_left(engine, microvm_id, which)
        .unwrap_or_else(|| panic!("case {case:?}: {what}"));
    assert!(
        left <= expected && left + DEADLINE_SLACK > expected,
        "case {case:?}: expected about {expected_seconds}s left of {what}, got {left:?}"
    );
}

/// RULE: an `idlePolicy` is enforced by the PLATFORM's own clock, with no call from the
/// client: a MicroVM that carries one is suspended once `maxIdleDurationSeconds` has
/// passed with no inbound traffic through its endpoint, a MicroVM that then rests longer
/// than `suspendedDurationSeconds` is terminated, each deadline keeps the duration the
/// request gave it, and a MicroVM carrying no policy owes neither move.
///
/// Source for the members and their bounds:
/// `smithy_query(query="shape",shape="IdlePolicy")` — `maxIdleDurationSeconds`
/// `@range(min: 60)`, `suspendedDurationSeconds` `@range(min: 0)`, both required.
#[tokio::test]
async fn an_idle_policy_suspends_and_then_terminates_a_microvm_on_the_platforms_own_clock() {
    let (router, engine) = emulator_with_engine();
    built_image(&router, "idling").await;

    // Rows differ along the durations the policy carries — the smallest idle duration the
    // member admits, a longer one, and the smallest rest the member admits (0, which
    // leaves a MicroVM no time to rest at all) — because the rule is the same mechanism
    // whatever the figures are.
    let rows: Vec<(&str, u64, u64, bool)> = vec![
        ("the smallest idle duration the member admits", 60, 300, true),
        ("ten minutes idle and five of rest", 600, 300, true),
        ("an hour idle and an hour of rest", 3_600, 3_600, true),
        ("no time to rest at all", 60, 0, false),
    ];

    for (case, max_idle, rest_for, rests) in rows {
        let id = microvm_with_idle_policy(&router, "idling", case, max_idle, rest_for, true).await;

        // The deadline a RUNNING MicroVM owes is one `maxIdleDurationSeconds` away: the
        // caller's own duration, not a span of this emulator's choosing.
        assert_deadline_owed(
            &engine,
            &id,
            Deadline::Idle,
            max_idle,
            case,
            "the automatic suspend a MicroVM with a policy owes",
        );

        // Waiting the idle duration out: the suspend is the platform's, so no
        // SuspendMicrovm is called here at all.
        set_deadline(&engine, &id, Deadline::Idle, None);
        if rests {
            poll_until_microvm_state(&router, case, &id, SUSPENDED).await;

            // A resting MicroVM owes the other deadline, one `suspendedDurationSeconds`
            // away.
            assert_deadline_owed(
                &engine,
                &id,
                Deadline::Rest,
                rest_for,
                case,
                "the automatic termination a resting MicroVM with a policy owes",
            );
            set_deadline(&engine, &id, Deadline::Rest, None);
        }

        // "The maximum time in seconds that a MicroVM can remain suspended before it is
        // automatically terminated."
        poll_until_microvm_state(&router, case, &id, TERMINATED).await;
        let (status, ended) = get_microvm(&router, &id).await;
        assert_eq!(status, 200, "case {case:?}: read got {status} {ended}");
        assert_eq!(
            ended["stateReason"].as_str(),
            Some(SUSPENDED_EXPIRY_REASON),
            "case {case:?}: the platform's own termination names its reason, got {ended}"
        );
        assert!(
            ended["terminatedAt"].as_f64().is_some(),
            "case {case:?}: a terminated MicroVM carries the instant it ended, got {ended}"
        );
    }

    // The control: a MicroVM carrying NO policy owes neither move, so nothing the
    // platform's clock materialises can take it out of RUNNING — there is no deadline to
    // bring forward, which is the whole of the claim.
    let (status, run) = run_microvm(&router, json!({"imageIdentifier": "idling"})).await;
    assert_eq!(status, 200, "run without a policy: got {status} {run}");
    let id = run["microvmId"].as_str().unwrap_or_default().to_string();
    poll_until_microvm_state(&router, "no policy at all", &id, RUNNING).await;
    assert!(
        deadline_left(&engine, &id, Deadline::Idle).is_none(),
        "a MicroVM with no idlePolicy owes no automatic suspend"
    );
    assert!(
        deadline_left(&engine, &id, Deadline::Rest).is_none(),
        "a MicroVM with no idlePolicy owes no automatic termination either"
    );
    // Every read materialises the moves the clock has made due, so a read that still
    // reports RUNNING is one that found none owed.
    let (_, got) = get_microvm(&router, &id).await;
    assert_eq!(
        got["state"].as_str(),
        Some(RUNNING),
        "a MicroVM with no idlePolicy is never auto-suspended, got {:?}",
        got["state"]
    );
}

/// RULE: a transition addressed at an identifier naming no stored MicroVM answers 404
/// `ResourceNotFoundException` built from the identifier the request carried — never a
/// 501, and never a hollow 200.
#[tokio::test]
async fn a_transition_on_an_unknown_identifier_answers_resource_not_found() {
    let router = emulator();
    built_image(&router, "present").await;
    let id = microvm_in_state(&router, "present", RUNNING).await;

    // Rows span the identifier FORMS `MicrovmIdentifier` admits ("The ARN or ID of the
    // MicroVm"): an id nothing holds, the longest id the member admits (len 1..256), an
    // ARN of this service's own form that was never minted, and another account's ARN
    // of the MicroVM that does exist.
    let rows: Vec<(&str, String)> = vec![
        ("an id no MicroVM carries", "mvm-does-not-exist".to_string()),
        ("the longest id the member admits", "m".repeat(256)),
        (
            "an ARN this service never minted",
            expected_microvm_arn("mvm-00000000deadbeef"),
        ),
        (
            "another account's ARN for a MicroVM that exists",
            format!("arn:aws:lambda:{REGION}:999999999999:microvm:{id}"),
        ),
    ];

    for (case, identifier) in rows {
        for asked in [SUSPEND, RESUME] {
            let (status, refusal) = transition(&router, asked, &identifier).await;
            assert_eq!(
                status, 404,
                "case {case:?} ({asked}): expected 404, got {status} {refusal}"
            );
            assert_eq!(
                refusal["message"].as_str(),
                Some(format!("MicroVM not found: {identifier}").as_str()),
                "case {case:?} ({asked}): the 404 names the identifier the request carried, got {refusal}"
            );
        }
    }

    let (_, got) = get_microvm(&router, &id).await;
    assert_eq!(
        got["state"].as_str(),
        Some(RUNNING),
        "no refused transition may touch the MicroVM that does exist, got {:?}",
        got["state"]
    );
}

/// How many times the idle-policy case below reads a MicroVM whose idle duration has NOT
/// passed. A clock-owed move is materialised by whatever call next reads the record, so a
/// read that brought the automatic suspend forward would show up here.
const READS_WHILE_STILL_IDLING: usize = 5;

/// RULE: the suspend an `idlePolicy` owes is the PLATFORM's own and is owed by the CLOCK
/// alone — a MicroVM carrying a policy keeps reporting `RUNNING` while
/// `maxIdleDurationSeconds` has not passed without inbound traffic, however often it is
/// read, so the span really runs rather than firing on launch — and once that duration has
/// passed the MicroVM comes to rest in the SAME `SUSPENDED` state a requested suspend
/// brings it to, carrying the whole record it was running with: a `SuspendMicrovm` is then
/// the no-op an already suspended MicroVM answers, and `ResumeMicrovm` puts it back in
/// `RUNNING` on a fresh idle span.
///
/// Source for the policy's members and their bounds:
/// `smithy_query(query="shape",shape="IdlePolicy")` — `maxIdleDurationSeconds`
/// `@range(min: 60)`, `suspendedDurationSeconds` `@range(min: 0)` and `autoResumeEnabled`,
/// all three required.
#[tokio::test]
async fn the_platforms_own_suspend_waits_for_the_idle_duration_and_rests_like_a_requested_one() {
    let (router, engine) = emulator_with_engine();
    built_image(&router, "idle-rest").await;

    // Rows differ along the durations the policy carries — the smallest idle duration the
    // member admits and two longer ones, each with a rest long enough that the automatic
    // termination is not what ends the case — and along `autoResumeEnabled`, which governs
    // the TRAFFIC-driven resume and so cannot change what a requested one does.
    //
    // `reads_while_idling` says whether the row also drives the reads that must NOT bring
    // the suspend forward. That half of the claim can only be made while the span is still
    // running, so it belongs to the rows whose duration leaves generous room for a loaded
    // host to get through the reads — never to the row carrying the SMALLEST duration a
    // caller may ask for, which would be this case bounding its own wall clock by one
    // minute. The minimum-bound row still drives everything after it.
    let rows: Vec<(&str, u64, u64, bool, bool)> = vec![
        (
            "the smallest idle duration the member admits",
            60,
            3_600,
            true,
            false,
        ),
        (
            "a quarter of an hour idle, auto-resume off",
            900,
            1_800,
            false,
            true,
        ),
        ("eight hours idle", 28_800, 28_800, true, true),
    ];

    for (case, max_idle, rest_for, auto_resume, reads_while_idling) in rows {
        let id =
            microvm_with_idle_policy(&router, "idle-rest", case, max_idle, rest_for, auto_resume)
                .await;

        if reads_while_idling {
            // Nothing has reached the MicroVM's endpoint and its idle duration has not
            // passed, so no read may take it out of RUNNING.
            for read in 1..=READS_WHILE_STILL_IDLING {
                let (_, got) = get_microvm(&router, &id).await;
                assert_eq!(
                    got["state"].as_str(),
                    Some(RUNNING),
                    "case {case:?}: read {read} must not bring the automatic suspend forward, \
                     got {:?}",
                    got["state"]
                );
            }
            // And the suspend is still OWED after them: the reads neither took it nor
            // dropped it. Its VALUE is asserted where it is armed — at the launch above by
            // the case beside this one, and after the resume below.
            let left = deadline_left(&engine, &id, Deadline::Idle)
                .unwrap_or_else(|| panic!("case {case:?}: a MicroVM with a policy owes a suspend"));
            assert!(
                left > Duration::ZERO && left <= Duration::from_secs(max_idle),
                "case {case:?}: the suspend must still be owed, within the caller's own \
                 {max_idle}s, got {left:?}"
            );
        }
        // The whole of the rest of this case is driven off the STORE's clock, so put the
        // idle deadline back a full span from here — where arming it on reaching RUNNING
        // put it — and no later step depends on how fast this host got through the reads.
        set_deadline(
            &engine,
            &id,
            Deadline::Idle,
            Some(Duration::from_secs(max_idle)),
        );
        let (_, running) = get_microvm(&router, &id).await;

        // Waiting that duration out, by moving the instant the store holds it at into the
        // past: the suspend is the PLATFORM's, so no SuspendMicrovm is called here at all.
        set_deadline(&engine, &id, Deadline::Idle, None);
        poll_until_microvm_state(&router, case, &id, SUSPENDED).await;
        let (_, suspended) = get_microvm(&router, &id).await;
        assert_eq!(
            without_run_state(&suspended),
            without_run_state(&running),
            "case {case:?}: the platform's own suspend preserves the whole record apart \
             from its run state: {suspended} vs {running}"
        );

        // It rests exactly where a requested suspend rests: asking for the suspend it has
        // already taken is the no-op `SuspendMicrovm` answers a suspended MicroVM with.
        let (status, asked) = transition(&router, SUSPEND, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: suspending an auto-suspended MicroVM got {status} {asked}"
        );
        let (_, after_suspend) = get_microvm(&router, &id).await;
        assert_eq!(
            after_suspend, suspended,
            "case {case:?}: that suspend must change nothing, got {after_suspend}"
        );

        // And a requested resume brings it back on a FRESH span of the caller's own idle
        // duration, which is what makes the automatic suspend repeatable rather than
        // terminal.
        let (status, resumed) = transition(&router, RESUME, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: resuming an auto-suspended MicroVM got {status} {resumed}"
        );
        let (_, back) = get_microvm(&router, &id).await;
        assert_eq!(
            back["state"].as_str(),
            Some(RUNNING),
            "case {case:?}: the resume restores RUNNING, got {:?}",
            back["state"]
        );
        // The whole record, run state included: a MicroVM the platform suspended and the
        // caller resumed is the one it was running with, not a copy of it.
        assert_eq!(
            back, running,
            "case {case:?}: the resume restores the record the MicroVM was running with, \
             got {back}"
        );
        assert_deadline_owed(
            &engine,
            &id,
            Deadline::Idle,
            max_idle,
            case,
            "the fresh automatic suspend a MicroVM that runs again owes",
        );
    }
}
