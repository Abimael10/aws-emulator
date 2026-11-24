// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateAsset`, the update of the Glue asset slice `PutAsset` writes
//! and `GetAsset` reads.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is the HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test is the one `UpdateAsset` documents — "updates the name and
//! description of an existing asset in Glue Data Catalog. Only the fields that you provide
//! are updated" — read as: the update addresses the ONE asset its `Identifier` resolves to
//! and writes the members the request NAMES, leaving every member it omits where the last
//! write put it; every member that is no member of the request — the asset type the asset is
//! an instance of, its forms, its attachments, its glossary terms and its creation instant —
//! is left exactly as stored; the update instant moves when the request changed something
//! and stays where it is when it changed nothing; and an identifier this account holds no
//! asset under is refused with the miss this operation models (`smithy_query
//! operation=UpdateAsset` lists `EntityNotFoundException`) without writing anything.
//!
//! The suites are tables whose rows differ along what the rule generalises over: which
//! members the update names, the form and length of the values it sends, whether the asset
//! was put with a description, how many forms and attachments it carries, whether it carries
//! a glossary term, the form and length of the identifier addressing it, whether the update
//! carried an idempotency token, and how an identifier misses.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_glue_awsglue::state::{
    ComAmazonawsGlueAwsglueState, CATALOG_RESOURCE_FORM_NAME,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{assert_catalog_form, call, emulator, emulator_on, error_code, error_message, ok};

/// The longest `Identifier` the model accepts: "len 1..1087" on `AssetId`
/// (`smithy_query operation=UpdateAsset`), whose pattern `^[a-zA-Z0-9\-\:\/\.\_\*]+$` is
/// what every identifier below keeps to.
const MAX_ASSET_ID_LENGTH: usize = 1087;

/// The longest `ClientToken` the model accepts: "len 1..255" on `HashString`
/// (`smithy_query operation=UpdateAsset`).
const MAX_CLIENT_TOKEN_LENGTH: usize = 255;

/// Register a form type and answer the `FormTypeId` the catalog assigned it, so an asset's
/// form entries can name a form type this account really holds.
async fn form_type(router: &MultiServiceRouter, name: &str) -> String {
    let stored = ok(
        router,
        "PutFormType",
        &json!({ "Name": name, "Schema": format!("structure {name} {{ a: String }}") }),
    )
    .await;
    id_of(name, &stored)
}

/// Register an asset type over the form types `forms` names and answer the `AssetTypeId` the
/// catalog minted for it.
async fn asset_type(
    router: &MultiServiceRouter,
    name: &str,
    forms: &[(&str, &str)],
) -> String {
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

/// Create a business glossary with one term in it and answer the term's identifier, so an
/// asset can carry an association an update must not disturb.
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

/// The `Id` a write reported, which every registration of this surface answers with.
fn id_of(case: &str, body: &Value) -> String {
    body.get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: the write must report an Id, got {body}"))
        .to_string()
}

/// `members` sent as an `UpdateAsset` request for `identifier`, which the member list of a
/// row never carries itself.
fn update_request(identifier: &str, members: &Value) -> Value {
    let mut request = members.clone();
    request["Identifier"] = json!(identifier);
    request
}

/// Update the asset `identifier` names and answer the response body.
async fn update(router: &MultiServiceRouter, identifier: &str, members: &Value) -> Value {
    ok(router, "UpdateAsset", &update_request(identifier, members)).await
}

/// The `GetAsset` response for the asset `identifier` names.
async fn get(router: &MultiServiceRouter, identifier: &str) -> Value {
    ok(router, "GetAsset", &json!({ "Identifier": identifier })).await
}

/// A timestamp member of a response, as its epoch-second value.
fn instant(case: &str, body: &Value, member: &str) -> f64 {
    body.get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case}: the response must carry {member}, got {body}"))
}

/// The forms a read reports MINUS the catalog's own, which [`assert_catalog_form`] checks on
/// its own against the SAME read: what the asset's own puts stored.
fn stored_forms(case: &str, read: &Value) -> Value {
    let mut forms = read
        .get("Forms")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: GetAsset must report Forms, got {read}"));
    let catalog_form = forms.remove(CATALOG_RESOURCE_FORM_NAME);
    assert_catalog_form(case, read, catalog_form.as_ref());
    Value::Object(forms)
}

/// The `(Id, Name, Description)` triple of an asset body, as the three modelled members both
/// the update and the read answer with.
fn triple(body: &Value) -> (Option<&str>, Option<&str>, Option<&str>) {
    (
        body.get("Id").and_then(Value::as_str),
        body.get("Name").and_then(Value::as_str),
        body.get("Description").and_then(Value::as_str),
    )
}

// ---------------------------------------------------------------------------
// The update writes the members the request names
// ---------------------------------------------------------------------------

/// One row of the partial-update table: the asset as put, the members an update names, and
/// the asset the update must leave behind.
struct UpdateRow {
    case: &'static str,
    /// The `Identifier` the asset is stored under, which also addresses the update.
    identifier: String,
    /// The `Description` the put sends, absent when it sends none.
    put_description: Option<&'static str>,
    /// The `Forms` the put sends, which no update may touch.
    forms: Value,
    /// The `UpdateAsset` members, WITHOUT the `Identifier` the harness adds.
    members: Value,
    /// The `Name` the asset must carry after the update.
    expected_name: String,
    /// The `Description` it must carry, absent when it must carry none.
    expected_description: Option<String>,
    /// Whether the request named a member, so the update instant must have moved off the
    /// creation instant rather than stayed on it.
    changes_something: bool,
}

/// An update writes the members it names and leaves every other member of the asset — the
/// ones it omits, the ones that are no member of the request at all, and the whole of the
/// rest of the store — where the writes that own them left them, and it reports what a later
/// read of the same identifier reports.
#[tokio::test]
async fn an_update_writes_the_members_it_names_and_leaves_the_rest() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    let details_form = form_type(&router, "OrderDetailsForm").await;
    let audit_form = form_type(&router, "OrderAuditForm").await;
    let order_type = asset_type(
        &router,
        "OrderAssetType",
        &[("Details", &details_form), ("Audit", &audit_form)],
    )
    .await;

    let one_form = json!({ "Details": { "FormTypeId": &details_form, "Content": "{\"a\":1}" } });
    let two_forms = json!({
        "Details": { "FormTypeId": &details_form, "Content": "{\"a\":1}" },
        "Audit": { "FormTypeId": &audit_form, "Content": "{\"reviewed\":true}" },
    });

    let rows = vec![
        UpdateRow {
            case: "the name alone, leaving the description as put",
            identifier: "order-name-only".to_string(),
            put_description: Some("as put"),
            forms: one_form.clone(),
            members: json!({ "Name": "Order renamed" }),
            expected_name: "Order renamed".to_string(),
            expected_description: Some("as put".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "the description alone, leaving the name as put",
            identifier: "order-description-only".to_string(),
            put_description: Some("as put"),
            forms: two_forms.clone(),
            members: json!({ "Description": "described by the update" }),
            expected_name: "Order as put".to_string(),
            expected_description: Some("described by the update".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "both members at once",
            identifier: "order-both".to_string(),
            put_description: Some("as put"),
            forms: two_forms.clone(),
            members: json!({ "Name": "Order renamed", "Description": "and redescribed" }),
            expected_name: "Order renamed".to_string(),
            expected_description: Some("and redescribed".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "no updatable member at all",
            identifier: "order-untouched".to_string(),
            put_description: Some("as put"),
            forms: one_form.clone(),
            members: json!({}),
            expected_name: "Order as put".to_string(),
            expected_description: Some("as put".to_string()),
            changes_something: false,
        },
        UpdateRow {
            case: "a description onto an asset put without one",
            identifier: "order-lately-described".to_string(),
            put_description: None,
            forms: one_form.clone(),
            members: json!({ "Description": "described after the fact" }),
            expected_name: "Order as put".to_string(),
            expected_description: Some("described after the fact".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "a name onto an asset put without a description",
            identifier: "order-plain".to_string(),
            put_description: None,
            forms: one_form.clone(),
            members: json!({ "Name": "Order renamed" }),
            expected_name: "Order renamed".to_string(),
            expected_description: None,
            changes_something: true,
        },
        UpdateRow {
            case: "an asset carrying no form at all",
            identifier: "order-formless".to_string(),
            put_description: Some("as put"),
            forms: json!({}),
            members: json!({ "Name": "Formless renamed" }),
            expected_name: "Formless renamed".to_string(),
            expected_description: Some("as put".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "a one-character name and a one-character description",
            identifier: "order-minimal".to_string(),
            put_description: Some("as put"),
            forms: one_form.clone(),
            members: json!({ "Name": "n", "Description": "d" }),
            expected_name: "n".to_string(),
            expected_description: Some("d".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "a name and description carrying spaces, punctuation and non-ASCII text",
            identifier: "order-non-ascii".to_string(),
            put_description: Some("as put"),
            forms: two_forms.clone(),
            members: json!({
                "Name": "Pedido — Ventas (ES/MX)",
                "Description": "Términos de negocio\tcon tabulador\ny salto de línea",
            }),
            expected_name: "Pedido — Ventas (ES/MX)".to_string(),
            expected_description: Some(
                "Términos de negocio\tcon tabulador\ny salto de línea".to_string(),
            ),
            changes_something: true,
        },
        UpdateRow {
            case: "a long name, far past the longest the put sent",
            identifier: "order-long-name".to_string(),
            put_description: Some("as put"),
            forms: one_form.clone(),
            members: json!({ "Name": "N".repeat(4096) }),
            expected_name: "N".repeat(4096),
            expected_description: Some("as put".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "an identifier of one character",
            identifier: "o".to_string(),
            put_description: Some("as put"),
            forms: one_form.clone(),
            members: json!({ "Name": "Single character identifier" }),
            expected_name: "Single character identifier".to_string(),
            expected_description: Some("as put".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "an identifier carrying the punctuation the pattern allows",
            identifier: "arn:aws:glue:us-east-1:000000000000/asset.9_x*".to_string(),
            put_description: Some("as put"),
            forms: one_form.clone(),
            members: json!({ "Description": "addressed through punctuation" }),
            expected_name: "Order as put".to_string(),
            expected_description: Some("addressed through punctuation".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "an identifier at the modelled length bound",
            identifier: "a".repeat(MAX_ASSET_ID_LENGTH),
            put_description: Some("as put"),
            forms: one_form.clone(),
            members: json!({ "Name": "Addressed at the bound" }),
            expected_name: "Addressed at the bound".to_string(),
            expected_description: Some("as put".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "an idempotency token sent with the update",
            identifier: "order-tokened".to_string(),
            put_description: Some("as put"),
            forms: one_form.clone(),
            members: json!({ "Description": "updated under a token", "ClientToken": "a-token" }),
            expected_name: "Order as put".to_string(),
            expected_description: Some("updated under a token".to_string()),
            changes_something: true,
        },
        UpdateRow {
            case: "an idempotency token at the modelled length bound",
            identifier: "order-maximally-tokened".to_string(),
            put_description: None,
            forms: one_form.clone(),
            members: json!({
                "Name": "Maximally tokened",
                "ClientToken": "t".repeat(MAX_CLIENT_TOKEN_LENGTH),
            }),
            expected_name: "Maximally tokened".to_string(),
            expected_description: None,
            changes_something: true,
        },
    ];

    // A bystander, to prove an update is scoped to the asset it names.
    let bystander = "order-bystander";
    ok(
        &router,
        "PutAsset",
        &json!({
            "Identifier": bystander,
            "AssetTypeId": &order_type,
            "Name": "Bystander order",
            "Description": "untouched",
            "Forms": &one_form,
        }),
    )
    .await;
    let bystander_before = get(&router, bystander).await;

    for row in &rows {
        let mut put = json!({
            "Identifier": &row.identifier,
            "AssetTypeId": &order_type,
            "Name": "Order as put",
            "Forms": &row.forms,
        });
        if let Some(description) = row.put_description {
            put["Description"] = json!(description);
        }
        ok(&router, "PutAsset", &put).await;
        let before = get(&router, &row.identifier).await;
        let created_at = instant(row.case, &before, "CreatedAt");

        let updated = update(&router, &row.identifier, &row.members).await;
        let expected = (
            Some(row.identifier.as_str()),
            Some(row.expected_name.as_str()),
            row.expected_description.as_deref(),
        );
        assert_eq!(
            triple(&updated),
            expected,
            "case {}: UpdateAsset must report the asset it stored, got {updated}",
            row.case
        );
        let reported_update_instant = instant(row.case, &updated, "UpdatedAt");

        // The update reports what the store holds: the read of the identifier the put stored
        // the asset under answers with the same three members.
        let read = get(&router, &row.identifier).await;
        assert_eq!(
            triple(&read),
            expected,
            "case {}: GetAsset must report what the update stored, got {read}",
            row.case
        );

        // Every member that is no member of the update request is exactly as the put left
        // it: the asset type the asset is an instance of, its forms, its attachments, its
        // glossary terms and the instant it was created at.
        assert_eq!(
            (
                read.get("AssetTypeId"),
                read.get("Type"),
                read.get("Attachments"),
                read.get("GlossaryTerms"),
            ),
            (
                before.get("AssetTypeId"),
                before.get("Type"),
                before.get("Attachments"),
                before.get("GlossaryTerms"),
            ),
            "case {}: an update must leave every member it does not name; expected {before}, \
             got {read}",
            row.case
        );
        assert_eq!(
            stored_forms(row.case, &read),
            stored_forms(row.case, &before),
            "case {}: an update must leave the forms the put stored, got {read}",
            row.case
        );
        assert_eq!(
            instant(row.case, &read, "CreatedAt"),
            created_at,
            "case {}: an update must keep the creation instant the put stamped, got {read}",
            row.case
        );

        // The update instant is the record's: the update and the read report one value, and
        // it moved off the creation instant only for a request that changed something.
        let update_instant = instant(row.case, &read, "UpdatedAt");
        assert_eq!(
            update_instant, reported_update_instant,
            "case {}: the update and the read must report one update instant; expected \
             {reported_update_instant}, got {update_instant}",
            row.case
        );
        if row.changes_something {
            assert!(
                update_instant >= created_at,
                "case {}: a changed asset must report an update instant at or after its \
                 creation instant {created_at}, got {update_instant}",
                row.case
            );
        } else {
            assert_eq!(
                update_instant, created_at,
                "case {}: an update that named no member must leave the update instant on \
                 the creation instant {created_at}, got {update_instant}",
                row.case
            );
        }

        // Re-sending the same update changes nothing further: the second answer reports the
        // same three members.
        let again = update(&router, &row.identifier, &row.members).await;
        assert_eq!(
            triple(&again),
            expected,
            "case {}: re-sending the update must report the same asset, got {again}",
            row.case
        );
    }

    // An update rewrites a record, it never adds one: the store holds exactly the assets the
    // puts wrote.
    let held = engine
        .state::<ComAmazonawsGlueAwsglueState>()
        .lock()
        .assets
        .len();
    assert_eq!(
        held,
        rows.len() + 1,
        "case every row: an update must store no second record; expected {} assets, got {held}",
        rows.len() + 1
    );

    // Nothing any update did reached the asset it did not name.
    let read = get(&router, bystander).await;
    assert_eq!(
        read, bystander_before,
        "case the bystander asset: an update must leave every other asset in place; expected \
         {bystander_before}, got {read}"
    );
}

// ---------------------------------------------------------------------------
// What the update leaves on an asset that carries more than its own members
// ---------------------------------------------------------------------------

/// An update of an asset that carries attachments and glossary terms leaves both where their
/// own writes put them, and the catalog's own form on the asset — the document the read
/// derives from the asset's name and description — follows the update.
///
/// The rows differ along how much the asset carries beside its own members: forms alone, an
/// attachment beside them, a glossary term beside them, and both at once.
#[tokio::test]
async fn an_update_leaves_the_attachments_and_terms_an_asset_carries() {
    let router = emulator();

    let details_form = form_type(&router, "DetailsForm").await;
    let attachment_form = form_type(&router, "AttachmentForm").await;
    let order_type = asset_type(&router, "CarriedAssetType", &[("Details", &details_form)]).await;
    let term = glossary_term(&router, "CarriedGlossary", "CarriedTerm").await;

    let rows: [(&str, bool, bool); 4] = [
        ("forms alone", false, false),
        ("an attachment beside the forms", true, false),
        ("a glossary term beside the forms", false, true),
        ("an attachment and a glossary term", true, true),
    ];

    for (case, attach, associate) in &rows {
        let identifier = format!("carried-{}", case.replace(' ', "-"));
        ok(
            &router,
            "PutAsset",
            &json!({
                "Identifier": &identifier,
                "AssetTypeId": &order_type,
                "Name": "Carried order",
                "Description": "as put",
                "Forms": {
                    "Details": { "FormTypeId": &details_form, "Content": "{\"a\":1}" },
                },
            }),
        )
        .await;
        if *attach {
            ok(
                &router,
                "PutAttachment",
                &json!({
                    "AssetIdentifier": &identifier,
                    "AttachmentName": "Review",
                    "FormTypeId": &attachment_form,
                    "Content": "{\"reviewed\":true}",
                }),
            )
            .await;
        }
        if *associate {
            ok(
                &router,
                "AssociateGlossaryTerms",
                &json!({
                    "AssetIdentifier": &identifier,
                    "GlossaryTermIdentifiers": [&term],
                }),
            )
            .await;
        }
        let before = get(&router, &identifier).await;

        update(
            &router,
            &identifier,
            &json!({ "Name": "Carried order renamed", "Description": "redescribed" }),
        )
        .await;

        // The read reports the renamed asset with everything else as it was, and
        // `stored_forms` checks the catalog's own form against the SAME read — so the
        // document it carries is the one derived from the name and description the update
        // wrote, not the pair the put stored.
        let read = get(&router, &identifier).await;
        assert_eq!(
            triple(&read),
            (
                Some(identifier.as_str()),
                Some("Carried order renamed"),
                Some("redescribed"),
            ),
            "case {case}: the read must report what the update wrote, got {read}"
        );
        assert_eq!(
            stored_forms(case, &read),
            stored_forms(case, &before),
            "case {case}: an update must leave the forms the put stored, got {read}"
        );
        assert_eq!(
            read.get("Attachments"),
            before.get("Attachments"),
            "case {case}: an update must leave the attachments the asset carries, got {read}"
        );
        assert_eq!(
            read.get("MetadataForms"),
            before.get("MetadataForms"),
            "case {case}: an update must leave the attachment list beside them, got {read}"
        );
        assert_eq!(
            read.get("GlossaryTerms"),
            before.get("GlossaryTerms"),
            "case {case}: an update must leave the glossary terms the asset carries, got {read}"
        );
    }
}

// ---------------------------------------------------------------------------
// An identifier that resolves to nothing
// ---------------------------------------------------------------------------

/// An update for an identifier this account holds no asset under is refused with the modelled
/// `EntityNotFoundException`, worded the way this surface words an asset miss, and writes
/// nothing — the asset a concurrent delete already removed included.
///
/// The rows differ along how the identifier misses: never stored, a prefix of a stored
/// identifier, a stored identifier with one more character, a stored identifier recased, an
/// identifier that names an asset TYPE rather than an asset, and an asset that was deleted.
#[tokio::test]
async fn updating_an_asset_the_store_does_not_hold_is_refused() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    let details_form = form_type(&router, "MissDetailsForm").await;
    let order_type = asset_type(&router, "MissAssetType", &[("Details", &details_form)]).await;
    let forms = json!({ "Details": { "FormTypeId": &details_form, "Content": "{\"a\":1}" } });

    let kept = "kept-order";
    ok(
        &router,
        "PutAsset",
        &json!({
            "Identifier": kept,
            "AssetTypeId": &order_type,
            "Name": "Kept order",
            "Description": "as put",
            "Forms": &forms,
        }),
    )
    .await;
    // An asset put and then deleted, so one row names an address that WAS live — the shape a
    // client that updates an asset another caller has just deleted sends.
    let gone = "gone-order";
    ok(
        &router,
        "PutAsset",
        &json!({
            "Identifier": gone,
            "AssetTypeId": &order_type,
            "Name": "Gone order",
            "Forms": &forms,
        }),
    )
    .await;
    ok(&router, "DeleteAsset", &json!({ "Identifier": gone })).await;
    let before = get(&router, kept).await;

    let rows: Vec<(&str, String)> = vec![
        ("never stored", "no-such-order".to_string()),
        ("a prefix of a stored identifier", kept[..kept.len() - 1].to_string()),
        ("a stored identifier with one more character", format!("{kept}X")),
        ("a stored identifier recased", kept.to_uppercase()),
        ("an asset type's own identifier", order_type.clone()),
        ("an asset that was deleted", gone.to_string()),
        ("an identifier at the modelled length bound", "z".repeat(MAX_ASSET_ID_LENGTH)),
    ];

    // The members the update names make no difference to a miss: the identifier is resolved
    // first.
    let member_sets = [
        ("a name alone", json!({ "Name": "Renamed by a miss" })),
        (
            "a description alone",
            json!({ "Description": "described by a miss" }),
        ),
        (
            "both members under a token",
            json!({ "Name": "Renamed", "Description": "described", "ClientToken": "a-token" }),
        ),
        ("no updatable member at all", json!({})),
    ];

    for (case, identifier) in &rows {
        for (members_case, members) in &member_sets {
            let (status, body) = call(
                &router,
                "UpdateAsset",
                &update_request(identifier, members),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case} with {members_case}: expected 400 for {identifier:?}, got {status} \
                 with {body}"
            );
            assert!(
                error_code(&body).contains("EntityNotFoundException"),
                "case {case} with {members_case}: expected EntityNotFoundException for \
                 {identifier:?}, got {body}"
            );
            // The refusal's `Message` member, pinned exactly: this surface's refusal text is
            // a JSON document carrying the sentence under a `Message` key, with the
            // identifier from the request as its one variable part — the same sentence
            // `GetAsset` words the same miss with.
            assert_eq!(
                error_message(&body),
                format!(r#"{{"Message":"Asset not found: {identifier}"}}"#),
                "case {case} with {members_case}: unexpected refusal wording for \
                 {identifier:?}, got {body}"
            );
        }
    }

    // No refused update wrote anything: the one stored asset is as its put left it, and it is
    // the only record the store holds.
    let read = get(&router, kept).await;
    assert_eq!(
        read, before,
        "case the stored asset: an update that resolved to nothing must write nothing; \
         expected {before}, got {read}"
    );
    let held = engine
        .state::<ComAmazonawsGlueAwsglueState>()
        .lock()
        .assets
        .len();
    assert_eq!(
        held, 1,
        "case every miss row: the store must still hold exactly the one asset, got {held}"
    );
}

/// A value one past a published bound on the update's own members is refused, and the stored
/// asset is left exactly as its put wrote it: an identifier past its length, an identifier of
/// no characters, an identifier outside the character class its pattern admits, and a client
/// token past its length.
#[tokio::test]
async fn an_update_past_a_published_bound_is_refused() {
    let router = emulator();

    let details_form = form_type(&router, "BoundedDetailsForm").await;
    let order_type = asset_type(&router, "BoundedAssetType", &[("Details", &details_form)]).await;
    let identifier = "bounded-order";
    ok(
        &router,
        "PutAsset",
        &json!({
            "Identifier": identifier,
            "AssetTypeId": &order_type,
            "Name": "Bounded order",
            "Description": "as put",
            "Forms": { "Details": { "FormTypeId": &details_form, "Content": "{\"a\":1}" } },
        }),
    )
    .await;
    let before = get(&router, identifier).await;

    let rows: [(&str, Value); 4] = [
        (
            "an identifier one character past the length bound",
            json!({
                "Identifier": "a".repeat(MAX_ASSET_ID_LENGTH + 1),
                "Name": "Past the bound",
            }),
        ),
        (
            "an identifier of no characters at all",
            json!({ "Identifier": "", "Name": "No identifier" }),
        ),
        (
            "an identifier outside the character class the pattern admits",
            json!({ "Identifier": "bounded order", "Name": "Spaced identifier" }),
        ),
        (
            "a client token one character past its length bound",
            json!({
                "Identifier": identifier,
                "Name": "Tokened too far",
                "ClientToken": "t".repeat(MAX_CLIENT_TOKEN_LENGTH + 1),
            }),
        ),
    ];

    for (case, request) in &rows {
        let (status, body) = call(&router, "UpdateAsset", request).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        let read = get(&router, identifier).await;
        assert_eq!(
            read, before,
            "case {case}: a refused update must write nothing; expected {before}, got {read}"
        );
    }
}
