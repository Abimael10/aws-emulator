// SPDX-License-Identifier: CC0-1.0

//! The configuration's RESOURCE read resolves its path identifier against the store and
//! nothing else, end to end over HTTP.
//!
//! The rule under test: where a route's path identifier is a plain `string` the model places no
//! `@pattern` and no `@length` on, and the route's recorded answers report the resource ABSENT
//! for a value of any form, the route reads NO form of the value — it carries every identifier
//! through to the store, and the one the store holds no record under is the resource reported
//! absent under the operation's own not-found code, status and fixed sentence. For
//! `GetFieldLevelEncryption` (`GET /2020-05-31/field-level-encryption/{Id}`) that is
//! `NoSuchFieldLevelEncryptionConfig`, HTTP 404 and "The specified field level encryption
//! config does not exist."
//! (`smithy_query(query="operation",operation="GetFieldLevelEncryption")` reports `Id` as a
//! required `@httpLabel` targeting `string` with no constraint, `http {GET
//! /2020-05-31/field-level-encryption/{Id}, 200}` and `errors: [AccessDenied,
//! NoSuchFieldLevelEncryptionConfig]` — no argument error at all). The sentence that tells a
//! caller its own identifier handling is broken ("Id format is invalid") is a different client
//! contract, and this route never answers it.
//!
//! The rows vary what the rule generalises over — the FORM of the submitted identifier, which
//! is the one thing this route must not read:
//!
//! * the ALPHABET: the minted identifier in lower case, with its last letter lower-cased, and
//!   its namespace letter over a lower-cased body;
//! * the SEPARATORS a value carries: a hyphen, an underscore, a dot, and a value made of
//!   separators and words alone;
//! * the WIDTH of the body: one character, one short of and one past the width this service
//!   issues, and far beyond it;
//! * the NAMESPACE the value opens with: the configuration's own letter, the sibling profile's
//!   really-minted identifier, a public key's, a letter no kind of this service mints under and
//!   a leading digit;
//! * the shapes other CloudFront resources' identifiers take: a UUID, and a namespace-labelled
//!   opaque identifier;
//! * and the values a client is most likely to confuse with an identifier: the configuration's
//!   own caller reference, and a name that reads like a host.
//!
//! Every width and every namespace letter is read off an identifier this emulator really
//! minted, so nothing here is a form written out by hand.
//!
//! The CONTRAST, in the second case below: how much of the form a route reads is each route's
//! own recorded contract, so the CONFIGURATION document read of the same record
//! (`GET /2020-05-31/field-level-encryption/{Id}/config`) still refuses the values WRITTEN as an
//! identifier of this service's mint that a configuration identifier's issued form rules out —
//! another body width, another kind's namespace letter — as the identifier it cannot read, where
//! this route reports the configuration absent. The two routes keep their own answers, and a
//! blanket loosening of either fails this suite.
//!
//! The wire harness, the document builders, the create paths, the identifier-alphabet body
//! ([`crate::field_level_encryption_identifier_alphabet::body`]) and the refusal assertion
//! ([`crate::field_level_encryption_identifier_width_and_namespace::assert_refusal`]) are the
//! crate's own, reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, API_VERSION};
use crate::field_level_encryption_config_lifecycle::{
    create_config, get_config, get_config_document, simple,
};
use crate::field_level_encryption_identifier_alphabet::body;
use crate::field_level_encryption_identifier_width_and_namespace::{
    assert_refusal, in_the_identifier_alphabet,
};
use crate::field_level_encryption_profile_lifecycle::{store_profile, upload_key};

/// The sentence this route answers an `Id` the store holds no configuration under, as
/// CloudFront sends it on the wire — spelled out here rather than taken from the crate, so a
/// reworded refusal fails this suite.
const NO_SUCH_CONFIG: &str = "The specified field level encryption config does not exist.";

/// The sentence that says a caller's own identifier handling is broken, which this route never
/// answers and the configuration document read still does.
const UNREADABLE_IDENTIFIER: &str = "Id format is invalid";

/// The one not-found code the configuration's routes model
/// (`smithy_query(query="operation",operation="GetFieldLevelEncryption")`), under which both
/// sentences travel.
const NO_SUCH_CONFIG_CODE: &str = "NoSuchFieldLevelEncryptionConfig";

/// The caller reference the one stored configuration is held under — one of the values the
/// table below submits where an identifier belongs.
const CONFIG_REFERENCE: &str = "fle-resource-read-any-id";

/// The configuration this emulator really stored, with the version token its create handed
/// back.
struct Stored {
    id: String,
    etag: String,
    /// The profile the stored configuration's mapping names: an identifier of a NEIGHBOURING
    /// kind this service really minted.
    profile_id: String,
    /// A public key this service really minted: another neighbouring kind's identifier.
    public_key_id: String,
}

/// Store one profile (encrypting with a freshly uploaded public key) and one configuration
/// naming it, and upload a second public key for the table to submit.
async fn seed(router: &MultiServiceRouter) -> Stored {
    let profile_id = store_profile(router, "fle-resource-read-any-id-profile").await;
    let created = create_config(
        router,
        &simple(
            "the one configuration the account holds",
            CONFIG_REFERENCE,
            &profile_id,
        )
        .xml(),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case seeding the configuration: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );

    Stored {
        id: created.root().text_at("Id").unwrap_or_default().to_string(),
        etag: created.header("etag").to_string(),
        profile_id,
        public_key_id: upload_key(router, 1).await,
    }
}

/// Every identifier this suite submits where a configuration identifier belongs that the store
/// holds no configuration under: `(case, value)`.
///
/// Derived from the identifiers this emulator really minted — the configuration's own, the
/// profile's and a public key's — so the width and the namespace letters are the ones the
/// service issues rather than values written out here.
fn identifiers_the_store_holds_nothing_for(stored: &Stored) -> Vec<(String, String)> {
    let minted = &stored.id;
    let (namespace, minted_body) = minted.split_at(1);
    let width = minted_body.len();

    // The LAST uppercase letter of the minted identifier, lower-cased: a value differing from
    // an identifier the store really holds in the CASE of one character alone. The namespace
    // letter is uppercase, so such a character always exists.
    let mut mixed: Vec<char> = minted.chars().collect();
    if let Some(letter) = mixed.iter_mut().rev().find(|c| c.is_ascii_uppercase()) {
        *letter = letter.to_ascii_lowercase();
    }
    let mixed: String = mixed.into_iter().collect();

    let rows = vec![
        // The alphabet.
        (
            "the minted identifier in lower case".to_string(),
            minted.to_lowercase(),
        ),
        (
            "the minted identifier with its last letter lower-cased".to_string(),
            mixed,
        ),
        (
            "the namespace letter over a lower-cased body".to_string(),
            format!("{namespace}{}", minted_body.to_lowercase()),
        ),
        // The separators.
        (
            "a hyphen inside a body of the issued width".to_string(),
            format!(
                "{namespace}{}-{}",
                &body(width - 1)[..1],
                &body(width - 1)[1..]
            ),
        ),
        (
            "an underscore inside a body of the issued width".to_string(),
            format!(
                "{namespace}{}_{}",
                &body(width - 1)[..1],
                &body(width - 1)[1..]
            ),
        ),
        (
            "a dotted name, as a host is written".to_string(),
            "config.example.com".to_string(),
        ),
        ("a hyphenated word".to_string(), "not-an-identifier".to_string()),
        // The width of the body.
        (
            "the namespace letter over a one-character body".to_string(),
            format!("{namespace}{}", body(1)),
        ),
        (
            "the namespace letter over a body one character short".to_string(),
            format!("{namespace}{}", body(width - 1)),
        ),
        (
            "the namespace letter over a body one character too long".to_string(),
            format!("{namespace}{}", body(width + 1)),
        ),
        (
            "the namespace letter over a body three times the issued width".to_string(),
            format!("{namespace}{}", body(width * 3)),
        ),
        // The namespace the value opens with.
        (
            "the configuration's own namespace letter, never minted".to_string(),
            format!("{namespace}{}", body(width)),
        ),
        (
            "the sibling profile's really-minted identifier".to_string(),
            stored.profile_id.clone(),
        ),
        (
            "a public key identifier this service minted".to_string(),
            stored.public_key_id.clone(),
        ),
        (
            "a namespace letter no kind of this service mints under".to_string(),
            format!("W{}", body(width)),
        ),
        ("a leading digit".to_string(), format!("7{}", body(width))),
        // The shapes other CloudFront resources' identifiers take.
        (
            "a UUID, the shape the policy kinds' identifiers take".to_string(),
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        (
            "a namespace-labelled opaque identifier".to_string(),
            "dt_3JO1OQa51Jb3BZh7XQh6pbvF3Wa".to_string(),
        ),
        // And the value a client most easily submits in an identifier's place.
        (
            "the configuration's caller reference rather than its identifier".to_string(),
            CONFIG_REFERENCE.to_string(),
        ),
    ];

    for (case, id) in &rows {
        assert_ne!(
            id, minted,
            "case {case:?}: the submitted identifier must not be the one the store holds"
        );
    }
    rows
}

/// The rows above that the CONFIGURATION document read of the same record refuses on their
/// FORM: a value written OUTSIDE the alphabet this service writes an identifier in, which is no
/// identifier of this mint for that route to resolve at all
/// ([`in_the_identifier_alphabet`]).
fn refused_on_their_form_by_the_document_read(stored: &Stored) -> Vec<(String, String)> {
    let _ = &stored.id;
    identifiers_the_store_holds_nothing_for(stored)
        .into_iter()
        .filter(|(_, id)| !in_the_identifier_alphabet(id))
        .collect()
}

/// RULE: every identifier the store holds no configuration under is the configuration reported
/// ABSENT by this route, whatever form the value takes — never the sentence that says the
/// identifier cannot be read.
#[tokio::test]
async fn every_identifier_the_store_cannot_resolve_is_the_absent_configuration() {
    let router = emulator();
    let stored = seed(&router).await;

    for (case, id) in identifiers_the_store_holds_nothing_for(&stored) {
        let answer = get_config(&router, &id).await;
        let case = format!("{case} ({id:?})");
        let actual = answer.root().find_text("Message").unwrap_or_default();
        assert_ne!(
            actual, UNREADABLE_IDENTIFIER,
            "case {case:?}: this route reads no form of its identifier, so it must not refuse \
             one as unreadable, got {actual:?}"
        );
        assert_refusal(&answer, &case, NO_SUCH_CONFIG_CODE, NO_SUCH_CONFIG);
    }

    // The configuration the account really holds is still served at its own identifier, at the
    // version its create handed back: every sentence above answered an identifier rather than a
    // store this suite broke.
    let read = get_config(&router, &stored.id).await;
    assert_eq!(
        read.status, 200,
        "case the one stored configuration: expected it to survive every refusal, got {} with {}",
        read.status, read.raw
    );
    assert_eq!(
        read.header("etag"),
        stored.etag,
        "case the one stored configuration: expected the version its create handed back ({:?}), \
         got {:?}",
        stored.etag,
        read.header("etag")
    );
}

/// The CONTRAST: the CONFIGURATION document read of the same record keeps its own recorded
/// contract, reporting every one of those values that is written OUTSIDE the alphabet this
/// service writes an identifier in as the identifier it cannot READ — so the rule above is
/// about this one route rather than a refusal dropped from the whole surface.
#[tokio::test]
async fn the_configuration_document_read_keeps_its_own_identifier_contract() {
    let router = emulator();
    let stored = seed(&router).await;

    let rows = refused_on_their_form_by_the_document_read(&stored);
    assert!(
        !rows.is_empty(),
        "case the contrast rows: expected the table to carry values the document read refuses \
         on their form, got none"
    );
    for (case, id) in rows {
        let case = format!("{case} ({id:?})");
        assert_refusal(
            &get_config_document(&router, &id).await,
            &case,
            NO_SUCH_CONFIG_CODE,
            UNREADABLE_IDENTIFIER,
        );
        // And the resource read of the same identifier answers the absent configuration, so the
        // two routes differ on the very same value.
        assert_refusal(
            &get_config(&router, &id).await,
            &case,
            NO_SUCH_CONFIG_CODE,
            NO_SUCH_CONFIG,
        );
    }

    assert_eq!(
        get_config_document(&router, &stored.id).await.status,
        200,
        "case the one stored configuration: expected its document to survive every refusal"
    );
}

/// The identifier this service really MINTED is served while the record stands, and reports the
/// configuration ABSENT once it is deleted — the condition the sentence above describes, reached
/// through the record's own lifecycle rather than through a generated identifier.
#[tokio::test]
async fn the_minted_identifier_is_served_and_then_reported_absent() {
    let router = emulator();
    let stored = seed(&router).await;

    let served = get_config(&router, &stored.id).await;
    assert_eq!(
        served.status, 200,
        "case the minted identifier: expected HTTP 200 while the record stands, got {} with {}",
        served.status, served.raw
    );

    let deleted = call(
        &router,
        "DELETE",
        &format!("/{API_VERSION}/field-level-encryption/{}", stored.id),
        None,
        Some(&stored.etag),
    )
    .await;
    assert_eq!(
        deleted.status, 204,
        "case deleting the stored configuration: expected HTTP 204, got {} with {}",
        deleted.status, deleted.raw
    );

    assert_refusal(
        &get_config(&router, &stored.id).await,
        "the minted identifier after its record is deleted",
        NO_SUCH_CONFIG_CODE,
        NO_SUCH_CONFIG,
    );
}
