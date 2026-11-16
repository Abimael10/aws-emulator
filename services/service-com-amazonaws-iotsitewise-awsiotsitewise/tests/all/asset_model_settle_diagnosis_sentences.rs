// SPDX-License-Identifier: CC0-1.0

//! WHICH sentence a graph-scoped asset-model fault settles a model at.
//!
//! The rule this suite pins: a write the model graph rejects settles the model at
//! `FAILED` carrying the service's OWN diagnosis of the rule that fired — the
//! whole fixed text of that rule's sentence(s), one distinct text per rule, with
//! nothing of the request interpolated into it
//! (`smithy_query(query="shape", shape="AssetModelStatus")`: `state @required` of
//! `AssetModelState` and `error` of `ErrorDetails{code @required of `ErrorCode`
//! [INTERNAL_FAILURE, VALIDATION_ERROR], message @required}` — the model types the
//! diagnosis, so the wording is the service's).
//!
//! The sibling suite `super::asset_model_settle_failures` pins WHICH definitions
//! settle `FAILED` at all; this one is about the TEXT a client matches on to tell
//! one rule from the next, so its rows differ along what the rule generalises
//! over: which NON-metric kind of the child model a metric reaches (a raw
//! measurement, an attribute, a transform), which spelling the reference uses (the
//! property's id, its `externalId:` alias), which write states it
//! (`CreateAssetModel`, `UpdateAssetModel`) and which rule fires (a reference to a
//! non-metric child property, a reference that resolves to nothing). The rows the
//! graph ACCEPTS sit beside them, so the table cannot pass by failing everything.
//!
//! The settle wait is `crate::support::settled_asset_model_status`, which polls for
//! the condition rather than waiting a span.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_lifecycle::{call, property_id, router};
use crate::support::settled_asset_model_status;

/// The `ErrorCode` every settle-time diagnosis of a definition carries.
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// The diagnosis a METRIC whose variable reaches through a hierarchy at a property
/// of the child model that is not itself aggregated settles the model at: two
/// sentences, the rule that fired and then what a metric MAY reference.
///
/// Entirely fixed text: no property, model or hierarchy of the request appears in
/// it.
const NON_METRIC_CHILD_PROPERTY: &str =
    "A metric property references a non-metric property from a child model. \
     Metrics can reference properties from the current model and metric properties \
     from child models.";

/// The diagnosis a formula variable reaching through a hierarchy at a property the
/// child model does not expose at all settles the model at — the NEIGHBOURING
/// rule, whose own fixed text must stay distinct from the one above.
const MISSING_CHILD_PROPERTY: &str =
    "A formula property references a child asset model property that doesn't exist.";

/// The hierarchy every subject model below declares over the child model.
const HIERARCHY: &str = "Turbines";

/// The child measurement's `externalId`, so a row can spell the reference as the
/// `externalId:` alias instead of the raw id.
const CHILD_MEASUREMENT_EXTERNAL: &str = "child_raw_power";

/// The name of the model each row writes.
const SUBJECT_NAME: &str = "Farm";

/// An id-shaped label the child model holds no property under.
const ABSENT_PROPERTY_ID: &str = "11111111-2222-3333-4444-555555555555";

/// `POST /asset-models`.
async fn create(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", "/asset-models", Some(body)).await
}

/// `PUT /asset-models/{label}`.
async fn update(router: &MultiServiceRouter, label: &str, body: Value) -> (u16, Value) {
    call(router, "PUT", &format!("/asset-models/{label}"), Some(body)).await
}

/// `GET /asset-models/{label}`.
async fn describe(router: &MultiServiceRouter, label: &str) -> (u16, Value) {
    call(router, "GET", &format!("/asset-models/{label}"), None).await
}

/// A `metric` property definition over one variable.
fn metric_property(name: &str, value: Value) -> Value {
    json!({
        "name": name,
        "dataType": "DOUBLE",
        "type": {"metric": {
            "expression": "avg(v)",
            "variables": [{"name": "v", "value": value}],
            "window": {"tumbling": {"interval": "1h"}},
        }},
    })
}

/// A `transform` property definition over one variable.
fn transform_property(name: &str, value: Value) -> Value {
    json!({
        "name": name,
        "dataType": "DOUBLE",
        "type": {"transform": {
            "expression": "v * 2",
            "variables": [{"name": "v", "value": value}],
        }},
    })
}

/// The child model every row reads through its hierarchy: one property of each
/// kind a reference can land on — a raw measurement (carrying an `externalId` so
/// the alias spelling has something to resolve), an attribute, a transform, and
/// the one AGGREGATED kind, a metric.
struct Child {
    /// The child model's id, which the subject's hierarchy names.
    id: String,
    /// Its stored property list, which a row resolves a property id in.
    properties: Value,
}

/// Create that child model and wait for it to settle `ACTIVE`.
async fn child_model(router: &MultiServiceRouter) -> Child {
    let (status, created) = create(
        router,
        json!({
            "assetModelName": "Turbine",
            "assetModelProperties": [
                {
                    "name": "Power",
                    "externalId": CHILD_MEASUREMENT_EXTERNAL,
                    "dataType": "DOUBLE",
                    "type": {"measurement": {}},
                },
                {
                    "name": "Rated",
                    "dataType": "DOUBLE",
                    "type": {"attribute": {"defaultValue": "1.5"}},
                },
                transform_property("Doubled", json!({"propertyId": "Power"})),
                metric_property("ChildAverage", json!({"propertyId": "Power"})),
            ],
        }),
    )
    .await;
    assert_eq!(status, 202, "the child model: got {status} {created}");
    let id = created["assetModelId"]
        .as_str()
        .unwrap_or_else(|| panic!("no assetModelId in {created}"))
        .to_string();
    let settled = settled_asset_model_status(router, &id).await;
    assert_eq!(
        settled["state"].as_str(),
        Some("ACTIVE"),
        "the child model must settle ACTIVE before a row reads it, got {settled}"
    );
    let (status, described) = describe(router, &id).await;
    assert_eq!(
        status, 200,
        "describing the child model: got {status} {described}"
    );
    let properties = described["assetModelProperties"].clone();
    Child { id, properties }
}

/// Which property of the child model a row's variable reaches, and under which
/// spelling.
#[derive(Debug, Clone, Copy)]
enum Reach {
    /// A NON-metric property of the child, by the name it is stored under — the
    /// row resolves it to the property's id.
    NonMetricById(&'static str),
    /// The child's raw measurement, by its `externalId:` alias.
    MeasurementByExternalId,
    /// The child's own `metric`: the one kind a parent metric may consume.
    ChildMetric,
    /// A label the child model holds no property under.
    Absent,
}

/// Which write states the row's definition.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// `CreateAssetModel` stating it outright.
    Create,
    /// `UpdateAssetModel` adding it to a model that settled `ACTIVE`.
    Update,
}

/// The `VariableValue` a row's formula declares.
fn variable(reach: Reach, properties: &Value) -> Value {
    let property_label = match reach {
        Reach::NonMetricById(name) => property_id(properties, name),
        Reach::MeasurementByExternalId => format!("externalId:{CHILD_MEASUREMENT_EXTERNAL}"),
        Reach::ChildMetric => property_id(properties, "ChildAverage"),
        Reach::Absent => ABSENT_PROPERTY_ID.to_string(),
    };
    json!({"propertyId": property_label, "hierarchyId": HIERARCHY})
}

/// Create the model a row's UPDATE writes to — the hierarchy and no formula at
/// all — and wait for its create to settle.
async fn settled_subject(router: &MultiServiceRouter, child: &str) -> String {
    let (status, created) = create(
        router,
        json!({
            "assetModelName": SUBJECT_NAME,
            "assetModelHierarchies": [{"name": HIERARCHY, "childAssetModelId": child}],
        }),
    )
    .await;
    assert_eq!(status, 202, "the subject model: got {status} {created}");
    let model_id = created["assetModelId"]
        .as_str()
        .unwrap_or_else(|| panic!("no assetModelId in {created}"))
        .to_string();
    let settled = settled_asset_model_status(router, &model_id).await;
    assert_eq!(
        settled["state"].as_str(),
        Some("ACTIVE"),
        "the subject model must settle ACTIVE before the row's update, got {settled}"
    );
    model_id
}

#[tokio::test]
async fn a_failed_settle_reports_the_whole_fixed_sentence_of_the_rule_that_fired() {
    // Two rules whose texts a client tells apart, each read back over the child
    // property kinds, reference spellings and writes it holds for — beside the
    // rows the graph accepts.
    let rows: Vec<(&str, Write, bool, Reach, Option<&str>)> = vec![
        (
            "a create whose metric reaches the child's raw measurement by id",
            Write::Create,
            true,
            Reach::NonMetricById("Power"),
            Some(NON_METRIC_CHILD_PROPERTY),
        ),
        (
            "a create whose metric reaches that measurement by its externalId alias",
            Write::Create,
            true,
            Reach::MeasurementByExternalId,
            Some(NON_METRIC_CHILD_PROPERTY),
        ),
        (
            "a create whose metric reaches the child's attribute",
            Write::Create,
            true,
            Reach::NonMetricById("Rated"),
            Some(NON_METRIC_CHILD_PROPERTY),
        ),
        (
            "a create whose metric reaches the child's transform",
            Write::Create,
            true,
            Reach::NonMetricById("Doubled"),
            Some(NON_METRIC_CHILD_PROPERTY),
        ),
        (
            "an update whose metric reaches the child's attribute",
            Write::Update,
            true,
            Reach::NonMetricById("Rated"),
            Some(NON_METRIC_CHILD_PROPERTY),
        ),
        (
            "a create whose metric reaches a label the child holds no property under",
            Write::Create,
            true,
            Reach::Absent,
            Some(MISSING_CHILD_PROPERTY),
        ),
        (
            "an update whose metric reaches a label the child holds no property under",
            Write::Update,
            true,
            Reach::Absent,
            Some(MISSING_CHILD_PROPERTY),
        ),
        (
            "a create whose metric reaches the child's own metric",
            Write::Create,
            true,
            Reach::ChildMetric,
            None,
        ),
        (
            "a create whose transform reaches the child's raw measurement",
            Write::Create,
            false,
            Reach::NonMetricById("Power"),
            None,
        ),
    ];

    // The two diagnoses are DISTINCT texts: a client matching on one must not
    // match the other, so neither may be collapsed into the other's wording.
    assert!(
        NON_METRIC_CHILD_PROPERTY != MISSING_CHILD_PROPERTY
            && !NON_METRIC_CHILD_PROPERTY.contains(MISSING_CHILD_PROPERTY)
            && !MISSING_CHILD_PROPERTY.contains(NON_METRIC_CHILD_PROPERTY),
        "the two rules' diagnoses must stay distinct, got {NON_METRIC_CHILD_PROPERTY:?} and \
         {MISSING_CHILD_PROPERTY:?}"
    );

    for (case, write, is_metric, reach, expected) in rows {
        // Each row stands on its own store, so only its own definition is in the
        // graph the rule is asked about.
        let router = router();
        let child = child_model(&router).await;
        let value = variable(reach, &child.properties);
        let property = if is_metric {
            metric_property("Rollup", value.clone())
        } else {
            transform_property("Rollup", value.clone())
        };

        let (status, answered, model_id) = match write {
            Write::Create => {
                let (status, answered) = create(
                    &router,
                    json!({
                        "assetModelName": SUBJECT_NAME,
                        "assetModelProperties": [property],
                        "assetModelHierarchies": [
                            {"name": HIERARCHY, "childAssetModelId": child.id},
                        ],
                    }),
                )
                .await;
                let model_id = answered["assetModelId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                (status, answered, model_id)
            }
            Write::Update => {
                let model_id = settled_subject(&router, &child.id).await;
                let (status, answered) = update(
                    &router,
                    &model_id,
                    json!({
                        "assetModelName": SUBJECT_NAME,
                        "assetModelProperties": [property],
                        "assetModelHierarchies": [
                            {"name": HIERARCHY, "childAssetModelId": child.id},
                        ],
                    }),
                )
                .await;
                (status, answered, model_id)
            }
        };
        assert_eq!(
            status, 202,
            "case {case:?}: the write must be accepted, got {status} {answered}"
        );

        let settled = settled_asset_model_status(&router, &model_id).await;
        let Some(diagnosis) = expected else {
            assert_eq!(
                settled["state"].as_str(),
                Some("ACTIVE"),
                "case {case:?}: a definition the graph accepts settles ACTIVE, got {settled}"
            );
            assert_eq!(
                settled["error"],
                Value::Null,
                "case {case:?}: a model that settled ACTIVE carries no diagnosis, got {settled}"
            );
            continue;
        };

        assert_eq!(
            settled["state"].as_str(),
            Some("FAILED"),
            "case {case:?}: expected the model to settle FAILED, got {settled}"
        );
        assert_eq!(
            settled["error"]["code"].as_str(),
            Some(VALIDATION_ERROR),
            "case {case:?}: expected a {VALIDATION_ERROR} diagnosis, got {settled}"
        );
        let message = settled["error"]["message"].as_str().unwrap_or_default();
        assert_eq!(
            message, diagnosis,
            "case {case:?}: expected {diagnosis:?}, got {message:?}"
        );

        // FIXED text: nothing the request stated, and nothing the store assigned,
        // is interpolated into the sentence.
        for interpolated in [
            child.id.as_str(),
            model_id.as_str(),
            HIERARCHY,
            SUBJECT_NAME,
            CHILD_MEASUREMENT_EXTERNAL,
            "Rollup",
            "Power",
            "Rated",
            "Doubled",
            "ChildAverage",
            ABSENT_PROPERTY_ID,
        ] {
            assert!(
                !message.contains(interpolated),
                "case {case:?}: the diagnosis is fixed text, but {message:?} carries \
                 {interpolated:?}"
            );
        }

        // The diagnosis the settle recorded is what a later read reports, not
        // something re-derived per read.
        let (status, described) = describe(&router, &model_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a FAILED model stays describable, got {status} {described}"
        );
        assert_eq!(
            described["assetModelStatus"]["error"]["message"].as_str(),
            Some(diagnosis),
            "case {case:?}: DescribeAssetModel must report {diagnosis:?}, got {described}"
        );
    }
}
