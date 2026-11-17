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
        let (status, run) = run_microvm(
            &router,
            json!({
                "imageIdentifier": "idling",
                "idlePolicy": {
                    "maxIdleDurationSeconds": max_idle,
                    "suspendedDurationSeconds": rest_for,
                    "autoResumeEnabled": true,
                },
            }),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: run got {status} {run}");
        let id = run["microvmId"].as_str().unwrap_or_default().to_string();
        poll_until_microvm_state(&router, case, &id, RUNNING).await;

        // The deadline a RUNNING MicroVM owes is one `maxIdleDurationSeconds` away: the
        // caller's own duration, not a span of this emulator's choosing.
        let left = deadline_left(&engine, &id, Deadline::Idle)
            .unwrap_or_else(|| panic!("case {case:?}: a MicroVM with a policy owes a suspend"));
        assert!(
            left <= Duration::from_secs(max_idle)
                && left + Duration::from_secs(30) > Duration::from_secs(max_idle),
            "case {case:?}: expected about {max_idle}s of idle time left, got {left:?}"
        );

        // Waiting the idle duration out: the suspend is the platform's, so no
        // SuspendMicrovm is called here at all.
        set_deadline(&engine, &id, Deadline::Idle, None);
        if rests {
            poll_until_microvm_state(&router, case, &id, SUSPENDED).await;

            // A resting MicroVM owes the other deadline, one `suspendedDurationSeconds`
            // away.
            let left = deadline_left(&engine, &id, Deadline::Rest).unwrap_or_else(|| {
                panic!("case {case:?}: a resting MicroVM with a policy owes a termination")
            });
            assert!(
                left <= Duration::from_secs(rest_for)
                    && left + Duration::from_secs(30) > Duration::from_secs(rest_for),
                "case {case:?}: expected about {rest_for}s of rest left, got {left:?}"
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
