// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the wire identity of the refusal
//! `TranslateDocument` answers when the terminology the request named is not a
//! resource this account holds.
//!
//! The rule: a synchronous translation that names a terminology this account
//! holds nothing under is a CLIENT-ERROR refusal — HTTP 400 carrying the
//! modelled `ResourceNotFoundException` shape and this service's not-found
//! sentence for the name the request asked for — never the 404 that shape's
//! `@httpError` declares (`smithy_query query=shape
//! shape=ResourceNotFoundException`). It holds for the request as a whole, so it
//! does not depend on the form the document arrived in, on which side of the
//! pair carries the pivot language, on whether the source language was named or
//! detected, or on why the terminology is absent.
//!
//! The suite is a TABLE whose rows vary exactly what the rule generalises over:
//! every content type a document may be in (`smithy_query shape=Document
//! member=ContentType`), both directions of the pivot language, a named and a
//! detected source language, and a terminology that was never imported against
//! one that was imported and deleted. A control test replays every row against
//! the terminology that IS stored, so each row's 400 is the not-found refusal
//! and not some other rule of this operation.
//!
//! The harness is the one `terminology_lifecycle` builds (`router`, `call`,
//! `error_code`, `message`, `import`) and the document helpers are
//! `document_translation`'s, so these rows travel the same wire path as the rest
//! of the suite.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::document_translation::{
    document_body, word_document, CONTENT_TYPE_HTML, CONTENT_TYPE_PLAIN, CONTENT_TYPE_WORD,
    ENGLISH_TEXT, FRENCH_TEXT,
};
use crate::terminology_lifecycle::{
    call, error_code, import, message, router, ACCOUNT, EN_FR_CSV,
};

/// The HTTP status Amazon Translate answers a resource-not-found refusal with:
/// the status of a client error, as its API reference publishes for that class.
const NOT_FOUND_STATUS: u16 = 400;

/// The status the modelled shape's own `@httpError` declares, which this service
/// does not answer — asserted against, so a regression back to it is named.
const MODELLED_STATUS: u16 = 404;

/// The `__type` the refusal carries, which is the modelled shape's name.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The resource word this service's not-found sentence names for the record a
/// translation resolves.
const RESOURCE_WORD: &str = "Terminology";

/// A terminology that IS imported and never deleted: the control rows resolve
/// it, and its presence keeps the table's rows refused because of the name they
/// asked for rather than because the store is empty.
const PRESENT: &str = "doc-status-terms";

/// A terminology imported and then deleted.
const DELETED: &str = "doc-status-deleted";

/// A terminology name no import ever stored.
const ABSENT: &str = "doc-status-absent";

/// The code a request uses to ask the service to determine the source language
/// from the document itself.
const AUTO: &str = "auto";

/// A paragraph of Spanish, carried by the rows whose named source language is
/// Spanish.
const SPANISH_TEXT: &str = "El rápido zorro marrón salta sobre el perro perezoso mientras el \
                            jardinero riega las rosas por la mañana.";

/// A paragraph of German, carried by the rows whose named source language is
/// German.
const GERMAN_TEXT: &str = "Der schnelle braune Fuchs springt über den faulen Hund, während der \
                           Gärtner am frühen Morgen die Rosen gießt.";

/// An HTML document whose single paragraph carries `text`.
fn html_document(text: &str) -> Vec<u8> {
    format!("<html><body><p>{text}</p></body></html>").into_bytes()
}

/// One row of the table: a document translation that names a terminology which
/// is not there.
struct Row {
    case: &'static str,
    content: Vec<u8>,
    content_type: &'static str,
    source: &'static str,
    target: &'static str,
    /// The terminology the request names, which this account holds nothing
    /// under.
    absent: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "unformatted text, English into French, a name no import stored",
            content: ENGLISH_TEXT.as_bytes().to_vec(),
            content_type: CONTENT_TYPE_PLAIN,
            source: "en",
            target: "fr",
            absent: ABSENT,
        },
        Row {
            case: "unformatted text, French into English, a terminology that was deleted",
            content: FRENCH_TEXT.as_bytes().to_vec(),
            content_type: CONTENT_TYPE_PLAIN,
            source: "fr",
            target: "en",
            absent: DELETED,
        },
        Row {
            case: "HTML, English into Spanish, a name no import stored",
            content: html_document(ENGLISH_TEXT),
            content_type: CONTENT_TYPE_HTML,
            source: "en",
            target: "es",
            absent: ABSENT,
        },
        Row {
            case: "HTML, Spanish into English, a terminology that was deleted",
            content: html_document(SPANISH_TEXT),
            content_type: CONTENT_TYPE_HTML,
            source: "es",
            target: "en",
            absent: DELETED,
        },
        Row {
            case: "a Word package, English into German, a name no import stored",
            content: word_document(&[ENGLISH_TEXT]),
            content_type: CONTENT_TYPE_WORD,
            source: "en",
            target: "de",
            absent: ABSENT,
        },
        Row {
            case: "a Word package, German into English, a terminology that was deleted",
            content: word_document(&[GERMAN_TEXT]),
            content_type: CONTENT_TYPE_WORD,
            source: "de",
            target: "en",
            absent: DELETED,
        },
        Row {
            case: "unformatted text, a detected source language, a name no import stored",
            content: ENGLISH_TEXT.as_bytes().to_vec(),
            content_type: CONTENT_TYPE_PLAIN,
            source: AUTO,
            target: "fr",
            absent: ABSENT,
        },
        Row {
            case: "HTML, a detected source language, a terminology that was deleted",
            content: html_document(FRENCH_TEXT),
            content_type: CONTENT_TYPE_HTML,
            source: AUTO,
            target: "en",
            absent: DELETED,
        },
    ]
}

/// Import [`PRESENT`] and [`DELETED`], then delete the second: the store holds
/// one resolvable terminology and one that is gone.
async fn stored_terminologies(router: &MultiServiceRouter) {
    for name in [PRESENT, DELETED] {
        let (status, body) = import(router, name, EN_FR_CSV, "CSV", Some("UNI")).await;
        assert_eq!(
            status, 200,
            "case {name:?}: ImportTerminology answered {status} {body}"
        );
    }
    let (status, body) = call(router, "DeleteTerminology", &json!({"Name": DELETED})).await;
    assert_eq!(
        status, 200,
        "case {DELETED:?}: DeleteTerminology answered {status} {body}"
    );
}

/// The message such a refusal states: this service's not-found sentence, whose
/// variable parts are the account the request was signed for and the name the
/// request asked for.
fn expected_message(name: &str) -> String {
    format!("The {RESOURCE_WORD} for {ACCOUNT} {name} was not found")
}

/// A document translation naming a terminology that is not there is refused as a
/// client error, whatever form the document is in, whichever direction the pair
/// runs, whether the source language was named or detected, and whether the
/// terminology never existed or was deleted.
#[tokio::test]
async fn a_document_translation_naming_an_absent_terminology_is_refused_as_a_client_error() {
    let router = router();
    stored_terminologies(&router).await;

    for row in rows() {
        let body = document_body(
            &row.content,
            row.content_type,
            row.source,
            row.target,
            &[row.absent],
        );
        let (status, answered) = call(&router, "TranslateDocument", &body).await;
        assert_ne!(
            status, MODELLED_STATUS,
            "case {:?}: the refusal must not carry the modelled status, got {status} {answered}",
            row.case
        );
        assert_eq!(
            status, NOT_FOUND_STATUS,
            "case {:?}: expected HTTP {NOT_FOUND_STATUS}, got {status} {answered}",
            row.case
        );
        assert_eq!(
            error_code(&answered),
            NOT_FOUND_CODE,
            "case {:?}: expected {NOT_FOUND_CODE}, got {answered}",
            row.case
        );
        assert_eq!(
            message(&answered),
            expected_message(row.absent),
            "case {:?}: the refusal names the terminology asked for, got {answered}",
            row.case
        );
    }
}

/// The control for the table above: every row, with the terminology that IS
/// stored, is translated — so each row's refusal is about the resource it named
/// and not about the document it carried or the pair it asked for.
#[tokio::test]
async fn the_same_document_translations_resolve_a_terminology_that_is_there() {
    let router = router();
    stored_terminologies(&router).await;

    for row in rows() {
        let body = document_body(
            &row.content,
            row.content_type,
            row.source,
            row.target,
            &[PRESENT],
        );
        let (status, answered) = call(&router, "TranslateDocument", &body).await;
        assert_eq!(
            status, 200,
            "case {:?}: the terminology is there, got {status} {answered}",
            row.case
        );
        assert_eq!(
            error_code(&answered),
            "<no __type>",
            "case {:?}: a served translation names no error, got {answered}",
            row.case
        );
        let target = answered
            .get("TargetLanguageCode")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            target, row.target,
            "case {:?}: the answer reports the target language asked for, got {answered}",
            row.case
        );
    }
}
