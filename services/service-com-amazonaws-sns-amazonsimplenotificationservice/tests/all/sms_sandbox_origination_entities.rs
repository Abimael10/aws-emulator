// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the TWO effects of an SMS sandbox registration, and what each of
//! them is judged on.
//!
//! The RULE asserted here: the operation "[a]dds a destination phone number to an Amazon
//! Web Services account in the SMS sandbox and sends a one-time password (OTP) to that
//! phone number" (`CreateSMSSandboxPhoneNumber` doc) — two effects, judged apart. The
//! REGISTRATION is judged on its own input and on this account's sandbox, so the
//! destination number is registered pending whatever else the account holds and reads
//! back from `ListSMSSandboxPhoneNumbers`. The SEND is judged on what the account has to
//! send a text message FROM: an SMS message leaves from an origination entity, so an
//! account whose inventory holds none able to carry SMS has the send fail, and SNS reports
//! that failure under the operation's modelled user error (`UserErrorException`, awsQuery
//! code `UserError`, HTTP 400) with the fixed sentence
//! `state::sms_origination::NO_ORIGINATION_ENTITIES`. The refusal takes nothing back: it
//! is the message that was not carried, not the registration.
//!
//! The rows differ along what the rule generalises over: the CAPABILITIES of the entities
//! the account holds (none at all, an entity that can only place a voice call or carry
//! MMS, one that can send SMS, several together, and every modelled capability at once),
//! the destination-number FORM (both branches of the modelled pattern and both published
//! length bounds, from the sandbox suite's own catalog), whether the sandbox already holds
//! the number, and whether an entity is acquired or given up out of band between two
//! calls.
//!
//! The registration is also carried through the whole verification state machine on an
//! account holding NO entity, because the registration is what every operation downstream
//! of it acts on: a registration the listing does not report leaves the sandbox a dead
//! end, with nothing to verify and nothing to delete.
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

/// Assert the destination number is registered and pending with a password of its own —
/// which is the half of the call the account's inventory does not decide.
fn assert_registered(case: &str, sns: &Sns, number: &str) {
    let state = sns.state();
    let state = state.lock();
    let record = state
        .sandbox_phone_numbers
        .get(number)
        .unwrap_or_else(|| panic!("case {case:?}: the registration must store a record"));
    assert_eq!(
        record.status, SANDBOX_NUMBER_PENDING,
        "case {case:?}: expected a pending destination number, got {record:?}"
    );
    assert!(
        !record.one_time_password.is_empty(),
        "case {case:?}: the registration must mint a one-time password"
    );
}

/// Assert the SEND half of the call was answered as `carried` says: served when the
/// account holds an entity able to carry the one-time password, and otherwise refused as
/// that send failing — 400 with the operation's modelled user error and its fixed
/// sentence, with none of the (`@sensitive`) destination number echoed back.
fn assert_send(case: &str, number: &str, carried: bool, answer: &Answer) {
    if carried {
        assert_eq!(
            answer.status, 200,
            "case {case:?}: an account able to send expects the call to be served, got {} / {}",
            answer.status, answer.body
        );
        return;
    }
    assert_eq!(
        answer.status, 400,
        "case {case:?}: an account with nothing to send from expects 400, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.code(),
        "UserError",
        "case {case:?}: expected the UserError code, got {}",
        answer.body
    );
    assert_eq!(
        answer.element("Message").as_deref(),
        Some(sms_origination::NO_ORIGINATION_ENTITIES),
        "case {case:?}: expected the fixed send-failure sentence, got {}",
        answer.body
    );
    assert!(
        !answer.body.contains(number),
        "case {case:?}: the destination number is a sensitive member, so the refusal must \
         not echo it, got {}",
        answer.body
    );
}

/// The inventory states every table below is driven over, each labelled: the capability
/// names are the values the modelled `NumberCapability` enum accepts
/// (`crate::ops::types::NUMBER_CAPABILITY_VALUES`), and the first row is the account a
/// freshly booted emulator serves — one holding no entity at all.
const INVENTORY_ROWS: &[(&str, &[&[&str]])] = &[
    ("no entity at all", &[]),
    ("one voice-only entity", &[&["VOICE"]]),
    ("one MMS-only entity", &[&["MMS"]]),
    ("two entities, neither able to send SMS", &[&["VOICE"], &["MMS"]]),
    (
        "one SMS-capable entity",
        &[&[sms_origination::SMS_CAPABILITY]],
    ),
    ("one entity able to send SMS and MMS", &[&["MMS", "SMS"]]),
    (
        "two entities, one of them able to send SMS",
        &[&["VOICE"], &[sms_origination::SMS_CAPABILITY]],
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

/// Whether `inventory` holds an entity a text message could leave from.
fn carries_sms(inventory: &[&[&str]]) -> bool {
    inventory
        .iter()
        .any(|capabilities| capabilities.contains(&sms_origination::SMS_CAPABILITY))
}

/// What the inventory decides is the SEND: an account holding an entity able to carry a
/// text message has the registration served, and one holding none is told the one-time
/// password could not be sent — while the destination number is registered pending either
/// way.
#[tokio::test]
async fn the_origination_inventory_decides_the_send_and_not_the_registration() {
    for (inventory_case, inventory) in INVENTORY_ROWS {
        for (form, number) in accepted_number_forms() {
            let case = format!("{inventory_case} / {form}");
            let sns = Sns::new();
            seed_inventory(&sns, inventory);

            let answer = register(&sns, &number).await;
            assert_send(&case, &number, carries_sms(inventory), &answer);
            assert_registered(&case, &sns, &number);

            // Nothing the registration did put an entity in the inventory the listing
            // reports: the two stores are separate.
            assert_eq!(
                sns.state().lock().origination_numbers.len(),
                inventory.len(),
                "case {case:?}: the registration must leave the inventory alone"
            );
        }
    }
}

/// The inventory is read at every call: an entity acquired out of band carries the next
/// registration, one given up stops carrying it, and a number the sandbox already holds is
/// judged the same way — keeping the record it has and superseding its one-time password.
#[tokio::test]
async fn each_registration_is_judged_on_the_inventory_as_it_then_stands() {
    let sns = Sns::new();
    let origination = "+12065550199";
    let destination = "+12065550100";
    let other = "+447700900123";

    // With nothing to send from, the send is refused and the number is registered.
    let refused = register(&sns, destination).await;
    assert_send("no entity yet", destination, false, &refused);
    assert_registered("no entity yet", &sns, destination);
    let first_password = delivered_code(&sns, destination);

    // An entity acquired out of band carries the next send, the repeat of a number the
    // sandbox already holds included.
    seed_origination(&sns, origination, &[sms_origination::SMS_CAPABILITY]);
    let served = register(&sns, destination).await;
    assert_send("an entity acquired", destination, true, &served);
    assert_registered("an entity acquired", &sns, destination);
    assert_ne!(
        delivered_code(&sns, destination),
        first_password,
        "a repeat registration must mint a fresh one-time password"
    );

    // And one given up stops carrying it.
    sns.state().lock().origination_numbers.remove(origination);
    for (case, number) in [
        ("the entity given up, a number the sandbox holds", destination),
        ("the entity given up, a number it does not", other),
    ] {
        let answer = register(&sns, number).await;
        assert_send(case, number, false, &answer);
        assert_registered(case, &sns, number);
    }

    let state = sns.state();
    let state = state.lock();
    assert_eq!(
        state.sandbox_phone_numbers.len(),
        2,
        "expected one record per registered number, got {:?}",
        state.sandbox_phone_numbers
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
            assert_send(&case, &number, carries_sms(inventory), &answer);
            assert_eq!(
                listed_status(&sns, &number).await.as_deref(),
                Some(SANDBOX_NUMBER_PENDING),
                "case {case:?}: expected the listing to report the registered number as \
                 {SANDBOX_NUMBER_PENDING:?}"
            );
        }

        // The listing reports one entry per registered number and nothing else: the
        // account's entities are a separate inventory.
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
/// holding NO origination entity — the account whose send was refused: the number is
/// reported pending, a code that was not delivered to it is refused as a failed
/// verification and leaves it pending, a number the sandbox never held is refused as a
/// lookup miss instead, the code the registration minted moves it to verified, and the
/// delete takes it back out of the listing.
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
        assert_send(case, &number, false, &answer);
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
            "case {case:?}: expected the minted code to verify the number, got {} / {}",
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
