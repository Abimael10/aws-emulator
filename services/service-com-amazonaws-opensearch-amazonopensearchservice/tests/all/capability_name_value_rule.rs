// SPDX-License-Identifier: CC0-1.0

//! The value rule `RegisterCapability` holds `capabilityName` to, over the WHOLE space of
//! names the published constraints admit.
//!
//! `capabilityName` is modelled only as a 3..30-character `^[a-zA-Z0-9-]+$` string
//! (`smithy_query(query="operation",operation="RegisterCapability")`), but the member's own
//! documentation fixes the value it may carry — "This identifies the type of capability
//! being enabled for the application. For registering AI Assistant capability, use
//! ai-capability" — so satisfying the length and the expression is NOT enough: a name
//! inside both that is not that identifier names a capability there is nothing to
//! register, and the register is the modelled client-input refusal (HTTP 400,
//! `ValidationException`) that persists nothing.
//!
//! [`super::application_capabilities`] asserts that rule over a hand-written table of
//! names; this suite asserts it as an INVARIANT over a generated sweep of the admitted
//! space — every length the member publishes, both its bounds and one past each, and the
//! characters its expression admits — plus the two properties the refusal has beyond its
//! wire answer:
//!
//! * it is decided from the REQUEST ALONE, before any resource is resolved, so an
//!   unregistrable name is that refusal even when the `applicationId` names no
//!   application, while the registrable name against the same id is the missing-resource
//!   refusal;
//! * it leaves the application exactly as it was, so a capability the application already
//!   holds still reads back and the rejected name still resolves to nothing.
//!
//! Every case drives the real wire path through the router, and the harness, the
//! application and the capability helpers come from the suites that already own them — no
//! second copy of either lives here.

use super::application_capabilities::{
    ai_config, get_capability, held_capabilities, refused, register, served, CAPABILITY,
};
use super::data_source_attachments::application;
use super::domain_lifecycle::{
    error_code, string_member, Answer, Emulator, RECORDED_NOT_FOUND_STATUS,
};

/// The characters `capabilityName`'s published expression admits, `^[a-zA-Z0-9-]+$`
/// (`smithy_query(query="operation",operation="RegisterCapability")`) — the alphabet every
/// generated name below is drawn from, so each of them is INSIDE the expression and what
/// refuses it is the value rule rather than the boundary.
const ADMITTED: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-";

/// The published length bounds of `capabilityName`: `@length(min: 3, max: 30)`.
const MIN_LENGTH: usize = 3;
const MAX_LENGTH: usize = 30;

/// A name of exactly `length` characters drawn from [`ADMITTED`], generated so the sweep
/// spans the admitted space rather than one hand-picked shape: the stride is coprime with
/// the alphabet's size, so each name mixes lower case, upper case, digits and hyphens, and
/// `length` shifts the start so no two rows repeat a prefix.
fn admitted_name(length: usize) -> String {
    (0..length)
        .map(|index| {
            let position = (length * 11 + index * 17) % ADMITTED.len();
            char::from(ADMITTED[position])
        })
        .collect()
}

/// The refusal `RegisterCapability` answers a name inside the published constraints that is
/// not the capability this service registers with: the fixed sentence naming the value the
/// request carried and the one name that is allowed.
fn value_refusal_message(capability_name: &str) -> String {
    format!(
        "Invalid capability name for AI Capability: {capability_name}. Only {CAPABILITY} is \
         allowed"
    )
}

/// Assert `answer` is the client-input refusal for `name`, with the message above.
fn refused_by_the_value_rule(answer: &Answer, case: &str, name: &str) {
    let expected = value_refusal_message(name);
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400 for {name:?}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(answer),
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
}

#[tokio::test]
async fn only_the_documented_capability_name_is_registrable() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-name-sweep").await;

    // Every length the member admits, from its published minimum to its published
    // maximum, each name drawn from the characters its expression admits — so every row is
    // inside both constraints and still not the capability this service registers.
    for length in MIN_LENGTH..=MAX_LENGTH {
        let name = admitted_name(length);
        assert_ne!(
            name, CAPABILITY,
            "the generated sweep must not produce the one registrable name"
        );
        let case = format!("an admitted name of {length} characters");
        refused_by_the_value_rule(&register(&emulator, &id, &name, ai_config()).await, &case, &name);
        assert!(
            held_capabilities(&emulator, &id).is_empty(),
            "case {case:?}: a refused register persists nothing, got {:?}",
            held_capabilities(&emulator, &id)
        );
    }

    // Names that differ from the registrable one only in case, in hyphen placement or by a
    // single character — the typos a client is most likely to send — are refused the same
    // way, because what decides the refusal is the VALUE and not how near it comes.
    for near in [
        "AI-CAPABILITY",
        "Ai-Capability",
        "ai-Capability",
        "aicapability",
        "ai--capability",
        "-ai-capability",
        "ai-capability-",
        "ai-capability1",
        "ai-capabilitie",
        "ai-capabilty",
    ] {
        let case = format!("the near miss {near:?}");
        refused_by_the_value_rule(&register(&emulator, &id, near, ai_config()).await, &case, near);
        assert!(
            held_capabilities(&emulator, &id).is_empty(),
            "case {case:?}: a refused register persists nothing, got {:?}",
            held_capabilities(&emulator, &id)
        );
    }

    // One character short of the published minimum and one past the published maximum are
    // outside the constraints themselves, so they are refused as client input too — the
    // boundary states that refusal, so only its status and code are asserted here.
    for (case, name) in [
        ("one under the published minimum", admitted_name(MIN_LENGTH - 1)),
        ("one past the published maximum", admitted_name(MAX_LENGTH + 1)),
    ] {
        let answer = register(&emulator, &id, &name, ai_config()).await;
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
        assert!(
            held_capabilities(&emulator, &id).is_empty(),
            "case {case:?}: a refused register persists nothing, got {:?}",
            held_capabilities(&emulator, &id)
        );
    }

    // ... and the ONE name the member's documentation names is served, so what refused
    // every row above is the value rule and not the operation refusing all registers.
    let case = "the documented capability name";
    let body = served(&register(&emulator, &id, CAPABILITY, ai_config()).await, case);
    assert_eq!(
        string_member(&body, "capabilityName", case),
        CAPABILITY,
        "case {case:?}: the answer names the capability that was registered, got {body:?}"
    );
    assert_eq!(
        held_capabilities(&emulator, &id),
        vec![CAPABILITY.to_string()],
        "case {case:?}: the application holds the one record the whole sweep made, got {:?}",
        held_capabilities(&emulator, &id)
    );
}

#[tokio::test]
async fn the_value_rule_is_decided_before_the_application_is_resolved() {
    let emulator = Emulator::new();

    let generated = admitted_name(9);
    // Ids this account holds no application under, differing in length and in the
    // characters an `applicationId` admits.
    for absent in ["noapplication0", "abc", "a1b2c3d4e5f6g7h8i9j0"] {
        // An unregistrable name is judged from the request alone, so the answer is the
        // value refusal rather than the missing-resource one: nothing had to be resolved
        // to know there is no such capability to register.
        for name in ["not-ai-capability", "aicapability", generated.as_str()] {
            refused_by_the_value_rule(
                &register(&emulator, absent, name, ai_config()).await,
                &format!("{name:?} against the absent application {absent:?}"),
                name,
            );
        }

        // The registrable name against that same id reaches the lookup, so THAT one is the
        // missing-resource refusal — the two refusals are distinguishable, which is what
        // tells a caller which value was wrong. A register states the missing application in
        // its own wording (`super::application_capabilities`'s own table of that rule).
        refused(
            &register(&emulator, absent, CAPABILITY, ai_config()).await,
            &format!("the registrable name against the absent application {absent:?}"),
            RECORDED_NOT_FOUND_STATUS,
            "ResourceNotFoundException",
            &format!("Application {absent} does not exist"),
        );
    }
}

#[tokio::test]
async fn a_refused_register_leaves_the_application_as_it_was() {
    let emulator = Emulator::new();
    let id = application(&emulator, "capability-refusal-effects").await;
    served(
        &register(&emulator, &id, CAPABILITY, ai_config()).await,
        "the registrable capability",
    );

    let longest = admitted_name(MAX_LENGTH);
    for name in ["not-ai-capability", "AI-capability", longest.as_str()] {
        let case = format!("a refused register of {name:?} beside the held capability");
        refused_by_the_value_rule(&register(&emulator, &id, name, ai_config()).await, &case, name);

        // The rejected name resolves to nothing, named back with the application it was
        // asked for ...
        refused(
            &get_capability(&emulator, &id, name).await,
            &format!("a read of the rejected {name:?}"),
            RECORDED_NOT_FOUND_STATUS,
            "ResourceNotFoundException",
            &format!("Capability {name} not found for application {id}"),
        );

        // ... while the capability the application DOES hold is untouched by the refusal.
        let read = get_capability(&emulator, &id, CAPABILITY).await;
        let reported = served(&read, &case);
        assert_eq!(
            string_member(&reported, "capabilityName", &case),
            CAPABILITY,
            "case {case:?}: the held capability still reads back, got {reported:?}"
        );
        assert_eq!(
            held_capabilities(&emulator, &id),
            vec![CAPABILITY.to_string()],
            "case {case:?}: the refusal added and removed no record, got {:?}",
            held_capabilities(&emulator, &id)
        );
    }
}
