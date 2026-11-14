// SPDX-License-Identifier: CC0-1.0

//! What a read of this service reports for a value that was never set, and for one
//! the service itself derives, driven over HTTP through the same
//! `MultiServiceRouter` the emulator boots with.
//!
//! Two rules, both about the projection a read writes rather than about any one
//! operation:
//!
//! 1. A response never carries a value a member's own constraints forbid. Every
//!    description this model declares targets `Description`, whose `@length`
//!    minimum of 1 means the empty string is not one of its values
//!    (`smithy_query(query="shape", shape="Description")`), while each read
//!    declares its description `@required` even though the member that writes it
//!    is optional on every create. So a resource created without a description is
//!    read back with NO description member — in every read that reports one, and
//!    in the summaries a listing reports beside them — and a resource created with
//!    one reads it back unchanged.
//! 2. A stored data point always reports a quality.
//!    `AssetPropertyValue$quality` is optional on the way in and is the `Quality`
//!    enum (`smithy_query(query="shape", shape="Quality")` — GOOD | BAD |
//!    UNCERTAIN), so the service derives GOOD on ingest for a TQV that names none
//!    and every read of that point names one of the three values.
//!
//! Each table spans what its rule generalises over — which resource carries the
//! description and which read reports it, the description's own length on both
//! published `@length` bounds, and the quality over the whole enum plus the
//! omitted form read back through three different readers — rather than the one
//! case that was recorded.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_lifecycle::{call, router};
use crate::support::now_seconds;

/// Where `BatchPutAssetPropertyValue` is bound: `POST /properties`.
const BATCH_PUT: &str = "/properties";

/// Where `GetAssetPropertyValue` is bound: `GET /properties/latest`.
const LATEST: &str = "/properties/latest";

/// Where `GetAssetPropertyValueHistory` is bound: `GET /properties/history`.
const HISTORY: &str = "/properties/history";

/// Where `BatchGetAssetPropertyValue` is bound: `POST /properties/batch/latest`.
const BATCH_LATEST: &str = "/properties/batch/latest";

/// The `Quality` the service derives for a TQV ingested without one: an ordinary
/// reading is GOOD, and `Quality` admits no other stand-in for "not stated".
const DERIVED_QUALITY: &str = "GOOD";

/// How long a poll for an owed transition waits before giving up. Generous: it
/// bounds a transition the store owes within a few hundred milliseconds, and
/// nothing here asserts on how long that took.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits between looks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `@length` maximum of `Description`, from
/// `smithy_query(query="shape", shape="Description")` — min 1, max 2048.
const DESCRIPTION_MAX_LENGTH: usize = 2048;

/// Which resource carries the description a row reads back, and therefore which
/// create writes it and which read reports it.
#[derive(Debug, Clone, Copy)]
enum Described {
    /// `CreateAssetModel` -> `DescribeAssetModel`.
    AssetModel,
    /// `CreateAsset` -> `DescribeAsset`.
    Asset,
    /// `CreateAssetModelCompositeModel` -> `DescribeAssetModelCompositeModel`.
    ModelCompositeModel,
    /// The same composite model as an asset's instance of it:
    /// `DescribeAssetCompositeModel`.
    AssetCompositeModel,
    /// `CreateDataset` -> `DescribeDataset`.
    Dataset,
}

impl Described {
    /// The response member this resource's read reports its description as.
    fn member(self) -> &'static str {
        match self {
            Described::AssetModel => "assetModelDescription",
            Described::Asset => "assetDescription",
            Described::ModelCompositeModel => "assetModelCompositeModelDescription",
            Described::AssetCompositeModel => "assetCompositeModelDescription",
            Described::Dataset => "datasetDescription",
        }
    }
}

/// Every resource of this model whose read reports a description of its own.
const DESCRIBED: [Described; 5] = [
    Described::AssetModel,
    Described::Asset,
    Described::ModelCompositeModel,
    Described::AssetCompositeModel,
    Described::Dataset,
];

/// Insert `member` into `body` when `value` is `Some`, leaving the request without
/// it otherwise — the difference the rule is about.
fn with_member(mut body: Value, member: &str, value: Option<&str>) -> Value {
    if let (Some(object), Some(value)) = (body.as_object_mut(), value) {
        object.insert(member.to_string(), json!(value));
    }
    body
}

/// Create the resource `kind` names, with `description` if the row supplies one,
/// and answer the body its own read reports for it.
async fn created_and_read(
    router: &MultiServiceRouter,
    kind: Described,
    label: &str,
    description: Option<&str>,
) -> Value {
    match kind {
        Described::AssetModel => {
            let id = create_model(router, label, description).await;
            read(router, &format!("/asset-models/{id}")).await
        }
        Described::Asset => {
            let model_id = create_model(router, &format!("{label}-model"), None).await;
            let body = with_member(
                json!({ "assetName": label, "assetModelId": model_id }),
                "assetDescription",
                description,
            );
            let id = accepted(router, "POST", "/assets", body, "assetId").await;
            read(router, &format!("/assets/{id}")).await
        }
        Described::ModelCompositeModel => {
            let model_id = create_model(router, &format!("{label}-model"), None).await;
            let composite_id = create_composite(router, &model_id, label, description).await;
            read(
                router,
                &format!("/asset-models/{model_id}/composite-models/{composite_id}"),
            )
            .await
        }
        Described::AssetCompositeModel => {
            let model_id = create_model(router, &format!("{label}-model"), None).await;
            let composite_id = create_composite(router, &model_id, label, description).await;
            let asset_id = accepted(
                router,
                "POST",
                "/assets",
                json!({ "assetName": label, "assetModelId": model_id }),
                "assetId",
            )
            .await;
            read(
                router,
                &format!("/assets/{asset_id}/composite-models/{composite_id}"),
            )
            .await
        }
        Described::Dataset => {
            let body = with_member(
                json!({
                    "datasetName": label,
                    "datasetSource": { "sourceType": "SITEWISE", "sourceFormat": "TIMESERIES" },
                }),
                "datasetDescription",
                description,
            );
            let id = accepted(router, "POST", "/datasets", body, "datasetId").await;
            read(router, &format!("/datasets/{id}")).await
        }
    }
}

/// Send a create and answer the id it minted, which it must accept with 202.
async fn accepted(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Value,
    id_member: &str,
) -> String {
    let (status, created) = call(router, method, uri, Some(body)).await;
    assert_eq!(
        status, 202,
        "{method} {uri}: expected the create to be accepted, got {status} {created}"
    );
    created[id_member]
        .as_str()
        .unwrap_or_else(|| panic!("{method} {uri}: no {id_member} in {created}"))
        .to_string()
}

/// `GET uri`, which must answer 200.
async fn read(router: &MultiServiceRouter, uri: &str) -> Value {
    let (status, body) = call(router, "GET", uri, None).await;
    assert_eq!(status, 200, "GET {uri}: expected 200, got {status} {body}");
    body
}

/// Create an asset model named `name`, described or not, and answer its id.
async fn create_model(
    router: &MultiServiceRouter,
    name: &str,
    description: Option<&str>,
) -> String {
    let body = with_member(
        json!({ "assetModelName": name }),
        "assetModelDescription",
        description,
    );
    accepted(router, "POST", "/asset-models", body, "assetModelId").await
}

/// Add a `CUSTOM` composite model to `model_id`, described or not, and answer its
/// id.
async fn create_composite(
    router: &MultiServiceRouter,
    model_id: &str,
    name: &str,
    description: Option<&str>,
) -> String {
    let body = with_member(
        json!({
            "assetModelCompositeModelName": name,
            "assetModelCompositeModelType": "CUSTOM",
        }),
        "assetModelCompositeModelDescription",
        description,
    );
    accepted(
        router,
        "POST",
        &format!("/asset-models/{model_id}/composite-models"),
        body,
        "assetModelCompositeModelId",
    )
    .await
}

/// Assert that `body` reports `member` as `expected`: the text itself, or no
/// member at all when the resource has no value for it.
fn reports(case: &str, body: &Value, member: &str, expected: Option<&str>) {
    match expected {
        None => assert!(
            body.get(member).is_none(),
            "case {case:?}: an unset {member} must be absent, got {body}"
        ),
        Some(text) => assert_eq!(
            body[member].as_str(),
            Some(text),
            "case {case:?}: expected {member} {:?}, got {body}",
            elided(text)
        ),
    }
}

/// A description shortened for a failure message, so a 2048-character row does not
/// print in full.
fn elided(text: &str) -> String {
    match text.len() > 48 {
        true => format!("{} … ({} chars)", &text[..48], text.len()),
        false => text.to_string(),
    }
}

/// The descriptions a row may be created with: none at all, one character (the
/// published `@length` minimum), an ordinary sentence, and one of exactly the
/// published maximum length.
fn description_cases() -> Vec<(&'static str, Option<String>)> {
    vec![
        ("no description at all", None),
        ("the shortest legal description", Some("d".to_string())),
        (
            "an ordinary description",
            Some("the boiler on the north line".to_string()),
        ),
        (
            "a description of the maximum length",
            Some("d".repeat(DESCRIPTION_MAX_LENGTH)),
        ),
    ]
}

#[tokio::test]
async fn a_description_is_reported_only_by_a_resource_that_has_one() {
    let router = router();

    for (at, (label, description)) in description_cases().into_iter().enumerate() {
        for kind in DESCRIBED {
            let case = format!("{label} on {kind:?}");
            // A name of its own per row: `assetModelName` is unique per account.
            let name = format!("Boiler-{at}-{kind:?}");
            let body =
                created_and_read(&router, kind, &name, description.as_deref()).await;
            reports(&case, &body, kind.member(), description.as_deref());
        }
    }
}

/// The members of an `AssetModelSummary` that a listing reports for a model
/// created with neither an external id nor a description: the scalar members that
/// always have a value. Source
/// `smithy_query(query="shape", shape="AssetModelSummary")` — `externalId` and
/// `description` are the only others it declares, and `version` is the model's
/// own.
const ALWAYS_SUMMARISED: [&str; 8] = [
    "arn",
    "assetModelType",
    "creationDate",
    "id",
    "lastUpdateDate",
    "name",
    "status",
    "version",
];

/// Members of the DESCRIBE projection that a summary never carries, whatever the
/// model: the summary is a strict subset of the read.
const NEVER_SUMMARISED: [&str; 2] = ["properties", "hierarchies"];

/// Each summary member paired with the `DescribeAssetModel` member that reports
/// the same value for the same model.
const SAME_AS_DESCRIBE: [(&str, &str); 7] = [
    ("id", "assetModelId"),
    ("arn", "assetModelArn"),
    ("name", "assetModelName"),
    ("assetModelType", "assetModelType"),
    ("description", "assetModelDescription"),
    ("creationDate", "assetModelCreationDate"),
    ("version", "assetModelVersion"),
];

/// Poll `DescribeAssetModel` until the model reports the ACTIVE state the store
/// owes it, and answer that read.
///
/// The transition is owed a short span after the create; this waits for the
/// CONDITION rather than for a span, so the suite neither races a slow host nor
/// asserts on how long the host took.
async fn settled_model(router: &MultiServiceRouter, model_id: &str) -> Value {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let body = read(router, &format!("/asset-models/{model_id}")).await;
        if body["assetModelStatus"]["state"] == json!("ACTIVE") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "model {model_id}: the owed ACTIVE transition never fell due, last read {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The summary `ListAssetModels` reports for `model_id`.
async fn summary(router: &MultiServiceRouter, model_id: &str) -> Value {
    let body = read(router, "/asset-models?maxResults=250").await;
    body["assetModelSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("a listing carries assetModelSummaries, got {body}"))
        .iter()
        .find(|summary| summary["id"] == json!(model_id))
        .unwrap_or_else(|| panic!("model {model_id} must be listed, got {body}"))
        .clone()
}

#[tokio::test]
async fn a_listed_summary_carries_exactly_the_members_the_model_has() {
    let router = router();

    // Two models, differing only in whether they were given a description, so the
    // key set of a summary is read off the model rather than off the operation.
    let rows: [(&str, &str, Option<&str>); 2] = [
        ("a model created with no description", "Pump", None),
        (
            "a model created with a description",
            "Turbine",
            Some("the turbine on the north line"),
        ),
    ];

    for (case, name, description) in rows {
        let model_id = create_model(&router, name, description).await;
        let described = settled_model(&router, &model_id).await;
        let listed = summary(&router, &model_id).await;

        let keys: Vec<String> = listed
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: a summary is an object, got {listed}"))
            .keys()
            .cloned()
            .collect();
        let mut expected: Vec<String> =
            ALWAYS_SUMMARISED.iter().map(|key| key.to_string()).collect();
        if description.is_some() {
            expected.push("description".to_string());
        }
        expected.sort();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(
            sorted, expected,
            "case {case:?}: expected exactly {expected:?}, got {sorted:?} in {listed}"
        );
        for member in NEVER_SUMMARISED {
            assert!(
                listed.get(member).is_none(),
                "case {case:?}: a summary carries no {member}, got {listed}"
            );
        }

        // And what it does carry is what the read of the same model carries.
        for (summarised, described_as) in SAME_AS_DESCRIBE {
            assert_eq!(
                listed.get(summarised).cloned().unwrap_or(Value::Null),
                described.get(described_as).cloned().unwrap_or(Value::Null),
                "case {case:?}: the summary's {summarised} must be the {described_as} the read \
                 reports: {listed} against {described}"
            );
        }
        assert_eq!(
            listed["status"], described["assetModelStatus"],
            "case {case:?}: the summary's status must be the read's assetModelStatus: {listed} \
             against {described}"
        );
    }
}

/// An asset of a one-measurement model, and that measurement's property id.
async fn asset_with_a_measurement(router: &MultiServiceRouter) -> (String, String) {
    let model_id = accepted(
        router,
        "POST",
        "/asset-models",
        json!({
            "assetModelName": "Sensor",
            "assetModelProperties": [
                {"name": "Speed", "dataType": "DOUBLE", "unit": "rpm", "type": {"measurement": {}}},
            ],
        }),
        "assetModelId",
    )
    .await;
    let asset_id = accepted(
        router,
        "POST",
        "/assets",
        json!({ "assetName": "Sensor1", "assetModelId": model_id }),
        "assetId",
    )
    .await;
    let described = read(router, &format!("/assets/{asset_id}")).await;
    let property_id = crate::asset_lifecycle::property_id(&described["assetProperties"], "Speed");
    (asset_id, property_id)
}

#[tokio::test]
async fn an_ingested_value_reports_the_quality_the_service_derived() {
    let router = router();
    let (asset_id, property) = asset_with_a_measurement(&router).await;

    // One row per form the quality may be submitted in: each value the `Quality`
    // enum declares, and the omitted form the member's optionality admits.
    let rows: [(&str, Option<&str>); 4] = [
        ("no quality at all", None),
        ("an explicit GOOD", Some("GOOD")),
        ("BAD", Some("BAD")),
        ("UNCERTAIN", Some("UNCERTAIN")),
    ];

    // Ascending seconds inside the ingest window, so each row's sample is the
    // newest in the series and the latest-value readers report exactly it.
    let base = now_seconds() - 600;
    for (at, (case, submitted)) in rows.into_iter().enumerate() {
        let seconds = base + at as i64;
        let mut sample = json!({
            "value": { "doubleValue": 12.5 },
            "timestamp": { "timeInSeconds": seconds, "offsetInNanos": 500 },
        });
        if let (Some(object), Some(quality)) = (sample.as_object_mut(), submitted) {
            object.insert("quality".to_string(), json!(quality));
        }
        let (status, put) = call(
            &router,
            "POST",
            BATCH_PUT,
            Some(json!({"entries": [{
                "entryId": format!("e{at}"),
                "assetId": asset_id,
                "propertyId": property,
                "propertyValues": [sample],
            }]})),
        )
        .await;
        assert_eq!(status, 200, "case {case:?}: the put got {status} {put}");
        assert_eq!(
            put["errorEntries"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: the put must ingest cleanly, got {put}"
        );

        // Whatever the submitted form, the stored point reports a quality: the one
        // submitted, or the derived GOOD for a TQV that named none.
        let expected = submitted.unwrap_or(DERIVED_QUALITY);

        let latest = read(
            &router,
            &format!("{LATEST}?assetId={asset_id}&propertyId={property}"),
        )
        .await;
        assert_eq!(
            latest["propertyValue"]["quality"].as_str(),
            Some(expected),
            "case {case:?}: GetAssetPropertyValue expected quality {expected:?}, got {latest}"
        );

        let history = read(
            &router,
            &format!("{HISTORY}?assetId={asset_id}&propertyId={property}"),
        )
        .await;
        let newest = history["assetPropertyValueHistory"]
            .as_array()
            .and_then(|samples| samples.last().cloned())
            .unwrap_or_else(|| panic!("case {case:?}: the history carries the sample, got {history}"));
        assert_eq!(
            newest["quality"].as_str(),
            Some(expected),
            "case {case:?}: GetAssetPropertyValueHistory expected quality {expected:?}, got \
             {history}"
        );

        let (status, batch) = call(
            &router,
            "POST",
            BATCH_LATEST,
            Some(json!({"entries": [{
                "entryId": "one",
                "assetId": asset_id,
                "propertyId": property,
            }]})),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: BatchGetAssetPropertyValue got {status} {batch}"
        );
        assert_eq!(
            batch["successEntries"][0]["assetPropertyValue"]["quality"].as_str(),
            Some(expected),
            "case {case:?}: BatchGetAssetPropertyValue expected quality {expected:?}, got {batch}"
        );
    }
}
