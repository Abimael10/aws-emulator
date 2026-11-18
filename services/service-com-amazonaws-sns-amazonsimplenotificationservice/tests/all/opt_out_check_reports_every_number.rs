// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for which numbers `CheckIfPhoneNumberIsOptedOut` reports on, and
//! what it reports for them.
//!
//! The RULE asserted here: the opt-out check only REPORTS on this account's record of a
//! number, so it asks of its member just that the value be a phone number AT ALL — digits,
//! optionally introduced by a `+` and grouped with the usual separators — and reports on every
//! such value, however it is written: what the operation states is a property of the record,
//! not a verdict on the number's form. A number no opt-out was recorded for is reported as
//! opted IN — `isOptedOut` is the modelled boolean whose default is `false`, documented
//! "false – The phone number is opted in" — a number the record says opted out reports `true`,
//! and the read changes nothing either way. A value that is NO phone number is not a record
//! this account could hold: it is refused as the malformed member it is, HTTP 400 /
//! `InvalidParameter` with the service's fixed badly-formatted-number sentence and none of
//! the (sensitive) value echoed back.
//!
//! The rows differ along what the rule generalises over: the FORM of the value (country codes
//! of one, two and three digits, both published E.164 digit bounds, a short code, a country
//! code beginning with zero, one digit past the E.164 maximum, national forms with
//! separators, and — on the refusal side — text, an empty value, a `+` alone, an extension
//! suffix, a URL and a run longer than a phone number is written) and the
//! opt-out state the record holds. The forms come from the opt-in suite's own catalogs,
//! so the two suites cannot drift on which value is which.
//!
//! Every call goes over the wire through the router the emulator serves in production,
//! so nothing but persisted state carries information between calls.

use std::time::SystemTime;

use service_com_amazonaws_sns_amazonsimplenotificationservice::state::invalid_phone_number_message;

use super::phone_number_opt_in::{
    admitted_but_not_e164_values, e164_numbers, not_written_as_phone_number_values,
    seed_opted_out_at, written_but_not_admitted_values,
};
use super::topics_subscriptions_publish::{Answer, Sns};

/// The member both opt-out operations spell their phone number with on the wire.
const PHONE_NUMBER_PARAM: &str = "phoneNumber";

/// One `CheckIfPhoneNumberIsOptedOut` call.
async fn check(sns: &Sns, number: &str) -> Answer {
    sns.call(
        "CheckIfPhoneNumberIsOptedOut",
        &[(PHONE_NUMBER_PARAM, number)],
    )
    .await
}

/// The `isOptedOut` member of an answered check, asserting the call was served.
fn served(case: &str, answer: &Answer) -> bool {
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the check to answer 200, got {} / {}",
        answer.status, answer.body
    );
    let reported = answer
        .element("isOptedOut")
        .or_else(|| answer.element("IsOptedOut"))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an isOptedOut member, got {}",
                answer.body
            )
        });
    assert!(
        reported == "true" || reported == "false",
        "case {case:?}: expected the modelled boolean, got {reported:?}"
    );
    reported == "true"
}

/// Every number of the documented FORM is answered by the check, and a number with no
/// opt-out record reports `false`.
#[tokio::test]
async fn a_number_with_no_opt_out_record_is_reported_opted_in() {
    let sns = Sns::new();

    for (case, value) in e164_numbers() {
        let answer = check(&sns, &value).await;
        assert!(
            !served(case, &answer),
            "case {case:?}: a number with no opt-out record must report false, got {}",
            answer.body
        );
        // A read is a report: the second check answers the same, and neither wrote a
        // record for the number it was asked about.
        let again = check(&sns, &value).await;
        assert!(
            !served(case, &again),
            "case {case:?}: a second check must report the same, got {}",
            again.body
        );
    }

    assert!(
        sns.state().lock().phone_numbers.is_empty(),
        "the checks must write no opt-out record, got {:?}",
        sns.state().lock().phone_numbers
    );
}

/// The state the check reports is the one the record HOLDS: the same numbers that report
/// `false` with no record report `true` once this account's out-of-band opt-out has
/// written one for them.
#[tokio::test]
async fn an_opted_out_number_is_reported_opted_out() {
    let sns = Sns::new();
    let numbers = e164_numbers();

    for (case, value) in &numbers {
        seed_opted_out_at(&sns, value, SystemTime::now(), None);
        let answer = check(&sns, value).await;
        assert!(
            served(case, &answer),
            "case {case:?}: the seeded opt-out must be reported, got {}",
            answer.body
        );
    }

    assert_eq!(
        sns.state().lock().phone_numbers.len(),
        numbers.len(),
        "the checks must leave exactly the seeded records, got {:?}",
        sns.state().lock().phone_numbers
    );
}

/// Every value that COULD be a phone number is REPORTED ON by the check, however it is
/// written: the operation states a property of this account's record, so a value no record was
/// ever written for is reported opted IN rather than refused — and the read still writes
/// nothing.
#[tokio::test]
async fn a_value_written_as_a_phone_number_is_reported_on_by_the_check() {
    let sns = Sns::new();
    // One number the record says opted out, so the reports below are told apart from a
    // store the calls emptied.
    let opted_out = "+447700900123";
    seed_opted_out_at(&sns, opted_out, SystemTime::now(), None);

    // The forms the published pattern does NOT admit although they are written as a number,
    // and the forms it admits although the destination rule refuses them: the check asks only
    // that the value be a phone number, so each of them is reported on.
    let mut rows: Vec<(String, String)> = written_but_not_admitted_values()
        .into_iter()
        .map(|(case, value)| (case.to_string(), value))
        .collect();
    rows.extend(
        admitted_but_not_e164_values()
            .into_iter()
            .map(|(case, value)| (case.to_string(), value)),
    );

    for (case, value) in &rows {
        let answer = check(&sns, value).await;
        assert!(
            !served(case, &answer),
            "case {case:?}: a value this account holds no record for must report false, got \
             {}",
            answer.body
        );
    }

    // The reports read and wrote nothing: the seeded record is exactly as it was, and
    // it is still what a served call reports.
    assert_eq!(
        sns.state().lock().phone_numbers.len(),
        1,
        "the checks must not add a record, got {:?}",
        sns.state().lock().phone_numbers
    );
    let answer = check(&sns, opted_out).await;
    assert!(
        served("the seeded number", &answer),
        "expected the seeded opt-out to survive every report, got {}",
        answer.body
    );
}

/// The other side of the boundary: a value that is NOT written as a phone number addresses no
/// record at all, so the check refuses it as bad input rather than reporting "opted in" for
/// it — HTTP 400, `InvalidParameter`, the service's fixed badly-formatted-number sentence,
/// none of the (sensitive) value echoed, no `isOptedOut` member, and nothing written.
#[tokio::test]
async fn a_value_that_is_no_phone_number_is_refused_by_the_check() {
    let sns = Sns::new();
    let opted_out = "+12065550100";
    seed_opted_out_at(&sns, opted_out, SystemTime::now(), None);

    for (case, value) in not_written_as_phone_number_values() {
        let answer = check(&sns, &value).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected the refusal, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            "InvalidParameter",
            "case {case:?}: expected the InvalidParameter code, got {}",
            answer.body
        );
        assert_eq!(
            answer.element("Message"),
            Some(invalid_phone_number_message()),
            "case {case:?}: expected the fixed badly-formatted-number sentence, got {}",
            answer.body
        );
        assert!(
            answer
                .element("isOptedOut")
                .or_else(|| answer.element("IsOptedOut"))
                .is_none(),
            "case {case:?}: a refused check must report no opt-out state, got {}",
            answer.body
        );
        if !value.is_empty() {
            assert!(
                !answer.body.contains(value.as_str()),
                "case {case:?}: the refusal must not echo the sensitive value, got {}",
                answer.body
            );
        }
    }

    // The refusals touched nothing, and the record a plausible number does address is still
    // read back over the wire.
    assert_eq!(
        sns.state().lock().phone_numbers.len(),
        1,
        "the refused checks must not write a record, got {:?}",
        sns.state().lock().phone_numbers
    );
    let answer = check(&sns, opted_out).await;
    assert!(
        served("the seeded number", &answer),
        "expected the seeded opt-out to survive every refusal, got {}",
        answer.body
    );
}
