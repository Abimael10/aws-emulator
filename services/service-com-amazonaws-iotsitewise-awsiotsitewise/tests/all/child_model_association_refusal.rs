// SPDX-License-Identifier: CC0-1.0

//! In whose words `AssociateAssets` refuses a child asset the requested
//! hierarchy does not group, driven over HTTP through the same
//! `MultiServiceRouter` the emulator boots with.
//!
//! The rule: a hierarchy is a grouping the PARENT asset's model declares over one
//! child asset model (`smithy_query(query="doc", shape="AssociateAssets",
//! member="hierarchyId")`: "The ID of a hierarchy in the parent asset's model…
//! Hierarchies allow different groupings of assets to be formed that all come
//! from the same asset model"), so a child that instantiates some other asset
//! model is refused as a bad request (400 `InvalidRequestException`) stated about
//! the MODELS: the asset model the child instantiates, the asset model the parent
//! asset instantiates, and the parent asset as the request addressed it. Nothing
//! of the hierarchy, and neither asset's own id but the parent's, appears in it.
//!
//! The rows differ along WHY the child's model is not the grouped one — a model
//! the parent's model names nowhere, a model it groups under a DIFFERENT
//! hierarchy, and the parent asset offered as its own child — and along the label
//! form each id is addressed by, raw or `externalId:`, since the parent slot
//! echoes the label while the two model slots are read off the store. An
//! association the definitions DO allow sits in the same suite, so it cannot pass
//! by refusing everything.
//!
//! The crate's wire harness lives in `super::asset_lifecycle` and the
//! create/associate walk in `super::asset_hierarchies`.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_hierarchies::{associate, create_asset, create_model, hierarchy_id};
use crate::asset_lifecycle::{call, error_code, router};

/// The error class a request the hierarchy rules do not allow is refused with.
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The sentence a child whose asset model the requested hierarchy does not group
/// is refused with, written out BY HAND as an independent oracle of the
/// handler's.
fn child_model_message(
    child_asset_model_id: &str,
    parent_asset_model_id: &str,
    parent_asset_label: &str,
) -> String {
    format!(
        "Child AssetModel {child_asset_model_id} is not present in the AssetModel \
         {parent_asset_model_id} of the given parent asset {parent_asset_label}"
    )
}

/// The asset ids `ListAssociatedAssets` reports under one hierarchy of an asset.
async fn children(router: &MultiServiceRouter, parent: &str, hierarchy: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "GET",
        &format!("/assets/{parent}/hierarchies?hierarchyId={hierarchy}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "ListAssociatedAssets {parent}/{hierarchy}: got {status} {body}"
    );
    body["assetSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| summary["id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The stored world every row below runs against: a `Farm` model with one
/// hierarchy over a `Turbine` model and another over a `Gearbox` model, a
/// `Weather` model the farm model names nowhere, and one asset of each.
struct World {
    router: MultiServiceRouter,
    farm_model: String,
    gearbox_model: String,
    weather_model: String,
    turbines: String,
    farm: String,
    turbine: String,
    gearbox: String,
    weather: String,
}

async fn world() -> World {
    let router = router();
    let turbine_model = create_model(&router, json!({"assetModelName": "Turbine"})).await;
    let gearbox_model = create_model(&router, json!({"assetModelName": "Gearbox"})).await;
    let weather_model = create_model(&router, json!({"assetModelName": "Weather"})).await;
    let farm_model = create_model(
        &router,
        json!({
            "assetModelName": "Farm",
            "assetModelHierarchies": [
                {"name": "turbines", "childAssetModelId": turbine_model},
                {"name": "gearboxes", "childAssetModelId": gearbox_model},
            ],
        }),
    )
    .await;
    let turbines = hierarchy_id(&router, &farm_model, "turbines").await;

    let farm = create_asset(
        &router,
        json!({
            "assetName": "Farm 1",
            "assetModelId": farm_model,
            "assetExternalId": "farm-1",
        }),
    )
    .await;
    let turbine = create_asset(
        &router,
        json!({"assetName": "Turbine 1", "assetModelId": turbine_model}),
    )
    .await;
    let gearbox = create_asset(
        &router,
        json!({"assetName": "Gearbox 1", "assetModelId": gearbox_model}),
    )
    .await;
    let weather = create_asset(
        &router,
        json!({
            "assetName": "Weather 1",
            "assetModelId": weather_model,
            "assetExternalId": "weather-1",
        }),
    )
    .await;

    World {
        router,
        farm_model,
        gearbox_model,
        weather_model,
        turbines,
        farm,
        turbine,
        gearbox,
        weather,
    }
}

/// RULE: a child whose asset model the requested hierarchy does not group is
/// refused 400 `InvalidRequestException`, stated about the child's asset model,
/// the parent asset's asset model and the parent asset as the request addressed
/// it — and nothing is recorded.
#[tokio::test]
async fn a_child_of_another_asset_model_is_refused_about_the_models() {
    let World {
        router,
        farm_model,
        gearbox_model,
        weather_model,
        turbines,
        farm,
        turbine,
        gearbox,
        weather,
    } = world().await;

    // `(case, parent label, child label, the child's model, the parent's model)`.
    let rows: Vec<(&str, String, String, &String, &String)> = vec![
        (
            "a child of a model the parent's model names nowhere",
            farm.clone(),
            weather.clone(),
            &weather_model,
            &farm_model,
        ),
        (
            "a child of a model the parent's model groups under ANOTHER hierarchy",
            farm.clone(),
            gearbox.clone(),
            &gearbox_model,
            &farm_model,
        ),
        (
            "the parent asset offered as its own child",
            farm.clone(),
            farm.clone(),
            &farm_model,
            &farm_model,
        ),
        (
            "the parent addressed by its externalId alias",
            "externalId:farm-1".to_string(),
            weather.clone(),
            &weather_model,
            &farm_model,
        ),
        (
            "the child addressed by its externalId alias",
            farm.clone(),
            "externalId:weather-1".to_string(),
            &weather_model,
            &farm_model,
        ),
    ];

    for (case, parent_label, child_label, child_model, parent_model) in &rows {
        let (status, body) = associate(&router, parent_label, &turbines, child_label).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST}, got {body}"
        );
        let expected = child_model_message(child_model, parent_model, parent_label);
        let stated = body["message"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
        // A refused association records nothing a later caller sees.
        let recorded = children(&router, &farm, &turbines).await;
        assert!(
            recorded.is_empty(),
            "case {case:?}: a refused association must record nothing, got {recorded:?}"
        );
    }

    // The control: the one child whose model the hierarchy DOES group is
    // associated, and reads back under it.
    let (status, body) = associate(&router, &farm, &turbines, &turbine).await;
    assert_eq!(
        status, 200,
        "the hierarchy's own child model must be accepted, got {status} {body}"
    );
    let recorded = children(&router, &farm, &turbines).await;
    assert_eq!(
        recorded,
        vec![turbine.clone()],
        "the accepted child must read back under the hierarchy, got {recorded:?}"
    );
}
