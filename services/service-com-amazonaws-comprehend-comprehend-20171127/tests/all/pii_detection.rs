// SPDX-License-Identifier: CC0-1.0

//! Comprehend's PII analysis: `ContainsPiiEntities`.
//!
//! The operation is a pure function of the submitted document, so these suites
//! assert the RULE — a label comes back for an entity type exactly when the document
//! carries a value of that type's published form, and for no other type — over a
//! TABLE that spans the model's whole `PiiEntityType` catalog, rather than over one
//! recorded request.
//!
//! The per-document byte quota this operation enforces is one row of the service's
//! shared quota table, so it is asserted there (`realtime_text_quotas.rs`) with
//! every other real-time inference operation rather than a second time here.

use serde_json::{json, Value};
use service_com_amazonaws_comprehend_comprehend_20171127::ops::contains_pii_entities::detect_pii_spans;
use service_com_amazonaws_comprehend_comprehend_20171127::ops::types::{
    LANGUAGE_CODE_VALUES, PII_ENTITY_TYPE_VALUES,
};

use crate::comprehend_client::{error_code, Comprehend};

/// `ALL` is the catalog's "every type" selector — the value a redaction job passes
/// to ask for all of them — not a type a document can carry, so no document is ever
/// labelled with it.
const CATALOG_SELECTOR: &str = "ALL";

/// One document per `PiiEntityType`, with the label set it must come back with.
///
/// The types are the model's own enum (`PII_ENTITY_TYPE_VALUES`, asserted below to
/// be covered row for row); each value is written in the form its defining authority
/// publishes — the SSA's issuance rules for an SSN, ISO/IEC 7812-1 for a card
/// number, ISO 13616 for an IBAN, ISO 3779 for a VIN, HMRC's format for a National
/// Insurance number, IANA/IEEE for an address — and every row is a value the issue
/// did not cite.
const DOCUMENTS: &[(&str, &str, &[&str])] = &[
    (
        "email address",
        "Write to jane.roe@example.com when the report is ready.",
        &["EMAIL"],
    ),
    (
        "absolute url",
        "The dashboard lives at https://example.com/profile for now.",
        &["URL"],
    ),
    (
        "scheme-less url",
        "See www.example.co.uk/help before calling.",
        &["URL"],
    ),
    (
        "ipv4 address",
        "The request came from 192.0.2.47 last night.",
        &["IP_ADDRESS"],
    ),
    (
        "ipv6 address",
        "The request came from 2001:db8::1 last night.",
        &["IP_ADDRESS"],
    ),
    (
        "mac address",
        "The laptop reports 00:1B:44:11:3A:B7 as its hardware address.",
        &["MAC_ADDRESS"],
    ),
    (
        "social security number",
        "The form lists 123-45-6789 on the second page.",
        &["SSN"],
    ),
    (
        "individual taxpayer identification number",
        "The filing lists 912-70-1234 on the second page.",
        &["US_INDIVIDUAL_TAX_IDENTIFICATION_NUMBER"],
    ),
    (
        "payment card number",
        "The charge went to 4111 1111 1111 1111 yesterday.",
        &["CREDIT_DEBIT_NUMBER"],
    ),
    ("card verification value", "CVV: 123", &["CREDIT_DEBIT_CVV"]),
    (
        "card expiry",
        "The card has expiry 12/2027 printed on it.",
        &["CREDIT_DEBIT_EXPIRY"],
    ),
    ("personal identification number", "PIN 4096", &["PIN"]),
    ("password", "password: hunter2!", &["PASSWORD"]),
    ("username", "username: jroe123", &["USERNAME"]),
    (
        "aws access key id",
        "The script signs with AKIAIOSFODNN7EXAMPLE today.",
        &["AWS_ACCESS_KEY"],
    ),
    (
        "aws secret access key",
        "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        &["AWS_SECRET_KEY"],
    ),
    (
        "north american phone number",
        "Reach the desk on 206-555-0147 before noon.",
        &["PHONE"],
    ),
    (
        "international phone number",
        "Reach the desk on +1 (206) 555-0147 before noon.",
        &["PHONE"],
    ),
    (
        "street address",
        "Deliveries go to 1600 Amphitheatre Parkway instead.",
        &["ADDRESS"],
    ),
    ("personal name", "My name is Jane Roe.", &["NAME"]),
    ("age", "She is 42 years old this spring.", &["AGE"]),
    (
        "iso 8601 date",
        "The account opened on 2024-03-17 in the morning.",
        &["DATE_TIME"],
    ),
    (
        "month-name date",
        "The account opened on March 17, 2024 in the morning.",
        &["DATE_TIME"],
    ),
    (
        "passport number",
        "passport number X1234567",
        &["PASSPORT_NUMBER"],
    ),
    (
        "driver licence number",
        "driver's license D1234567",
        &["DRIVER_ID"],
    ),
    ("licence plate", "license plate 7ABC123", &["LICENSE_PLATE"]),
    (
        "vehicle identification number",
        "The car is 1HGBH41JXMN109186 on the title.",
        &["VEHICLE_IDENTIFICATION_NUMBER"],
    ),
    (
        "aba routing number",
        "routing number 021000021",
        &["BANK_ROUTING"],
    ),
    (
        "bank account number",
        "bank account number 00123456789",
        &["BANK_ACCOUNT_NUMBER"],
    ),
    (
        "iban",
        "Send it to GB82WEST12345698765432 by Friday.",
        &["INTERNATIONAL_BANK_ACCOUNT_NUMBER"],
    ),
    ("swift code", "SWIFT code DEUTDEFF", &["SWIFT_CODE"]),
    (
        "uk national insurance number",
        "The record shows AB123456C on file.",
        &["UK_NATIONAL_INSURANCE_NUMBER"],
    ),
    (
        "nhs number",
        "NHS number 943 476 5919",
        &["UK_NATIONAL_HEALTH_SERVICE_NUMBER"],
    ),
    (
        "unique taxpayer reference",
        "UTR 1234567890",
        &["UK_UNIQUE_TAXPAYER_REFERENCE_NUMBER"],
    ),
    (
        "canadian social insurance number",
        "social insurance number 046 454 286",
        &["CA_SOCIAL_INSURANCE_NUMBER"],
    ),
    (
        "canadian health number",
        "health card 1234567890",
        &["CA_HEALTH_NUMBER"],
    ),
    (
        "indian permanent account number",
        "The filing shows ABCDE1234F on page two.",
        &["IN_PERMANENT_ACCOUNT_NUMBER"],
    ),
    ("aadhaar number", "Aadhaar 2234 5678 9012", &["IN_AADHAAR"]),
    ("voter id", "voter id ABC1234567", &["IN_VOTER_NUMBER"]),
    ("nrega job card", "NREGA job card number UP1234567", &["IN_NREGA"]),
    (
        // The document writes a COMPONENT of a physical address and no address: a
        // place name is part of what one is written with, so `DetectPiiEntities`
        // places the span and the document's label set does not name the category.
        "a place name written without a street line",
        "The parcel went to Portland last week.",
        &[],
    ),
    (
        "several categories at once",
        "My name is Jane Roe, write to jane.roe@example.com or call 206-555-0147; \
         the file lists 123-45-6789.",
        &["NAME", "EMAIL", "PHONE", "SSN"],
    ),
    (
        "clean prose",
        "The quarterly report is attached for your review.",
        &[],
    ),
    (
        "clean prose with numbers that are not identifiers",
        "We shipped 12 units and 7 spares to the warehouse.",
        &[],
    ),
];

/// The label names in a `ContainsPiiEntities` response, in the order it returned
/// them, after the shared invariants have been checked.
fn labels(case: &str, output: &Value) -> Vec<String> {
    let entries = output["Labels"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Labels array, got {output}"))
        .clone();

    let mut names: Vec<String> = Vec::new();
    let mut previous = f64::INFINITY;
    for entry in &entries {
        let name = entry["Name"].as_str().unwrap_or_default().to_string();
        assert!(
            PII_ENTITY_TYPE_VALUES.contains(&name.as_str()),
            "case {case:?}: {name:?} is not a modelled PiiEntityType value"
        );
        assert_ne!(
            name, CATALOG_SELECTOR,
            "case {case:?}: {CATALOG_SELECTOR} selects every type, so it is never a \
             label of a document, got {output}"
        );
        assert!(
            !names.contains(&name),
            "case {case:?}: expected each type at most once, got {name:?} twice in {output}"
        );
        let score = entry["Score"].as_f64().unwrap_or(f64::NAN);
        assert!(
            score > 0.0 && score <= 1.0,
            "case {case:?}: expected a Score in (0, 1], got {score} for {name:?}"
        );
        assert!(
            score <= previous,
            "case {case:?}: Labels is not sorted by descending Score, got {score} after \
             {previous} in {output}"
        );
        previous = score;
        names.push(name);
    }
    names
}

#[tokio::test]
async fn contains_pii_entities_labels_exactly_the_types_the_document_carries() {
    let comprehend = Comprehend::new();

    for (case, text, expected) in DOCUMENTS {
        let output = comprehend
            .ok(
                "ContainsPiiEntities",
                &json!({ "Text": text, "LanguageCode": "en" }),
            )
            .await;

        let mut found = labels(case, &output);
        found.sort();
        let mut wanted: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        wanted.sort();
        assert_eq!(
            found, wanted,
            "case {case:?}: expected the label set {wanted:?}, got {found:?} in {output}"
        );
    }
}

/// The table above must span the model's catalog: every `PiiEntityType` other than
/// the `ALL` selector is the expected label of at least one row, so no type is
/// claimed to be recognised without a document proving it.
#[test]
fn the_document_table_covers_every_modelled_pii_entity_type() {
    for entity_type in PII_ENTITY_TYPE_VALUES {
        if *entity_type == CATALOG_SELECTOR {
            continue;
        }
        assert!(
            DOCUMENTS
                .iter()
                .any(|(_, _, expected)| expected.contains(entity_type)),
            "case {entity_type:?}: no document in the table is expected to carry this \
             modelled PiiEntityType"
        );
    }
}

/// Values whose standard gives them a check digit, written with that digit WRONG.
///
/// Each is one character away from a row of the table above, so the only thing that
/// can tell them apart is the check the recognizer performs: the type under test is
/// NOT reported for these. The fourth column is the label set that remains, which is
/// empty unless the value's digits still carry another type's form on their own.
const CHECK_DIGIT_FAILURES: &[(&str, &str, &str, &[&str])] = &[
    // ISO/IEC 7812-1 Luhn digit: 4111 1111 1111 1111 passes, ...2 does not.
    (
        "payment card number",
        "The charge went to 4111111111111112 yesterday.",
        "CREDIT_DEBIT_NUMBER",
        &[],
    ),
    // ISO 13616 mod-97: GB82WEST12345698765432 passes, ...33 does not.
    (
        "iban",
        "Send it to GB82WEST12345698765433 by Friday.",
        "INTERNATIONAL_BANK_ACCOUNT_NUMBER",
        &[],
    ),
    // ISO 3779 check digit: position nine of 1HGBH41JXMN109186 is X, not 1.
    (
        "vehicle identification number",
        "The car is 1HGBH41J1MN109186 on the title.",
        "VEHICLE_IDENTIFICATION_NUMBER",
        &[],
    ),
    // Not a dotted quad any parser accepts.
    (
        "ipv4 address",
        "The request came from 999.1.1.1 last night.",
        "IP_ADDRESS",
        &[],
    ),
    // The SSA issues no number whose area is 000.
    (
        "social security number",
        "The form lists 000-45-6789 on the second page.",
        "SSN",
        &[],
    ),
    // An ABA routing number's 3-7-1 weighted sum is a multiple of ten; this one's
    // is not.
    (
        "aba routing number",
        "routing number 021000022",
        "BANK_ROUTING",
        &[],
    ),
    // An NHS number's mod-11 check digit is 9 for 943 476 591, not 8. The digits are
    // still a three-three-four group, which is the published form of a North
    // American phone number, so PHONE remains — on its own evidence, not the NHS
    // keyword's.
    (
        "nhs number",
        "NHS number 943 476 5918",
        "UK_NATIONAL_HEALTH_SERVICE_NUMBER",
        &["PHONE"],
    ),
];

#[tokio::test]
async fn a_value_that_fails_its_own_check_digit_is_not_labelled() {
    let comprehend = Comprehend::new();

    for (case, text, refused, remaining) in CHECK_DIGIT_FAILURES {
        let output = comprehend
            .ok(
                "ContainsPiiEntities",
                &json!({ "Text": text, "LanguageCode": "en" }),
            )
            .await;
        let mut found = labels(case, &output);
        assert!(
            !found.contains(&(*refused).to_string()),
            "case {case:?}: the value fails the check its standard defines, so it is not \
             a {refused}, got {found:?} in {output}"
        );
        found.sort();
        let mut wanted: Vec<String> = remaining.iter().map(|name| (*name).to_string()).collect();
        wanted.sort();
        assert_eq!(
            found, wanted,
            "case {case:?}: expected the label set {wanted:?} once the failed check is \
             dropped, got {found:?} in {output}"
        );
    }
}

/// Forces the lazily built rule table to compile and checks every rule it holds is
/// declared for a MODELLED entity type, so a malformed pattern or a type outside the
/// model's enum fails a test rather than a request.
#[test]
fn every_recognizer_rule_is_declared_for_a_modelled_entity_type() {
    assert!(
        detect_pii_spans("").is_empty(),
        "case \"empty document\": a document with no text carries no PII"
    );

    // One document carrying every row of the table at once: each span it yields comes
    // from a rule, so the spans are what the rule table actually declares.
    let document = DOCUMENTS
        .iter()
        .map(|(_, text, _)| *text)
        .collect::<Vec<&str>>()
        .join("\n");
    let spans = detect_pii_spans(&document);
    assert!(
        spans.len() >= DOCUMENTS.len(),
        "case \"every row at once\": expected at least {} spans, got {}",
        DOCUMENTS.len(),
        spans.len()
    );
    for span in &spans {
        assert!(
            PII_ENTITY_TYPE_VALUES.contains(&span.entity_type),
            "case {:?}: a rule reported a type outside the modelled PiiEntityType enum, \
             got {span:?}",
            span.entity_type
        );
        assert_ne!(
            span.entity_type, CATALOG_SELECTOR,
            "case {:?}: {CATALOG_SELECTOR} selects every type, so no rule may report it, \
             got {span:?}",
            span.entity_type
        );
        assert!(
            span.score > 0.0 && span.score <= 1.0,
            "case {:?}: expected a score in (0, 1], got {span:?}",
            span.entity_type
        );
        assert!(
            span.begin < span.end && span.end <= document.chars().count(),
            "case {:?}: expected offsets inside the document, got {span:?}",
            span.entity_type
        );
    }
}

/// The spans the recognizer hands `DetectPiiEntities` carry CHARACTER offsets — the
/// scale `PiiEntity$BeginOffset`/`EndOffset` are documented on — so a document with
/// a multi-byte character ahead of an entity must not shift them.
#[test]
fn the_recognizer_reports_character_offsets_in_document_order() {
    // The euro sign is three UTF-8 bytes, so every byte offset after it is larger
    // than the character offset the model asks for.
    let text = "Precio 100\u{20ac}: escribe a jane.roe@example.com o llama al 206-555-0147.";
    let expected = [("EMAIL", "jane.roe@example.com"), ("PHONE", "206-555-0147")];

    let spans = detect_pii_spans(text);
    assert_eq!(
        spans.len(),
        expected.len(),
        "case \"multi-byte document\": expected {} spans, got {spans:?}",
        expected.len()
    );
    for (span, (entity_type, value)) in spans.iter().zip(expected) {
        let byte_offset = text
            .find(value)
            .unwrap_or_else(|| panic!("case {value:?}: the document carries the value"));
        let begin = text[..byte_offset].chars().count();
        let end = begin + value.chars().count();
        assert_eq!(
            span.entity_type, entity_type,
            "case {value:?}: expected {entity_type}, got {span:?}"
        );
        assert_eq!(
            (span.begin, span.end),
            (begin, end),
            "case {value:?}: expected the character offsets ({begin}, {end}), got {span:?}"
        );
        assert_ne!(
            span.begin, byte_offset,
            "case {value:?}: the offsets are characters, not bytes, and this document \
             makes the two differ, got {span:?}"
        );
    }
}

/// The recognizer's per-rule match budget must not hide a type: a document that
/// saturates an early rule still gets the labels of the rules after it.
#[tokio::test]
async fn saturating_one_rule_does_not_suppress_the_types_of_later_rules() {
    let comprehend = Comprehend::new();

    // Well past the per-rule budget of IPv6 addresses, then a value only a much
    // later rule recognises.
    let text = format!(
        "{}The form lists 123-45-6789 on the second page.",
        "f::1 ".repeat(1_200)
    );
    let output = comprehend
        .ok(
            "ContainsPiiEntities",
            &json!({ "Text": text, "LanguageCode": "en" }),
        )
        .await;

    let found = labels("saturated rule", &output);
    for expected in ["IP_ADDRESS", "SSN"] {
        assert!(
            found.contains(&expected.to_string()),
            "case \"saturated rule\": expected {expected} to survive the per-rule match \
             budget, got {found:?}"
        );
    }
}

#[tokio::test]
async fn contains_pii_entities_is_deterministic() {
    let comprehend = Comprehend::new();
    let body = json!({
        "Text": "My name is Jane Roe and my card is 4111 1111 1111 1111.",
        "LanguageCode": "en",
    });

    let first = comprehend.ok("ContainsPiiEntities", &body).await;
    let second = comprehend.ok("ContainsPiiEntities", &body).await;
    assert_eq!(
        first, second,
        "case \"repeated identical call\": expected identical labels, got {first} then \
         {second}"
    );
}

#[tokio::test]
async fn a_spanish_document_is_analysed_too() {
    let comprehend = Comprehend::new();

    // `DetectPiiEntities` is the PII operation whose accepted set includes Spanish
    // (`ContainsPiiEntities` admits English alone — see `language_value_sets`), so the
    // Spanish document goes through it; the recognizer both operations project is the
    // same one, so the types it places are the same types.
    for (case, text, expected) in [
        (
            "spanish document with pii",
            "Mi correo es jane.roe@example.com y mi tel\u{e9}fono es 206-555-0147.",
            vec!["EMAIL", "PHONE"],
        ),
        (
            "spanish prose with none",
            "El informe trimestral est\u{e1} adjunto para su revisi\u{f3}n.",
            vec![],
        ),
    ] {
        let output = comprehend
            .ok(
                "DetectPiiEntities",
                &json!({ "Text": text, "LanguageCode": "es" }),
            )
            .await;
        let mut found: Vec<String> = output["Entities"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entity| entity["Type"].as_str().map(str::to_string))
            .collect();
        found.sort();
        found.dedup();
        let mut wanted: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        wanted.sort();
        assert_eq!(
            found, wanted,
            "case {case:?}: expected the type set {wanted:?}, got {found:?} in {output}"
        );
    }
}

#[tokio::test]
async fn a_language_pii_analysis_does_not_accept_is_refused() {
    let comprehend = Comprehend::new();

    // This operation admits English alone, and a value outside that set is refused at
    // the request-validation boundary rather than with the modelled
    // `UnsupportedLanguageException`. The clause itself is asserted over every
    // operation applying a set of its own in `language_value_sets`.
    let accepted = ["en"];
    for code in LANGUAGE_CODE_VALUES {
        let (status, body) = comprehend
            .call(
                "ContainsPiiEntities",
                &json!({ "Text": "Write to jane.roe@example.com.", "LanguageCode": code }),
            )
            .await;

        if accepted.contains(code) {
            assert_eq!(
                status, 200,
                "case {code:?}: PII analysis accepts this language, expected 200, got \
                 {status}: {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {code:?}: expected 400, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {code:?}: expected ValidationException, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn a_language_code_outside_the_modelled_enum_is_refused() {
    let comprehend = Comprehend::new();

    for code in ["xx", "EN", "english", ""] {
        let (status, body) = comprehend
            .call(
                "ContainsPiiEntities",
                &json!({ "Text": "Write to jane.roe@example.com.", "LanguageCode": code }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {code:?}: expected 400 for a value outside the LanguageCode enum, got \
             {status}: {body}"
        );
    }
}

#[tokio::test]
async fn an_absent_or_empty_text_is_refused_by_the_modelled_length_floor() {
    let comprehend = Comprehend::new();

    for (case, body) in [
        ("empty Text", json!({ "Text": "", "LanguageCode": "en" })),
        ("absent Text", json!({ "LanguageCode": "en" })),
    ] {
        let (status, response) = comprehend.call("ContainsPiiEntities", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {response}"
        );
    }
}

// ---------------------------------------------------------------------------
// The confidence a PII detection carries is the one ITS OWN evidence leaves
// ---------------------------------------------------------------------------

/// `(case, document, the modelled PiiEntityType, what the detection rests on)`.
///
/// `PiiEntity$Score` and `EntityLabel$Score` are both documented as "the level of
/// confidence that Amazon Comprehend has in the accuracy of the detection", so the
/// value belongs to the DETECTION: what the recognizer verified about the value, how
/// many of its characters that accounts for, what the document writes beside it, and
/// how often the document writes it. The rows differ along each of those in turn —
/// the kind of verification (a published check digit, a distinctive form, the
/// document's own label), the length of the value, and the number of writings — so a
/// confidence read off a table of per-class constants would repeat across rows that
/// carry plainly different evidence.
///
/// Every value is written in the form its defining authority publishes and none is a
/// value the issue cited.
const CONFIDENCES: &[(&str, &str, &str, &str)] = &[
    (
        "a card number its Luhn check digit confirms",
        "Charge the card 4111111111111111 for the balance.",
        "CREDIT_DEBIT_NUMBER",
        "ISO/IEC 7812-1 check digit over sixteen characters",
    ),
    (
        "an iban its mod-97 check confirms",
        "Send it to GB82WEST12345698765432 before Friday.",
        "INTERNATIONAL_BANK_ACCOUNT_NUMBER",
        "ISO 13616 check over twenty-two characters",
    ),
    (
        "a short e-mail address its form fixes",
        "Write to jo@ex.com today.",
        "EMAIL",
        "an RFC 5322 addr-spec of nine characters",
    ),
    (
        "a longer e-mail address its form fixes",
        "Write to jane.roe@example.co.uk today.",
        "EMAIL",
        "an RFC 5322 addr-spec of twenty-four characters",
    ),
    (
        "the same e-mail address written twice",
        "Write to jane.roe@example.co.uk today, then to jane.roe@example.co.uk again.",
        "EMAIL",
        "the same form, corroborated by a second writing",
    ),
    (
        "a card security code the document labels",
        "The CVV is 123 on the back.",
        "CREDIT_DEBIT_CVV",
        "three ambiguous digits admitted by a short label",
    ),
    (
        "an account number the document labels at length",
        "The bank account number is 123456789 on file.",
        "BANK_ACCOUNT_NUMBER",
        "nine ambiguous digits admitted by a spelled-out label",
    ),
    (
        "a name standing in running text",
        "Contact John Smith at the office.",
        "NAME",
        "the shape of a personal name in a person-bearing position",
    ),
];

/// The `Score` of the one entity of `entity_type` the document carries.
async fn score_of(comprehend: &Comprehend, case: &str, text: &str, entity_type: &str) -> f64 {
    let output = comprehend
        .ok(
            "DetectPiiEntities",
            &json!({ "Text": text, "LanguageCode": "en" }),
        )
        .await;
    let found: Vec<f64> = output["Entities"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected Entities, got {output}"))
        .iter()
        .filter(|entity| entity["Type"].as_str() == Some(entity_type))
        .map(|entity| entity["Score"].as_f64().unwrap_or(f64::NAN))
        .collect();
    assert!(
        !found.is_empty(),
        "case {case:?}: expected a {entity_type} entity, got {output}"
    );
    for score in &found {
        assert!(
            *score > 0.0 && *score < 1.0,
            "case {case:?}: expected a Score strictly inside (0, 1) for {entity_type}, \
             got {score} in {output}"
        );
    }
    // Where the document writes the value more than once, every writing carries the
    // same evidence, so the confidence does not depend on which one a caller reads.
    found.iter().copied().fold(0.0, f64::max)
}

#[tokio::test]
async fn a_pii_score_is_the_confidence_the_detections_own_evidence_leaves() {
    let comprehend = Comprehend::new();
    let mut scored: Vec<(&str, f64)> = Vec::new();
    for (case, text, entity_type, _) in CONFIDENCES {
        scored.push((case, score_of(&comprehend, case, text, entity_type).await));
    }

    // No ladder: the rows differ in what their detections rest on, so the confidences
    // differ too. Three constants — one per evidence class — would collapse these
    // eight rows onto three values.
    let spread = crate::batch_inference_calibration::distinct(
        scored.iter().map(|(_, score)| *score),
    );
    assert!(
        spread >= 6,
        "expected the confidences to follow each detection's own evidence, got {spread} \
         distinct values across {} rows: {scored:?}",
        scored.len()
    );

    let score = |wanted: &str| -> f64 {
        scored
            .iter()
            .find(|(case, _)| *case == wanted)
            .map(|(_, score)| *score)
            .unwrap_or_else(|| panic!("no score for case {wanted:?}"))
    };

    // More evidence, more confidence — each pair below differs in exactly one thing
    // about the evidence.
    for (stronger, weaker, why) in [
        (
            "a card number its Luhn check digit confirms",
            "a card security code the document labels",
            "a value its own check digit confirms is a surer detection than ambiguous \
             digits a label admits",
        ),
        (
            "a longer e-mail address its form fixes",
            "a short e-mail address its form fixes",
            "the same form fixing more characters is less likely to be a coincidence",
        ),
        (
            "the same e-mail address written twice",
            "a longer e-mail address its form fixes",
            "a document that writes the value again corroborates the reading",
        ),
        (
            "an account number the document labels at length",
            "a card security code the document labels",
            "a label that spells out what the value is says more than an abbreviation \
             beside three digits",
        ),
    ] {
        assert!(
            score(stronger) > score(weaker),
            "case {stronger:?} vs {weaker:?}: {why}, so expected a higher confidence, \
             got {} against {}",
            score(stronger),
            score(weaker)
        );
    }

    // A detection whose own check digit accounts for every character of the value is
    // an unambiguous one, which is where a redaction client's gate sits; one admitted
    // by a bare label stays materially below it.
    assert!(
        score("an iban its mod-97 check confirms") > 0.99,
        "expected a checksum-confirmed value near certainty, got {}",
        score("an iban its mod-97 check confirms")
    );
    assert!(
        score("a card security code the document labels") < 0.95,
        "expected three ambiguous digits under a short label below a near-certain gate, \
         got {}",
        score("a card security code the document labels")
    );
}

#[tokio::test]
async fn a_label_confidence_combines_the_detections_that_support_it() {
    let comprehend = Comprehend::new();

    // The rows differ in how many detections of the one type the document carries.
    // Each is its own evidence that the document CONTAINS that type, which is the
    // detection `EntityLabel$Score` is about, and the type here is identified by its
    // own published form: a document writing one value of that form contains the
    // category, so the label is already at full confidence and a further writing of it
    // cannot raise the confidence further.
    let rows = [
        (
            "one e-mail address",
            "Write to jane.roe@example.co.uk today.",
            1usize,
        ),
        (
            "two e-mail addresses",
            "Write to jane.roe@example.co.uk and to john.doe@example.co.uk today.",
            2,
        ),
        (
            "three e-mail addresses",
            "Write to jane.roe@example.co.uk, to john.doe@example.co.uk or to \
             ada.k@example.co.uk today.",
            3,
        ),
    ];

    let mut by_count: Vec<(&str, usize, f64)> = Vec::new();
    for (case, text, count) in rows {
        let output = comprehend
            .ok(
                "ContainsPiiEntities",
                &json!({ "Text": text, "LanguageCode": "en" }),
            )
            .await;
        let label = output["Labels"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected Labels, got {output}"))
            .iter()
            .find(|label| label["Name"].as_str() == Some("EMAIL"))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected an EMAIL label, got {output}"));
        let score = label["Score"].as_f64().unwrap_or(f64::NAN);

        // The label's confidence is at least as high as the best single detection of
        // the type: every further detection is further evidence for the same claim.
        let strongest = score_of(&comprehend, case, text, "EMAIL").await;
        assert!(
            score + 1e-9 >= strongest && score < 1.0 + f64::EPSILON,
            "case {case:?}: expected a label confidence at or above the strongest \
             detection {strongest}, got {score}"
        );
        by_count.push((case, count, score));
    }

    for pair in by_count.windows(2) {
        let (weaker_case, weaker_count, weaker) = pair[0];
        let (stronger_case, stronger_count, stronger) = pair[1];
        assert!(
            stronger >= weaker,
            "case {stronger_case:?} vs {weaker_case:?}: {stronger_count} detections are \
             no less evidence that the document contains the type than {weaker_count}, so \
             expected a confidence at or above it, got {stronger} against {weaker}"
        );
    }

    // A category a published form identifies is CONTAINED in the document the moment
    // the document writes one value of that form: the label question asks only that,
    // not where the value's own boundaries are, so it is answered at full confidence —
    // which is where the cheap pre-filter in front of redaction has to sit for a
    // client's "act only at full confidence" threshold to route the document at all.
    for (case, count, score) in &by_count {
        assert_eq!(
            *score, 1.0,
            "case {case:?}: a form-identified category written {count} time(s) is \
             contained in the document, so expected a label confidence of 1, got {score}"
        );
    }
}
