// SPDX-License-Identifier: CC0-1.0

//! The account's use case for model access end to end: `Put` then `Get` over HTTP,
//! through the same `MultiServiceRouter` the emulator boots.
//!
//! # The rule
//!
//! The use case for model access is ONE account-level form, and `formData` carries that form
//! as a DOCUMENT rather than as opaque bytes: Amazon Bedrock parses it, so a blob whose bytes
//! are no form document is refused as input (400 `ValidationException`, "Invalid form data.")
//! and nothing is stored. A form it accepts replaces the account's form whole, and
//! `GetUseCaseForModelAccess` reports back exactly the bytes that were submitted — byte for
//! byte, nothing re-encoded and nothing merged in from an earlier put. While no form has been
//! accepted the get has no `@required formData` to report, so it answers the modelled
//! `ResourceNotFoundException` (HTTP 404).
//!
//! # What the tables below generalise over
//!
//! [`cases`] is one row per independent form along the axes an ACCEPTED form spans: its SIZE
//! (both published bounds of `AcknowledgementFormDataBody`, `@length(10..16384)` in this
//! service's Smithy model, and sizes between them), its CONTENT (the fields the form carries,
//! their count, UTF-8 beyond ASCII, and values that are not text at all) and its ORDER (the
//! rows are driven in sequence against ONE emulator, so each get also witnesses that the put
//! before it replaced the previous row rather than appending to or merging with it).
//!
//! [`a_blob_that_is_no_form_document_is_refused_and_changes_nothing`] is the same span from
//! the other side: one row per independent way a blob fails to be a form document, each
//! inside the published size bounds so the refusal is about the DOCUMENT and not its length.
//! [`a_form_outside_the_published_size_bounds_is_refused_and_changes_nothing`] drives one form
//! under and one past each of those bounds.

use serde_json::{json, Value};

use crate::support::{emulator, Emulator, Reply};

/// The URI both `PutUseCaseForModelAccess` and `GetUseCaseForModelAccess` are bound to.
const USE_CASE_URI: &str = "/use-case-for-model-access";

/// `AcknowledgementFormDataBody` is `@length(min: 10, max: 16384)`, from this service's
/// Smithy model.
const MIN_FORM_BYTES: usize = 10;
const MAX_FORM_BYTES: usize = 16384;

/// One form this suite submits and reads back.
struct Case {
    name: &'static str,
    form_data: Vec<u8>,
}

/// `n` bytes of `filler`, for a form driven at a bound of its `@length`.
fn filler_bytes(filler: u8, n: usize) -> Vec<u8> {
    vec![filler; n]
}

/// A form document whose one field is padded with `filler` until the whole blob is `n`
/// bytes, so a chosen SIZE can be driven by a blob that is still a form.
///
/// The field name is one character so the envelope fits inside the shortest form
/// `AcknowledgementFormDataBody` admits.
fn form_of_size(filler: char, n: usize) -> Vec<u8> {
    let envelope = br#"{"u":""}"#.len();
    assert!(
        n >= envelope,
        "a form document cannot be shorter than its own envelope ({envelope} bytes)"
    );
    let value: String = std::iter::repeat_n(filler, n - envelope).collect();
    format!(r#"{{"u":"{value}"}}"#).into_bytes()
}

/// The forms the rule must round-trip, one per independent shape of the document.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "an acknowledgement form carrying several fields",
            form_data: br#"{"companyName":"Acme","useCase":"document summarisation"}"#.to_vec(),
        },
        Case {
            name: "the shortest form the model admits (10 bytes)",
            form_data: form_of_size('a', MIN_FORM_BYTES),
        },
        Case {
            name: "the longest form the model admits (16384 bytes)",
            form_data: form_of_size('f', MAX_FORM_BYTES),
        },
        Case {
            name: "a field value in UTF-8 beyond ASCII",
            form_data: r#"{"useCase":"résumé analysis — 日本語 🙂"}"#.as_bytes().to_vec(),
        },
        Case {
            name: "fields whose values are not text: a number, a boolean, null, a list",
            form_data: br#"{"headcount":42,"production":true,"other":null,"regions":["eu"]}"#
                .to_vec(),
        },
        Case {
            name: "a nested form document",
            form_data: br#"{"company":{"name":"Acme","site":"https://acme.example"}}"#.to_vec(),
        },
        Case {
            name: "one byte past the shortest admitted form",
            form_data: form_of_size('x', MIN_FORM_BYTES + 1),
        },
        Case {
            name: "one byte under the longest admitted form",
            form_data: form_of_size('z', MAX_FORM_BYTES - 1),
        },
    ]
}

/// A blob member as restJson1 carries it: base64 of the raw bytes.
fn blob_member(bytes: &[u8]) -> Value {
    Value::String(::aws_smithy_types::base64::encode(bytes))
}

/// The raw bytes a response's blob member carries, decoded with the same codec the wire
/// uses; panics naming the case when the member is absent or not base64.
fn blob_bytes(case: &str, reply: &Reply, member: &str) -> Vec<u8> {
    let value = reply.get(member);
    let text = value.as_str().unwrap_or_else(|| {
        panic!("case {case}: member {member} must be a base64 string, got {value} in {:?}", reply.body)
    });
    ::aws_smithy_types::base64::decode(text)
        .unwrap_or_else(|error| panic!("case {case}: {text:?} does not decode as base64: {error}"))
}

/// `POST /use-case-for-model-access` with `formData`.
async fn put(emulator: &Emulator, form_data: &[u8]) -> Reply {
    emulator
        .call(
            "POST",
            USE_CASE_URI,
            Some(json!({ "formData": blob_member(form_data) })),
        )
        .await
}

/// `GET /use-case-for-model-access`.
async fn get(emulator: &Emulator) -> Reply {
    emulator.call("GET", USE_CASE_URI, None).await
}

#[tokio::test]
async fn a_get_before_any_put_is_a_not_found() {
    let emulator = emulator();
    for attempt in 1..=2 {
        let reply = get(&emulator).await;
        assert_eq!(
            reply.status, 404,
            "case unsubmitted attempt {attempt}: GetUseCaseForModelAccess must answer 404, \
             got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ResourceNotFoundException"),
            "case unsubmitted attempt {attempt}: the refusal must be the modelled \
             ResourceNotFoundException, got {:?}",
            reply.error
        );
        assert_eq!(
            reply.get("formData"),
            Value::Null,
            "case unsubmitted attempt {attempt}: a refusal reports no form, got {:?}",
            reply.body
        );
    }
}

#[tokio::test]
async fn every_form_is_reported_back_exactly_as_it_was_put() {
    let emulator = emulator();
    for case in cases() {
        let put_reply = put(&emulator, &case.form_data).await;
        assert_eq!(
            put_reply.status, 201,
            "case {}: PutUseCaseForModelAccess must answer 201, got {} {:?}",
            case.name, put_reply.status, put_reply.body
        );

        let reply = get(&emulator).await;
        assert_eq!(
            reply.status, 200,
            "case {}: GetUseCaseForModelAccess must answer 200, got {} {:?}",
            case.name, reply.status, reply.body
        );
        let got = blob_bytes(case.name, &reply, "formData");
        assert_eq!(
            got, case.form_data,
            "case {}: the stored form must be reported byte for byte; got {} bytes, expected {}",
            case.name,
            got.len(),
            case.form_data.len()
        );

        // Reading twice changes nothing: this operation is @readonly.
        let again = get(&emulator).await;
        let got_again = blob_bytes(case.name, &again, "formData");
        assert_eq!(
            got_again, case.form_data,
            "case {}: a second read must report the same form; got {} bytes, expected {}",
            case.name,
            got_again.len(),
            case.form_data.len()
        );
    }
}

#[tokio::test]
async fn putting_the_same_form_twice_leaves_the_same_form() {
    let emulator = emulator();
    for case in cases() {
        for attempt in 1..=2 {
            let put_reply = put(&emulator, &case.form_data).await;
            assert_eq!(
                put_reply.status, 201,
                "case {} attempt {attempt}: the put must answer 201, got {} {:?}",
                case.name, put_reply.status, put_reply.body
            );
        }
        let reply = get(&emulator).await;
        let got = blob_bytes(case.name, &reply, "formData");
        assert_eq!(
            got, case.form_data,
            "case {}: a put applied twice must leave one form; got {} bytes, expected {}",
            case.name,
            got.len(),
            case.form_data.len()
        );
    }
}

/// RULE: a blob that is no form document is refused 400 `ValidationException` with the
/// service's fixed sentence and stores nothing, so the read behind it still reports whatever
/// form was accepted before it.
///
/// Every row is inside `AcknowledgementFormDataBody`'s published size bounds, so what is
/// refused is the DOCUMENT and not the length — the length is the table below.
#[tokio::test]
async fn a_blob_that_is_no_form_document_is_refused_and_changes_nothing() {
    /// The one sentence Amazon Bedrock states for form data it cannot read, written out
    /// here so this suite pins the wording a client branches on.
    const INVALID_FORM: &str = "Invalid form data.";

    let refused: Vec<(String, Vec<u8>)> = vec![
        (
            "plain text that is no document at all".to_string(),
            b"not a form at all".to_vec(),
        ),
        (
            "bytes that are not text: NUL, 0xFF and everything between".to_string(),
            (0u8..=255).collect(),
        ),
        (
            "a form of nothing but NUL bytes".to_string(),
            filler_bytes(0, 64),
        ),
        (
            "a document that is a JSON list rather than a form".to_string(),
            br#"["document summarisation"]"#.to_vec(),
        ),
        (
            "a document that is a bare JSON string".to_string(),
            br#""document summarisation""#.to_vec(),
        ),
        (
            "a document that is a bare JSON number".to_string(),
            b"1234567890".to_vec(),
        ),
        (
            "a form document carrying no field at all".to_string(),
            br#"{         }"#.to_vec(),
        ),
        (
            "a form document that was truncated in transit".to_string(),
            br#"{"useCase":"summar"#.to_vec(),
        ),
        (
            "a form document the size of the longest admitted form, truncated".to_string(),
            {
                let mut bytes = form_of_size('t', MAX_FORM_BYTES);
                bytes.truncate(MAX_FORM_BYTES - 1);
                bytes
            },
        ),
    ];

    let emulator = emulator();
    // A form that IS accepted, so each refusal is checked against a known stored form
    // rather than against an empty store.
    let stored = br#"{"useCase":"text generation"}"#.to_vec();
    let put_reply = put(&emulator, &stored).await;
    assert_eq!(
        put_reply.status, 201,
        "the baseline put must answer 201, got {} {:?}",
        put_reply.status, put_reply.body
    );

    for (case, form_data) in refused {
        assert!(
            (MIN_FORM_BYTES..=MAX_FORM_BYTES).contains(&form_data.len()),
            "case {case}: this row must sit inside the published size bounds, got {} bytes",
            form_data.len()
        );
        let reply = put(&emulator, &form_data).await;
        assert_eq!(
            reply.status, 400,
            "case {case}: the put must be refused 400, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case {case}: a blob that is no form is a ValidationException, got {:?}",
            reply.error
        );
        assert_eq!(
            reply.body.get("message").and_then(Value::as_str),
            Some(INVALID_FORM),
            "case {case}: the refusal must be the service's fixed sentence, got {:?}",
            reply.body
        );
        let after = get(&emulator).await;
        let got = blob_bytes(&case, &after, "formData");
        assert_eq!(
            got, stored,
            "case {case}: a refused put must store nothing; got {} bytes, expected {}",
            got.len(),
            stored.len()
        );
    }
}

#[tokio::test]
async fn a_blob_that_is_no_form_document_is_refused_before_any_form_is_accepted() {
    let emulator = emulator();
    let reply = put(&emulator, b"not a form at all").await;
    assert_eq!(
        (reply.status, reply.error.as_deref()),
        (400, Some("ValidationException")),
        "case \"a junk form against an empty store\": the put must be refused 400, got {} {:?}",
        reply.status,
        reply.body
    );
    // Nothing was stored, so the read behind the refusal still tells the caller to fill the
    // form out.
    let after = get(&emulator).await;
    assert_eq!(
        (after.status, after.error.as_deref()),
        (404, Some("ResourceNotFoundException")),
        "case \"a junk form against an empty store\": the read must still be a 404, got {} {:?}",
        after.status,
        after.body
    );
}

#[tokio::test]
async fn a_form_outside_the_published_size_bounds_is_refused_and_changes_nothing() {
    // Each row drives the form past a bound its Smithy model declares; the bounds
    // themselves are exercised in `cases()`.
    let refused: Vec<(String, Vec<u8>)> = vec![
        (
            format!("one byte under @length min {MIN_FORM_BYTES}"),
            filler_bytes(b'a', MIN_FORM_BYTES - 1),
        ),
        ("an empty form".to_string(), Vec::new()),
        (
            format!("one byte past @length max {MAX_FORM_BYTES}"),
            form_of_size('a', MAX_FORM_BYTES + 1),
        ),
        (
            format!("far past @length max {MAX_FORM_BYTES}"),
            form_of_size('a', MAX_FORM_BYTES * 2),
        ),
    ];

    let emulator = emulator();
    // A form that IS accepted, so each refusal is checked against a known stored form
    // rather than against an empty store.
    let stored = br#"{"useCase":"text generation"}"#.to_vec();
    let put_reply = put(&emulator, &stored).await;
    assert_eq!(
        put_reply.status, 201,
        "the baseline put must answer 201, got {} {:?}",
        put_reply.status, put_reply.body
    );

    for (case, form_data) in refused {
        let reply = put(&emulator, &form_data).await;
        assert_eq!(
            reply.status, 400,
            "case {case}: the put must be refused 400, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case {case}: a constraint violation is a ValidationException, got {:?}",
            reply.error
        );
        let after = get(&emulator).await;
        let got = blob_bytes(&case, &after, "formData");
        assert_eq!(
            got, stored,
            "case {case}: a refused put must store nothing; got {} bytes, expected {}",
            got.len(),
            stored.len()
        );
    }

    // The member is @required: a request carrying no formData is refused the same way,
    // and stores nothing either.
    let missing = emulator.call("POST", USE_CASE_URI, Some(json!({}))).await;
    assert_eq!(
        missing.status, 400,
        "case missing formData: the put must be refused 400, got {} {:?}",
        missing.status, missing.body
    );
    let after = get(&emulator).await;
    let got = blob_bytes("missing formData", &after, "formData");
    assert_eq!(
        got, stored,
        "case missing formData: a refused put must store nothing; got {} bytes, expected {}",
        got.len(),
        stored.len()
    );
}
