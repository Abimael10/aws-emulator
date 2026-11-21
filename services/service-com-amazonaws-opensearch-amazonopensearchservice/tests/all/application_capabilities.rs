// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the capabilities of an OpenSearch UI application:
//! `RegisterCapability`, `GetCapability` and `DeregisterCapability`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the application a capability is registered for from
//! [`super::data_source_attachments`]; this suite adds no second copy of either.
//!
//! The tests assert RULES over sets of inputs, not one recorded call:
//!
//! * a register persists the capability on the application it names and answers with the
//!   application, the name, the status and the configuration it carried, and the SAME
//!   record reads back through `GetCapability`;
//! * `capabilityName` names WHICH capability is registered and the service registers one,
//!   so a name outside the published length bounds or expression is refused at the
//!   boundary and a name inside them that is not that identifier is refused by the
//!   service — over a table of names differing in length, case, digits and hyphen
//!   placement — and neither persists anything, after which a read of the rejected name
//!   reports that the application holds no such capability;
//! * a capability is registered in a transient status and the clock moves it to active
//!   without any call asking for it;
//! * the name is unique per application, so registering it again registers no second
//!   capability and answers with the one record the application holds;
//! * a capability is scoped to the application it was registered for, so one
//!   application's capability is neither reported nor removed through another's;
//! * a deregistration reports the capability being removed, is idempotent, and the clock
//!   takes the record away — after which a read is the modelled not-found refusal and a
//!   register mints the capability afresh;
//! * a removal starts only from a capability whose lifecycle has SETTLED: while a status
//!   move the clock still owes the record is outstanding the deregistration is the modelled
//!   conflict refusal and the stored record is left where it stood, over a table of states
//!   differing in the status the record stands in, in whether a move is owed and in whether
//!   a removal is already running;
//! * every one of the three operations refuses an id naming no application, and the two
//!   that address a registration refuse a name the application holds none under — each
//!   stating the missing application in ITS own wording, over a table of ids differing in
//!   length and in the characters the service admits;
//! * the set one application holds is bounded: one register per name and one name, so
//!   however many registers arrive the application holds a single record.

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    OwedStatus, CAPABILITY_STATUS_ACTIVE, CAPABILITY_STATUS_CREATE_FAILED,
    CAPABILITY_STATUS_CREATING, CAPABILITY_STATUS_DELETING, CAPABILITY_STATUS_UPDATING,
};
use std::time::{Duration, SystemTime};

use super::data_source_attachments::application;
use super::domain_lifecycle::{
    error_code, poll_until, string_member, Answer, Emulator, PREFIX, RECORDED_NOT_FOUND_STATUS,
};

/// The ONE capability name this service registers: "for registering AI Assistant
/// capability, use ai-capability" (`RegisterCapabilityRequest$capabilityName`'s own
/// documentation). Every other value inside `capabilityName`'s published constraints is
/// refused by [`a_name_that_is_not_the_registrable_capability_is_refused`].
pub(crate) const CAPABILITY: &str = "ai-capability";

/// The only configuration branch `CapabilityBaseRequestConfig` declares — the AI settings
/// of an application's capability (`smithy_query(query="shape",shape="CapabilityBaseRequestConfig")`),
/// whose target `AIConfig` declares no member of its own.
pub(crate) fn ai_config() -> Value {
    json!({ "aiConfig": {} })
}

/// `RegisterCapability`:
/// `POST /2021-01-01/opensearch/application/{applicationId}/capability/register`.
pub(crate) async fn register(
    emulator: &Emulator,
    application_id: &str,
    capability_name: &str,
    config: Value,
) -> Answer {
    emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/application/{application_id}/capability/register"),
            Some(json!({
                "capabilityName": capability_name,
                "capabilityConfig": config,
            })),
        )
        .await
}

/// `GetCapability`:
/// `GET /2021-01-01/opensearch/application/{applicationId}/capability/{capabilityName}`.
pub(crate) async fn get_capability(
    emulator: &Emulator,
    application_id: &str,
    capability_name: &str,
) -> Answer {
    emulator
        .call(
            "GET",
            &format!(
                "{PREFIX}/opensearch/application/{application_id}/capability/{capability_name}"
            ),
            None,
        )
        .await
}

/// `DeregisterCapability`:
/// `DELETE /2021-01-01/opensearch/application/{applicationId}/capability/deregister/{capabilityName}`.
async fn deregister(emulator: &Emulator, application_id: &str, capability_name: &str) -> Answer {
    emulator
        .call(
            "DELETE",
            &format!(
                "{PREFIX}/opensearch/application/{application_id}/capability/deregister/\
                 {capability_name}"
            ),
            None,
        )
        .await
}

/// The capability names this application holds a record for, read out of the store the
/// handlers write — the evidence a register persisted a capability rather than only
/// answering about one, and that a repeat added no second record.
pub(crate) fn held_capabilities(emulator: &Emulator, application_id: &str) -> Vec<String> {
    let store = emulator.store();
    let store = store.lock();
    store
        .applications
        .get(application_id)
        .map(|application| application.capabilities.keys().cloned().collect())
        .unwrap_or_default()
}

/// Assert a success and answer the body, naming the case on failure.
pub(crate) fn served(answer: &Answer, case: &str) -> Value {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body.clone()
}

/// Assert a refusal: the recorded missing-resource status, the error code and the message.
pub(crate) fn refused(answer: &Answer, case: &str, status: u16, code: &str, message: &str) {
    assert_eq!(
        answer.status, status,
        "case {case:?}: expected HTTP {status}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(answer),
        code,
        "case {case:?}: expected {code}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.body["message"].as_str().unwrap_or_default(),
        message,
        "case {case:?}: expected the refusal to state {message:?}, got {:?}",
        answer.body
    );
}

/// The names a register is held to, from `capabilityName`'s published constraints:
/// `@length(min: 3, max: 30)` and `@pattern("^[a-zA-Z0-9-]+$")`
/// (`smithy_query(query="operation",operation="RegisterCapability")`). Each row is inside
/// both and is still not the capability this service registers ([`CAPABILITY`]), and they
/// differ in length, case, digits and hyphen placement — so what refuses them is the value
/// rule rather than a property of one name.
fn unregistrable_names() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a name of its own", "not-ai-capability"),
        ("the published minimum length", "abc"),
        (
            "the published maximum length",
            "ai-capability-at-the-upper-max",
        ),
        ("the registrable name in another case", "AI-Capability"),
        ("the registrable name without its hyphen", "aicapability"),
        ("digits and hyphens throughout", "a-b-c-1-2-3"),
    ]
}

#[tokio::test]
async fn a_registered_capability_reads_back_through_its_own_get() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-round-trip").await;
    let case = "the registrable capability";

    let answer = register(&emulator, &id, CAPABILITY, ai_config()).await;
    let body = served(&answer, case);
    assert_eq!(
        string_member(&body, "applicationId", case),
        id,
        "case {case:?}: the answer names the application the capability was registered \
         for, got {body:?}"
    );
    assert_eq!(
        string_member(&body, "capabilityName", case),
        CAPABILITY,
        "case {case:?}: the answer names the capability that was registered, got {body:?}"
    );
    assert_eq!(
        string_member(&body, "status", case),
        CAPABILITY_STATUS_CREATING,
        "case {case:?}: a capability is being enabled the instant the register returns, \
         got {body:?}"
    );
    assert_eq!(
        body["capabilityConfig"],
        ai_config(),
        "case {case:?}: the answer carries the configuration the register named, got \
         {body:?}"
    );

    // The SAME record, read back through the operation that reports one.
    let read = get_capability(&emulator, &id, CAPABILITY).await;
    let reported = served(&read, case);
    assert_eq!(
        string_member(&reported, "applicationId", case),
        id,
        "case {case:?}: the read names the application, got {reported:?}"
    );
    assert_eq!(
        string_member(&reported, "capabilityName", case),
        CAPABILITY,
        "case {case:?}: the read names the capability, got {reported:?}"
    );
    assert_eq!(
        reported["capabilityConfig"],
        ai_config(),
        "case {case:?}: the read reports the stored configuration, got {reported:?}"
    );
    assert!(
        reported["failures"].is_null(),
        "case {case:?}: a capability nothing failed for carries no failures, got \
         {reported:?}"
    );

    // And it was persisted on the application, under the name it was registered with.
    assert_eq!(
        held_capabilities(&emulator, &id),
        vec![CAPABILITY.to_string()],
        "case {case:?}: the application holds the record the register made"
    );
}

#[tokio::test]
async fn a_name_that_is_not_the_registrable_capability_is_refused() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-value-rule").await;

    for (case, name) in unregistrable_names() {
        let answer = register(&emulator, &id, name, ai_config()).await;
        let expected = format!(
            "Invalid capability name for AI Capability: {name}. Only {CAPABILITY} is allowed"
        );
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400 for {name:?}, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer),
            "ValidationException",
            "case {case:?}: expected ValidationException for {name:?}, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.body["message"].as_str().unwrap_or_default(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answer.body
        );

        // Nothing was registered for it, so a read of that name reports that the
        // application holds no such capability.
        assert!(
            held_capabilities(&emulator, &id).is_empty(),
            "case {case:?}: a refused register persists nothing, got {:?}",
            held_capabilities(&emulator, &id)
        );
        refused(
            &get_capability(&emulator, &id, name).await,
            &format!("a read of the rejected {name:?}"),
            RECORDED_NOT_FOUND_STATUS,
            "ResourceNotFoundException",
            &format!("Capability {name} not found for application {id}"),
        );
    }

    // ... while the capability this service DOES register is served, so what refuses the
    // rows above is the name and nothing else.
    served(
        &register(&emulator, &id, CAPABILITY, ai_config()).await,
        "the registrable capability beside them",
    );
}

#[tokio::test]
async fn a_name_outside_its_published_constraints_is_refused_and_persists_nothing() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-refusals").await;

    // Each row breaks one of `capabilityName`'s published constraints: one character
    // under the minimum, one past the maximum, and three values outside the expression.
    let rows: Vec<(&str, String)> = vec![
        ("one under the published minimum", "ab".to_string()),
        ("one past the published maximum", "a".repeat(31)),
        ("an underscore the expression refuses", "ai_capability".to_string()),
        ("a dot the expression refuses", "ai.capability".to_string()),
        ("a space the expression refuses", "ai capability".to_string()),
    ];
    for (case, name) in rows {
        let answer = register(&emulator, &id, &name, ai_config()).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            error_code(&answer),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            answer.body
        );
        assert!(
            held_capabilities(&emulator, &id).is_empty(),
            "case {case:?}: a refused register persists nothing, got {:?}",
            held_capabilities(&emulator, &id)
        );
    }
}

#[tokio::test]
async fn a_capability_settles_from_creating_to_active_on_the_clock() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-settling").await;
    let name = "ai-capability";

    let answer = register(&emulator, &id, name, ai_config()).await;
    let body = served(&answer, "a fresh register");
    assert_eq!(
        string_member(&body, "status", "a fresh register"),
        CAPABILITY_STATUS_CREATING,
        "case \"a fresh register\": the span is never zero, so the caller observes the \
         transient status, got {body:?}"
    );
    let read = get_capability(&emulator, &id, name).await;
    assert_eq!(
        string_member(&served(&read, "a read while it is being enabled"), "status", "creating"),
        CAPABILITY_STATUS_CREATING,
        "case \"a read while it is being enabled\": the read reports the transient status, \
         got {:?}",
        read.body
    );

    // Nothing asks for the move: the span the clock owed it falls due on its own, and the
    // read that comes after it reports the status enabling left the capability in. The
    // case waits for THAT condition rather than for a stretch of time.
    poll_until("the settled capability", || async {
        let read = get_capability(&emulator, &id, name).await;
        read.body["status"].as_str() == Some(CAPABILITY_STATUS_ACTIVE)
    })
    .await;
    let settled = get_capability(&emulator, &id, name).await;
    assert_eq!(
        string_member(&served(&settled, "the settled capability"), "status", "active"),
        CAPABILITY_STATUS_ACTIVE,
        "case \"the settled capability\": the move the clock owed has landed, got {:?}",
        settled.body
    );

    // A capability registered after that wait is still owed its own span: the move is
    // owed per record rather than once per application, so a second application's
    // registration is being enabled while this one is already active.
    let other = application(&emulator, "capability-settling-other").await;
    let second = register(&emulator, &other, name, ai_config()).await;
    assert_eq!(
        string_member(&served(&second, "a later register"), "status", "a later register"),
        CAPABILITY_STATUS_CREATING,
        "case \"a later register\": a fresh capability is being enabled, got {:?}",
        second.body
    );
}

#[tokio::test]
async fn registering_a_held_capability_again_registers_no_second_capability() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-idempotent").await;
    let case = "a repeat of the registration";

    let first = served(&register(&emulator, &id, CAPABILITY, ai_config()).await, case);
    let again = served(&register(&emulator, &id, CAPABILITY, ai_config()).await, case);
    for (member, expected) in [
        ("applicationId", id.clone()),
        ("capabilityName", CAPABILITY.to_string()),
    ] {
        assert_eq!(
            string_member(&again, member, case),
            expected,
            "case {case:?}: the repeat answers about the capability the first register \
             made, got {again:?}"
        );
    }
    assert_eq!(
        again["capabilityConfig"], first["capabilityConfig"],
        "case {case:?}: the repeat reports the stored configuration, got {again:?}"
    );
    assert_eq!(
        held_capabilities(&emulator, &id),
        vec![CAPABILITY.to_string()],
        "case {case:?}: the name is unique per application, got {:?}",
        held_capabilities(&emulator, &id)
    );

    // The same holds once the capability is active: a repeat answers with the record as
    // it stands rather than putting it back into its transient status.
    poll_until("the capability becoming active", || async {
        let read = get_capability(&emulator, &id, CAPABILITY).await;
        read.body["status"].as_str() == Some(CAPABILITY_STATUS_ACTIVE)
    })
    .await;
    let name = CAPABILITY;
    let repeat = served(
        &register(&emulator, &id, name, ai_config()).await,
        "a repeat of an active capability",
    );
    assert_eq!(
        string_member(&repeat, "status", "a repeat of an active capability"),
        CAPABILITY_STATUS_ACTIVE,
        "case \"a repeat of an active capability\": the answer reports where the capability \
         stands, got {repeat:?}"
    );
    assert_eq!(
        held_capabilities(&emulator, &id),
        vec![CAPABILITY.to_string()],
        "case \"a repeat of an active capability\": no record was added, got {:?}",
        held_capabilities(&emulator, &id)
    );
}

#[tokio::test]
async fn a_capability_is_scoped_to_the_application_it_was_registered_for() {
    let emulator = Emulator::new();
    let one = application(&emulator, "capability-scope-one").await;
    let other = application(&emulator, "capability-scope-two").await;
    let name = "ai-capability";

    served(&register(&emulator, &one, name, ai_config()).await, "the first application");
    for (case, application_id) in [("the first application", &one), ("the second", &other)] {
        let held = held_capabilities(&emulator, application_id);
        let expected: Vec<String> = if case == "the first application" {
            vec![name.to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(
            held, expected,
            "case {case:?}: a capability is held by the application it was registered for"
        );
    }

    // The other application holds no such capability, so a read and a deregistration
    // addressed to it are refused rather than reaching the first application's record.
    let expected = format!("Capability {name} not found for application {other}");
    for (case, answer) in [
        ("a read scoped to another application", get_capability(&emulator, &other, name).await),
        (
            "a deregistration scoped to another application",
            deregister(&emulator, &other, name).await,
        ),
    ] {
        refused(
            &answer,
            case,
            RECORDED_NOT_FOUND_STATUS,
            "ResourceNotFoundException",
            &expected,
        );
    }
    assert_eq!(
        held_capabilities(&emulator, &one),
        vec![name.to_string()],
        "case \"the first application\": a call about another application left its record \
         where it was"
    );
}

#[tokio::test]
async fn a_deregistered_capability_is_removed_on_the_clock() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-lifecycle").await;
    let name = "ai-capability";

    served(&register(&emulator, &id, name, ai_config()).await, "the registration");
    poll_until("the registration becoming active", || async {
        let read = get_capability(&emulator, &id, name).await;
        read.body["status"].as_str() == Some(CAPABILITY_STATUS_ACTIVE)
    })
    .await;

    // The call reports the capability being removed, and so does a read taken before the
    // removal falls due: the record rests in that status rather than vanishing at once.
    let gone = served(&deregister(&emulator, &id, name).await, "the deregistration");
    assert_eq!(
        string_member(&gone, "status", "the deregistration"),
        CAPABILITY_STATUS_DELETING,
        "case \"the deregistration\": the capability is being removed, got {gone:?}"
    );
    let read = get_capability(&emulator, &id, name).await;
    assert_eq!(
        string_member(&served(&read, "a read while it is being removed"), "status", "deleting"),
        CAPABILITY_STATUS_DELETING,
        "case \"a read while it is being removed\": the read reports the same status, got {:?}",
        read.body
    );
    // Idempotent: repeating the call reports that same status rather than refusing or
    // starting a second removal.
    let repeat = served(&deregister(&emulator, &id, name).await, "a repeated deregistration");
    assert_eq!(
        string_member(&repeat, "status", "a repeated deregistration"),
        CAPABILITY_STATUS_DELETING,
        "case \"a repeated deregistration\": the capability is still being removed, got \
         {repeat:?}"
    );

    // The clock takes the record away, after which the application holds no such
    // capability at all: the removal is materialised by whichever later call reads the
    // record, so the read that no longer resolves it is what takes it out of the store.
    poll_until("the removal landing", || async {
        get_capability(&emulator, &id, name).await.status != 200
    })
    .await;
    refused(
        &get_capability(&emulator, &id, name).await,
        "a read of a removed capability",
        RECORDED_NOT_FOUND_STATUS,
        "ResourceNotFoundException",
        &format!("Capability {name} not found for application {id}"),
    );
    assert!(
        held_capabilities(&emulator, &id).is_empty(),
        "case \"the removed capability\": the record is gone, got {:?}",
        held_capabilities(&emulator, &id)
    );

    // A register after the removal is a fresh registration rather than the old record
    // reported back.
    let again = served(&register(&emulator, &id, name, ai_config()).await, "a later register");
    assert_eq!(
        string_member(&again, "status", "a later register"),
        CAPABILITY_STATUS_CREATING,
        "case \"a later register\": the capability is being enabled again, got {again:?}"
    );
    assert_eq!(
        held_capabilities(&emulator, &id),
        vec![name.to_string()],
        "case \"a later register\": the application holds the capability again"
    );
}

#[tokio::test]
async fn every_capability_operation_refuses_an_id_that_names_no_application() {
    let emulator = Emulator::new();
    let held = application(&emulator, "capability-misses").await;
    let name = "ai-capability";
    served(&register(&emulator, &held, name, ai_config()).await, "the held capability");

    // Ids this account holds no application under, differing in length and in the
    // characters the service admits, so the refusal is not a property of one value. Each
    // operation states the miss in ITS own wording: the register says the application does
    // not exist, the two that address a registration say it was not found.
    for absent in ["noapplication0", "abc", "a1b2c3d4e5f6g7h8i9j0"] {
        let not_found = format!("Application {absent} not found");
        for (case, answer, expected) in [
            (
                "register",
                register(&emulator, absent, name, ai_config()).await,
                format!("Application {absent} does not exist"),
            ),
            (
                "get",
                get_capability(&emulator, absent, name).await,
                not_found.clone(),
            ),
            (
                "deregister",
                deregister(&emulator, absent, name).await,
                not_found.clone(),
            ),
        ] {
            refused(
                &answer,
                &format!("{case} on the absent application {absent}"),
                RECORDED_NOT_FOUND_STATUS,
                "ResourceNotFoundException",
                &expected,
            );
        }
    }

    // A name the application holds no registration under is refused by the two operations
    // that address one.
    for unregistered in ["other-capability", "xyz", "ai-capability-never-registered"] {
        let expected = format!("Capability {unregistered} not found for application {held}");
        for (case, answer) in [
            ("get", get_capability(&emulator, &held, unregistered).await),
            ("deregister", deregister(&emulator, &held, unregistered).await),
        ] {
            refused(
                &answer,
                &format!("{case} of the unregistered capability {unregistered}"),
                RECORDED_NOT_FOUND_STATUS,
                "ResourceNotFoundException",
                &expected,
            );
        }
    }
}

#[tokio::test]
async fn the_capability_set_of_one_application_is_bounded() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-quota").await;

    // The name IS the key the application holds the record under, and one name is
    // registrable, so however many registers arrive — with that name, or with names the
    // value rule refuses — the application holds exactly one record and the set cannot be
    // grown by a caller.
    for (case, name) in unregistrable_names() {
        let answer = register(&emulator, &id, name, ai_config()).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: a register of {name:?} adds no record, got {} with {:?}",
            answer.status, answer.body
        );
    }
    for round in 0..3 {
        let answer = register(&emulator, &id, CAPABILITY, ai_config()).await;
        assert_eq!(
            answer.status, 200,
            "case \"register round {round}\": the registrable capability is served, got {} with \
             {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            held_capabilities(&emulator, &id),
            vec![CAPABILITY.to_string()],
            "case \"register round {round}\": the application holds one record, got {:?}",
            held_capabilities(&emulator, &id)
        );
    }
}

// =============================================================================
// Which states a removal can start from
// =============================================================================

/// `ConflictException`'s modelled HTTP status
/// (`smithy_query(query="operation",operation="DeregisterCapability")`).
const CONFLICT_STATUS: u16 = 409;

/// A span far longer than any case of this suite takes, so a move or a removal the store is
/// given at this distance is still outstanding when the call under test arrives: the state
/// the row is about is PINNED rather than raced against the clock.
const WELL_BEYOND_THE_CASE: Duration = Duration::from_secs(3_600);

/// A second, shorter such span, so no row is pinned by one distance only.
const ALSO_BEYOND_THE_CASE: Duration = Duration::from_secs(900);

/// One row of the removal table: the state the stored capability stands in when the
/// deregistration arrives, and whether the service starts a removal from it.
struct Removal {
    case: &'static str,
    /// The status the record stands in.
    status: &'static str,
    /// The status move the clock still owes the record, as the span left until it falls due,
    /// and `None` for a record that is owed nothing.
    owed_in: Option<Duration>,
    /// The removal already under way, as the span left until it lands, and `None` for a
    /// record nothing has asked to remove.
    removal_in: Option<Duration>,
    /// Whether the service refuses to start a removal from this state.
    conflicts: bool,
}

/// The states the rule is asserted over: they differ in the status the record stands in, in
/// whether a status move is still owed and in whether a removal is already running — so what
/// decides the answer is the outstanding work rather than one status name.
fn removal_rows() -> Vec<Removal> {
    vec![
        Removal {
            case: "a registration still being enabled",
            status: CAPABILITY_STATUS_CREATING,
            owed_in: Some(WELL_BEYOND_THE_CASE),
            removal_in: None,
            conflicts: true,
        },
        Removal {
            case: "a capability with a move owed while it is updating",
            status: CAPABILITY_STATUS_UPDATING,
            owed_in: Some(ALSO_BEYOND_THE_CASE),
            removal_in: None,
            conflicts: true,
        },
        Removal {
            case: "an enabled capability",
            status: CAPABILITY_STATUS_ACTIVE,
            owed_in: None,
            removal_in: None,
            conflicts: false,
        },
        Removal {
            case: "a capability whose registration failed",
            status: CAPABILITY_STATUS_CREATE_FAILED,
            owed_in: None,
            removal_in: None,
            conflicts: false,
        },
        Removal {
            case: "a removal already under way",
            status: CAPABILITY_STATUS_DELETING,
            owed_in: None,
            removal_in: Some(WELL_BEYOND_THE_CASE),
            conflicts: false,
        },
    ]
}

/// Put the application's capability into the state a row is about, in the store the handlers
/// read — a state no sequence of calls can hold a record in for the length of a case, since
/// the clock would settle it.
fn hold(emulator: &Emulator, application_id: &str, row: &Removal) {
    let now = SystemTime::now();
    let store = emulator.store();
    let mut guard = store.lock();
    let capability = guard
        .applications
        .get_mut(application_id)
        .unwrap_or_else(|| panic!("case {:?}: the account holds the application", row.case))
        .capabilities
        .get_mut(CAPABILITY)
        .unwrap_or_else(|| panic!("case {:?}: the capability was registered", row.case));
    capability.status = row.status.to_string();
    capability.owed = row.owed_in.map(|left| OwedStatus {
        status: CAPABILITY_STATUS_ACTIVE.to_string(),
        message: None,
        due: now + left,
    });
    capability.removal_due = row.removal_in.map(|left| now + left);
}

/// What the store holds for this application's capability: the status it stands in and the
/// instant its removal is due, if any.
fn stored_capability(
    emulator: &Emulator,
    application_id: &str,
) -> Option<(String, Option<SystemTime>)> {
    let store = emulator.store();
    let guard = store.lock();
    let capability = guard
        .applications
        .get(application_id)?
        .capabilities
        .get(CAPABILITY)?;
    Some((capability.status.clone(), capability.removal_due))
}

#[tokio::test]
async fn a_removal_starts_only_from_a_settled_capability() {
    let emulator = Emulator::new();

    for (index, row) in removal_rows().into_iter().enumerate() {
        // One application per row, so no row is answered about a record another row left.
        let id = application(&emulator, &format!("capability-states-{index}")).await;
        served(
            &register(&emulator, &id, CAPABILITY, ai_config()).await,
            row.case,
        );
        hold(&emulator, &id, &row);
        let before = stored_capability(&emulator, &id);

        let answer = deregister(&emulator, &id, CAPABILITY).await;
        if row.conflicts {
            refused(
                &answer,
                row.case,
                CONFLICT_STATUS,
                "ConflictException",
                &format!("Conflict while deregister AI capability for application {id}"),
            );
            // The refusal is the whole of what the call did: the record stands exactly where
            // it stood, and no removal was started for it.
            assert_eq!(
                stored_capability(&emulator, &id),
                before,
                "case {:?}: a refused deregistration leaves the stored capability untouched",
                row.case
            );
            assert_eq!(
                held_capabilities(&emulator, &id),
                vec![CAPABILITY.to_string()],
                "case {:?}: the application still holds the capability, got {:?}",
                row.case,
                held_capabilities(&emulator, &id)
            );
            continue;
        }

        let body = served(&answer, row.case);
        assert_eq!(
            string_member(&body, "status", row.case),
            CAPABILITY_STATUS_DELETING,
            "case {:?}: the removal started, so the capability is being removed, got {body:?}",
            row.case
        );
        let (status, removal_due) = stored_capability(&emulator, &id).unwrap_or_else(|| {
            panic!("case {:?}: the record rests in its deleting status", row.case)
        });
        assert_eq!(
            status, CAPABILITY_STATUS_DELETING,
            "case {:?}: the stored record carries the status the call reported",
            row.case
        );
        assert!(
            removal_due.is_some(),
            "case {:?}: the clock owes the record's removal",
            row.case
        );
        if let Some((_, already_due)) = before.filter(|(_, due)| due.is_some()) {
            // A removal already under way keeps the instant it was owed at, so repeating the
            // call cannot push that removal back.
            assert_eq!(
                removal_due, already_due,
                "case {:?}: the removal keeps the instant it was owed at",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn the_removal_conflict_lifts_once_the_owed_move_lands() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-conflict-lifts").await;
    let case = "a registration that settles";
    served(
        &register(&emulator, &id, CAPABILITY, ai_config()).await,
        case,
    );

    // Nothing asks for the move: the span the clock owed the registration falls due on its
    // own, and from there the removal starts. Nothing is written to the store for this case,
    // and it waits for THAT condition rather than for a stretch of time.
    poll_until("the capability becoming active", || async {
        let read = get_capability(&emulator, &id, CAPABILITY).await;
        read.body["status"].as_str() == Some(CAPABILITY_STATUS_ACTIVE)
    })
    .await;

    let body = served(&deregister(&emulator, &id, CAPABILITY).await, case);
    assert_eq!(
        string_member(&body, "status", case),
        CAPABILITY_STATUS_DELETING,
        "case {case:?}: a settled capability's removal starts, got {body:?}"
    );
}

// =============================================================================
// How a missing application is stated
// =============================================================================

/// The ids the register's wording is asserted over: each is inside `ApplicationId`'s
/// published expression `^[a-z0-9]{3,30}$`
/// (`smithy_query(query="operation",operation="RegisterCapability")`) — both of its length
/// bounds included — and none of them names an application of this account.
fn absent_application_ids() -> Vec<(&'static str, String)> {
    vec![
        ("the published minimum length", "abc".to_string()),
        ("the published maximum length", "z".repeat(30)),
        ("digits throughout", "0123456789".to_string()),
        ("letters and digits", "noapplication9x".to_string()),
    ]
}

#[tokio::test]
async fn the_register_states_a_missing_application_as_one_that_does_not_exist() {
    let emulator = Emulator::new();
    // An application this account DOES hold, so what refuses the rows below is the id each
    // carries rather than an account holding no application at all.
    let held = application(&emulator, "capability-missing-application").await;
    served(
        &register(&emulator, &held, CAPABILITY, ai_config()).await,
        "the held application",
    );

    for (case, absent) in absent_application_ids() {
        refused(
            &register(&emulator, &absent, CAPABILITY, ai_config()).await,
            &format!("a register on {case}"),
            RECORDED_NOT_FOUND_STATUS,
            "ResourceNotFoundException",
            &format!("Application {absent} does not exist"),
        );
        // The two operations that address a registration state the same miss in their own
        // wording, so the sentence belongs to the operation rather than to the lookup.
        for (operation, answer) in [
            (
                "a read",
                get_capability(&emulator, &absent, CAPABILITY).await,
            ),
            (
                "a deregistration",
                deregister(&emulator, &absent, CAPABILITY).await,
            ),
        ] {
            refused(
                &answer,
                &format!("{operation} on {case}"),
                RECORDED_NOT_FOUND_STATUS,
                "ResourceNotFoundException",
                &format!("Application {absent} not found"),
            );
        }
    }
}
