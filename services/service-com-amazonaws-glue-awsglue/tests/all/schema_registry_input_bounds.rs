// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals the SCHEMA REGISTRY words itself: the lookup that
//! resolves no schema set, a page size outside the range its member's modelled type declares,
//! and a `SchemaDefinition` outside the length its member's modelled type declares.
//!
//! The rules under test:
//!
//! 1. A `ListSchemaVersions` or `GetSchema` whose `SchemaId` resolves to no schema set is the
//!    modelled `EntityNotFoundException` (HTTP 400) stating the ADDRESS the lookup looked that
//!    set up by: the fixed sentence plus the three `SchemaId` members as the request sent them,
//!    a member the request did not carry spelled `null`.
//! 2. A `MaxResults` outside the range its modelled type declares (`MaxResultsNumber`, range
//!    1..100) is the operation's own `InvalidInputException` stating both ends of that range
//!    and the value the request sent — never the cross-service `ValidationException`, which
//!    none of these operations models — while each published bound itself is served.
//! 3. A `SchemaDefinition` outside the length its modelled type declares
//!    (`SchemaDefinitionString`, length 1..170000) is the operation's own
//!    `InvalidInputException` naming the PARAMETER and echoing nothing of the document, while
//!    a definition AT the maximum is accepted and stored byte for byte.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: which form
//! the `SchemaId` addressed the set by, which listing operation is called, the page size asked
//! for (each published bound, one past each, and the extremes the member's type can carry),
//! and the definition's length (the maximum, one past it, and far past it). Every call goes
//! over the wire through the shared [`crate::wire`] harness.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{messages, DEFAULT_REGISTRY_NAME};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The published bounds of `MaxResults` on the schema-registry listings: `MaxResultsNumber`,
/// range 1..100 (`smithy_query operation=ListSchemaVersions`).
const MIN_MAX_RESULTS: i64 = 1;
const MAX_MAX_RESULTS: i64 = 100;

/// The published maximum of `SchemaDefinition`: `SchemaDefinitionString`, length 1..170000
/// (`smithy_query operation=CreateSchema`).
const MAX_DEFINITION_LENGTH: usize = 170_000;

/// The schema set every suite here addresses.
const SCHEMA: &str = "payments";

/// An Avro record definition, the first version of the schema sets below.
const AVRO_BASE: &str =
    r#"{"type":"record","name":"Payment","fields":[{"name":"id","type":"string"}]}"#;

/// A JSON Schema document of exactly `length` characters: the document's own frame, padded
/// out with a description, so a row states only the length it is about.
fn json_definition_of_length(length: usize) -> String {
    let opening = r#"{"type":"object","description":""#;
    let closing = r#""}"#;
    let padding = length.saturating_sub(opening.len() + closing.len());
    let definition = format!("{opening}{}{closing}", "d".repeat(padding));
    assert_eq!(
        definition.len(),
        length,
        "the suite's own definition must be {length} characters"
    );
    definition
}

/// Create the schema set [`SCHEMA`] in the default registry, holding one version.
async fn given_schema(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateSchema",
        &json!({
            "SchemaName": SCHEMA,
            "DataFormat": "AVRO",
            "Compatibility": "NONE",
            "SchemaDefinition": AVRO_BASE,
        }),
    )
    .await;
}

/// A lookup that resolves no schema set reports the address it looked one up by.
///
/// The rows differ along the form the `SchemaId` addressed the set by: a registry and a
/// schema name, a schema name alone (which resolves the default registry, and so carries no
/// `RegistryName` of its own), the ARN of a schema this endpoint holds none of, and the ARN
/// of a schema of another Region.
#[tokio::test]
async fn a_listing_of_a_schema_set_this_endpoint_holds_none_of_reports_the_address() {
    let router = emulator();
    given_schema(&router).await;

    let local_arn = format!("arn:aws:glue:{REGION}:{ACCOUNT_ID}:schema/{DEFAULT_REGISTRY_NAME}/absent");
    let elsewhere_arn = format!(
        "arn:aws:glue:eu-west-1:{ACCOUNT_ID}:schema/{DEFAULT_REGISTRY_NAME}/{SCHEMA}"
    );
    // (case, the SchemaId the listing sends, the whole Message it earns)
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "a registry and a schema name the registry does not hold",
            json!({ "RegistryName": DEFAULT_REGISTRY_NAME, "SchemaName": "absent" }),
            messages::schema_not_found_at(Some(DEFAULT_REGISTRY_NAME), Some("absent"), None),
        ),
        (
            "a schema name alone",
            json!({ "SchemaName": "absent" }),
            messages::schema_not_found_at(None, Some("absent"), None),
        ),
        (
            "a schema name in another casing",
            json!({ "RegistryName": DEFAULT_REGISTRY_NAME, "SchemaName": "Payments" }),
            messages::schema_not_found_at(Some(DEFAULT_REGISTRY_NAME), Some("Payments"), None),
        ),
        (
            "a registry nothing created",
            json!({ "RegistryName": "absent-registry", "SchemaName": SCHEMA }),
            messages::schema_not_found_at(Some("absent-registry"), Some(SCHEMA), None),
        ),
        (
            "the ARN of a schema this endpoint holds none of",
            json!({ "SchemaArn": local_arn }),
            messages::schema_not_found_at(None, None, Some(&local_arn)),
        ),
        (
            "the ARN of a schema of another Region",
            json!({ "SchemaArn": elsewhere_arn }),
            messages::schema_not_found_at(None, None, Some(&elsewhere_arn)),
        ),
    ];

    for (case, schema_id, expected) in rows {
        let (status, body) = call(
            &router,
            "ListSchemaVersions",
            &json!({ "SchemaId": schema_id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case:?}: expected EntityNotFoundException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected the refusal to report the address looked up, got {body}"
        );
    }

    // The set that IS held is listed, so the refusals above are about the lookup and not
    // about the listing.
    let listed = ok(
        &router,
        "ListSchemaVersions",
        &json!({ "SchemaId": { "RegistryName": DEFAULT_REGISTRY_NAME, "SchemaName": SCHEMA } }),
    )
    .await;
    assert_eq!(
        listed["Schemas"].as_array().map(Vec::len),
        Some(1),
        "the schema set this endpoint holds must be listed, got {listed}"
    );
}

/// A page size outside the range its member's modelled type declares is the registry's own
/// input refusal, stating both ends of the range and the value sent; each published bound is
/// served.
///
/// The rows differ along the listing called and the page size asked for: one below the
/// minimum, a negative size, the smallest value the member's type can carry, one above the
/// maximum, and the largest value it can carry.
#[tokio::test]
async fn a_page_size_outside_the_published_range_is_the_registrys_own_refusal() {
    let router = emulator();
    given_schema(&router).await;

    // (the listing, the request members it needs beside `MaxResults`, whether this emulator
    //  serves the listing at all — `ListSchemas` is still scaffolded, so only its input
    //  refusal is observable)
    let listings: [(&str, Value, bool); 3] = [
        ("ListRegistries", json!({}), true),
        ("ListSchemas", json!({}), false),
        (
            "ListSchemaVersions",
            json!({ "SchemaId": { "RegistryName": DEFAULT_REGISTRY_NAME, "SchemaName": SCHEMA } }),
            true,
        ),
    ];
    let refused: [i64; 5] = [
        MIN_MAX_RESULTS - 1,
        -1,
        i64::from(i32::MIN),
        MAX_MAX_RESULTS + 1,
        i64::from(i32::MAX),
    ];

    for (listing, base, served) in &listings {
        for max_results in refused {
            let case = format!("{listing} with MaxResults {max_results}");
            let mut body = base.clone();
            body["MaxResults"] = json!(max_results);
            let (status, answered) = call(&router, listing, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status} with {answered}"
            );
            assert!(
                error_code(&answered).contains("InvalidInputException"),
                "case {case:?}: expected InvalidInputException, got {answered}"
            );
            assert_eq!(
                error_message(&answered),
                messages::max_results_outside_range(
                    MAX_MAX_RESULTS,
                    MIN_MAX_RESULTS,
                    &max_results.to_string()
                ),
                "case {case:?}: expected the registry's own sentence, got {answered}"
            );
        }

        // Each published bound is a page the service serves.
        if !served {
            continue;
        }
        for max_results in [MIN_MAX_RESULTS, MAX_MAX_RESULTS] {
            let case = format!("{listing} with MaxResults {max_results}");
            let mut body = base.clone();
            body["MaxResults"] = json!(max_results);
            let page = ok(&router, listing, &body).await;
            assert!(
                page.is_object(),
                "case {case:?}: a published bound must be served, got {page}"
            );
        }
    }

    // The bound the recorded refusal is about, served on the same schema set: `MaxResults` 1
    // lists its one version.
    let page = ok(
        &router,
        "ListSchemaVersions",
        &json!({
            "SchemaId": { "RegistryName": DEFAULT_REGISTRY_NAME, "SchemaName": SCHEMA },
            "MaxResults": 1,
        }),
    )
    .await;
    assert_eq!(
        page["Schemas"].as_array().map(Vec::len),
        Some(1),
        "case \"ListSchemaVersions with MaxResults 1\": the one version must be listed, got \
         {page}"
    );
}

/// A definition AT the published maximum is accepted and stored byte for byte; one past it is
/// the registry's own input refusal, naming the parameter and echoing nothing of the document
/// — and the schema name it was submitted under stays unread.
///
/// The rows differ along how far past the maximum the definition reaches: one character, and
/// far past the framework walk's own value-echo ceiling.
#[tokio::test]
async fn a_definition_outside_the_published_length_is_the_registrys_own_refusal() {
    let router = emulator();

    // The maximum itself is accepted, and the version reads back byte for byte.
    let at_maximum = json_definition_of_length(MAX_DEFINITION_LENGTH);
    let created = ok(
        &router,
        "CreateSchema",
        &json!({
            "SchemaName": "at-the-maximum",
            "DataFormat": "JSON",
            "Compatibility": "NONE",
            "SchemaDefinition": at_maximum,
        }),
    )
    .await;
    let version_id = created["SchemaVersionId"]
        .as_str()
        .unwrap_or_else(|| panic!("the create must report a version id, got {created}"));
    let read = ok(
        &router,
        "GetSchemaVersion",
        &json!({ "SchemaVersionId": version_id }),
    )
    .await;
    assert_eq!(
        read["SchemaDefinition"].as_str(),
        Some(at_maximum.as_str()),
        "case \"the published maximum\": the definition must read back byte for byte"
    );

    let expected = messages::parameter_length_outside_range("schemaDefinition");
    for (case, length) in [
        ("one character past the maximum", MAX_DEFINITION_LENGTH + 1),
        ("far past the maximum", MAX_DEFINITION_LENGTH + 1_024),
    ] {
        let over_long = json_definition_of_length(length);
        let schema = format!("over-long-{length}");
        let (status, refused) = call(
            &router,
            "CreateSchema",
            &json!({
                "SchemaName": schema,
                "DataFormat": "JSON",
                "Compatibility": "NONE",
                "SchemaDefinition": over_long,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with a body of {} bytes",
            refused.to_string().len()
        );
        assert!(
            error_code(&refused).contains("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {}",
            error_code(&refused)
        );
        assert_eq!(
            error_message(&refused),
            expected,
            "case {case:?}: expected the registry's own sentence, got {}",
            error_message(&refused)
        );

        // Nothing was stored: the name the refused create named stays unread, and that
        // refusal reports the address the read looked it up by.
        let (status, missed) = call(
            &router,
            "GetSchema",
            &json!({
                "SchemaId": { "RegistryName": DEFAULT_REGISTRY_NAME, "SchemaName": schema },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create stores nothing, got {status} with {missed}"
        );
        assert!(
            error_code(&missed).contains("EntityNotFoundException"),
            "case {case:?}: expected EntityNotFoundException, got {missed}"
        );
        assert_eq!(
            error_message(&missed),
            messages::schema_not_found_at(Some(DEFAULT_REGISTRY_NAME), Some(&schema), None),
            "case {case:?}: expected the miss to report the address looked up, got {missed}"
        );

        // The same member of the OTHER two operations that carry it earns the same refusal.
        for (operation, body) in [
            (
                "RegisterSchemaVersion",
                json!({
                    "SchemaId": { "SchemaName": "at-the-maximum" },
                    "SchemaDefinition": json_definition_of_length(length),
                }),
            ),
            (
                "CheckSchemaVersionValidity",
                json!({
                    "DataFormat": "JSON",
                    "SchemaDefinition": json_definition_of_length(length),
                }),
            ),
        ] {
            let case = format!("{operation}, {case}");
            let (status, refused) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected 400, got {status}"
            );
            assert!(
                error_code(&refused).contains("InvalidInputException"),
                "case {case:?}: expected InvalidInputException, got {}",
                error_code(&refused)
            );
            assert_eq!(
                error_message(&refused),
                expected,
                "case {case:?}: expected the registry's own sentence, got {}",
                error_message(&refused)
            );
        }
    }
}
