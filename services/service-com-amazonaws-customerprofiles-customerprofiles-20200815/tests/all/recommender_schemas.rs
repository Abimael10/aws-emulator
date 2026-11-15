// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `CreateRecommenderSchema` and the reader that serves what it
//! stored (`GetRecommenderSchema`).
//!
//! The rule under test: a recommender schema is a resource of its domain, identified by
//! the (domain, `RecommenderSchemaName`) pair, and it selects COLUMNS OF the domain's data
//! tables. A create whose dataset type the domain holds no data table for is refused
//! `BadRequestException` (400); otherwise it persists the dataset-type -> column-definitions
//! map the request carried, mints the schema's ARN once, and answers from the record it
//! stored, so the reader reports the same schema. A `Fields` key that names a dataset type
//! the service does not support, a column list outside the length the model declares for
//! one, a column defined twice, a feature type on a numeric column, a tag key outside the
//! bounds `TagKey` declares, and a name the domain already holds are each refused
//! `BadRequestException` (400) with nothing persisted; a domain, or a schema in it, that is
//! not stored is `ResourceNotFoundException` (404).
//!
//! Every case goes through the real router, so what is asserted is the wire behaviour a
//! client sees. The HTTP plumbing and the create helpers are this crate's one test
//! harness, in `tests/all/domain_profile_lifecycle.rs`.

use serde_json::{json, Value};

use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::{
    ARN_SERVICE, MAX_RECOMMENDER_SCHEMA_COLUMNS, SUPPORTED_RECOMMENDER_DATASET_TYPES,
};

use crate::domain_profile_lifecycle::{error_code, Emulator};
use crate::profile_object_types::error_message;

/// The region the test harness signs for, which is the region a minted ARN carries.
const REGION: &str = "us-east-1";

/// The longest a `RecommenderSchemaFieldList` may be: the model's `@length(max: 9)` on
/// `RecommenderSchemaFieldList`.
const MAX_FIELDS_PER_DATASET: usize = MAX_RECOMMENDER_SCHEMA_COLUMNS;

/// The service's refusal message for a domain that holds no backing data table for the
/// recommender data: fixed text, with no part of it taken from the request — the
/// `_webAnalytics` in it is literal and not the dataset type the request named.
const NO_DATA_TABLE_MESSAGE: &str = "Unable to create recommender: Required data table \
     for _webAnalytics is missing. Before creating a recommender, please ensure you have: \
     1. Ingested user interaction data (such as clicks, purchases, or views) and 2. \
     Uploaded your product/item catalog data. The system will automatically create \
     necessary tables when you import this data. Refer documentation for details: \
     https://docs.aws.amazon.com/connect/latest/adminguide/predictive-insights-get-started.html";

/// Define the domain's data table for `dataset_type` — the object type the domain ingests
/// that data through — asserting it was stored.
async fn define_data_table(emulator: &Emulator, domain: &str, dataset_type: &str) {
    let (status, body) = emulator
        .send(
            "PUT",
            &format!("/domains/{domain}/object-types/{dataset_type}"),
            Some(json!({ "Description": format!("the {dataset_type} data table") })),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {dataset_type:?}: expected PutProfileObjectType 200, got {status} {body:?}"
    );
}

/// A domain holding a data table for every dataset type the service supports, so a create
/// over any of them has columns to select.
async fn domain_with_data_tables(emulator: &Emulator, domain: &str, expiration_days: u32) {
    emulator.create_domain(domain, expiration_days).await;
    for dataset_type in SUPPORTED_RECOMMENDER_DATASET_TYPES {
        define_data_table(emulator, domain, dataset_type).await;
    }
}

/// `CreateRecommenderSchema` over the wire:
/// `POST /domains/{DomainName}/recommender-schemas/{RecommenderSchemaName}`.
async fn create(emulator: &Emulator, domain: &str, name: &str, body: Value) -> (u16, Value) {
    emulator
        .post(
            &format!("/domains/{domain}/recommender-schemas/{name}"),
            body,
        )
        .await
}

/// `GetRecommenderSchema` over the wire:
/// `GET /domains/{DomainName}/recommender-schemas/{RecommenderSchemaName}`.
async fn get(emulator: &Emulator, domain: &str, name: &str) -> (u16, Value) {
    emulator
        .send(
            "GET",
            &format!("/domains/{domain}/recommender-schemas/{name}"),
            None,
        )
        .await
}

/// `ListRecommenderSchemas` over the wire: `GET /domains/{DomainName}/recommender-schemas`,
/// with the paging parameters the model binds to the query string (`max-results`,
/// `next-token`) appended as `query`.
async fn list(emulator: &Emulator, domain: &str, query: &str) -> (u16, Value) {
    emulator
        .send(
            "GET",
            &format!("/domains/{domain}/recommender-schemas{query}"),
            None,
        )
        .await
}

/// The `RecommenderSchemas` of a listing, as a vector of summaries.
fn listed(body: &Value) -> Vec<Value> {
    body.get("RecommenderSchemas")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `RecommenderSchemaName` of every summary in a listing, in the order listed.
fn listed_names(body: &Value) -> Vec<String> {
    listed(body)
        .iter()
        .map(|schema| {
            schema
                .get("RecommenderSchemaName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// A column definition: the required target field name, plus whichever of the two
/// optional settings the case carries.
fn field(target: &str, content_type: Option<&str>, feature_type: Option<&str>) -> Value {
    let mut column = json!({ "TargetFieldName": target });
    if let Some(content_type) = content_type {
        column["ContentType"] = json!(content_type);
    }
    if let Some(feature_type) = feature_type {
        column["FeatureType"] = json!(feature_type);
    }
    column
}

/// `n` column definitions, each naming a distinct target field.
fn fields(n: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| field(&format!("Attributes.Column{i}"), None, None))
            .collect(),
    )
}

/// The requests the success table creates, spanning what the rule generalises over: which
/// supported dataset types the schema covers, how many columns each one carries, which
/// optional column settings are set, and whether the request carries tags.
fn success_rows() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "one dataset type with one column",
            "WebOnly",
            json!({
                "Fields": {
                    "_webAnalytics": [field("Attributes.PageUrl", None, None)],
                },
            }),
        ),
        (
            "the other dataset type, with both optional column settings set",
            "catalog_only-1",
            json!({
                "Fields": {
                    "_catalogItem": [
                        field("Attributes.MealTime", Some("STRING"), Some("CATEGORICAL")),
                        field("Attributes.Price", Some("NUMBER"), None),
                        field("Location.City", None, Some("TEXTUAL")),
                    ],
                },
            }),
        ),
        (
            "both dataset types at once, and tags",
            "BothDatasets",
            json!({
                "Fields": {
                    "_webAnalytics": [field("Attributes.PageUrl", Some("STRING"), None)],
                    "_catalogItem": [field("Attributes.Sku", Some("STRING"), None)],
                },
                "Tags": { "env": "dev", "team": "recs" },
            }),
        ),
        (
            "the longest column list the model admits",
            "MaxColumns",
            json!({ "Fields": { "_catalogItem": fields(MAX_FIELDS_PER_DATASET) } }),
        ),
    ]
}

/// Every stored schema round trips: the create answers from the record it stored and the
/// reader reports that same record, over a table of requests spanning the dataset types,
/// the column counts and the optional members.
#[tokio::test]
async fn a_created_recommender_schema_round_trips_through_the_reader() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "recdom", 90).await;

    for (case, name, request) in success_rows() {
        let (status, created) = create(&emulator, "recdom", name, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateRecommenderSchema 200, got {status} {created:?}"
        );
        assert_eq!(
            created.get("RecommenderSchemaName").and_then(Value::as_str),
            Some(name),
            "case {case:?}: expected RecommenderSchemaName {name:?}, got {:?}",
            created.get("RecommenderSchemaName")
        );
        assert_eq!(
            created.get("Status").and_then(Value::as_str),
            Some("ACTIVE"),
            "case {case:?}: expected Status ACTIVE, got {:?}",
            created.get("Status")
        );
        assert_eq!(
            created.get("Fields"),
            request.get("Fields"),
            "case {case:?}: expected the Fields the request carried, got {:?}",
            created.get("Fields")
        );
        assert_eq!(
            created.get("Tags"),
            request.get("Tags"),
            "case {case:?}: expected the Tags the request carried, got {:?}",
            created.get("Tags")
        );
        let expected_arn = engine::emulator_arn(
            ARN_SERVICE,
            REGION,
            &format!("domains/recdom/recommender-schemas/{name}"),
        );
        assert_eq!(
            created.get("RecommenderSchemaArn").and_then(Value::as_str),
            Some(expected_arn.as_str()),
            "case {case:?}: expected RecommenderSchemaArn {expected_arn:?}, got {:?}",
            created.get("RecommenderSchemaArn")
        );
        let created_at = created.get("CreatedAt").cloned().unwrap_or(Value::Null);
        assert!(
            created_at.is_number(),
            "case {case:?}: expected a CreatedAt timestamp, got {created_at:?}"
        );

        let (status, read) = get(&emulator, "recdom", name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRecommenderSchema 200, got {status} {read:?}"
        );
        assert_eq!(
            read.get("RecommenderSchemaName").and_then(Value::as_str),
            Some(name),
            "case {case:?}: expected the stored name {name:?}, got {:?}",
            read.get("RecommenderSchemaName")
        );
        assert_eq!(
            read.get("Fields"),
            request.get("Fields"),
            "case {case:?}: expected the stored Fields, got {:?}",
            read.get("Fields")
        );
        assert_eq!(
            read.get("Status").and_then(Value::as_str),
            Some("ACTIVE"),
            "case {case:?}: expected Status ACTIVE on the read, got {:?}",
            read.get("Status")
        );
        assert_eq!(
            read.get("CreatedAt"),
            Some(&created_at),
            "case {case:?}: expected the CreatedAt the create reported, got {:?}",
            read.get("CreatedAt")
        );

        // A read changes nothing a later caller sees: the second read answers exactly
        // what the first one did.
        let (status, again) = get(&emulator, "recdom", name).await;
        assert_eq!(
            (status, &again),
            (200, &read),
            "case {case:?}: expected the read to be repeatable, got {status} {again:?}"
        );
    }
}

/// A `Fields` key naming a dataset type the service does not define is refused, and the
/// refusal stores nothing. The rows differ in HOW the key misses the supported set:
/// unknown word, wrong case, missing or extra characters, and the empty key the map's
/// unconstrained `String` key admits.
#[tokio::test]
async fn an_unsupported_dataset_type_is_refused_and_persists_nothing() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "recdom", 90).await;

    let rows = [
        ("an unknown dataset type", "_clickstream"),
        ("the supported name without its leading underscore", "webAnalytics"),
        ("the supported name in another case", "_WebAnalytics"),
        ("the supported name with a trailing character", "_catalogItems"),
        ("the empty key the map admits", ""),
    ];

    for (index, (case, dataset_type)) in rows.into_iter().enumerate() {
        let name = format!("Unsupported{index}");
        let (status, body) = create(
            &emulator,
            "recdom",
            &name,
            json!({
                "Fields": { dataset_type: [field("Attributes.Column", None, None)] },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 for dataset type {dataset_type:?}, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {:?}",
            error_code(&body)
        );
        let expected = format!(
            "Fields contains an unsupported dataset type {dataset_type}. \
             The supported dataset types are {}",
            SUPPORTED_RECOMMENDER_DATASET_TYPES.join(", ")
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&body)
        );

        let (status, read) = get(&emulator, "recdom", &name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused schema to be absent, got {status} {read:?}"
        );
    }

    // A request that mixes a supported dataset type with an unsupported one is refused
    // for the unsupported one: the whole schema is rejected, not the half of it that was
    // well formed.
    let (status, body) = create(
        &emulator,
        "recdom",
        "Mixed",
        json!({
            "Fields": {
                "_webAnalytics": [field("Attributes.PageUrl", None, None)],
                "_clickstream": [field("Attributes.Column", None, None)],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"one supported and one unsupported dataset type\": expected 400, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "BadRequestException",
        "case \"one supported and one unsupported dataset type\": expected BadRequestException, got {:?}",
        error_code(&body)
    );
    let (status, read) = get(&emulator, "recdom", "Mixed").await;
    assert_eq!(
        status, 404,
        "case \"one supported and one unsupported dataset type\": expected nothing stored, got {status} {read:?}"
    );
}

/// Every dataset type the service documents as supported is accepted, each on its own
/// schema — the catalog the refusal above is the complement of. The source of the list is
/// `CreateRecommenderSchemaRequest$Fields`, which names `_webAnalytics` and `_catalogItem`.
#[tokio::test]
async fn every_supported_dataset_type_is_accepted_on_its_own() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "recdom", 90).await;

    for (index, dataset_type) in SUPPORTED_RECOMMENDER_DATASET_TYPES.into_iter().enumerate() {
        let name = format!("Supported{index}");
        let request = json!({
            "Fields": { dataset_type: [field("Attributes.Column", None, None)] },
        });
        let (status, created) = create(&emulator, "recdom", &name, request.clone()).await;
        assert_eq!(
            status, 200,
            "case {dataset_type:?}: expected 200, got {status} {created:?}"
        );
        let (status, read) = get(&emulator, "recdom", &name).await;
        assert_eq!(
            status, 200,
            "case {dataset_type:?}: expected the schema to be readable, got {status} {read:?}"
        );
        assert_eq!(
            read.get("Fields"),
            request.get("Fields"),
            "case {dataset_type:?}: expected the stored Fields, got {:?}",
            read.get("Fields")
        );
    }
}

/// The name identifies the schema within its domain: the first create of a name wins and
/// every later create of it is refused without changing what the winner stored, while the
/// same name in another domain is a different schema.
#[tokio::test]
async fn the_name_identifies_the_schema_within_its_domain() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "first", 90).await;
    domain_with_data_tables(&emulator, "second", 120).await;

    let winner = json!({ "Fields": { "_webAnalytics": [field("Attributes.PageUrl", None, None)] } });
    let (status, created) = create(&emulator, "first", "Shared", winner.clone()).await;
    assert_eq!(
        status, 200,
        "case \"the first create of a name\": expected 200, got {status} {created:?}"
    );

    for (case, loser) in [
        (
            "a later create carrying other columns",
            json!({ "Fields": { "_catalogItem": [field("Attributes.Sku", None, None)] } }),
        ),
        (
            "a later create carrying the same columns",
            winner.clone(),
        ),
    ] {
        let (status, body) = create(&emulator, "first", "Shared", loser).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 for an existing name, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {:?}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            "Recommender schema with name Shared already exists",
            "case {case:?}: expected the already-exists sentence, got {:?}",
            error_message(&body)
        );
        let (status, read) = get(&emulator, "first", "Shared").await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the winner to still be readable, got {status} {read:?}"
        );
        assert_eq!(
            read.get("Fields"),
            winner.get("Fields"),
            "case {case:?}: expected the winner's Fields to be unchanged, got {:?}",
            read.get("Fields")
        );
    }

    // The identity is the (domain, name) PAIR, so the same name in another domain is a
    // different schema, with its own columns and its own ARN.
    let other = json!({ "Fields": { "_catalogItem": [field("Attributes.Sku", None, None)] } });
    let (status, created) = create(&emulator, "second", "Shared", other.clone()).await;
    assert_eq!(
        status, 200,
        "case \"the same name in another domain\": expected 200, got {status} {created:?}"
    );
    let expected_arn = engine::emulator_arn(
        ARN_SERVICE,
        REGION,
        "domains/second/recommender-schemas/Shared",
    );
    assert_eq!(
        created.get("RecommenderSchemaArn").and_then(Value::as_str),
        Some(expected_arn.as_str()),
        "case \"the same name in another domain\": expected {expected_arn:?}, got {:?}",
        created.get("RecommenderSchemaArn")
    );
    let (_, read) = get(&emulator, "second", "Shared").await;
    assert_eq!(
        read.get("Fields"),
        other.get("Fields"),
        "case \"the same name in another domain\": expected its own Fields, got {:?}",
        read.get("Fields")
    );
    let (_, first) = get(&emulator, "first", "Shared").await;
    assert_eq!(
        first.get("Fields"),
        winner.get("Fields"),
        "case \"the same name in another domain\": expected the first domain's schema untouched, got {:?}",
        first.get("Fields")
    );
}

/// A domain, or a schema in it, that is not stored is reported absent rather than
/// invented, on both the create and the read path.
#[tokio::test]
async fn an_absent_domain_or_schema_is_reported_absent() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "recdom", 90).await;

    let request = json!({ "Fields": { "_webAnalytics": [field("Attributes.PageUrl", None, None)] } });
    for (case, domain) in [
        ("a domain that was never created", "nosuchdomain"),
        ("a domain whose name only resembles a stored one", "recdom2"),
    ] {
        let (status, body) = create(&emulator, domain, "Schema", request.clone()).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 for domain {domain:?}, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            error_code(&body)
        );
        // The wording the service answers an absent domain with, shared by every
        // domain-scoped operation (`crate::state::domain_not_found`).
        let expected = format!("Domain {domain} is not found");
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&body)
        );

        let (status, body) = get(&emulator, domain, "Schema").await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 reading in domain {domain:?}, got {status} {body:?}"
        );
    }

    create(&emulator, "recdom", "Stored", request).await;
    for (case, name) in [
        ("a schema that was never created", "NeverCreated"),
        ("a name that only resembles a stored one", "Stored1"),
    ] {
        let (status, body) = get(&emulator, "recdom", name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 for schema {name:?}, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            error_code(&body)
        );
        let expected = format!("RecommenderSchema {name} is not found");
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&body)
        );
    }
}

/// A schema selects columns OF a data table, so a dataset type the domain holds no data
/// table for is refused and nothing is stored — and the same request is admitted once the
/// domain defines that table.
///
/// The refusal states the DOMAIN's readiness, not one dataset type's, so its message is
/// ONE fixed sentence that carries nothing from the request: the rows span which data
/// tables the domain already holds (none, either one of them) and which dataset types the
/// request names, and every row expects the identical text — including the rows whose
/// request never mentions `_webAnalytics`, the token the sentence names literally.
#[tokio::test]
async fn a_dataset_type_with_no_data_table_is_refused_until_the_domain_defines_one() {
    let emulator = Emulator::new();
    emulator.create_domain("tabled", 90).await;

    let web = || json!({ "Fields": { "_webAnalytics": [field("Attributes.PageUrl", None, None)] } });
    let catalog = || json!({ "Fields": { "_catalogItem": [field("Attributes.Sku", None, None)] } });

    let rows: [(&str, &[&str], Value); 5] = [
        ("no table at all, the request naming the web-analytics one", &[], web()),
        ("no table at all, the request naming the catalog one", &[], catalog()),
        (
            "only the catalog table defined, the request naming the web-analytics one",
            &["_catalogItem"],
            web(),
        ),
        (
            "only the web-analytics table defined, the request naming the catalog one",
            &["_webAnalytics"],
            catalog(),
        ),
        (
            "both dataset types named, with neither table defined",
            &[],
            json!({
                "Fields": {
                    "_catalogItem": [field("Attributes.Sku", None, None)],
                    "_webAnalytics": [field("Attributes.PageUrl", None, None)],
                },
            }),
        ),
    ];

    for (index, (case, already_defined, request)) in rows.into_iter().enumerate() {
        let domain = format!("tableless{index}");
        emulator.create_domain(&domain, 90).await;
        for dataset_type in already_defined {
            define_data_table(&emulator, &domain, dataset_type).await;
        }

        let name = format!("NeedsTable{index}");
        let (status, body) = create(&emulator, &domain, &name, request.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 while the domain holds no data table, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {:?}",
            error_code(&body)
        );
        // The sentence is written out here rather than taken from the handler's own
        // constant, so a change to that wording has to be made in both places
        // deliberately. It is the service's remediation text for a domain that has not
        // ingested the recommender source data, and every character of it is fixed.
        assert_eq!(
            error_message(&body),
            NO_DATA_TABLE_MESSAGE,
            "case {case:?}: expected {NO_DATA_TABLE_MESSAGE:?}, got {:?}",
            error_message(&body)
        );
        let (status, read) = get(&emulator, &domain, &name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused schema to be absent, got {status} {read:?}"
        );

        // The very same request, in a domain that holds a data table for every dataset
        // type it names.
        for dataset_type in SUPPORTED_RECOMMENDER_DATASET_TYPES {
            define_data_table(&emulator, "tabled", dataset_type).await;
        }
        let (status, created) = create(&emulator, "tabled", &name, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 once the data tables are defined, got {status} {created:?}"
        );
    }
}

/// The bounds the model declares on one dataset type's column list, and the two rules a
/// column list has to satisfy that no modelled constraint states.
///
/// The list's own `@length` (1..9, from `RecommenderSchemaFieldList`) is checked by the
/// handler because the boundary walk cannot reach it: the list is a map value, and a
/// collection that is itself a collection element is walked against its enclosing member's
/// schema. Each published bound is exercised, and one count past it.
#[tokio::test]
async fn a_column_list_outside_its_bounds_or_rules_is_refused() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "recdom", 90).await;

    let refused: [(&str, Value); 4] = [
        (
            "no column at all, one under the published minimum",
            json!({ "Fields": { "_webAnalytics": [] } }),
        ),
        (
            "one column past the published maximum",
            json!({ "Fields": { "_catalogItem": fields(MAX_FIELDS_PER_DATASET + 1) } }),
        ),
        (
            "the same column defined twice",
            json!({
                "Fields": {
                    "_webAnalytics": [
                        field("Attributes.PageUrl", None, None),
                        field("Attributes.PageUrl", Some("STRING"), None),
                    ],
                },
            }),
        ),
        (
            "a feature type on a column holding numbers",
            json!({
                "Fields": {
                    "_catalogItem": [field("Attributes.Price", Some("NUMBER"), Some("CATEGORICAL"))],
                },
            }),
        ),
    ];

    for (index, (case, request)) in refused.into_iter().enumerate() {
        let name = format!("Refused{index}");
        let (status, body) = create(&emulator, "recdom", &name, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {:?}",
            error_code(&body)
        );
        let (status, read) = get(&emulator, "recdom", &name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused schema to be absent, got {status} {read:?}"
        );
    }

    // Each published bound itself is admitted, and so is a numeric column that declares no
    // feature type.
    let admitted: [(&str, Value); 3] = [
        (
            "exactly the published minimum",
            json!({ "Fields": { "_webAnalytics": fields(1) } }),
        ),
        (
            "exactly the published maximum",
            json!({ "Fields": { "_catalogItem": fields(MAX_FIELDS_PER_DATASET) } }),
        ),
        (
            "a numeric column with no feature type",
            json!({
                "Fields": { "_catalogItem": [field("Attributes.Price", Some("NUMBER"), None)] },
            }),
        ),
    ];

    for (index, (case, request)) in admitted.into_iter().enumerate() {
        let name = format!("Admitted{index}");
        let (status, created) = create(&emulator, "recdom", &name, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {created:?}"
        );
    }
}

/// A tag key is the one half of a tag map the boundary's constraint walk does not reach,
/// so the bounds `TagKey` declares are applied by the handler: the rows span the reserved
/// prefix, a character the pattern excludes, the empty key and a key past the declared
/// maximum, with a key at that maximum admitted.
#[tokio::test]
async fn a_tag_key_outside_the_modelled_bounds_is_refused() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "recdom", 90).await;

    let request = |key: &str| {
        json!({
            "Fields": { "_webAnalytics": [field("Attributes.PageUrl", None, None)] },
            "Tags": { key: "value" },
        })
    };

    let refused = [
        ("the prefix AWS reserves", "aws:cost-centre".to_string()),
        ("a character the pattern excludes", "team space".to_string()),
        ("the empty key", String::new()),
        ("one character past the declared maximum", "k".repeat(129)),
    ];
    for (index, (case, key)) in refused.into_iter().enumerate() {
        let name = format!("Tagged{index}");
        let (status, body) = create(&emulator, "recdom", &name, request(&key)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 for tag key {key:?}, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {:?}",
            error_code(&body)
        );
        let (status, read) = get(&emulator, "recdom", &name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused schema to be absent, got {status} {read:?}"
        );
    }

    for (index, (case, key)) in [
        ("a key at the declared maximum", "k".repeat(128)),
        (
            "a key of the characters the pattern admits",
            "team-1_a.b:c/d".to_string(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("TaggedOk{index}");
        let (status, created) = create(&emulator, "recdom", &name, request(&key)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 for tag key {key:?}, got {status} {created:?}"
        );
    }
}

/// The listing reports the schemas of the domain it names and nothing else: every stored
/// schema, each summary carrying what the create stored, a schema whose create was refused
/// omitted, a schema of another domain omitted, and a domain that holds none answered with
/// an empty list rather than an absence.
#[tokio::test]
async fn the_listing_reports_every_stored_schema_of_its_domain() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "recdom", 90).await;
    domain_with_data_tables(&emulator, "other", 90).await;
    domain_with_data_tables(&emulator, "empty", 90).await;

    // The rows differ along what a summary reports: which dataset types the schema
    // covers, how many columns each carries, and which optional column settings are set.
    let stored: [(&str, Value); 3] = [
        (
            "Beta",
            json!({ "Fields": { "_webAnalytics": [field("Attributes.PageUrl", None, None)] } }),
        ),
        (
            "Alpha",
            json!({
                "Fields": {
                    "_catalogItem": [
                        field("Attributes.Sku", Some("STRING"), Some("CATEGORICAL")),
                        field("Attributes.Price", Some("NUMBER"), None),
                    ],
                },
            }),
        ),
        (
            "Gamma",
            json!({
                "Fields": {
                    "_catalogItem": [field("Attributes.Sku", None, None)],
                    "_webAnalytics": fields(MAX_FIELDS_PER_DATASET),
                },
            }),
        ),
    ];

    let mut created_bodies = Vec::new();
    for (name, request) in stored.clone() {
        let (status, created) = create(&emulator, "recdom", name, request).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected 200, got {status} {created:?}"
        );
        created_bodies.push((name, created));
    }

    // A refused create stores nothing, so the listing does not report it.
    let (status, refused) = create(
        &emulator,
        "recdom",
        "Refused",
        json!({ "Fields": { "_clickstream": [field("Attributes.PageUrl", None, None)] } }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a refused create\": expected 400, got {status} {refused:?}"
    );
    // A schema of another domain belongs to that domain only.
    create(
        &emulator,
        "other",
        "Elsewhere",
        json!({ "Fields": { "_webAnalytics": [field("Attributes.PageUrl", None, None)] } }),
    )
    .await;

    let (status, body) = list(&emulator, "recdom", "").await;
    assert_eq!(
        status, 200,
        "case \"the whole listing\": expected 200, got {status} {body:?}"
    );
    assert_eq!(
        listed_names(&body),
        vec!["Alpha".to_string(), "Beta".to_string(), "Gamma".to_string()],
        "case \"the whole listing\": expected the stored names in name order, got {:?}",
        listed_names(&body)
    );
    assert!(
        body.get("NextToken").is_none_or(Value::is_null),
        "case \"the whole listing\": expected no continuation token, got {:?}",
        body.get("NextToken")
    );

    // Each summary reports what the create stored, for every member the summary carries.
    for (name, created) in &created_bodies {
        let summary = listed(&body)
            .into_iter()
            .find(|schema| schema.get("RecommenderSchemaName").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("case {name:?}: the listing omits the stored schema"));
        for member in ["Fields", "CreatedAt", "Status"] {
            assert_eq!(
                summary.get(member),
                created.get(member),
                "case {name:?}: expected the listed {member} to be the stored one {:?}, got {:?}",
                created.get(member),
                summary.get(member)
            );
        }
    }

    let (status, body) = list(&emulator, "other", "").await;
    assert_eq!(
        status, 200,
        "case \"the other domain\": expected 200, got {status} {body:?}"
    );
    assert_eq!(
        listed_names(&body),
        vec!["Elsewhere".to_string()],
        "case \"the other domain\": expected only its own schema, got {:?}",
        listed_names(&body)
    );

    let (status, body) = list(&emulator, "empty", "").await;
    assert_eq!(
        status, 200,
        "case \"a domain holding no schema\": expected 200, got {status} {body:?}"
    );
    assert_eq!(
        listed(&body).len(),
        0,
        "case \"a domain holding no schema\": expected an empty list, got {body:?}"
    );

    let (status, body) = list(&emulator, "nosuchdomain", "").await;
    assert_eq!(
        status, 404,
        "case \"a domain that was never created\": expected 404, got {status} {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "case \"a domain that was never created\": expected ResourceNotFoundException, got {:?}",
        error_code(&body)
    );
}

/// Paging the listing walks every stored schema exactly once, in name order, whatever page
/// size the caller asks for: the rows span a page size under, at and over the number of
/// stored schemas, and the published bounds of `MaxResults`.
#[tokio::test]
async fn paging_the_listing_walks_every_schema_exactly_once() {
    let emulator = Emulator::new();
    domain_with_data_tables(&emulator, "recdom", 90).await;

    let names: Vec<String> = (0..5).map(|i| format!("Schema{i}")).collect();
    for name in &names {
        let (status, created) = create(
            &emulator,
            "recdom",
            name,
            json!({ "Fields": { "_webAnalytics": [field("Attributes.PageUrl", None, None)] } }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected 200, got {status} {created:?}"
        );
    }

    // `MaxResults` is `maxSize100` (range 1..100), so 1 and 100 are its published bounds.
    for page_size in [1, 2, 4, 5, 6, 100] {
        let mut walked: Vec<String> = Vec::new();
        let mut query = format!("?max-results={page_size}");
        let mut pages = 0;
        loop {
            let (status, body) = list(&emulator, "recdom", &query).await;
            assert_eq!(
                status, 200,
                "case {page_size}: expected 200 on page {pages}, got {status} {body:?}"
            );
            let page = listed_names(&body);
            assert!(
                page.len() <= page_size,
                "case {page_size}: expected at most {page_size} per page, got {:?}",
                page
            );
            walked.extend(page);
            pages += 1;
            match body.get("NextToken").and_then(Value::as_str) {
                Some(token) => query = format!("?max-results={page_size}&next-token={token}"),
                None => break,
            }
            assert!(
                pages <= names.len() + 1,
                "case {page_size}: paging did not terminate, walked {walked:?}"
            );
        }
        assert_eq!(
            walked, names,
            "case {page_size}: expected every schema once in name order, got {walked:?}"
        );
    }

    // A page size past the published maximum is refused by the modelled range, with
    // nothing listed.
    let (status, body) = list(&emulator, "recdom", "?max-results=101").await;
    assert_eq!(
        status, 400,
        "case \"one past the published maximum\": expected 400, got {status} {body:?}"
    );
    let (status, body) = list(&emulator, "recdom", "?max-results=0").await;
    assert_eq!(
        status, 400,
        "case \"one under the published minimum\": expected 400, got {status} {body:?}"
    );
}
