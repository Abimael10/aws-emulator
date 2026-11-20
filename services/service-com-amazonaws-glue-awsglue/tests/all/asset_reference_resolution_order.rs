// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ORDER in which a write onto a catalog asset resolves what its
//! request names: the asset the write is addressed to — the resource it targets — before the
//! references the request carries beside it (`PutAttachment`'s `FormTypeId`,
//! `AssociateGlossaryTerms`' `GlossaryTermIdentifiers`).
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: an operation addressed to an asset resolves that asset, and the item
//! inside it the request addresses, before any other reference the request names, so a
//! request whose asset is absent is refused naming the ASSET however many of its other
//! references are absent too — while a request whose asset resolves is still refused on the
//! reference that does not.
//!
//! The suites are tables whose rows differ along what the rule generalises over: which
//! operation is addressed to the asset, how the asset identifier misses (never stored, a
//! prefix of a stored one, recased, one at the modelled length bound), how many of the
//! request's other references are absent, and whether the request also addresses an item
//! inside an iterable form.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok};

/// The longest `AssetIdentifier` the model accepts: "len 1..1087" on `AssetId`
/// (`smithy_query operation=PutAttachment`), whose pattern `^[a-zA-Z0-9\-\:\/\.\_\*]+$` is
/// what the rows below keep to.
const MAX_ASSET_ID_LENGTH: usize = 1087;

/// The identifier a response reports under `Id`.
fn id_of(case: &str, body: &Value) -> String {
    body.get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: the response must carry an Id, got {body}"))
        .to_string()
}

/// Register a form type and answer the identifier the catalog assigned it.
async fn form_type(router: &MultiServiceRouter, name: &str) -> String {
    let stored = ok(
        router,
        "PutFormType",
        &json!({ "Name": name, "Schema": format!("structure {name} {{ note: String }}") }),
    )
    .await;
    id_of(name, &stored)
}

/// Register an asset type over one form type and answer the identifier the put reported.
async fn asset_type(router: &MultiServiceRouter, name: &str, form_type_id: &str) -> String {
    let stored = ok(
        router,
        "PutAssetType",
        &json!({
            "Name": name,
            "Forms": { "Details": { "FormTypeIdentifier": form_type_id } },
        }),
    )
    .await;
    id_of(name, &stored)
}

/// Store an asset under `identifier`, an instance of `asset_type_id`.
async fn put_asset(router: &MultiServiceRouter, identifier: &str, asset_type_id: &str) {
    ok(
        router,
        "PutAsset",
        &json!({
            "Identifier": identifier,
            "AssetTypeId": asset_type_id,
            "Name": "Orders Table",
            "Forms": {},
        }),
    )
    .await;
}

/// Create a business glossary and answer the identifier it minted.
async fn glossary(router: &MultiServiceRouter, name: &str) -> String {
    let created = ok(router, "CreateGlossary", &json!({ "Name": name })).await;
    id_of(name, &created)
}

/// Create a glossary term inside `glossary_id` and answer the identifier it minted.
async fn glossary_term(router: &MultiServiceRouter, glossary_id: &str, name: &str) -> String {
    let created = ok(
        router,
        "CreateGlossaryTerm",
        &json!({ "GlossaryIdentifier": glossary_id, "Name": name }),
    )
    .await;
    id_of(name, &created)
}

/// The `Message` this surface refuses an identifier it holds no asset under with, pinned
/// exactly: the refusal text is itself a JSON document carrying the sentence under a
/// `Message` key, with the identifier from the request as its one variable part.
fn expected_asset_miss(identifier: &str) -> String {
    format!(r#"{{"Message":"Asset not found: {identifier}"}}"#)
}

/// The `Message` this surface refuses an identifier it holds no form type under with, in the
/// same document form.
fn expected_form_type_miss(identifier: &str) -> String {
    format!(r#"{{"Message":"Metadata form type {identifier} not found."}}"#)
}

/// The `Message` an association refuses a term id it holds no live term under with, in the
/// same document form.
fn expected_term_miss(identifier: &str) -> String {
    format!(r#"{{"Message":"Glossary term not found: {identifier}"}}"#)
}

/// The asset identifiers that resolve to nothing, each with the case that names how it
/// misses — one table, shared by both suites below, so the two operations are driven over the
/// same class of absent target.
fn absent_asset_identifiers() -> Vec<(&'static str, String)> {
    vec![
        ("never stored", "no-such-asset".to_string()),
        ("a prefix of a stored identifier", "stored-asse".to_string()),
        (
            "a stored identifier with one more character",
            "stored-asset0".to_string(),
        ),
        ("a stored identifier recased", "Stored-Asset".to_string()),
        (
            "an identifier at the modelled length bound",
            "z".repeat(MAX_ASSET_ID_LENGTH),
        ),
    ]
}

/// Assert that `operation` refused `request` with HTTP 400 `EntityNotFoundException` worded
/// `expected`.
async fn assert_entity_miss(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    expected: &str,
) {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case}: expected 400 from {operation}, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("EntityNotFoundException"),
        "case {case}: expected EntityNotFoundException, got {body}"
    );
    assert_eq!(
        error_message(&body),
        expected,
        "case {case}: expected Message {expected:?}, got {body}"
    );
}

// ---------------------------------------------------------------------------
// PutAttachment resolves the asset before the form type
// ---------------------------------------------------------------------------

/// A `PutAttachment` whose `AssetIdentifier` resolves to nothing is refused naming the ASSET
/// — whether or not the `FormTypeId` beside it resolves, and whether or not the request also
/// addresses an item inside an iterable form — while a write whose asset resolves is refused
/// on the form type that does not.
#[tokio::test]
async fn an_attachment_write_resolves_the_asset_before_the_form_type() {
    let router = emulator();
    let held_form_type = form_type(&router, "DetailsForm").await;
    let asset_type_id = asset_type(&router, "OrderAssetType", &held_form_type).await;
    put_asset(&router, "stored-asset", &asset_type_id).await;

    // (how the form type beside the absent asset is named, the value to send)
    let form_type_rows: Vec<(&str, String)> = vec![
        (
            "with a form type that was never registered",
            "NeverRegisteredFormType".to_string(),
        ),
        ("with a form type this account holds", held_form_type.clone()),
    ];

    for (case, asset_identifier) in absent_asset_identifiers() {
        for (form_type_case, form_type_id) in &form_type_rows {
            let case = format!("{case}, {form_type_case}");
            let request = json!({
                "AssetIdentifier": &asset_identifier,
                "AttachmentName": "audit",
                "FormTypeId": form_type_id,
                "Content": "{}",
            });
            assert_entity_miss(
                &router,
                &case,
                "PutAttachment",
                &request,
                &expected_asset_miss(&asset_identifier),
            )
            .await;

            // The same write, addressed at an ITEM inside an iterable form: the item is part
            // of what the request addresses, so the absent asset is still what is reported.
            let mut with_item = request.clone();
            let members = with_item
                .as_object_mut()
                .expect("the request is a JSON object");
            members.insert("IterableFormName".to_string(), json!("Lineitems"));
            members.insert("ItemIdentifier".to_string(), json!("line-1"));
            assert_entity_miss(
                &router,
                &format!("{case}, addressed at an item"),
                "PutAttachment",
                &with_item,
                &expected_asset_miss(&asset_identifier),
            )
            .await;
        }
    }

    // An asset that RESOLVES leaves the form type the thing that does not: the reference is
    // still resolved, and still reported, once the target is found.
    assert_entity_miss(
        &router,
        "a stored asset with a form type that was never registered",
        "PutAttachment",
        &json!({
            "AssetIdentifier": "stored-asset",
            "AttachmentName": "audit",
            "FormTypeId": "NeverRegisteredFormType",
            "Content": "{}",
        }),
        &expected_form_type_miss("NeverRegisteredFormType"),
    )
    .await;

    // Both resolve: the write is served, so the refusals above are orderings of real misses
    // and not a broken lookup.
    let stored = ok(
        &router,
        "PutAttachment",
        &json!({
            "AssetIdentifier": "stored-asset",
            "AttachmentName": "audit",
            "FormTypeId": &held_form_type,
            "Content": "{\"note\":\"ok\"}",
        }),
    )
    .await;
    assert_eq!(
        stored.get("AttachmentName").and_then(Value::as_str),
        Some("audit"),
        "case both resolve: the write must report the attachment it stored, got {stored}"
    );

    // Read the asset back: the write that was served is the only one that landed.
    let read = ok(
        &router,
        "GetAsset",
        &json!({ "Identifier": "stored-asset" }),
    )
    .await;
    let attachments = read
        .get("Attachments")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("the read must report Attachments, got {read}"));
    assert_eq!(
        attachments.keys().cloned().collect::<Vec<String>>(),
        vec!["audit".to_string()],
        "case both resolve: only the served write may have landed, got {read}"
    );
}

// ---------------------------------------------------------------------------
// AssociateGlossaryTerms resolves the asset before the terms
// ---------------------------------------------------------------------------

/// An `AssociateGlossaryTerms` whose `AssetIdentifier` resolves to nothing is refused naming
/// the ASSET — however many of the term ids beside it resolve to nothing, and whether or not
/// the request also addresses an item inside an iterable form — while an association whose
/// asset resolves is refused on the term that does not.
#[tokio::test]
async fn an_association_resolves_the_asset_before_the_terms() {
    let router = emulator();
    let held_form_type = form_type(&router, "TermForm").await;
    let asset_type_id = asset_type(&router, "TermAssetType", &held_form_type).await;
    put_asset(&router, "stored-asset", &asset_type_id).await;
    let glossary_id = glossary(&router, "OrderGlossary").await;
    let held_term = glossary_term(&router, &glossary_id, "Retention").await;

    // (how the terms beside the absent asset are named, the ids to send)
    let term_rows: Vec<(&str, Vec<String>)> = vec![
        (
            "with one term that was never minted",
            vec!["term-never-minted".to_string()],
        ),
        (
            "with several terms that were never minted",
            vec![
                "term-never-minted".to_string(),
                "another-missing-term".to_string(),
            ],
        ),
        (
            "with a term this account holds",
            vec![held_term.clone()],
        ),
        (
            "with a held term beside one that was never minted",
            vec![held_term.clone(), "term-never-minted".to_string()],
        ),
    ];

    for (case, asset_identifier) in absent_asset_identifiers() {
        for (term_case, terms) in &term_rows {
            let case = format!("{case}, {term_case}");
            let request = json!({
                "AssetIdentifier": &asset_identifier,
                "GlossaryTermIdentifiers": terms,
            });
            assert_entity_miss(
                &router,
                &case,
                "AssociateGlossaryTerms",
                &request,
                &expected_asset_miss(&asset_identifier),
            )
            .await;

            let mut with_item = request.clone();
            let members = with_item
                .as_object_mut()
                .expect("the request is a JSON object");
            members.insert("IterableFormName".to_string(), json!("Lineitems"));
            members.insert("ItemIdentifier".to_string(), json!("line-1"));
            assert_entity_miss(
                &router,
                &format!("{case}, addressed at an item"),
                "AssociateGlossaryTerms",
                &with_item,
                &expected_asset_miss(&asset_identifier),
            )
            .await;
        }
    }

    // An asset that RESOLVES leaves the term the thing that does not, refused with the client
    // error that reading names an unresolvable reference with.
    let (status, body) = call(
        &router,
        "AssociateGlossaryTerms",
        &json!({
            "AssetIdentifier": "stored-asset",
            "GlossaryTermIdentifiers": ["term-never-minted"],
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a stored asset with a term that was never minted: expected 400, got {status} with \
         {body}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case a stored asset with a term that was never minted: expected InvalidInputException, \
         got {body}"
    );
    assert_eq!(
        error_message(&body),
        expected_term_miss("term-never-minted"),
        "case a stored asset with a term that was never minted: expected the term's miss, got \
         {body}"
    );

    // Both resolve: the association is served, so the refusals above are orderings of real
    // misses and not a broken lookup.
    let associated = ok(
        &router,
        "AssociateGlossaryTerms",
        &json!({
            "AssetIdentifier": "stored-asset",
            "GlossaryTermIdentifiers": [&held_term],
        }),
    )
    .await;
    let reported: Vec<String> = associated
        .get("GlossaryTerms")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the association must report GlossaryTerms, got {associated}"))
        .iter()
        .map(|term| {
            term.as_str()
                .unwrap_or_else(|| panic!("GlossaryTerms holds strings, got {term}"))
                .to_string()
        })
        .collect();
    assert_eq!(
        reported,
        vec![held_term],
        "case both resolve: the association must report the term it merged, got {associated}"
    );
}
