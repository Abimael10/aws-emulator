// SPDX-License-Identifier: CC0-1.0

//! What an accepted asset-model write SETTLES at, and the state a model has to be
//! in before a write to it is accepted at all.
//!
//! The rule these suites pin: IoT SiteWise decides an asset-model write on two
//! planes. The request's own faults and the STATE the model is in are answered
//! synchronously (`InvalidRequestException`, HTTP 400). A fault about the MODEL
//! GRAPH — what another asset model exposes, and whether the hierarchies lead back
//! to the model being written — does NOT refuse the call: the write is accepted,
//! the model reports `CREATING` / `UPDATING` / `DELETING` like any other, and when
//! the transition that write owes falls due the model settles at `FAILED` carrying
//! `assetModelStatus.error` = `ErrorDetails{VALIDATION_ERROR, <the rule that
//! fired>}` (`smithy_query(query="shape", shape="AssetModelStatus")`:
//! `state @required` of `AssetModelState` `[CREATING, ACTIVE, UPDATING,
//! PROPAGATING, DELETING, FAILED]` and `error` of `ErrorDetails{code @required of
//! ErrorCode [INTERNAL_FAILURE, VALIDATION_ERROR], message @required}`). A model
//! that settled `FAILED` is a stored model like any other — listed, describable,
//! and moved back to `ACTIVE` by a write whose definition the graph accepts.
//!
//! Each suite is a table whose rows differ along what the rule generalises over:
//! which graph-scoped fault the definition carries (a reference to a property the
//! child model does not expose, a metric reaching a raw property of it, a
//! hierarchy leading back to the model directly or through another model), which
//! formula member carries it (`metric`, `transform`), whether the property list
//! sits on the model or on one of its composite models, which write states it
//! (`CreateAssetModel`, `UpdateAssetModel`, `DeleteAssetModel`) and which state the
//! model is in when the write lands. The rows the graph ACCEPTS sit beside them, so
//! a suite cannot pass by failing everything.
//!
//! The wire harness lives in `super::asset_lifecycle`; the settle wait every suite
//! here uses is `crate::support::settled_asset_model_status`, which polls for the
//! condition rather than waiting a span.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_lifecycle::{call, error_code, property_id, router};
use crate::hierarchies_etags_and_refusals::{describe_e_tag, is_modelled_e_tag};
use crate::support::settled_asset_model_status;

/// The `ErrorCode` every settle-time diagnosis of a definition carries.
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// The diagnosis a formula variable reaching through a hierarchy at a property the
/// child model does not expose settles the model at. Fixed text: it names no value
/// of the request.
const MISSING_CHILD_PROPERTY: &str =
    "A formula property references a child asset model property that doesn't exist.";

/// The diagnosis a METRIC whose variable reaches through a hierarchy at a raw
/// property of the child model settles the model at: two sentences of fixed text,
/// the second stating what a metric MAY reference. Distinct from
/// [`MISSING_CHILD_PROPERTY`], the diagnosis of a reference that resolves to
/// nothing at all.
const NON_METRIC_CHILD_PROPERTY: &str =
    "A metric property references a non-metric property from a child model. \
     Metrics can reference properties from the current model and metric properties \
     from child models.";

/// The diagnosis a hierarchy graph that leads back to the model being written
/// settles the model at.
const CYCLICAL_HIERARCHY: &str = "A cyclical hierarchy definition exists. \
     Asset models can't contain cyclical hierarchy references.";

/// The diagnosis a delete of a model another model's hierarchy still names settles
/// that model at.
const REFERENCED_BY_PARENT: &str =
    "Attempted to delete an asset model that is referenced by a hierarchy in a parent asset \
     model. You must remove hierarchy references before you can delete the asset model.";

/// The hierarchy every subject model below declares over the child model.
const HIERARCHY: &str = "Children";

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

/// `DELETE /asset-models/{label}`.
async fn delete(router: &MultiServiceRouter, label: &str) -> (u16, Value) {
    call(router, "DELETE", &format!("/asset-models/{label}"), None).await
}

/// Whether `GET /asset-models` reports `model_id`, and the status it reports for
/// it.
async fn listed_status(router: &MultiServiceRouter, model_id: &str) -> Option<Value> {
    let (status, listed) = call(router, "GET", "/asset-models?maxResults=250", None).await;
    assert_eq!(status, 200, "ListAssetModels: got {status} {listed}");
    listed["assetModelSummaries"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .find(|summary| summary["id"].as_str() == Some(model_id))
        .map(|summary| summary["status"].clone())
}

/// A measurement property definition: a RAW property, which no metric of a parent
/// model may consume through a hierarchy.
fn measurement(name: &str) -> Value {
    json!({"name": name, "dataType": "DOUBLE", "type": {"measurement": {}}})
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

/// The child model every row reads through its hierarchy: a raw measurement and a
/// metric that aggregates it, so a row can reach an aggregated property of the
/// child and a raw one.
///
/// Answers `(assetModelId, the metric's id, the measurement's id)`.
async fn child_model(router: &MultiServiceRouter) -> (String, String, String) {
    let (status, created) = create(
        router,
        json!({
            "assetModelName": "Turbine",
            "assetModelProperties": [
                measurement("Power"),
                metric_property("ChildAverage", json!({"propertyId": "Power"})),
            ],
        }),
    )
    .await;
    assert_eq!(status, 202, "the child model: got {status} {created}");
    let child = created["assetModelId"]
        .as_str()
        .unwrap_or_else(|| panic!("no assetModelId in {created}"))
        .to_string();
    let (status, described) = describe(router, &child).await;
    assert_eq!(status, 200, "describing the child model: got {status} {described}");
    let properties = &described["assetModelProperties"];
    (
        child,
        property_id(properties, "ChildAverage"),
        property_id(properties, "Power"),
    )
}

/// Which property of the child model a row's variable reaches through the
/// hierarchy.
#[derive(Debug, Clone, Copy)]
enum Reached {
    /// The child's own METRIC: an aggregated property, which a parent's metric may
    /// consume.
    ChildMetric,
    /// The child's RAW measurement.
    ChildMeasurement,
    /// An id-shaped label the child model holds no property under.
    Nothing,
}

/// What a row of the settle table writes.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// `CreateAssetModel` whose own property list carries a `metric` over the
    /// reference.
    CreateMetric(Reached),
    /// `CreateAssetModel` whose own property list carries a `transform` over it.
    CreateTransform(Reached),
    /// `CreateAssetModel` stating the reference on a COMPOSITE model's property
    /// list.
    CreateCompositeMetric(Reached),
    /// `UpdateAssetModel` adding a `metric` over the reference to a settled model.
    UpdateMetric(Reached),
    /// `UpdateAssetModel` adding a hierarchy whose child model is the model
    /// itself.
    UpdateSelfHierarchy,
    /// `UpdateAssetModel` adding a hierarchy to a model that already declares one
    /// back to this one.
    UpdateIndirectCycle,
}

/// The `VariableValue` reaching `reached` through the declared hierarchy.
fn variable(reached: Reached, child_metric: &str, child_measurement: &str) -> Value {
    let property_id = match reached {
        Reached::ChildMetric => child_metric.to_string(),
        Reached::ChildMeasurement => child_measurement.to_string(),
        // An id-shaped label: the SPELLING a reference into another model has to
        // use, so what this row exercises is resolution and not the spelling rule.
        Reached::Nothing => "11111111-2222-3333-4444-555555555555".to_string(),
    };
    json!({"propertyId": property_id, "hierarchyId": HIERARCHY})
}

/// Create the subject model a row's UPDATE writes to — a model with the hierarchy
/// and no formula at all — and wait for its create to settle.
async fn settled_subject(router: &MultiServiceRouter, child: &str) -> String {
    let (status, created) = create(
        router,
        json!({
            "assetModelName": "Farm",
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
async fn a_definition_the_model_graph_rejects_is_accepted_and_settles_failed() {
    // Rows: every graph-scoped fault, on each formula member, on the model's own
    // property list and on a composite model's, stated by the create and by the
    // update — beside the references the graph ACCEPTS.
    let rows: Vec<(&str, Write, Option<&str>)> = vec![
        (
            "a create whose metric names the child's own metric",
            Write::CreateMetric(Reached::ChildMetric),
            None,
        ),
        (
            "a create whose metric names the child's raw measurement",
            Write::CreateMetric(Reached::ChildMeasurement),
            Some(NON_METRIC_CHILD_PROPERTY),
        ),
        (
            "a create whose metric names a property the child does not expose",
            Write::CreateMetric(Reached::Nothing),
            Some(MISSING_CHILD_PROPERTY),
        ),
        (
            "a create whose transform names the child's raw measurement",
            Write::CreateTransform(Reached::ChildMeasurement),
            None,
        ),
        (
            "a create whose transform names a property the child does not expose",
            Write::CreateTransform(Reached::Nothing),
            Some(MISSING_CHILD_PROPERTY),
        ),
        (
            "a create whose composite model's metric names the child's raw measurement",
            Write::CreateCompositeMetric(Reached::ChildMeasurement),
            Some(NON_METRIC_CHILD_PROPERTY),
        ),
        (
            "a create whose composite model's metric names the child's own metric",
            Write::CreateCompositeMetric(Reached::ChildMetric),
            None,
        ),
        (
            "an update whose metric names a property the child does not expose",
            Write::UpdateMetric(Reached::Nothing),
            Some(MISSING_CHILD_PROPERTY),
        ),
        (
            "an update whose metric names the child's own metric",
            Write::UpdateMetric(Reached::ChildMetric),
            None,
        ),
        (
            "an update adding a hierarchy whose child model is the model itself",
            Write::UpdateSelfHierarchy,
            Some(CYCLICAL_HIERARCHY),
        ),
        (
            "an update adding a hierarchy that leads back through another model",
            Write::UpdateIndirectCycle,
            Some(CYCLICAL_HIERARCHY),
        ),
    ];

    for (case, write, expected) in rows {
        // Each row stands on its own store, so only its own definition is in the
        // graph the rule is asked about.
        let router = router();
        let (child, child_metric, child_measurement) = child_model(&router).await;

        let (status, answered, model_id, accepted_state) = match write {
            Write::CreateMetric(reached) | Write::CreateTransform(reached) => {
                let value = variable(reached, &child_metric, &child_measurement);
                let property = if matches!(write, Write::CreateMetric(_)) {
                    metric_property("Rollup", value)
                } else {
                    transform_property("Rollup", value)
                };
                let (status, answered) = create(
                    &router,
                    json!({
                        "assetModelName": "Farm",
                        "assetModelProperties": [property],
                        "assetModelHierarchies": [
                            {"name": HIERARCHY, "childAssetModelId": child},
                        ],
                    }),
                )
                .await;
                let model_id = answered["assetModelId"].as_str().unwrap_or_default().to_string();
                (status, answered, model_id, "CREATING")
            }
            Write::CreateCompositeMetric(reached) => {
                let value = variable(reached, &child_metric, &child_measurement);
                let (status, answered) = create(
                    &router,
                    json!({
                        "assetModelName": "Farm",
                        "assetModelHierarchies": [
                            {"name": HIERARCHY, "childAssetModelId": child},
                        ],
                        "assetModelCompositeModels": [{
                            "name": "Rollups",
                            "type": "CUSTOM",
                            "properties": [metric_property("Rollup", value)],
                        }],
                    }),
                )
                .await;
                let model_id = answered["assetModelId"].as_str().unwrap_or_default().to_string();
                (status, answered, model_id, "CREATING")
            }
            Write::UpdateMetric(reached) => {
                let model_id = settled_subject(&router, &child).await;
                let value = variable(reached, &child_metric, &child_measurement);
                let (status, answered) = update(
                    &router,
                    &model_id,
                    json!({
                        "assetModelName": "Farm",
                        "assetModelProperties": [metric_property("Rollup", value)],
                        "assetModelHierarchies": [
                            {"name": HIERARCHY, "childAssetModelId": child},
                        ],
                    }),
                )
                .await;
                (status, answered, model_id, "UPDATING")
            }
            Write::UpdateSelfHierarchy => {
                let model_id = settled_subject(&router, &child).await;
                let (status, answered) = update(
                    &router,
                    &model_id,
                    json!({
                        "assetModelName": "Farm",
                        "assetModelHierarchies": [
                            {"name": HIERARCHY, "childAssetModelId": child},
                            {"name": "Self", "childAssetModelId": model_id},
                        ],
                    }),
                )
                .await;
                (status, answered, model_id, "UPDATING")
            }
            Write::UpdateIndirectCycle => {
                let model_id = settled_subject(&router, &child).await;
                // A model that already names the subject as its own child, so the
                // hierarchy the update adds closes the loop through it.
                let (status, created) = create(
                    &router,
                    json!({
                        "assetModelName": "Region",
                        "assetModelHierarchies": [
                            {"name": "Farms", "childAssetModelId": model_id},
                        ],
                    }),
                )
                .await;
                assert_eq!(status, 202, "case {case:?}: the middle model: got {status} {created}");
                let middle = created["assetModelId"].as_str().unwrap_or_default().to_string();
                let (status, answered) = update(
                    &router,
                    &model_id,
                    json!({
                        "assetModelName": "Farm",
                        "assetModelHierarchies": [
                            {"name": HIERARCHY, "childAssetModelId": child},
                            {"name": "Regions", "childAssetModelId": middle},
                        ],
                    }),
                )
                .await;
                (status, answered, model_id, "UPDATING")
            }
        };

        // Whatever the definition says, the call is ACCEPTED and reports the state
        // the write put the model in: nothing in the synchronous answer tells a
        // definition the graph rejects from one it accepts.
        assert_eq!(
            status, 202,
            "case {case:?}: the write must be accepted, got {status} {answered}"
        );
        assert_eq!(
            answered["assetModelStatus"]["state"].as_str(),
            Some(accepted_state),
            "case {case:?}: expected the accepted write to report {accepted_state}, got {answered}"
        );
        assert_eq!(
            answered["assetModelStatus"]["error"],
            Value::Null,
            "case {case:?}: the accepted write carries no diagnosis yet, got {answered}"
        );

        let settled = settled_asset_model_status(&router, &model_id).await;
        match expected {
            None => {
                assert_eq!(
                    settled["state"].as_str(),
                    Some("ACTIVE"),
                    "case {case:?}: a definition the graph accepts settles ACTIVE, got {settled}"
                );
                assert_eq!(
                    settled["error"],
                    Value::Null,
                    "case {case:?}: a model that settled ACTIVE carries no error, got {settled}"
                );
            }
            Some(diagnosis) => {
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
                assert_eq!(
                    settled["error"]["message"].as_str(),
                    Some(diagnosis),
                    "case {case:?}: expected {diagnosis:?}, got {settled}"
                );

                // A model that settled FAILED is a stored model like any other.
                let listed = listed_status(&router, &model_id).await;
                assert_eq!(
                    listed.as_ref().and_then(|status| status["state"].as_str()),
                    Some("FAILED"),
                    "case {case:?}: a FAILED model stays in the listing, got {listed:?}"
                );
                let (status, described) = describe(&router, &model_id).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: a FAILED model stays describable, got {status} {described}"
                );
                assert!(
                    described["assetModelHierarchies"].is_array()
                        && described["assetModelProperties"].is_array()
                        && described["assetModelVersion"].is_string(),
                    "case {case:?}: a FAILED model reports its whole definition, got {described}"
                );
                // The entity tag of that definition, which the model binds to the
                // `ETag` response header rather than to the body.
                let tag = describe_e_tag(&router, &model_id).await;
                assert!(
                    is_modelled_e_tag(&tag),
                    "case {case:?}: a FAILED model reports its entity tag, got {tag:?}"
                );
                assert_eq!(
                    described["assetModelStatus"]["error"]["message"].as_str(),
                    Some(diagnosis),
                    "case {case:?}: the describe reports the diagnosis too, got {described}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_update_whose_definition_the_graph_accepts_repairs_a_failed_model() {
    let router = router();
    let (child, child_metric, child_measurement) = child_model(&router).await;

    // A create the graph rejects, so the model to repair is reached the only way a
    // client can reach it: through an accepted write that settles FAILED.
    let (status, created) = create(
        &router,
        json!({
            "assetModelName": "Repaired",
            "assetModelProperties": [metric_property(
                "Rollup",
                variable(Reached::Nothing, &child_metric, &child_measurement),
            )],
            "assetModelHierarchies": [{"name": HIERARCHY, "childAssetModelId": child}],
        }),
    )
    .await;
    assert_eq!(status, 202, "the illegal create: got {status} {created}");
    let model_id = created["assetModelId"]
        .as_str()
        .unwrap_or_else(|| panic!("no assetModelId in {created}"))
        .to_string();
    let failed = settled_asset_model_status(&router, &model_id).await;
    assert_eq!(
        failed["state"].as_str(),
        Some("FAILED"),
        "the illegal create must settle FAILED, got {failed}"
    );

    let (status, before) = describe(&router, &model_id).await;
    assert_eq!(status, 200, "describing the FAILED model: got {status} {before}");
    let version_before = before["assetModelVersion"].as_str().unwrap_or("").to_string();

    // The corrective update names the stored property by its id — an id-less
    // definition carrying a name the model already holds would re-create that
    // property — and points its variable at the child's own metric.
    let properties = json!([{
        "id": property_id(&before["assetModelProperties"], "Rollup"),
        "name": "Rollup",
        "dataType": "DOUBLE",
        "type": {"metric": {
            "expression": "avg(v)",
            "variables": [{"name": "v", "value": variable(
                Reached::ChildMetric,
                &child_metric,
                &child_measurement,
            )}],
            "window": {"tumbling": {"interval": "1h"}},
        }},
    }]);
    let (status, answered) = update(
        &router,
        &model_id,
        json!({
            "assetModelName": "Repaired",
            "assetModelProperties": properties,
            "assetModelHierarchies": [{"name": HIERARCHY, "childAssetModelId": child}],
        }),
    )
    .await;
    assert_eq!(status, 202, "the corrective update: got {status} {answered}");
    assert_eq!(
        answered["assetModelStatus"]["state"].as_str(),
        Some("UPDATING"),
        "the corrective update leaves the model UPDATING, got {answered}"
    );

    let repaired = settled_asset_model_status(&router, &model_id).await;
    assert_eq!(
        repaired["state"].as_str(),
        Some("ACTIVE"),
        "a corrected definition settles the model ACTIVE again, got {repaired}"
    );
    assert_eq!(
        repaired["error"],
        Value::Null,
        "the repaired model carries no diagnosis, got {repaired}"
    );
    let (status, after) = describe(&router, &model_id).await;
    assert_eq!(status, 200, "describing the repaired model: got {status} {after}");
    assert_ne!(
        after["assetModelVersion"].as_str(),
        Some(version_before.as_str()),
        "the corrective update carries a new version, got {after}"
    );
}

#[tokio::test]
async fn a_delete_a_parent_hierarchy_holds_back_settles_failed_and_the_model_survives() {
    let router = router();
    let (child, _, _) = child_model(&router).await;
    let parent = settled_subject(&router, &child).await;
    settled_asset_model_status(&router, &child).await;

    let (status, answered) = delete(&router, &child).await;
    assert_eq!(
        status, 202,
        "deleting a referenced child must be accepted, got {status} {answered}"
    );
    assert_eq!(
        answered["assetModelStatus"]["state"].as_str(),
        Some("DELETING"),
        "the accepted delete reports DELETING, got {answered}"
    );

    let settled = settled_asset_model_status(&router, &child).await;
    assert_eq!(
        settled["state"].as_str(),
        Some("FAILED"),
        "a delete a parent hierarchy holds back settles FAILED, got {settled}"
    );
    assert_eq!(
        settled["error"]["code"].as_str(),
        Some(VALIDATION_ERROR),
        "expected a {VALIDATION_ERROR} diagnosis, got {settled}"
    );
    assert_eq!(
        settled["error"]["message"].as_str(),
        Some(REFERENCED_BY_PARENT),
        "expected {REFERENCED_BY_PARENT:?}, got {settled}"
    );

    // The model is NOT deleted: it stays in the listing and stays describable
    // under the name it already had.
    let listed = listed_status(&router, &child).await;
    assert_eq!(
        listed.as_ref().and_then(|status| status["state"].as_str()),
        Some("FAILED"),
        "the surviving model stays in the listing, got {listed:?}"
    );
    let (status, described) = describe(&router, &child).await;
    assert_eq!(status, 200, "the surviving model stays describable, got {described}");
    assert_eq!(
        described["assetModelName"].as_str(),
        Some("Turbine"),
        "the surviving model keeps its name, got {described}"
    );

    // With the hierarchy reference removed, the identical delete goes through.
    let (status, answered) = update(
        &router,
        &parent,
        json!({"assetModelName": "Farm", "assetModelHierarchies": []}),
    )
    .await;
    assert_eq!(status, 202, "dropping the hierarchy: got {status} {answered}");
    settled_asset_model_status(&router, &parent).await;
    let (status, answered) = delete(&router, &child).await;
    assert_eq!(
        status, 202,
        "with the reference gone the delete goes through, got {status} {answered}"
    );
    let (status, gone) = describe(&router, &child).await;
    assert_eq!(
        status, 404,
        "the deleted model reads back as absent, got {status} {gone}"
    );
}

#[tokio::test]
async fn a_delete_is_refused_while_the_model_has_not_settled() {
    // Rows: the two states an asset-model write leaves a model in before the
    // transition it owes falls due. Each is a state the model may not be modified
    // in, so the delete is refused with the state it reports.
    let rows = ["straight after the create", "straight after an update"];
    for case in rows {
        let router = router();
        let (child, _, _) = child_model(&router).await;
        let (status, created) = create(
            &router,
            json!({
                "assetModelName": "Unsettled",
                "assetModelHierarchies": [{"name": HIERARCHY, "childAssetModelId": child}],
            }),
        )
        .await;
        assert_eq!(status, 202, "case {case:?}: the create: got {status} {created}");
        let model_id = created["assetModelId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no assetModelId in {created}"))
            .to_string();
        if case == "straight after an update" {
            settled_asset_model_status(&router, &model_id).await;
            let (status, answered) = update(
                &router,
                &model_id,
                json!({
                    "assetModelName": "Unsettled",
                    "assetModelHierarchies": [{"name": HIERARCHY, "childAssetModelId": child}],
                }),
            )
            .await;
            assert_eq!(status, 202, "case {case:?}: the update: got {status} {answered}");
        }

        // The state the refusal has to name is the one the model reports.
        let (status, described) = describe(&router, &model_id).await;
        assert_eq!(status, 200, "case {case:?}: the describe: got {status} {described}");
        let state = described["assetModelStatus"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let (status, refused) = delete(&router, &model_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a model that has not settled may not be deleted, got \
             {status} {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: unexpected error code in {refused}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(
                format!(
                    "This asset model must be ACTIVE or FAILED before you can modify it. \
                     AssetModelID: {model_id} AssetModelName: Unsettled AssetModelState: {state}"
                )
                .as_str()
            ),
            "case {case:?}: unexpected refusal in {refused}"
        );

        // The in-flight transition is untouched: the model settles where it was
        // going rather than into DELETING, and the identical delete then succeeds.
        let settled = settled_asset_model_status(&router, &model_id).await;
        assert_eq!(
            settled["state"].as_str(),
            Some("ACTIVE"),
            "case {case:?}: the refused delete left the transition alone, got {settled}"
        );
        let (status, answered) = delete(&router, &model_id).await;
        assert_eq!(
            status, 202,
            "case {case:?}: the identical delete succeeds once settled, got {status} {answered}"
        );
        let (status, gone) = describe(&router, &model_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the deleted model reads back as absent, got {status} {gone}"
        );
    }
}
