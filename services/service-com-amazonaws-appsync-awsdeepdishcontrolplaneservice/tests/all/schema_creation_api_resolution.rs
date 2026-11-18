// SPDX-License-Identifier: CC0-1.0

//! How `StartSchemaCreation` classifies the `apiId` it is addressed at, over the whole
//! class of values that resolve to no API.
//!
//! One rule, asserted over a table rather than over the one request that exposed the
//! defect: the FORM of the reference decides which refusal class the call takes.
//!
//! * a value of the form an id the service ISSUES takes — 26 characters
//!   (`state::API_ID_LEN`) from the base-32 alphabet those ids are written in — is a
//!   reference the service could have handed out, so the request is well formed and an
//!   API this account does not hold (one deleted earlier in the session included) is the
//!   missing resource it is: 404 `NotFoundException` with AppSync's fixed sentence
//!   (`state::API_NOT_FOUND_MESSAGE`), the same text every operation resolving an `apiId`
//!   states an absent API with;
//! * a value no issued id could be — too long, too short, or carrying a character those
//!   ids are never written in — names no API at all, so the request itself is malformed:
//!   400 `BadRequestException` whose message echoes the rejected value
//!   ("Invalid API ID `<apiId>`").
//!
//! The rows differ along what the rule reads: the length of the value, the alphabet its
//! characters come from, the form it takes altogether, and — for the not-found class —
//! whether the API was never there or was deleted after holding a schema. The harness is
//! the one the other schema suites carry (the router the production emulator mounts,
//! `start_creation`, `creation_status`), reused rather than copied.

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, create_api, router};
use super::schema_creation::{await_creation, creation_status, start_creation};

/// The document every call in this suite carries: the smallest definition that defines a
/// type system, so what differs between the rows is only the `apiId`.
const DOCUMENT: &[u8] = b"type Query {\n  ping: String\n}\nschema { query: Query }\n";

/// The alphabet the ids AppSync issues are written in: RFC 4648 base-32, lowercased.
///
/// Source: the ids this service mints (`state::is_opaque_id`, which recognises exactly the
/// form the minting side writes), cross-checked below against an id the service really
/// minted, so the catalog cannot drift from what is issued.
const ISSUED_ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz234567";

/// One row of the table: a value an `apiId` can carry that resolves to no API.
#[derive(Debug)]
struct Row {
    label: &'static str,
    api_id: String,
    /// Whether the value has the form of an id the service issues.
    issued_form: bool,
}

/// The values the rule is asserted over.
///
/// Spans both sides of every ground the form test reads: the issued length and one
/// character either side of it, the issued alphabet and characters from outside it
/// (uppercase letters, the decimal digits base-32 leaves out, punctuation), and values of
/// another form altogether.
fn rows() -> Vec<Row> {
    let issued: Vec<char> = ISSUED_ALPHABET.chars().collect();
    // An id of the issued length drawn across the whole alphabet, so the not-found class is
    // asserted on a value the issue did not cite.
    let spread: String = (0..state::API_ID_LEN)
        .map(|index| issued[(index * 7) % issued.len()])
        .collect();

    vec![
        Row {
            label: "an id of the issued form this account does not hold",
            api_id: "abcdefghijklmnopqrstuvwxyz".to_string(),
            issued_form: true,
        },
        Row {
            label: "an id spread across the whole issued alphabet",
            api_id: spread,
            issued_form: true,
        },
        Row {
            label: "an id of the issued length written only in its digits",
            api_id: "23456723456723456723456723".to_string(),
            issued_form: true,
        },
        Row {
            label: "one character past the issued length",
            api_id: "a".repeat(state::API_ID_LEN + 1),
            issued_form: false,
        },
        Row {
            label: "one character short of the issued length",
            api_id: "a".repeat(state::API_ID_LEN - 1),
            issued_form: false,
        },
        Row {
            label: "the issued length in uppercase",
            api_id: "ABCDEFGHIJKLMNOPQRSTUVWXYZ".to_string(),
            issued_form: false,
        },
        Row {
            label: "the issued length carrying a digit base-32 leaves out",
            api_id: "abcdefghijklmnopqrstuvwxy0".to_string(),
            issued_form: false,
        },
        Row {
            label: "the issued length carrying punctuation",
            api_id: "abcdefghijklmnopqrstuvwx-z".to_string(),
            issued_form: false,
        },
        Row {
            label: "a value far shorter than an id",
            api_id: "zzz".to_string(),
            issued_form: false,
        },
        Row {
            label: "a value of another form altogether",
            api_id: "no.such.api".to_string(),
            issued_form: false,
        },
        Row {
            label: "a word-and-digits token of neither length nor alphabet",
            api_id: "nonexistent0000000000".to_string(),
            issued_form: false,
        },
    ]
}

/// What the refusal of `api_id` must be: the status, the error shape and the message.
fn expected(api_id: &str, issued_form: bool) -> (u16, &'static str, String) {
    if issued_form {
        (
            404,
            "NotFoundException",
            state::API_NOT_FOUND_MESSAGE.to_string(),
        )
    } else {
        (
            400,
            "BadRequestException",
            format!("Invalid API ID {api_id}"),
        )
    }
}

/// Assert `StartSchemaCreation` refuses `api_id` the way its form says it must.
async fn assert_refused(router: &MultiServiceRouter, case: &str, api_id: &str, issued_form: bool) {
    let refused = start_creation(router, api_id, DOCUMENT).await;
    let expected = expected(api_id, issued_form);
    assert_eq!(
        (
            refused.status,
            refused.error_type.as_str(),
            refused.body["message"].as_str().unwrap_or_default()
        ),
        (expected.0, expected.1, expected.2.as_str()),
        "case {case:?} ({api_id}): expected {expected:?}, got {} {} {:?}",
        refused.status,
        refused.error_type,
        refused.body
    );
}

#[tokio::test]
async fn an_api_id_that_resolves_to_no_api_is_classified_by_its_form() {
    let router = router();

    for row in rows() {
        assert_refused(&router, row.label, &row.api_id, row.issued_form).await;
    }

    // The refusal is about the reference resolving to no API, nothing else: the same
    // document on an API this account does hold is accepted.
    let api_id = create_api(&router, "resolvable-for-schema-creation").await;
    let accepted = start_creation(&router, &api_id, DOCUMENT).await;
    assert_eq!(
        (accepted.status, accepted.body["status"].as_str()),
        (200, Some(state::SCHEMA_PROCESSING)),
        "case an API this account holds ({api_id}): expected 200 {:?}, got {} {:?}",
        state::SCHEMA_PROCESSING,
        accepted.status,
        accepted.body
    );
}

#[tokio::test]
async fn a_minted_api_id_has_the_form_the_not_found_class_is_asserted_on() {
    let router = router();
    let api_id = create_api(&router, "minted-id-form").await;

    assert_eq!(
        api_id.chars().count(),
        state::API_ID_LEN,
        "case a minted apiId: expected {} characters, got {api_id:?}",
        state::API_ID_LEN
    );
    assert!(
        api_id.chars().all(|letter| ISSUED_ALPHABET.contains(letter)),
        "case a minted apiId: expected every character from {ISSUED_ALPHABET:?}, got {api_id:?}"
    );
}

#[tokio::test]
async fn deleting_an_api_turns_schema_submission_into_a_not_found() {
    let router = router();

    // An API that really held a schema, so the id under test is one the service itself
    // issued and the account held — the ground the refusal class turns on is the
    // DELETION, not the shape of the value.
    let api_id = create_api(&router, "deleted-before-schema-creation").await;
    let started = start_creation(&router, &api_id, DOCUMENT).await;
    assert_eq!(
        started.status, 200,
        "case before the delete ({api_id}): expected StartSchemaCreation 200, got {} {:?}",
        started.status, started.body
    );
    await_creation(&router, &api_id, "before the delete").await;

    let deleted = call(&router, "DELETE", &format!("/v1/apis/{api_id}"), None).await;
    assert_eq!(
        deleted.status, 200,
        "case the delete ({api_id}): expected DeleteGraphqlApi 200, got {} {:?}",
        deleted.status, deleted.body
    );

    // The id is still one the service could have issued, so the follow-up call is a well
    // formed request about a resource that is gone.
    assert_refused(&router, "after the delete", &api_id, true).await;

    // The reporting call of the same plane says the same about the deleted API.
    let read = creation_status(&router, &api_id).await;
    assert_eq!(
        (
            read.status,
            read.error_type.as_str(),
            read.body["message"].as_str().unwrap_or_default()
        ),
        (
            404,
            "NotFoundException",
            state::API_NOT_FOUND_MESSAGE as &str
        ),
        "case after the delete ({api_id}) GetSchemaCreationStatus: expected 404 \
         NotFoundException {:?}, got {} {} {:?}",
        state::API_NOT_FOUND_MESSAGE,
        read.status,
        read.error_type,
        read.body
    );

    // Deleting one API leaves the plane working for another: the same document is accepted
    // on an API created afterwards.
    let replacement = create_api(&router, "after-the-delete").await;
    let accepted = start_creation(&router, &replacement, DOCUMENT).await;
    assert_eq!(
        (accepted.status, accepted.body["status"].as_str()),
        (200, Some(state::SCHEMA_PROCESSING)),
        "case an API created after the delete ({replacement}): expected 200 {:?}, got {} {:?}",
        state::SCHEMA_PROCESSING,
        accepted.status,
        accepted.body
    );
}
