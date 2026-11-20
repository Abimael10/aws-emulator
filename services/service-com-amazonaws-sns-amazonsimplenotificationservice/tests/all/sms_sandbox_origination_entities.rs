// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what an SMS sandbox registration is judged on, and what it is
//! NOT judged on: this account's dedicated origination-entity inventory.
//!
//! The RULE asserted here: a sandbox destination number "sends a one-time password (OTP)
//! to that phone number" (`CreateSMSSandboxPhoneNumber` doc) from AWS's own side, so the
//! registration is judged on its own input and on this account's sandbox — never on the
//! account's DEDICATED origination entities, which `ListOriginationNumbers` reports and
//! which an account acquires out of band. A registration is therefore served, and the
//! destination number reported pending with a one-time password delivered to it, whether
//! the account holds no entity at all, entities that cannot send SMS, or SMS-capable
//! ones — and the inventory the listing reports is unchanged by it.
//!
//! The rows differ along what the rule generalises over: the destination-number FORM
//! (both branches of the modelled pattern and both published length bounds, from the
//! sandbox suite's own catalog), the CAPABILITIES of the entities the account holds
//! (none at all, an entity that can only place a voice call or carry MMS, one that can
//! send SMS, and several together), and whether the sandbox already holds the number.
//!
//! The registration is also read back through `ListSMSSandboxPhoneNumbers` and carried
//! through the whole verification state machine on an account holding NO entity, because
//! the registration is what every operation downstream of it acts on: a registration the
//! listing does not report leaves the sandbox a dead end, with nothing to verify and
//! nothing to delete.
//!
//! Every call goes over the wire through the router the emulator serves in production,
//! so nothing but persisted state carries information between calls.

use std::time::{Duration, SystemTime};

use service_com_amazonaws_sns_amazonsimplenotificationservice::ops::types::NUMBER_CAPABILITY_VALUES;
use service_com_amazonaws_sns_amazonsimplenotificationservice::state::{
    sms_origination, OriginationNumberRecord, INVALID_OTP_MESSAGE, RESOURCE_DOES_NOT_EXIST,
    SANDBOX_NUMBER_PENDING, SANDBOX_NUMBER_VERIFIED,
};

use super::sms_sandbox_phone_numbers::{accepted_number_forms, reported_numbers};
use super::sms_sandbox_verification::{delivered_code, listed_status, verify};
use super::topics_subscriptions_publish::{Answer, Sns};

/// Put one origination number with the capabilities given into the account's inventory,
/// the way the out-of-band provisioning writes it.
fn seed_origination(sns: &Sns, number: &str, capabilities: &[&str]) {
    let state = sns.state();
    let mut state = state.lock();
    state.origination_numbers.insert(
        number.to_string(),
        OriginationNumberRecord {
            phone_number: number.to_string(),
            status: "Active".to_string(),
            iso2_country_code: "US".to_string(),
            route_type: "Transactional".to_string(),
            number_capabilities: capabilities.iter().map(|value| (*value).to_string()).collect(),
            created_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        },
    );
}

/// One registration call.
async fn register(sns: &Sns, number: &str) -> Answer {
    sns.call("CreateSMSSandboxPhoneNumber", &[("PhoneNumber", number)])
        .await
}

/// Assert the registration was served and left the number pending with a password of
/// its own.
fn assert_registered(case: &str, sns: &Sns, number: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the registration to be served, got {} / {}",
        answer.status, answer.body
    );
    let state = sns.state();
    let state = state.lock();
    let record = state
        .sandbox_phone_numbers
        .get(number)
        .unwrap_or_else(|| panic!("case {case:?}: the served registration must store a record"));
    assert_eq!(
        record.status, SANDBOX_NUMBER_PENDING,
        "case {case:?}: expected a pending destination number, got {record:?}"
    );
    assert!(
        !record.one_time_password.is_empty(),
        "case {case:?}: the served registration must deliver a one-time password"
    );
}

/// An account holding no origination entity registers a sandbox destination number all
/// the same, whatever form that number takes — which is what makes the verification
/// flow reachable at all.
#[tokio::test]
async fn a_registration_is_served_while_the_account_holds_no_origination_entity() {
    let sns = Sns::new();

    for (case, number) in accepted_number_forms() {
        let answer = register(&sns, &number).await;
        assert_registered(case, &sns, &number, &answer);
    }

    // Nothing the registrations did put an entity in the inventory the listing reports.
    assert!(
        sns.state().lock().origination_numbers.is_empty(),
        "the registrations must not write an origination entity, got {:?}",
        sns.state().lock().origination_numbers
    );
}

/// What the inventory holds does not decide the registration: every capability set — and
/// none at all — leaves the destination number pending just the same.
#[tokio::test]
async fn the_origination_inventory_does_not_decide_the_registration() {
    // (case, the capabilities the account's entities hold). The capability names are the
    // values the modelled `NumberCapability` enum accepts
    // (`crate::ops::types::NUMBER_CAPABILITY_VALUES`).
    let rows: &[(&str, &[&[&str]])] = &[
        ("no entity at all", &[]),
        ("one voice-only entity", &[&["VOICE"]]),
        ("one MMS-only entity", &[&["MMS"]]),
        ("two entities, neither able to send SMS", &[&["VOICE"], &["MMS"]]),
        ("one SMS-capable entity", &[&[sms_origination::SMS_CAPABILITY]]),
        ("one entity able to send SMS and MMS", &[&["MMS", "SMS"]]),
        ("two entities, one of them able to send SMS", &[&["VOICE"], &["SMS"]]),
        ("an entity with every modelled capability", &[NUMBER_CAPABILITY_VALUES]),
    ];

    let destination = "+447700900123";
    for (case, inventory) in rows {
        let sns = Sns::new();
        for (at, capabilities) in inventory.iter().enumerate() {
            seed_origination(&sns, &format!("+1206555{at:04}"), capabilities);
        }

        let answer = register(&sns, destination).await;
        assert_registered(case, &sns, destination, &answer);
    }
}

/// The registration is judged the same on EVERY call, the repeat of one the sandbox
/// already holds included: an inventory emptied out of band stops nothing, and the
/// repeat supersedes the one-time password of the record it keeps.
#[tokio::test]
async fn a_repeat_registration_is_served_once_the_inventory_holds_no_sms_entity() {
    let sns = Sns::new();
    let origination = "+12065550199";
    let destination = "+12065550100";
    seed_origination(&sns, origination, &[sms_origination::SMS_CAPABILITY]);

    let served = register(&sns, destination).await;
    assert_eq!(
        served.status, 200,
        "the first registration must be served, got {} / {}",
        served.status, served.body
    );
    let delivered = sns.state().lock().sandbox_phone_numbers[destination]
        .one_time_password
        .clone();

    sns.state().lock().origination_numbers.remove(origination);

    for (case, number) in [
        ("a number the sandbox already holds", destination),
        ("a number the sandbox does not hold", "+447700900123"),
    ] {
        let answer = register(&sns, number).await;
        assert_registered(case, &sns, number, &answer);
    }

    let state = sns.state();
    let state = state.lock();
    assert_eq!(
        state.sandbox_phone_numbers.len(),
        2,
        "expected one record per registered number, got {:?}",
        state.sandbox_phone_numbers
    );
    assert_ne!(
        state.sandbox_phone_numbers[destination].one_time_password, delivered,
        "a repeat registration must deliver a fresh one-time password"
    );
}

/// The inventory states the flow below is driven against, each labelled: the capability
/// names are the values the modelled `NumberCapability` enum accepts
/// (`crate::ops::types::NUMBER_CAPABILITY_VALUES`), and the first row is the account a
/// freshly booted emulator serves — one holding no entity at all.
const INVENTORY_ROWS: &[(&str, &[&[&str]])] = &[
    ("no entity at all", &[]),
    ("one voice-only entity", &[&["VOICE"]]),
    (
        "one SMS-capable entity",
        &[&[sms_origination::SMS_CAPABILITY]],
    ),
    (
        "an entity with every modelled capability beside a voice-only one",
        &[NUMBER_CAPABILITY_VALUES, &["VOICE"]],
    ),
];

/// Put `inventory` into the account, one entity per entry.
fn seed_inventory(sns: &Sns, inventory: &[&[&str]]) {
    for (at, capabilities) in inventory.iter().enumerate() {
        seed_origination(sns, &format!("+1206555{at:04}"), capabilities);
    }
}

/// Assert the registration's two effects were judged APART: the call is either served,
/// or refused only as the one-time-password SEND failing — which AWS reports under the
/// operation's modelled user error (`UserErrorException`, awsQuery `UserError`, HTTP
/// 400) — and never as the registration not having happened.
fn assert_send_judged_apart(case: &str, answer: &Answer) {
    let served = answer.status == 200;
    let send_refused = answer.status == 400 && answer.code() == "UserError";
    assert!(
        served || send_refused,
        "case {case:?}: expected the registration to be served, or refused only as the \
         one-time password send failing, got {} / {}",
        answer.status,
        answer.body
    );
}

/// Whatever the account's origination inventory holds, the destination number a
/// registration names reads back from `ListSMSSandboxPhoneNumbers` as a PENDING entry —
/// the listing being where a client, which cannot see the store, learns the registration
/// happened at all.
#[tokio::test]
async fn a_registration_reaches_the_sandbox_listing_as_pending_whatever_the_inventory_holds() {
    for (inventory_case, inventory) in INVENTORY_ROWS {
        let sns = Sns::new();
        seed_inventory(&sns, inventory);

        for (form, number) in accepted_number_forms() {
            let case = format!("{inventory_case} / {form}");
            let answer = register(&sns, &number).await;
            assert_send_judged_apart(&case, &answer);
            assert_eq!(
                listed_status(&sns, &number).await.as_deref(),
                Some(SANDBOX_NUMBER_PENDING),
                "case {case:?}: expected the listing to report the registered number as \
                 {SANDBOX_NUMBER_PENDING:?}"
            );
        }

        // The listing reports one entry per registered number and nothing else: the
        // account's dedicated entities are a separate inventory.
        let listing = sns.call("ListSMSSandboxPhoneNumbers", &[]).await;
        assert_eq!(
            reported_numbers(&listing).len(),
            accepted_number_forms().len(),
            "case {inventory_case:?}: expected one entry per registered number, got {}",
            listing.body
        );
    }
}

/// `number` with one digit of its code changed, each labelled: a code that is NOT the
/// delivered one, derived from it so it cannot collide with it.
fn codes_other_than(delivered: &str) -> Vec<(&'static str, String)> {
    let bump = |at: usize| -> String {
        delivered
            .chars()
            .enumerate()
            .map(|(position, digit)| {
                if position == at {
                    let next = (digit.to_digit(10).unwrap_or(0) + 1) % 10;
                    char::from_digit(next, 10).unwrap_or('0')
                } else {
                    digit
                }
            })
            .collect()
    };
    vec![
        ("the delivered code's first digit bumped", bump(0)),
        (
            "the delivered code's last digit bumped",
            bump(delivered.len().saturating_sub(1)),
        ),
    ]
}

/// The whole verification state machine runs from a registration made by an account
/// holding NO origination entity: the number is reported pending, a code that was not
/// delivered to it is refused as a failed verification and leaves it pending, a number
/// the sandbox never held is refused as a lookup miss instead, the delivered code moves
/// it to verified, and the delete takes it back out of the listing.
#[tokio::test]
async fn the_verification_flow_runs_from_a_registration_made_with_no_origination_entity() {
    let sns = Sns::new();

    // A number the sandbox never held is refused in a DIFFERENT class from a code that
    // does not verify, so a client can tell "not registered" from "wrong code".
    let absent = verify(&sns, "+12065559999", "123456").await;
    assert_eq!(
        absent.status, 404,
        "a number never registered: expected 404, got {} / {}",
        absent.status, absent.body
    );
    assert_eq!(
        absent.code(),
        "ResourceNotFound",
        "a number never registered: expected the ResourceNotFound code, got {}",
        absent.body
    );
    assert_eq!(
        absent.element("Message").as_deref(),
        Some(RESOURCE_DOES_NOT_EXIST),
        "a number never registered: expected the fixed missing-resource sentence, got {}",
        absent.body
    );

    for (case, number) in accepted_number_forms() {
        let answer = register(&sns, &number).await;
        assert_send_judged_apart(case, &answer);
        assert_eq!(
            listed_status(&sns, &number).await.as_deref(),
            Some(SANDBOX_NUMBER_PENDING),
            "case {case:?}: expected the registration to be reported \
             {SANDBOX_NUMBER_PENDING:?}"
        );

        let delivered = delivered_code(&sns, &number);
        for (row, wrong) in codes_other_than(&delivered) {
            let refused = verify(&sns, &number, &wrong).await;
            assert_eq!(
                refused.status, 400,
                "case {case:?} / {row:?}: expected 400, got {} / {}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.code(),
                "VerificationException",
                "case {case:?} / {row:?}: expected the VerificationException code, got {}",
                refused.body
            );
            assert_eq!(
                refused.element("Message").as_deref(),
                Some(INVALID_OTP_MESSAGE),
                "case {case:?} / {row:?}: expected the fixed invalid-code sentence, got {}",
                refused.body
            );
            assert_eq!(
                listed_status(&sns, &number).await.as_deref(),
                Some(SANDBOX_NUMBER_PENDING),
                "case {case:?} / {row:?}: a refused verification must leave the status alone"
            );
        }

        let verified = verify(&sns, &number, &delivered).await;
        assert_eq!(
            verified.status, 200,
            "case {case:?}: expected the delivered code to verify the number, got {} / {}",
            verified.status, verified.body
        );
        assert_eq!(
            listed_status(&sns, &number).await.as_deref(),
            Some(SANDBOX_NUMBER_VERIFIED),
            "case {case:?}: expected the listing to report {SANDBOX_NUMBER_VERIFIED:?}"
        );

        let deleted = sns
            .call("DeleteSMSSandboxPhoneNumber", &[("PhoneNumber", &number)])
            .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected the delete to be served, got {} / {}",
            deleted.status, deleted.body
        );
        assert_eq!(
            listed_status(&sns, &number).await,
            None,
            "case {case:?}: expected the deleted number to leave the listing"
        );
    }

    assert!(
        sns.state().lock().origination_numbers.is_empty(),
        "the whole flow must not write an origination entity, got {:?}",
        sns.state().lock().origination_numbers
    );
}
