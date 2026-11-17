// SPDX-License-Identifier: CC0-1.0

//! What a submitted tax registration NUMBER earns, over the restJson1 wire:
//! refusal by the write, or a stored record whose validation reaches an outcome.
//!
//! ONE rule, in the two senses the API answers separately. The authority of the
//! jurisdiction a registration is held in publishes how a number of that kind is
//! WRITTEN — its length and its character layout — and, for many of them, how
//! one is CONFIRMED: a check digit computed from the rest of the number. So:
//!
//! * a value that is not written the way such a number is written is not a
//!   registration number of that type at all, and the write REFUSES it (HTTP 400
//!   `ValidationException`, the `errorCode` for a field that failed validation,
//!   the class's own message token and `fieldList` naming the parameter that
//!   carried it), storing nothing;
//! * a number written that way is stored `Pending`, and the validation the write
//!   started settles it to what the authority's confirming rule says: `Verified`
//!   where that rule confirms the number or publishes nothing to confirm it
//!   with, `Rejected` where it does not — a well-formed number the register does
//!   not know.
//!
//! A jurisdiction and type this service publishes no structure for is judged by
//! neither question.
//!
//! Beside the status, a settled registration reports WHY its validation settled
//! there: a registration the validation did not verify carries a `statusReason`
//! whose `code` names the reason class and whose `message` is the sentence worded
//! for that class, and one with no reason to report — an outstanding validation,
//! a verified registration — carries none. The last tests in this file assert
//! that over the same table.
//!
//! The table spans the class the rule generalises over: each jurisdiction whose
//! structure this service holds, with a number written the published way and
//! confirmed, one written that way but not confirmed, and one not written that
//! way at all (each differing from an admitted number only in that check digit
//! or in that layout), plus a jurisdiction it holds none for, and both read
//! paths. `tests/all/accepted_registration_verifies.rs` spans the admitted side
//! across the dimensions the outcome is NOT a function of, and
//! `tests/all/batch_refusal_rows.rs` the per-account channel the BATCH write
//! reports the refusal on.
//!
//! The harness (`emulator_with_engine`, `call`, the two helpers that move a
//! clock-owed TRN validation, and the status reader) lives in
//! `tax_registration_lifecycle.rs` and `fidelity_rules.rs`; nothing here builds a
//! second one.

use serde_json::{json, Value};
use service_com_amazonaws_taxsettings_taxsettings::state::{
    ERROR_CODE_FIELD_VALIDATION_FAILED, INVALID_TAX_REGISTRATION_NUMBER, STATUS_PENDING,
    STATUS_REJECTED, STATUS_VERIFIED,
};

use crate::fidelity_rules::{read_status, reported_registration};
use crate::tax_registration_lifecycle::{
    call, emulator_with_engine, field_list, hold_validations_open, make_validations_due,
};

/// The read paths that report the status of an account's own TRN.
const TRN_READERS: &[&str] = &["GetTaxRegistration", "ListTaxRegistrations"];

/// What a submitted number earns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// The write refuses it: it is not written the way such a number is written.
    Refused,
    /// The write stores it, and the validation it started settles to this
    /// status.
    Settles(&'static str),
}

/// One submitted entry and what its number earns.
struct Row {
    case: &'static str,
    /// The `taxRegistrationEntry` to submit. Every row satisfies the per-country
    /// write rules, which are about the entry's MEMBERS, so what decides each
    /// answer is the number alone.
    entry: Value,
    outcome: Outcome,
}

/// A German VAT entry carrying `registration_id`.
fn german_vat(registration_id: &str) -> Value {
    json!({
        "registrationId": registration_id,
        "registrationType": "VAT",
        "legalName": "Beispiel GmbH",
        "legalAddress": {
            "addressLine1": "Unter den Linden 1",
            "city": "Berlin",
            "postalCode": "10117",
            "countryCode": "DE",
        },
    })
}

/// A French VAT entry carrying `registration_id`. France's documented write rule
/// is the `sirenNumber` block (`crate::state::COUNTRY_RULES`), which every row
/// carries so the entry's members are never what is refused.
fn french_vat(registration_id: &str) -> Value {
    json!({
        "registrationId": registration_id,
        "registrationType": "VAT",
        "legalName": "Exemple SARL",
        "legalAddress": {
            "addressLine1": "1 Rue de la Paix",
            "city": "Paris",
            "postalCode": "75002",
            "countryCode": "FR",
        },
        "additionalTaxInformation": {
            "franceAdditionalInfo": { "sirenNumber": "345678901" },
        },
    })
}

/// A Brazilian CNPJ entry, whose legal name and address the service fills in.
fn brazilian_cnpj(registration_id: &str) -> Value {
    json!({
        "registrationId": registration_id,
        "registrationType": "CNPJ",
    })
}

/// A Brazilian CPF entry. Brazil's CPF rows of the write rules require the
/// address's `stateOrRegion`, so every row carries it.
fn brazilian_cpf(registration_id: &str) -> Value {
    json!({
        "registrationId": registration_id,
        "registrationType": "CPF",
        "legalName": "Exemplo da Silva",
        "legalAddress": {
            "addressLine1": "Avenida Paulista 1",
            "city": "Sao Paulo",
            "stateOrRegion": "SP",
            "postalCode": "01310-000",
            "countryCode": "BR",
        },
    })
}

/// An Indian PAN entry, whose legal address the service fills in from the number.
fn indian_pan(registration_id: &str) -> Value {
    json!({
        "registrationId": registration_id,
        "registrationType": "PAN",
        "legalName": "Example India Private Limited",
    })
}

/// An Indian GST entry. India's write rule requires the address's
/// `stateOrRegion`.
fn indian_gst(registration_id: &str) -> Value {
    json!({
        "registrationId": registration_id,
        "registrationType": "GST",
        "legalName": "Example India Private Limited",
        "legalAddress": {
            "stateOrRegion": "Karnataka",
            "city": "Bengaluru",
            "postalCode": "560025",
            "countryCode": "IN",
        },
    })
}

/// A Kenyan TIN entry. Kenya's write rule requires the `personType`, and a
/// `Business` one needs no certificate document.
fn kenyan_tin(registration_id: &str) -> Value {
    json!({
        "registrationId": registration_id,
        "registrationType": "TIN",
        "legalName": "Example Kenya Limited",
        "legalAddress": {
            "addressLine1": "1 Kenyatta Avenue",
            "city": "Nairobi",
            "postalCode": "00100",
            "countryCode": "KE",
        },
        "additionalTaxInformation": {
            "kenyaAdditionalInfo": { "personType": "Business" },
        },
    })
}

/// A Malaysian NRIC entry — a jurisdiction and type this service holds no
/// published structure for. Malaysia's write rules pair `NRIC` with the
/// `Individual` sector.
fn malaysian_nric(registration_id: &str) -> Value {
    json!({
        "registrationId": registration_id,
        "registrationType": "NRIC",
        "legalName": "Siti Example",
        "sector": "Individual",
        "legalAddress": {
            "addressLine1": "1 Jalan Example",
            "city": "Kuala Lumpur",
            "postalCode": "50000",
            "countryCode": "MY",
        },
    })
}

/// Each jurisdiction whose structure this service holds, with a number written
/// the published way and confirmed, one written that way but not confirmed where
/// its authority publishes a confirming rule, and one not written that way at
/// all; plus a jurisdiction this service holds no structure for.
///
/// Every number below differs from an admitted one only in the check digit its
/// authority's rule computes or in the layout that authority writes, so no row
/// passes because the number looks odd.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a German VAT number whose check digit is the published one",
            entry: german_vat("DE136695976"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "the same German number without the country prefix",
            entry: german_vat("136695976"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "a German VAT number whose check digit is not the published one",
            entry: german_vat("DE123456789"),
            outcome: Outcome::Settles(STATUS_REJECTED),
        },
        Row {
            case: "a German VAT number of the wrong length",
            entry: german_vat("DE13669597"),
            outcome: Outcome::Refused,
        },
        Row {
            case: "a French VAT number whose key matches its SIREN",
            entry: french_vat("FR15345678901"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "a French VAT number whose key does not match its SIREN",
            entry: french_vat("FR12345678901"),
            outcome: Outcome::Settles(STATUS_REJECTED),
        },
        Row {
            case: "a French VAT number of the wrong length",
            entry: french_vat("FR1534567890"),
            outcome: Outcome::Refused,
        },
        Row {
            case: "a CNPJ written with the separators the register prints",
            entry: brazilian_cnpj("12.345.678/0001-95"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "the same CNPJ written as bare digits",
            entry: brazilian_cnpj("12345678000195"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "a CNPJ whose second check digit is wrong",
            entry: brazilian_cnpj("12345678000196"),
            outcome: Outcome::Settles(STATUS_REJECTED),
        },
        Row {
            case: "a CNPJ of the wrong length",
            entry: brazilian_cnpj("1234567800019"),
            outcome: Outcome::Refused,
        },
        Row {
            case: "a CPF whose check digits are the published ones",
            entry: brazilian_cpf("111.444.777-35"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "a CPF whose check digits are not",
            entry: brazilian_cpf("111.444.777-36"),
            outcome: Outcome::Settles(STATUS_REJECTED),
        },
        Row {
            case: "a PAN in the published layout",
            entry: indian_pan("ABCDE1234F"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "a PAN whose letters and digits are in the wrong places",
            entry: indian_pan("ABCD12345F"),
            outcome: Outcome::Refused,
        },
        Row {
            case: "a GSTIN in the published layout",
            entry: indian_gst("29ABCDE1234F1Z5"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "a GSTIN missing the fixed letter of the layout",
            entry: indian_gst("29ABCDE1234F1X5"),
            outcome: Outcome::Refused,
        },
        Row {
            case: "a Kenyan PIN in the published layout",
            entry: kenyan_tin("P051234567M"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "a Kenyan PIN whose taxpayer-class letter is not one published",
            entry: kenyan_tin("X051234567M"),
            outcome: Outcome::Refused,
        },
        Row {
            case: "a Malaysian NRIC, whose structure this service holds none of",
            entry: malaysian_nric("800101011234"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
        Row {
            case: "a Malaysian number nothing resembles, judged by no structure",
            entry: malaysian_nric("not-a-number-at-all"),
            outcome: Outcome::Settles(STATUS_VERIFIED),
        },
    ]
}

/// RULE: a number not written the way its jurisdiction's authority writes one is
/// refused by the write and nothing is stored for the account; one written that
/// way is stored `Pending` and settles to what that authority's confirming rule
/// says, reported the same way by every read path and by every read.
#[tokio::test]
async fn a_number_not_written_the_published_way_is_refused_and_every_other_settles() {
    for row in rows() {
        let (router, engine) = emulator_with_engine();
        let account_id = "000000000001";
        let put = call(
            &router,
            "PutTaxRegistration",
            &json!({ "accountId": account_id, "taxRegistrationEntry": row.entry }),
        )
        .await;

        let settles_to = match row.outcome {
            Outcome::Refused => {
                assert_eq!(
                    (put.status, put.error_code.as_str()),
                    (400, "ValidationException"),
                    "case {:?}: the number must be refused, got {put:?}",
                    row.case
                );
                assert_eq!(
                    (
                        put.get("errorCode").as_str(),
                        put.get("message").as_str(),
                        field_list(&put),
                    ),
                    (
                        Some(ERROR_CODE_FIELD_VALIDATION_FAILED),
                        Some(INVALID_TAX_REGISTRATION_NUMBER),
                        vec!["REGISTRATION_ID".to_string()],
                    ),
                    "case {:?}: expected the refusal class's own envelope, got {:?}",
                    row.case,
                    put.body
                );
                // A refused write stores nothing, so the account still has no
                // record.
                let read = call(
                    &router,
                    "GetTaxRegistration",
                    &json!({ "accountId": account_id }),
                )
                .await;
                assert_eq!(
                    (read.status, read.error_code.as_str()),
                    (404, "ResourceNotFoundException"),
                    "case {:?}: a refused write must store nothing, got {read:?}",
                    row.case
                );
                continue;
            }
            Outcome::Settles(status) => status,
        };

        // Acceptance and the outcome of validating the number are separate
        // facts: the write stores the entry and reports the outstanding status,
        // whichever status the validation will reach.
        assert_eq!(
            put.status, 200,
            "case {:?}: the entry must be accepted, got {put:?}",
            row.case
        );
        assert_eq!(
            put.get("status").as_str(),
            Some(STATUS_PENDING),
            "case {:?}: the write must report the outstanding status, got {:?}",
            row.case,
            put.body
        );

        // While the validation is outstanding no reader reports an outcome.
        hold_validations_open(&engine);
        for reader in TRN_READERS {
            let status = read_status(&router, reader, account_id).await;
            assert_eq!(
                status.as_deref(),
                Some(STATUS_PENDING),
                "case {:?}: {reader} must report the outstanding status, got {status:?}",
                row.case
            );
        }

        // Once the clock has made it due, every reader reports the outcome the
        // number earned — and keeps reporting it.
        make_validations_due(&engine);
        for reader in TRN_READERS.iter().chain(TRN_READERS.iter()) {
            let status = read_status(&router, reader, account_id).await;
            assert_eq!(
                status.as_deref(),
                Some(settles_to),
                "case {:?}: {reader} must report {settles_to:?}, got {status:?}",
                row.case
            );
        }
    }
}

/// RULE: both validation outcomes are reachable — the state machine of a stored
/// registration has more than one terminal value, so a client that polls for one
/// of them can see it.
#[tokio::test]
async fn both_validation_outcomes_are_reachable() {
    let mut reached = Vec::new();
    for row in rows() {
        let (router, engine) = emulator_with_engine();
        let account_id = "000000000001";
        let put = call(
            &router,
            "PutTaxRegistration",
            &json!({ "accountId": account_id, "taxRegistrationEntry": row.entry }),
        )
        .await;
        if row.outcome == Outcome::Refused {
            continue;
        }
        assert_eq!(
            put.status, 200,
            "case {:?}: the entry must be accepted, got {put:?}",
            row.case
        );
        make_validations_due(&engine);
        if let Some(status) = read_status(&router, "GetTaxRegistration", account_id).await {
            if !reached.contains(&status) {
                reached.push(status);
            }
        }
    }
    reached.sort();
    assert_eq!(
        reached,
        vec![STATUS_REJECTED.to_string(), STATUS_VERIFIED.to_string()],
        "the stored registrations above must reach both terminal outcomes, got {reached:?}"
    );
}

/// RULE: a validation that failed changes nothing but the status — the record the
/// write stored is still the record every read reports, so a client that polls to
/// a `Rejected` status can see what it submitted.
#[tokio::test]
async fn a_failed_validation_leaves_the_stored_record_as_it_was() {
    let (router, engine) = emulator_with_engine();
    let account_id = "000000000002";
    // Nine digits, so the write admits it, closed by a digit Germany's rule does
    // not compute for the eight before it.
    let registration_id = "DE123456789";
    let put = call(
        &router,
        "PutTaxRegistration",
        &json!({
            "accountId": account_id,
            "taxRegistrationEntry": german_vat(registration_id),
        }),
    )
    .await;
    assert_eq!(put.status, 200, "the entry must be accepted, got {put:?}");

    make_validations_due(&engine);
    let read = call(
        &router,
        "GetTaxRegistration",
        &json!({ "accountId": account_id }),
    )
    .await;
    assert_eq!(read.status, 200, "GetTaxRegistration expected 200, got {read:?}");
    let reported = read.get("taxRegistration").clone();
    assert_eq!(
        reported.get("status").and_then(Value::as_str),
        Some(STATUS_REJECTED),
        "the record must report the outcome its number earned, got {reported:?}"
    );
    assert_eq!(
        reported.get("registrationId").and_then(Value::as_str),
        Some(registration_id),
        "the record must still carry the submitted number, got {reported:?}"
    );
    assert_eq!(
        reported.get("legalName").and_then(Value::as_str),
        Some("Beispiel GmbH"),
        "the record must still carry the submitted legal name, got {reported:?}"
    );
}

/// RULE: a refusal of the number changes nothing the account already holds — the
/// record a previous write stored is still the record every read reports.
#[tokio::test]
async fn a_refused_number_leaves_the_stored_record_as_it_was() {
    let (router, engine) = emulator_with_engine();
    let account_id = "000000000003";
    let held = "DE136695976";
    let put = call(
        &router,
        "PutTaxRegistration",
        &json!({
            "accountId": account_id,
            "taxRegistrationEntry": german_vat(held),
        }),
    )
    .await;
    assert_eq!(put.status, 200, "the entry must be accepted, got {put:?}");
    make_validations_due(&engine);

    let refused = call(
        &router,
        "PutTaxRegistration",
        &json!({
            "accountId": account_id,
            // Eight digits: not the length Germany writes such a number in.
            "taxRegistrationEntry": german_vat("DE13669597"),
        }),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "the rewrite's number must be refused, got {refused:?}"
    );

    let read = call(
        &router,
        "GetTaxRegistration",
        &json!({ "accountId": account_id }),
    )
    .await;
    assert_eq!(read.status, 200, "GetTaxRegistration expected 200, got {read:?}");
    let reported = read.get("taxRegistration").clone();
    assert_eq!(
        reported.get("registrationId").and_then(Value::as_str),
        Some(held),
        "the record must still carry the number the admitted write stored, got {reported:?}"
    );
    assert_eq!(
        reported.get("status").and_then(Value::as_str),
        Some(STATUS_VERIFIED),
        "the held record must still report its own status, got {reported:?}"
    );
}

/// The reason `code` a registration whose validation did not verify it is
/// reported under, and the sentence worded for that class — both as AWS answers
/// `GetTaxRegistration` for a registration in the `Rejected` status (recorded;
/// the checked-out model declares neither). The message is FIXED text for the
/// class: it interpolates nothing from the request and nothing from the store,
/// which `the_reason_of_one_class_is_the_same_text_for_every_registration` pins.
const UNVERIFIED_REASON_CODE: &str = "OTHER";
/// See [`UNVERIFIED_REASON_CODE`].
const UNVERIFIED_REASON_MESSAGE: &str =
    "Your tax registration could not be verified. Please review your information and resubmit.";

/// The `statusReason` members a read reports for `account_id`, as
/// `(code, message)`, or `None` when the reported registration carries no
/// reason at all.
async fn reported_reason(
    router: &smithy_server_runtime::MultiServiceRouter,
    reader: &str,
    account_id: &str,
) -> Option<(String, String)> {
    let reported = reported_registration(router, reader, account_id)
        .await
        .unwrap_or_else(|| panic!("{reader} must report a registration for {account_id}"));
    let reason = reported.get("statusReason")?;
    let member = |name: &str| {
        reason
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("the reported reason must carry {name:?}, got {reason:?}")
            })
            .to_string()
    };
    Some((member("code"), member("message")))
}

/// RULE: a registration reported in a status its validation did not verify
/// carries a `statusReason` whose `code` names the reason class and whose
/// `message` is the sentence worded for that class; a registration with no reason
/// to report — one whose validation is still outstanding, or one it verified —
/// carries none. Every read path reports the one record the same way, and a
/// repeated read reports it again.
///
/// Driven over the same table as the status rule above, so every jurisdiction
/// whose numbers this service judges is covered on both sides of the outcome.
#[tokio::test]
async fn a_settled_registration_reports_why_its_validation_settled_there() {
    for row in rows() {
        let Outcome::Settles(settles_to) = row.outcome else {
            // A number the write refuses stores no registration, so there is
            // nothing for a reader to report a reason about; the status rule
            // above pins that refusal.
            continue;
        };
        let (router, engine) = emulator_with_engine();
        let account_id = "000000000001";
        let put = call(
            &router,
            "PutTaxRegistration",
            &json!({ "accountId": account_id, "taxRegistrationEntry": row.entry }),
        )
        .await;
        assert_eq!(
            put.status, 200,
            "case {:?}: the entry must be accepted, got {put:?}",
            row.case
        );

        // A validation that is still outstanding has reached no outcome, so
        // there is nothing to explain yet.
        hold_validations_open(&engine);
        for reader in TRN_READERS {
            let reason = reported_reason(&router, reader, account_id).await;
            assert_eq!(
                reason, None,
                "case {:?}: {reader} must report no reason while the validation is \
                 outstanding, got {reason:?}",
                row.case
            );
        }

        // Once the clock has made it due, the reason beside the status is the one
        // the outcome has — and stays it, for either read path.
        make_validations_due(&engine);
        let expected = (settles_to == STATUS_REJECTED).then(|| {
            (
                UNVERIFIED_REASON_CODE.to_string(),
                UNVERIFIED_REASON_MESSAGE.to_string(),
            )
        });
        for reader in TRN_READERS.iter().chain(TRN_READERS.iter()) {
            let status = read_status(&router, reader, account_id).await;
            assert_eq!(
                status.as_deref(),
                Some(settles_to),
                "case {:?}: {reader} must report {settles_to:?}, got {status:?}",
                row.case
            );
            let reason = reported_reason(&router, reader, account_id).await;
            assert_eq!(
                reason, expected,
                "case {:?}: {reader} must report the reason {expected:?} beside \
                 {settles_to:?}, got {reason:?}",
                row.case
            );
        }
    }
}

/// One rejected registration, written under its own account with its own number
/// and its own legal name — the values a message that interpolated anything would
/// carry.
struct ReasonRow {
    case: &'static str,
    account_id: &'static str,
    registration_id: &'static str,
    /// The name the entry carries, or the one the service fills in for a
    /// jurisdiction that supplies it.
    legal_name: &'static str,
    entry: Value,
}

/// A registration of each jurisdiction whose confirming rule this service holds,
/// each failing that rule for its own reason, each held by a different account
/// under a different name.
fn reason_rows() -> Vec<ReasonRow> {
    vec![
        ReasonRow {
            case: "a German VAT number whose check digit is not the published one",
            account_id: "000000000011",
            registration_id: "DE123456789",
            legal_name: "Beispiel GmbH",
            entry: german_vat("DE123456789"),
        },
        ReasonRow {
            case: "a French VAT number whose key does not match its SIREN",
            account_id: "000000000222",
            registration_id: "FR12345678901",
            legal_name: "Exemple SARL",
            entry: french_vat("FR12345678901"),
        },
        ReasonRow {
            case: "a CNPJ whose second check digit is wrong",
            account_id: "000000003333",
            registration_id: "12345678000196",
            // Brazil is a jurisdiction whose legal name the service fills in.
            legal_name: "CNPJ",
            entry: brazilian_cnpj("12345678000196"),
        },
        ReasonRow {
            case: "a CPF whose check digits are not the published ones",
            account_id: "000000044444",
            registration_id: "111.444.777-36",
            legal_name: "Exemplo da Silva",
            entry: brazilian_cpf("111.444.777-36"),
        },
    ]
}

/// RULE: the reason reported for one class is the same `code` and the same
/// `message` for every registration of that class — the message is the class's
/// own text, so it carries no account id, no registration number, no legal name
/// and no jurisdiction of the registration it explains.
#[tokio::test]
async fn the_reason_of_one_class_is_the_same_text_for_every_registration() {
    let mut reported: Vec<(String, String)> = Vec::new();
    for row in reason_rows() {
        let (router, engine) = emulator_with_engine();
        let put = call(
            &router,
            "PutTaxRegistration",
            &json!({ "accountId": row.account_id, "taxRegistrationEntry": row.entry }),
        )
        .await;
        assert_eq!(
            put.status, 200,
            "case {:?}: the entry must be accepted, got {put:?}",
            row.case
        );
        make_validations_due(&engine);
        let status = read_status(&router, "GetTaxRegistration", row.account_id).await;
        assert_eq!(
            status.as_deref(),
            Some(STATUS_REJECTED),
            "case {:?}: the number must settle to {STATUS_REJECTED:?}, got {status:?}",
            row.case
        );
        let (code, message) = reported_reason(&router, "GetTaxRegistration", row.account_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: a registration in {STATUS_REJECTED:?} must report a reason, got \
                     none",
                    row.case
                )
            });
        assert_eq!(
            (code.as_str(), message.as_str()),
            (UNVERIFIED_REASON_CODE, UNVERIFIED_REASON_MESSAGE),
            "case {:?}: expected the class's own reason, got {:?}",
            row.case,
            (&code, &message)
        );
        // Nothing of this registration appears in the text that explains its
        // class.
        for value in [row.account_id, row.registration_id, row.legal_name] {
            assert!(
                !message.contains(value),
                "case {:?}: the message must interpolate nothing of the registration, but it \
                 carries {value:?}: {message:?}",
                row.case
            );
        }
        reported.push((code, message));
    }
    reported.dedup();
    assert_eq!(
        reported.len(),
        1,
        "every registration of the class must report one reason, got {reported:?}"
    );
}
