// SPDX-License-Identifier: CC0-1.0

//! Billing's three Enterprise Support reads refuse a caller that is not the
//! Support profile's payer account WITHOUT holding Billing's own store while
//! they resolve, out of the Security Token Service's store, the principal that
//! refusal names.
//!
//! The refusal message names the caller's identity, and that identity is read
//! from STS's store, so composing it takes STS's lock. The decision itself
//! needs one fact out of Billing's store — which account pays for the
//! consolidated billing family — so that store is read under a guard released
//! again before the refusal is composed: neither service's store is ever
//! locked inside the other's critical section, so two callers taking them from
//! opposite ends cannot wedge each other. Only a cross-service suite can pin
//! that, because the two stores belong to two crates.
//!
//! The rows span the class the rule generalises over: every read of the profile
//! (the two gets and the paginated list), and the requests each one takes — a
//! closed billing month, a much earlier one, the current month and a future one
//! (months the read would refuse as not past, were the caller allowed to ask at
//! all), plus the list's account filter and page size.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_state;
use smithy_server_runtime::MultiServiceRouter;

// The Billing wire call and the identity STS reports for the credential it
// signs with already have helpers in this test crate, so this suite imports
// them rather than keeping a second copy.
use crate::billing_access_denied_principal_agrees_with_sts::{billing, caller_identity_arn};

/// An account that is not the caller: the payer of the consolidated billing
/// family in the worlds where the caller may not read the Support profile.
const OTHER_PAYER_ACCOUNT_ID: &str = "111122223333";

/// A second 12-digit account, for the list read's `accountId` filter.
const MEMBER_ACCOUNT_ID: &str = "123456789012";

/// The ARN of the `PRIMARY` billing view every account owns by default — what
/// the probe below reads Billing's store with.
const PRIMARY_ARN: &str = "arn:aws:billing::000000000000:billingview/primary";

/// The Billing read the probe drives: it answers from Billing's own store and
/// reaches no other service's, so a probe left unanswered is a probe that
/// cannot take Billing's lock.
const PROBE_OPERATION: &str = "GetBillingView";

/// How many probes must be served while a refused read waits for STS's store.
///
/// One alone could be served before that read has even reached Billing's
/// store, so a run of them is what pins the rule; each probe is an in-process
/// call, so the run costs milliseconds even on a loaded host.
const PROBES: usize = 64;

/// How long any wait here may take before the test calls it stuck.
///
/// Every step it bounds is a handful of in-process calls, so this is orders of
/// magnitude more room than the busiest host needs: it is here only to turn a
/// lock cycle into a named failure instead of a hung test.
const DEADLINE: Duration = Duration::from_secs(120);

/// Billing and STS on ONE engine, as `core/router/src/lib.rs` mounts them, with
/// the consolidated billing family paid for by `payer_account_id`.
fn emulator(payer_account_id: &str) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_billing_awsbilling::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    {
        let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
        let mut state = state.lock();
        state.billing_family_management_account_id = payer_account_id.to_string();
    }
    (router, engine)
}

/// The `__type` an awsJson error body carries, without its namespace prefix.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The sentence a refusal stated.
fn message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A current-thread runtime, for a thread that drives the emulator in-process.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime builds")
}

/// The `YYYY-MM` billing month `offset` whole months from the current one, on
/// the UTC civil calendar — so a row can name a month the reads answer for and
/// one they do not, whenever this suite runs.
fn month_from_now(offset: i32) -> String {
    let rendered = ::aws_smithy_types::DateTime::from(SystemTime::now())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("the clock renders");
    let year: i32 = rendered[0..4].parse().expect("a four-digit year");
    let month: i32 = rendered[5..7].parse().expect("a two-digit month");
    let index = year * 12 + (month - 1) + offset;
    format!("{:04}-{:02}", index.div_euclid(12), index.rem_euclid(12) + 1)
}

/// Every read of the Enterprise Support profile, with the requests to drive it.
///
/// Source: the three operations of this service whose output is the profile's
/// own data, `smithy_query(query="digest",
/// model_path="services/service-com-amazonaws-billing-awsbilling/smithy-model/com-amazonaws-billing-awsbilling.json")`.
/// The rule is about WHO is asking, so the requests span the months a read
/// answers for and the months it does not, and the list read's own filter and
/// page size besides.
fn enterprise_support_reads() -> Vec<(&'static str, Vec<(String, Value)>)> {
    let months = [
        ("a closed billing month", "2024-03".to_string()),
        ("a much earlier closed month", "2019-12".to_string()),
        ("the current month", month_from_now(0)),
        ("a future month", month_from_now(1)),
    ];
    [
        "GetEnterpriseSupportContractDetails",
        "GetEnterpriseSupportChargeSummary",
        "ListEnterpriseSupportLinkedAccountCharges",
    ]
    .into_iter()
    .map(|operation| {
        let mut requests: Vec<(String, Value)> = months
            .iter()
            .map(|(what, month)| {
                (
                    format!("{operation} for {what}"),
                    json!({ "billingMonth": month }),
                )
            })
            .collect();
        if operation == "ListEnterpriseSupportLinkedAccountCharges" {
            requests.push((
                format!("{operation} filtered to one linked account"),
                json!({ "billingMonth": "2024-03", "accountId": MEMBER_ACCOUNT_ID }),
            ));
            requests.push((
                format!("{operation} with the smallest page"),
                json!({ "billingMonth": "2024-03", "maxResults": 1 }),
            ));
        }
        (operation, requests)
    })
    .collect()
}

#[tokio::test]
async fn every_enterprise_support_read_refuses_a_caller_that_is_not_the_payer_account() {
    let (router, _engine) = emulator(OTHER_PAYER_ACCOUNT_ID);
    // The identity STS reports for the credential these calls are signed with:
    // the principal the refusal must name, which is why composing it reaches
    // STS's store at all.
    let caller = caller_identity_arn(&router).await;

    for (operation, requests) in enterprise_support_reads() {
        for (case, body) in requests {
            let (status, got) = billing(&router, operation, &body).await;
            assert_eq!(
                (status, error_code(&got).as_str()),
                (403, "AccessDeniedException"),
                "case {case:?}: expected 403 AccessDeniedException, got {status} {got:?}"
            );
            let stated = message(&got);
            assert!(
                stated.contains(&format!("User: {caller} is not authorized to perform:")),
                "case {case:?}: expected the refusal to name the identity STS reports \
                 ({caller:?}), got {stated:?}"
            );
            assert!(
                stated.contains(&format!("billing:{operation}")),
                "case {case:?}: expected the refusal to name the denied action, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_payer_account_itself_is_never_refused_on_this_boundary() {
    // The control for the rule above: the boundary is about the payer account,
    // so the account that DOES pay for the family is answered — or refused for
    // the month it asked about, never for who it is.
    let (router, _engine) = emulator(::engine::emulator_account_id());
    for (operation, requests) in enterprise_support_reads() {
        for (case, body) in requests {
            let (status, got) = billing(&router, operation, &body).await;
            assert_ne!(
                (status, error_code(&got).as_str()),
                (403, "AccessDeniedException"),
                "case {case:?}: the payer account is not refused the profile's own data, \
                 got {status} {got:?}"
            );
        }
    }
}

/// Drive one refused read while this test holds the store its refusal must
/// read the caller's identity from, and assert Billing's own store stays
/// reachable for every other caller meanwhile.
fn assert_refusal_leaves_the_billing_store_reachable(case: &str, operation: &str, body: &Value) {
    let (router, engine) = emulator(OTHER_PAYER_ACCOUNT_ID);
    let sts = sts_state::service_state(&*engine)
        .expect("the STS store is reachable from the shared engine");

    // The other service's store, held for as long as this guard lives: the
    // refused read below cannot compose its message until it is released.
    let sts_guard = sts.lock();

    let (read_started, read_starting) = mpsc::channel();
    let (read_answered, read_answering) = mpsc::channel();
    let read_router = router.clone();
    let read_operation = operation.to_string();
    let read_body = body.clone();
    let reader = thread::spawn(move || {
        let runtime = runtime();
        read_started.send(()).expect("the test awaits the start");
        let answer = runtime.block_on(billing(&read_router, &read_operation, &read_body));
        read_answered.send(answer).expect("the test awaits the answer");
    });
    read_starting
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("case {case:?}: the refused read never started"));

    // THE RULE: the read decides the boundary under a guard of Billing's store
    // that it releases before it reaches for the identity in STS's store, so
    // while it waits there every other caller of Billing is still served.
    let (probes_done, probing) = mpsc::channel();
    let probe_router = router.clone();
    let prober = thread::spawn(move || {
        let runtime = runtime();
        for probe in 0..PROBES {
            let (status, got) = runtime.block_on(billing(
                &probe_router,
                PROBE_OPERATION,
                &json!({ "arn": PRIMARY_ARN }),
            ));
            assert_eq!(
                status, 200,
                "probe {probe}: expected Billing's store to answer {PROBE_OPERATION}, \
                 got {status} {got:?}"
            );
        }
        probes_done.send(()).expect("the test awaits the probes");
    });
    match probing.recv_timeout(DEADLINE) {
        Ok(()) => {}
        Err(RecvTimeoutError::Timeout) => panic!(
            "case {case:?}: expected Billing's store to stay reachable while the refused read \
             resolved the caller's identity from the other service's store, but \
             {PROBE_OPERATION} was still waiting for it after {DEADLINE:?}"
        ),
        Err(RecvTimeoutError::Disconnected) => {
            panic!("case {case:?}: a probe failed; its own assertion says which")
        }
    }

    // Releasing the other store lets the refusal be composed: the read answers
    // the 403 it was always going to.
    drop(sts_guard);
    let (status, got) = read_answering
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|_| panic!("case {case:?}: the refused read never answered"));
    assert_eq!(
        (status, error_code(&got).as_str()),
        (403, "AccessDeniedException"),
        "case {case:?}: expected 403 AccessDeniedException, got {status} {got:?}"
    );
    let stated = message(&got);
    assert!(
        stated.contains(&format!("billing:{operation}")),
        "case {case:?}: expected the refusal to name the denied action, got {stated:?}"
    );
    assert!(
        stated.contains(&sts_state::ambient_identity().arn),
        "case {case:?}: expected the refusal to name the principal held in the other \
         service's store, got {stated:?}"
    );

    reader.join().expect("the reader thread finished");
    prober.join().expect("the prober thread finished");
}

#[test]
fn no_enterprise_support_read_holds_the_billing_store_while_it_reads_the_sts_store() {
    for (operation, requests) in enterprise_support_reads() {
        // One request per read is enough for the lock order — which pair of
        // stores a read takes does not depend on the month it was asked about,
        // and every request of every read is refused the same way in the table
        // above.
        let (case, body) = requests
            .into_iter()
            .next()
            .expect("every read has at least one request");
        assert_refusal_leaves_the_billing_store_reachable(&case, operation, &body);
    }
}
