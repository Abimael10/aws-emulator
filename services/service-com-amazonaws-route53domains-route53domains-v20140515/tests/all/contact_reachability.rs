// SPDX-License-Identifier: CC0-1.0

//! `GetContactReachabilityStatus` over awsJson1_1: whether the registrant contact has
//! answered the confirmation email.
//!
//! The rule pinned here is the operation's own documentation: "[f]or operations that
//! require confirmation that the email address for the registrant contact is valid, such
//! as registering a new domain, this operation returns information about whether the
//! registrant contact has responded" — `PENDING` while "[w]e sent the confirmation email
//! and haven't received a response yet", `DONE` once "[w]e sent the email and got
//! confirmation from the registrant contact". The reply is a thing the clock owes: the
//! answer follows the domain's own due instant against the clock, so it is `PENDING`
//! before that instant and `DONE` from it on, whether or not anybody asked in between.
//!
//! The rule is read against the due instant rather than against how fast the host runs:
//! the harness here keeps the engine the service is mounted on, so a case can place a
//! domain's due instant where it needs it and read the answer back over the wire.

use crate::domain_lifecycle::{error_code, person_contact, registration};
use serde_json::{json, Value};
use service_com_amazonaws_route53domains_route53domains_v20140515::state::{
    now_millis, ComAmazonawsRoute53domainsRoute53domainsV20140515State,
};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// How long a test waits for a status the clock owes. Far longer than the span itself:
/// the test waits for the CONDITION, and a loaded host may take a while to get there.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// One emulator, with the engine it is mounted on kept beside it so a case can read and
/// write the store the handlers see — the same single shared engine `router/src/lib.rs`
/// hands the service.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router
            // The suite exercises behaviour, not SigV4.
            .set_request_authentication(false)
            .register_service(
                service_com_amazonaws_route53domains_route53domains_v20140515::EmulatedService::new(
                    Arc::clone(&engine),
                ),
            );
        Self { router, engine }
    }

    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "route53domains.us-east-1.amazonaws.com")
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("Route53Domains_v20140515.{operation}"),
            )
            .body(bytes::Bytes::from(body.to_string()))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// Register `name`, asserting the registration succeeded.
    async fn register(&self, name: &str) {
        let (status, body) = self
            .call("RegisterDomain", &registration(name, 1, &person_contact()))
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected RegisterDomain to succeed, got {status} {body}"
        );
    }

    /// Place `name`'s registrant-reply due instant `offset_millis` from now, and return
    /// the instant that was written.
    fn set_reply_due(&self, name: &str, offset_millis: i64) -> i64 {
        let state = self
            .engine
            .state::<ComAmazonawsRoute53domainsRoute53domainsV20140515State>();
        let mut state = state.lock();
        let domain = state
            .domains
            .get_mut(name)
            .unwrap_or_else(|| panic!("case {name:?}: the registration must be in the store"));
        let due = now_millis().saturating_add(offset_millis);
        domain.reachability_due_millis = due;
        due
    }

    /// `GetContactReachabilityStatus` for `name`, asserting a 200 and returning the
    /// reported domain name and status.
    async fn reachability(&self, name: &str) -> (String, String) {
        let (status, body) = self
            .call(
                "GetContactReachabilityStatus",
                &json!({ "domainName": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected GetContactReachabilityStatus to answer 200, got \
             {status} {body}"
        );
        (
            body["domainName"].as_str().unwrap_or_default().to_string(),
            body["status"].as_str().unwrap_or_default().to_string(),
        )
    }
}

// ---------------------------------------------------------------------------
// The rule: the answer follows the due instant against the clock
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_reported_status_follows_the_due_instant_of_the_reply() {
    // Rows differ along where the due instant sits relative to the clock, by a margin no
    // host's speed can close: an hour away, a minute away, already passed a minute ago,
    // and already passed an hour ago. The modelled members are `PENDING` and `DONE`
    // (`ReachabilityStatus`; its third member `EXPIRED` is never reported, because
    // nothing here lets the reply window lapse).
    let hour: i64 = 60 * 60 * 1000;
    let cases: [(&str, i64, &str); 4] = [
        ("an hour before the reply is due", hour, "PENDING"),
        ("a minute before the reply is due", 60_000, "PENDING"),
        ("a minute after the reply was due", -60_000, "DONE"),
        ("an hour after the reply was due", -hour, "DONE"),
    ];

    let emulator = Emulator::new();
    let name = "sim-probe-reach-clock.com";
    emulator.register(name).await;

    for (case, offset, expected) in cases {
        emulator.set_reply_due(name, offset);
        let (reported, status) = emulator.reachability(name).await;
        assert_eq!(
            (reported.as_str(), status.as_str()),
            (name, expected),
            "case {case:?}: expected {name:?} to report {expected}, got {reported:?} \
             {status:?}"
        );

        // Reading the status is a read: it leaves the due instant where it was, so the
        // next caller sees the same answer.
        let (_, again) = emulator.reachability(name).await;
        assert_eq!(
            again, status,
            "case {case:?}: a second read must report the same status, got {again:?} after \
             {status:?}"
        );
    }
}

#[tokio::test]
async fn each_domain_carries_its_own_reply() {
    // The status is a statement about ONE domain. Rows differ along the TLD and along
    // which side of the clock each domain's due instant sits on, so one domain's answer
    // cannot be the other's.
    let hour: i64 = 60 * 60 * 1000;
    let waiting = "sim-probe-reach-waiting.dev";
    let confirmed = "sim-probe-reach-confirmed.co.uk";

    let emulator = Emulator::new();
    emulator.register(waiting).await;
    emulator.register(confirmed).await;
    emulator.set_reply_due(waiting, hour);
    emulator.set_reply_due(confirmed, -hour);

    for (name, expected) in [(waiting, "PENDING"), (confirmed, "DONE")] {
        let (_, status) = emulator.reachability(name).await;
        assert_eq!(
            status, expected,
            "case {name:?}: expected {expected}, got {status:?}"
        );
    }

    // A later registration of its own does not move an existing domain's reply.
    emulator.register("sim-probe-reach-newcomer.net").await;
    for (name, expected) in [(waiting, "PENDING"), (confirmed, "DONE")] {
        let (_, status) = emulator.reachability(name).await;
        assert_eq!(
            status, expected,
            "case {name:?} after another registration: expected {expected}, got {status:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The rule: a registration owes a reply the clock reaches
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_registration_owes_a_reply_the_clock_reaches() {
    // Registration is one of the operations that "require confirmation that the email
    // address for the registrant contact is valid", so it is what puts a due instant in
    // the store: a caller that keeps asking reads `DONE` once the clock passes it,
    // without anything else being called. The wait is for the CONDITION, with a deadline
    // far longer than the span.
    let emulator = Emulator::new();
    let name = "sim-probe-reach-registered.com";
    emulator.register(name).await;

    let started = std::time::Instant::now();
    let mut last = emulator.reachability(name).await.1;
    while last != "DONE" {
        assert!(
            started.elapsed() < DEADLINE,
            "case {name:?}: the clock owed the registrant's reply, but the status stayed \
             {last:?}"
        );
        assert_eq!(
            last, "PENDING",
            "case {name:?}: expected PENDING until the reply is due, got {last:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        last = emulator.reachability(name).await.1;
    }
}

// ---------------------------------------------------------------------------
// A name this account does not hold has no reply to report
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_name_without_a_registration_is_refused() {
    // Rows differ along why there is nothing to report: a well-formed name the account
    // does not hold, and a request that names no domain at all.
    let emulator = Emulator::new();
    let cases = [
        ("sim-probe-reach-missing.com", "InvalidInput"),
        ("", "InvalidInput"),
    ];

    for (name, expected_code) in cases {
        let (status, body) = emulator
            .call(
                "GetContactReachabilityStatus",
                &json!({ "domainName": name }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body)),
            (400, expected_code.to_string()),
            "case {name:?}: expected 400 {expected_code}, got {status} {body}"
        );
    }
}
