// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT a read of a Glue asset projects, whichever write left the
//! asset in the state being read.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: a read of an asset reports every member the asset record carries —
//! its identity, the asset type it is an instance of, its forms as both the keyed map and
//! the list beside it, its attachments as both of theirs, its iterable forms and its
//! glossary terms — projected out of the stored record, with a collection the asset holds
//! nothing in serialised PRESENT-AND-EMPTY rather than left out ("The forms on the asset,
//! keyed by form name" / "Additional attachments on the asset for more context, keyed by
//! attachment name" / "The iterable forms available on the asset, keyed by form name",
//! `smithy_query operation=GetAsset`). The projection is a function of the record alone, so
//! it does not depend on WHICH write put the asset in that state, and reading an asset
//! changes nothing a later caller sees.
//!
//! The suites are tables whose rows differ along what the rule generalises over: the write
//! path the state was reached through (a put, a put replaced by a second put, an attachment
//! write, an attachment write undone by its delete, and a glossary-term association), how
//! many forms the asset carries and whether a form carries content, whether the asset type
//! is addressed by name or by the identifier its registration reported, and how much of the
//! asset's envelope is non-empty.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use service_com_amazonaws_glue_awsglue::state::CATALOG_RESOURCE_FORM_NAME;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{assert_catalog_form, emulator, ok};

/// The name every put below stores the asset under, and the description beside it.
const PUT_NAME: &str = "Orders Table";
const PUT_DESCRIPTION: &str = "Raw order records";

/// The one attachment the attachment paths write, and the document it carries. The name
/// keeps to `AttachmentName`'s pattern `^[a-zA-Z][a-zA-Z0-9_]*$`
/// (`smithy_query shape=AttachmentName`).
const ATTACHMENT_NAME: &str = "sensitivity";
const ATTACHMENT_CONTENT: &str = r#"{"sensitivity":"HIGH"}"#;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The `Id` member of a response, which must be a string.
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
        &json!({ "Name": name, "Schema": format!("structure {name} {{ a: String }}") }),
    )
    .await;
    id_of(name, &stored)
}

/// Register an asset type over the form types `forms` names and answer the identifier the
/// registration reported.
async fn asset_type(router: &MultiServiceRouter, name: &str, forms: &[(&str, &str)]) -> String {
    let mut map = Map::new();
    for (form_name, form_type_id) in forms {
        map.insert(
            (*form_name).to_string(),
            json!({ "FormTypeIdentifier": form_type_id }),
        );
    }
    let stored = ok(
        router,
        "PutAssetType",
        &json!({ "Name": name, "Forms": Value::Object(map) }),
    )
    .await;
    id_of(name, &stored)
}

/// Store an asset under `identifier` as an instance of the asset type `asset_type_address`
/// names, carrying `forms`.
async fn put_asset(
    router: &MultiServiceRouter,
    identifier: &str,
    asset_type_address: &str,
    forms: &Value,
) {
    ok(
        router,
        "PutAsset",
        &json!({
            "Identifier": identifier,
            "AssetTypeId": asset_type_address,
            "Name": PUT_NAME,
            "Description": PUT_DESCRIPTION,
            "Forms": forms,
        }),
    )
    .await;
}

/// The `GetAsset` response for the asset `identifier` names.
async fn get_asset(router: &MultiServiceRouter, identifier: &str) -> Value {
    ok(router, "GetAsset", &json!({ "Identifier": identifier })).await
}

/// Create a business glossary with one term in it and answer the term's identifier.
async fn glossary_term(router: &MultiServiceRouter, glossary: &str, term: &str) -> String {
    let created = ok(router, "CreateGlossary", &json!({ "Name": glossary })).await;
    let glossary_id = id_of(glossary, &created);
    let created = ok(
        router,
        "CreateGlossaryTerm",
        &json!({ "GlossaryIdentifier": glossary_id, "Name": term }),
    )
    .await;
    id_of(term, &created)
}

/// The write path an asset's state was reached through: each one is an entry point that
/// leaves a record for a read to project, so the read must answer the same way after any of
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WritePath {
    /// One `PutAsset`.
    Put,
    /// A `PutAsset` storing no form at all, replaced by a second put under the same
    /// identifier that stores the row's forms.
    PutThenRePut,
    /// A `PutAsset`, then the `PutAttachment` that writes one attachment onto the asset.
    PutThenAttach,
    /// The same, then the `DeleteAttachment` that takes it off again — so the asset holds
    /// no attachment, reached through a path that once held one.
    PutThenAttachThenDetach,
    /// A `PutAsset`, then the `AssociateGlossaryTerms` that merges one term onto the asset.
    PutThenAssociate,
}

/// One row of the table: the state to reach, and how to reach it.
struct Row {
    case: &'static str,
    identifier: &'static str,
    /// The forms the LAST put of this asset sends, which is what a read must report beside
    /// the catalog's own form.
    forms: Value,
    /// How the put addresses the asset type: by the identifier its registration reported,
    /// or by its name.
    asset_type_address: String,
    path: WritePath,
}

/// Drive `row`'s write path against the emulator.
async fn drive(router: &MultiServiceRouter, row: &Row, attachment_form_type: &str, term: &str) {
    if row.path == WritePath::PutThenRePut {
        put_asset(router, row.identifier, &row.asset_type_address, &json!({})).await;
    }
    put_asset(
        router,
        row.identifier,
        &row.asset_type_address,
        &row.forms,
    )
    .await;

    match row.path {
        WritePath::Put | WritePath::PutThenRePut => {}
        WritePath::PutThenAttach | WritePath::PutThenAttachThenDetach => {
            ok(
                router,
                "PutAttachment",
                &json!({
                    "AssetIdentifier": row.identifier,
                    "AttachmentName": ATTACHMENT_NAME,
                    "FormTypeId": attachment_form_type,
                    "Content": ATTACHMENT_CONTENT,
                }),
            )
            .await;
            if row.path == WritePath::PutThenAttachThenDetach {
                ok(
                    router,
                    "DeleteAttachment",
                    &json!({
                        "AssetIdentifier": row.identifier,
                        "AttachmentName": ATTACHMENT_NAME,
                    }),
                )
                .await;
            }
        }
        WritePath::PutThenAssociate => {
            ok(
                router,
                "AssociateGlossaryTerms",
                &json!({
                    "AssetIdentifier": row.identifier,
                    "GlossaryTermIdentifiers": [term],
                }),
            )
            .await;
        }
    }
}

/// The forms a read reports MINUS the catalog's own, which [`assert_catalog_form`] checks
/// on its own: what the row's own puts stored.
fn stored_forms(case: &str, read: &Value) -> Value {
    let mut forms = read
        .get("Forms")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: the read must report Forms, got {read}"));
    let catalog_form = forms.remove(CATALOG_RESOURCE_FORM_NAME);
    assert_catalog_form(case, read, catalog_form.as_ref());
    Value::Object(forms)
}

/// Assert that `list` — the list a read answers with beside a keyed map of forms — carries
/// one entry per entry of `map`, each naming its key and agreeing with it about the form
/// type and the content.
fn assert_list_mirrors_map(case: &str, member: &str, read: &Value, map: &Value) {
    let expected = map
        .as_object()
        .unwrap_or_else(|| panic!("case {case}: {member} mirrors a map, got {map}"));
    let listed = read
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case}: the read must report {member}, got {read}"));
    assert_eq!(
        listed.len(),
        expected.len(),
        "case {case}: {member} must carry one entry per reported form; expected \
         {} entries, got {read}",
        expected.len()
    );
    let listed_names: BTreeSet<&str> = listed
        .iter()
        .map(|entry| {
            entry
                .get("FormName")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case}: every {member} entry names its form, got {entry}"))
        })
        .collect();
    let expected_names: BTreeSet<&str> = expected.keys().map(String::as_str).collect();
    assert_eq!(
        listed_names, expected_names,
        "case {case}: {member} must name the same forms as the map beside it, got {read}"
    );
    for entry in listed {
        let name = entry["FormName"].as_str().expect("checked above");
        assert_eq!(
            (entry.get("FormTypeId"), entry.get("Content")),
            (
                expected[name].get("FormTypeId"),
                expected[name].get("Content")
            ),
            "case {case}: {member} and the map beside it must agree about {name}, got {entry}"
        );
    }
}

// ---------------------------------------------------------------------------
// The whole envelope, after every write path
// ---------------------------------------------------------------------------

/// A read of an asset reports the asset type it is an instance of and every collection it
/// can carry — forms, attachments, iterable forms, metadata forms and glossary terms —
/// whichever write path left the asset in that state, with the collections it holds nothing
/// in reported empty rather than left out, and reading it changes nothing a later read sees.
#[tokio::test]
async fn every_asset_write_path_leaves_a_read_that_reports_the_whole_asset() {
    let router = emulator();
    let details = form_type(&router, "DetailsForm").await;
    let audit = form_type(&router, "AuditForm").await;
    let asset_type_id = asset_type(
        &router,
        "OrderAssetType",
        &[("Details", &details), ("Audit", &audit)],
    )
    .await;
    let term = glossary_term(&router, "AssetGlossary", "Revenue").await;

    let one_form = json!({ "Details": { "FormTypeId": &details, "Content": "{\"a\":\"one\"}" } });
    let two_forms = json!({
        "Details": { "FormTypeId": &details, "Content": "{\"a\":\"one\"}" },
        "Audit": { "FormTypeId": &audit, "Content": "{\"b\":\"two\"}" },
    });
    let rows: Vec<Row> = vec![
        Row {
            case: "a put storing one form",
            identifier: "put-one-form",
            forms: one_form.clone(),
            asset_type_address: asset_type_id.clone(),
            path: WritePath::Put,
        },
        Row {
            case: "a put storing no form at all",
            identifier: "put-no-form",
            forms: json!({}),
            asset_type_address: asset_type_id.clone(),
            path: WritePath::Put,
        },
        Row {
            case: "a put storing two forms, addressing the asset type by name",
            identifier: "put-two-forms",
            forms: two_forms.clone(),
            asset_type_address: "OrderAssetType".to_string(),
            path: WritePath::Put,
        },
        Row {
            case: "a put storing a form that carries no content",
            identifier: "put-no-content",
            forms: json!({ "Details": { "FormTypeId": &details } }),
            asset_type_address: asset_type_id.clone(),
            path: WritePath::Put,
        },
        Row {
            case: "a second put replacing the first",
            identifier: "re-put",
            forms: two_forms.clone(),
            asset_type_address: asset_type_id.clone(),
            path: WritePath::PutThenRePut,
        },
        Row {
            case: "an attachment written onto the asset",
            identifier: "attached",
            forms: one_form.clone(),
            asset_type_address: asset_type_id.clone(),
            path: WritePath::PutThenAttach,
        },
        Row {
            case: "an attachment written onto the asset and taken off again",
            identifier: "detached",
            forms: one_form.clone(),
            asset_type_address: asset_type_id.clone(),
            path: WritePath::PutThenAttachThenDetach,
        },
        Row {
            case: "a glossary term associated with the asset",
            identifier: "associated",
            forms: one_form.clone(),
            asset_type_address: "OrderAssetType".to_string(),
            path: WritePath::PutThenAssociate,
        },
    ];

    for row in &rows {
        let case = row.case;
        drive(&router, row, &details, &term).await;
        let read = get_asset(&router, row.identifier).await;

        // The scalar identity, and the asset type the asset is an instance of — reported
        // both as the reference the put was addressed with and by the asset type's name.
        assert_eq!(
            (
                read.get("Id").and_then(Value::as_str),
                read.get("Name").and_then(Value::as_str),
                read.get("Description").and_then(Value::as_str),
                read.get("AssetTypeId").and_then(Value::as_str),
                read.get("Type").and_then(Value::as_str),
            ),
            (
                Some(row.identifier),
                Some(PUT_NAME),
                Some(PUT_DESCRIPTION),
                Some(row.asset_type_address.as_str()),
                Some("OrderAssetType"),
            ),
            "case {case}: the read must report the asset's identity and its asset type, got \
             {read}"
        );
        for member in ["CreatedAt", "UpdatedAt"] {
            assert!(
                read.get(member).and_then(Value::as_f64).is_some(),
                "case {case}: the read must report {member}; got {read}"
            );
        }

        // The forms the asset's own puts stored, beside the catalog's own form, and the
        // list that mirrors the whole map.
        assert_eq!(
            stored_forms(case, &read),
            row.forms,
            "case {case}: the read must report the forms the put stored, got {read}"
        );
        assert_list_mirrors_map(case, "AssetForms", &read, &read["Forms"]);

        // The attachments the asset holds, as the map and the list beside it: present and
        // EMPTY for an asset holding none, including one whose attachment was deleted.
        let expected_attachments = if row.path == WritePath::PutThenAttach {
            json!({
                ATTACHMENT_NAME: { "FormTypeId": &details, "Content": ATTACHMENT_CONTENT },
            })
        } else {
            json!({})
        };
        assert_eq!(
            read.get("Attachments"),
            Some(&expected_attachments),
            "case {case}: the read must report the attachments the asset holds, got {read}"
        );
        assert_list_mirrors_map(case, "MetadataForms", &read, &expected_attachments);

        // The iterable forms of the asset: a map no operation of this surface writes, so
        // every asset reports it empty rather than leaving it out.
        assert_eq!(
            read.get("IterableForms"),
            Some(&json!({})),
            "case {case}: the read must report IterableForms as the empty map, got {read}"
        );

        // The terms associated with the asset, as a list that is empty rather than absent.
        let expected_terms = if row.path == WritePath::PutThenAssociate {
            vec![Value::String(term.clone())]
        } else {
            Vec::new()
        };
        assert_eq!(
            read.get("GlossaryTerms").and_then(Value::as_array),
            Some(&expected_terms),
            "case {case}: the read must report the terms the asset carries, got {read}"
        );

        // A read reports, it does not write: the next caller sees the same record.
        let again = get_asset(&router, row.identifier).await;
        assert_eq!(
            again, read,
            "case {case}: two reads of one asset must report one record, got {again}"
        );
    }

    // Every asset is still its own after every write path has landed: no row's writes
    // reached another row's record.
    for row in &rows {
        let read = get_asset(&router, row.identifier).await;
        assert_eq!(
            stored_forms(row.case, &read),
            row.forms,
            "case {}: each asset keeps its own forms, got {read}",
            row.case
        );
    }
}

// ---------------------------------------------------------------------------
// Which members a read names does not depend on what the asset holds
// ---------------------------------------------------------------------------

/// The members a read of an asset names are the same set however much the asset holds: a
/// collection it holds nothing in is reported empty, so a client cannot tell "none" from
/// "not reported" by which members are there.
///
/// The rows differ along how much of the envelope is non-empty: nothing but the asset's
/// identity, forms only, an attachment only, a term only, and all three at once.
#[tokio::test]
async fn a_read_names_the_same_members_however_much_the_asset_holds() {
    let router = emulator();
    let details = form_type(&router, "DetailsForm").await;
    let asset_type_id = asset_type(&router, "OrderAssetType", &[("Details", &details)]).await;
    let term = glossary_term(&router, "AssetGlossary", "Revenue").await;

    /// Every member a read of an asset reports. The modelled ones are
    /// `GetAssetOutput`'s (`smithy_query operation=GetAsset`): `Id`, `Name`, `Description`,
    /// `CreatedAt`, `UpdatedAt`, `AssetTypeId`, `Forms`, `Attachments`, `IterableForms` and
    /// `GlossaryTerms`; `Type`, `AssetForms` and `MetadataForms` are recorded behaviour of
    /// this read, which the on-disk model does not declare.
    const REPORTED_MEMBERS: [&str; 13] = [
        "AssetForms",
        "AssetTypeId",
        "Attachments",
        "CreatedAt",
        "Description",
        "Forms",
        "GlossaryTerms",
        "Id",
        "IterableForms",
        "MetadataForms",
        "Name",
        "Type",
        "UpdatedAt",
    ];

    // (case, identifier, the forms the put sends, whether an attachment is written,
    //  whether a term is associated)
    let rows: Vec<(&str, &str, Value, bool, bool)> = vec![
        ("an asset holding nothing at all", "bare", json!({}), false, false),
        (
            "an asset holding one form",
            "forms-only",
            json!({ "Details": { "FormTypeId": &details, "Content": "{\"a\":1}" } }),
            false,
            false,
        ),
        (
            "an asset holding one attachment and no form",
            "attachment-only",
            json!({}),
            true,
            false,
        ),
        ("an asset holding one term and nothing else", "term-only", json!({}), false, true),
        (
            "an asset holding a form, an attachment and a term",
            "everything",
            json!({ "Details": { "FormTypeId": &details, "Content": "{\"a\":1}" } }),
            true,
            true,
        ),
    ];

    for (case, identifier, forms, attach, associate) in &rows {
        put_asset(&router, identifier, &asset_type_id, forms).await;
        if *attach {
            ok(
                &router,
                "PutAttachment",
                &json!({
                    "AssetIdentifier": identifier,
                    "AttachmentName": ATTACHMENT_NAME,
                    "FormTypeId": &details,
                    "Content": ATTACHMENT_CONTENT,
                }),
            )
            .await;
        }
        if *associate {
            ok(
                &router,
                "AssociateGlossaryTerms",
                &json!({
                    "AssetIdentifier": identifier,
                    "GlossaryTermIdentifiers": [&term],
                }),
            )
            .await;
        }

        let read = get_asset(&router, identifier).await;
        let reported: BTreeSet<&str> = read
            .as_object()
            .unwrap_or_else(|| panic!("case {case}: the read must answer an object, got {read}"))
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            reported,
            REPORTED_MEMBERS.into_iter().collect::<BTreeSet<&str>>(),
            "case {case}: the read must name every member of the asset envelope, got {read}"
        );
    }
}
