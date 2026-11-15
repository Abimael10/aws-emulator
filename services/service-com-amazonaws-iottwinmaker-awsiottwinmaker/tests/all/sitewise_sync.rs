// SPDX-License-Identifier: CC0-1.0

//! What a `SITEWISE` sync job SYNCHRONISES, end to end over HTTP.
//!
//! Both services are mounted on ONE engine (`harness::Emulator::with_sitewise`),
//! exactly as the assembled router mounts them, and the source hierarchy every
//! case is about is staged through IoT SiteWise's OWN wire — `CreateAssetModel`,
//! `CreateAsset`, `AssociateAssets` — so what a sync job projects is a hierarchy
//! that service really holds and not a fixture written into a store.
//!
//! The suites assert the RULE over a table of inputs rather than one recorded
//! case: a sync job projects its source's asset models to component types and
//! its assets to entities, each carrying the job's `syncSource`, with the
//! source's hierarchies as the workspace's parent/child relationships; the job
//! then reaches the standing `ACTIVE` state and reports one synchronised
//! resource per source resource; a resource the workspace will not take fails
//! that resource and settles the job in the modelled failure state; and a
//! resource a CALLER created carries no `syncSource` at all.

use serde_json::{json, Value};
use service_com_amazonaws_iottwinmaker_awsiottwinmaker::ops::types::{
    SYNC_RESOURCE_STATE_VALUES, SYNC_RESOURCE_TYPE_VALUES, TYPE_VALUES,
};
use service_com_amazonaws_iottwinmaker_awsiottwinmaker::state::{
    sitewise_component_type_id, SITEWISE_ASSET_ID_PROPERTY, SITEWISE_ASSET_MODEL_ID_PROPERTY,
    SITEWISE_COMPONENT_NAME, SITEWISE_CONNECTOR_COMPONENT_TYPE_ID, SYNC_JOB_ERROR_CODE,
    SYNC_JOB_STATE_ACTIVE, SYNC_JOB_STATE_ERROR, SYNC_RESOURCE_ERROR_CODE,
    SYNC_RESOURCE_STATE_ERROR, SYNC_RESOURCE_STATE_IN_SYNC, SYNC_SOURCE_SITEWISE,
};

use crate::harness::{sync_job_in_state, Emulator, Resp, SYNC_ROLE};

/// The workspace every case projects into.
const WORKSPACE: &str = "sitewiseSyncWorkspace";

/// One property of a source asset model a case stages, and what the projection
/// must make of it: `(the source's name, its PropertyDataType, its PropertyType
/// member, the projected property name, the projected Type, whether the
/// projected definition is a time series)`.
///
/// The rows differ along what the rule generalises over — the shape of the
/// source's name (a plain one, one carrying characters the modelled `Name` of a
/// projected definition does not admit, one already admissible but mixed), the
/// data type, and whether the property is a static attribute or a stream — not
/// along a rename. The data types are the ones the source admits on an asset
/// model property: "the VIDEO, ANNOTATION, and JSON data types aren't supported
/// for asset model properties"
/// (`smithy_query(query="shape",shape="AssetModelPropertyDefinition")`).
struct SourceProperty {
    source_name: &'static str,
    data_type: &'static str,
    property_type: fn() -> Value,
    projected_name: &'static str,
    projected_type: &'static str,
    time_series: bool,
}

/// A static attribute of an asset, which carries one value rather than a series.
fn attribute() -> Value {
    json!({"attribute": {"defaultValue": "0"}})
}

/// A device's raw sensor stream, which is a series of values over time.
fn measurement() -> Value {
    json!({"measurement": {}})
}

/// The properties the staged asset models declare.
const SOURCE_PROPERTIES: &[SourceProperty] = &[
    SourceProperty {
        source_name: "serialNumber",
        data_type: "STRING",
        property_type: attribute,
        projected_name: "serialNumber",
        projected_type: "STRING",
        time_series: false,
    },
    SourceProperty {
        source_name: "Max Temperature",
        data_type: "DOUBLE",
        property_type: measurement,
        projected_name: "Max_Temperature",
        projected_type: "DOUBLE",
        time_series: true,
    },
    SourceProperty {
        source_name: "line.1:count",
        data_type: "INTEGER",
        property_type: measurement,
        projected_name: "line_1_count",
        projected_type: "INTEGER",
        time_series: true,
    },
    SourceProperty {
        source_name: "running",
        data_type: "BOOLEAN",
        property_type: measurement,
        projected_name: "running",
        projected_type: "BOOLEAN",
        time_series: true,
    },
];

/// The `assetModelProperties` body the staged models carry.
fn source_property_definitions() -> Value {
    Value::Array(
        SOURCE_PROPERTIES
            .iter()
            .map(|property| {
                json!({
                    "name": property.source_name,
                    "dataType": property.data_type,
                    "type": (property.property_type)(),
                })
            })
            .collect(),
    )
}

/// Create one asset model in the source and answer the `assetModelId` it minted.
async fn create_asset_model(emulator: &Emulator, body: Value) -> String {
    let created = emulator.call_sitewise("POST", "/asset-models", Some(body)).await;
    assert_eq!(
        created.status, 202,
        "expected 202 from CreateAssetModel, got {created:?}"
    );
    created
        .member("assetModelId")
        .as_str()
        .unwrap_or_else(|| panic!("CreateAssetModel must mint an assetModelId, got {created:?}"))
        .to_string()
}

/// Create one asset in the source and answer the `assetId` it minted.
async fn create_asset(emulator: &Emulator, body: Value) -> String {
    let created = emulator.call_sitewise("POST", "/assets", Some(body)).await;
    assert_eq!(
        created.status, 202,
        "expected 202 from CreateAsset, got {created:?}"
    );
    created
        .member("assetId")
        .as_str()
        .unwrap_or_else(|| panic!("CreateAsset must mint an assetId, got {created:?}"))
        .to_string()
}

/// Associate `child` under the hierarchy `hierarchy_external_id` of `parent`,
/// which is what makes the source's hierarchy a parent/child relationship.
async fn associate_assets(
    emulator: &Emulator,
    parent: &str,
    hierarchy_external_id: &str,
    child: &str,
) {
    let associated = emulator
        .call_sitewise(
            "POST",
            &format!("/assets/{parent}/associate"),
            Some(json!({
                "hierarchyId": format!("externalId:{hierarchy_external_id}"),
                "childAssetId": child,
            })),
        )
        .await;
    assert_eq!(
        associated.status, 200,
        "expected 200 from AssociateAssets, got {associated:?}"
    );
}

/// The source hierarchy a case stages: the ids the source minted for the two
/// asset models and the three assets, so the projection can be asserted against
/// the resources it was projected FROM.
struct StagedSource {
    /// The model the parent asset is an instance of, which declares a hierarchy
    /// of the child model.
    plant_model_id: String,
    /// The model the two child assets are instances of.
    line_model_id: String,
    /// The root asset: nobody's child in the source.
    plant_asset_id: String,
    /// A child of the root asset.
    line_asset_id: String,
    /// A second child of the root asset, so the rule holds for more than one
    /// child of one parent.
    other_line_asset_id: String,
}

/// The external id the staged parent model's hierarchy carries, so a case can
/// associate assets under it without reading the id the source minted.
const HIERARCHY_EXTERNAL_ID: &str = "plantToLine";

/// Stage the source hierarchy every case projects: two asset models (one
/// declaring a hierarchy of the other) and three assets (a root with two
/// children).
async fn stage_source(emulator: &Emulator) -> StagedSource {
    let line_model_id = create_asset_model(
        emulator,
        json!({
            "assetModelName": "Production Line",
            "assetModelDescription": "a line of the plant",
            "assetModelProperties": source_property_definitions(),
        }),
    )
    .await;
    let plant_model_id = create_asset_model(
        emulator,
        json!({
            "assetModelName": "Plant",
            "assetModelProperties": source_property_definitions(),
            "assetModelHierarchies": [{
                "name": "lines",
                "externalId": HIERARCHY_EXTERNAL_ID,
                "childAssetModelId": line_model_id,
            }],
        }),
    )
    .await;
    let plant_asset_id = create_asset(
        emulator,
        json!({"assetName": "Plant 1", "assetModelId": plant_model_id}),
    )
    .await;
    let line_asset_id = create_asset(
        emulator,
        json!({
            "assetName": "Line A",
            "assetModelId": line_model_id,
            "assetDescription": "the first line",
        }),
    )
    .await;
    let other_line_asset_id = create_asset(
        emulator,
        json!({"assetName": "Line B", "assetModelId": line_model_id}),
    )
    .await;
    associate_assets(emulator, &plant_asset_id, HIERARCHY_EXTERNAL_ID, &line_asset_id).await;
    associate_assets(
        emulator,
        &plant_asset_id,
        HIERARCHY_EXTERNAL_ID,
        &other_line_asset_id,
    )
    .await;
    StagedSource {
        plant_model_id,
        line_model_id,
        plant_asset_id,
        line_asset_id,
        other_line_asset_id,
    }
}

/// An emulator holding the workspace the cases project into, with IoT SiteWise
/// mounted beside this service on the same engine.
async fn emulator_with_workspace() -> Emulator {
    let emulator = Emulator::with_sitewise();
    assert_eq!(
        emulator.create_workspace(WORKSPACE).await.status,
        200,
        "the workspace the cases project into must exist"
    );
    emulator
}

/// Create the sync job every case is about.
async fn create_sync_job(emulator: &Emulator) -> Resp {
    let created = emulator
        .create_sync_job(WORKSPACE, SYNC_SOURCE_SITEWISE, json!({"syncRole": SYNC_ROLE}))
        .await;
    assert_eq!(
        created.status, 200,
        "expected 200 from CreateSyncJob, got {created:?}"
    );
    created
}

/// The rows `ListSyncResources` reports for the job, keyed by `externalId`.
async fn sync_resource_rows(emulator: &Emulator) -> Vec<Value> {
    let listed = emulator
        .list_sync_resources(WORKSPACE, SYNC_SOURCE_SITEWISE, json!({"maxResults": 200}))
        .await;
    assert_eq!(
        listed.status, 200,
        "expected 200 from ListSyncResources, got {listed:?}"
    );
    listed
        .member("syncResources")
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("syncResources must be a present array, got {listed:?}"))
}

/// RULE: a sync job projects the resources of its source into the workspace that
/// holds it — one component type per asset model, extending the connector
/// built-in and declaring one property per property the model declares, and one
/// entity per asset, named as the asset is named, hanging off the entity its
/// parent asset was projected to and holding a component of its model's
/// component type — and every projected resource reports the job's `syncSource`.
#[tokio::test]
async fn a_sync_job_projects_its_sources_resources_into_the_workspace() {
    let emulator = emulator_with_workspace().await;
    let source = stage_source(&emulator).await;
    create_sync_job(&emulator).await;

    // `(case, the asset model the component type was projected from, the name
    //  the source gave it)`: the rows differ in whether the model declares a
    //  hierarchy of another model and whether it carries a description.
    let model_rows = [
        ("the parent model", &source.plant_model_id, "Plant"),
        ("the child model", &source.line_model_id, "Production Line"),
    ];
    for (case, asset_model_id, model_name) in model_rows {
        let component_type_id = sitewise_component_type_id(asset_model_id);
        let read = emulator
            .get_component_type(WORKSPACE, &component_type_id)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the asset model must be projected to a component type, got {read:?}"
        );
        assert_eq!(
            (
                read.member("syncSource").as_str(),
                read.member("componentTypeName").as_str(),
                read.member("extendsFrom"),
            ),
            (
                Some(SYNC_SOURCE_SITEWISE),
                Some(model_name),
                &json!([SITEWISE_CONNECTOR_COMPONENT_TYPE_ID]),
            ),
            "case {case:?}: a projected component type reports the job that projected it, the \
             source's own name and the connector it extends, got {read:?}"
        );
        // One property definition per property the source model declares, under
        // the name the modelled `Name` admits for it and with the data type and
        // the time-series flag the source's own kind of property gives it.
        for property in SOURCE_PROPERTIES {
            let definition = &read.member("propertyDefinitions")[property.projected_name];
            assert_eq!(
                (
                    definition["dataType"]["type"].as_str(),
                    definition["isTimeSeries"].as_bool(),
                    definition["displayName"].as_str(),
                ),
                (
                    Some(property.projected_type),
                    Some(property.time_series),
                    Some(property.source_name),
                ),
                "case {case:?}: the property {:?} must be projected as the source declares it, \
                 got {definition:?}",
                property.source_name
            );
            assert!(
                TYPE_VALUES.contains(&property.projected_type),
                "case {case:?}: the projected type must be a modelled Type, got {:?}",
                property.projected_type
            );
        }
    }

    // `(case, the asset the entity was projected from, its name in the source,
    //  the parentEntityId the projection gives it, the asset model it is an
    //  instance of)`: the rows differ in where the asset sits in the source's
    //  hierarchy — the root, a child, a second child of the same parent.
    let root = "$ROOT";
    let asset_rows = [
        (
            "the root asset",
            &source.plant_asset_id,
            "Plant 1",
            root.to_string(),
            &source.plant_model_id,
        ),
        (
            "a child asset",
            &source.line_asset_id,
            "Line A",
            source.plant_asset_id.clone(),
            &source.line_model_id,
        ),
        (
            "a second child of one parent",
            &source.other_line_asset_id,
            "Line B",
            source.plant_asset_id.clone(),
            &source.line_model_id,
        ),
    ];
    for (case, asset_id, asset_name, parent_entity_id, asset_model_id) in asset_rows {
        let read = emulator.get_entity(WORKSPACE, asset_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the asset must be projected to an entity, got {read:?}"
        );
        assert_eq!(
            (
                read.member("entityId").as_str(),
                read.member("entityName").as_str(),
                read.member("parentEntityId").as_str(),
                read.member("syncSource").as_str(),
            ),
            (
                Some(asset_id.as_str()),
                Some(asset_name),
                Some(parent_entity_id.as_str()),
                Some(SYNC_SOURCE_SITEWISE),
            ),
            "case {case:?}: a projected entity stands for its asset and reports the job that \
             projected it, got {read:?}"
        );
        // The component the entity holds instantiates its model's component
        // type and names the source resources it reads from, on the two
        // external-id properties the connector built-in declares.
        let component = &read.member("components")[SITEWISE_COMPONENT_NAME];
        assert_eq!(
            (
                component["componentTypeId"].as_str(),
                component["syncSource"].as_str(),
                component["properties"][SITEWISE_ASSET_ID_PROPERTY]["value"]["stringValue"]
                    .as_str(),
                component["properties"][SITEWISE_ASSET_MODEL_ID_PROPERTY]["value"]["stringValue"]
                    .as_str(),
            ),
            (
                Some(sitewise_component_type_id(asset_model_id).as_str()),
                Some(SYNC_SOURCE_SITEWISE),
                Some(asset_id.as_str()),
                Some(asset_model_id.as_str()),
            ),
            "case {case:?}: the projected component must name the source resources it stands \
             for, got {component:?}"
        );
        // The same component, through the listing that reports one: the two
        // readers must agree about where the resource came from.
        let listed = emulator
            .list_components(WORKSPACE, asset_id, json!({}))
            .await;
        let summaries = listed
            .member("componentSummaries")
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            summaries.iter().any(|summary| {
                summary["componentName"].as_str() == Some(SITEWISE_COMPONENT_NAME)
                    && summary["syncSource"].as_str() == Some(SYNC_SOURCE_SITEWISE)
            }),
            "case {case:?}: ListComponents must report the projected component's sync source, \
             got {listed:?}"
        );
    }

    // The child entities really hang off the parent entity, which is what the
    // source's hierarchy means in the workspace.
    let parent = emulator.get_entity(WORKSPACE, &source.plant_asset_id).await;
    assert_eq!(
        parent.member("hasChildEntities").as_bool(),
        Some(true),
        "case \"the root asset\": the projected parent must have the projected children, \
         got {parent:?}"
    );
}

/// RULE: a job reports one synchronised resource per resource of its source,
/// named at both ends — its `externalId` in the source and the `resourceId` it
/// was projected to in the workspace — in sync, of the type that says which kind
/// of workspace resource it is; and the job itself reaches the standing `ACTIVE`
/// state with no error.
#[tokio::test]
async fn a_job_reports_every_resource_it_synchronised() {
    let emulator = emulator_with_workspace().await;
    let source = stage_source(&emulator).await;
    create_sync_job(&emulator).await;

    // `(case, the resource's identity in the source, the id it is projected to,
    //  its resourceType)`: one row per source resource, differing in which kind
    //  of resource it is and where it sits in the source.
    let expected: Vec<(&str, String, String, &str)> = vec![
        (
            "the parent asset model",
            source.plant_model_id.clone(),
            sitewise_component_type_id(&source.plant_model_id),
            "COMPONENT_TYPE",
        ),
        (
            "the child asset model",
            source.line_model_id.clone(),
            sitewise_component_type_id(&source.line_model_id),
            "COMPONENT_TYPE",
        ),
        (
            "the root asset",
            source.plant_asset_id.clone(),
            source.plant_asset_id.clone(),
            "ENTITY",
        ),
        (
            "a child asset",
            source.line_asset_id.clone(),
            source.line_asset_id.clone(),
            "ENTITY",
        ),
        (
            "a second child asset",
            source.other_line_asset_id.clone(),
            source.other_line_asset_id.clone(),
            "ENTITY",
        ),
    ];
    let rows = sync_resource_rows(&emulator).await;
    assert_eq!(
        rows.len(),
        expected.len(),
        "the job must report one resource per source resource, got {rows:?}"
    );
    for (case, external_id, resource_id, resource_type) in &expected {
        let row = rows
            .iter()
            .find(|row| row["externalId"].as_str() == Some(external_id.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the job must report {external_id:?}, got {rows:?}")
            });
        assert_eq!(
            (
                row["resourceId"].as_str(),
                row["resourceType"].as_str(),
                row["status"]["state"].as_str(),
                &row["status"]["error"],
            ),
            (
                Some(resource_id.as_str()),
                Some(*resource_type),
                Some(SYNC_RESOURCE_STATE_IN_SYNC),
                &json!({}),
            ),
            "case {case:?}: a synchronised resource is named at both ends and in sync, \
             got {row:?}"
        );
        assert!(
            SYNC_RESOURCE_TYPE_VALUES.contains(&row["resourceType"].as_str().unwrap_or_default())
                && SYNC_RESOURCE_STATE_VALUES
                    .contains(&row["status"]["state"].as_str().unwrap_or_default()),
            "case {case:?}: the row's type and state must be modelled values, got {row:?}"
        );
    }

    // The job's own standing state, waited for as a condition and never as a
    // span: its link is up, and nothing about it failed.
    let settled =
        sync_job_in_state(&emulator, WORKSPACE, SYNC_SOURCE_SITEWISE, SYNC_JOB_STATE_ACTIVE).await;
    assert_eq!(
        &settled.member("status")["error"],
        &json!({}),
        "a job whose projection landed carries no error, got {settled:?}"
    );
}

/// RULE: a resource the workspace will not take fails THAT resource — which is
/// reported in the modelled `ERROR` state carrying the reason the rule that
/// refused it produced, and leaves no workspace resource behind — and the job
/// settles in the modelled failure state carrying that same reason, rather than
/// reporting a link that came up.
#[tokio::test]
async fn a_resource_the_workspace_refuses_fails_that_resource_and_the_job() {
    let emulator = emulator_with_workspace().await;
    let source = stage_source(&emulator).await;
    // A component type the workspace ALREADY holds under the id the child
    // model projects to, and which no component may instantiate: it declares a
    // property stored outside the workspace and no connector that could serve
    // it, which is what makes a component type abstract
    // (`GetComponentType$isAbstract`). The projection therefore finds that id
    // taken and the entities of that model cannot hold a component of it.
    let taken = sitewise_component_type_id(&source.line_model_id);
    let blocking = emulator
        .create_component_type(
            WORKSPACE,
            &taken,
            json!({
                "propertyDefinitions": {
                    "externalOnly": {"dataType": {"type": "STRING"}, "isStoredExternally": true},
                },
            }),
        )
        .await;
    assert_eq!(
        blocking.status, 200,
        "the component type that blocks the projection must be created, got {blocking:?}"
    );

    create_sync_job(&emulator).await;

    let rows = sync_resource_rows(&emulator).await;
    // `(case, the source resource, whether the workspace took it)`.
    let expected: Vec<(&str, String, bool)> = vec![
        ("the parent asset model", source.plant_model_id.clone(), true),
        ("the blocked child model", source.line_model_id.clone(), true),
        ("the root asset", source.plant_asset_id.clone(), true),
        ("a blocked child asset", source.line_asset_id.clone(), false),
        (
            "a second blocked child asset",
            source.other_line_asset_id.clone(),
            false,
        ),
    ];
    for (case, external_id, landed) in &expected {
        let row = rows
            .iter()
            .find(|row| row["externalId"].as_str() == Some(external_id.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the job must report {external_id:?}, got {rows:?}")
            });
        let expected_state = match landed {
            true => SYNC_RESOURCE_STATE_IN_SYNC,
            false => SYNC_RESOURCE_STATE_ERROR,
        };
        assert_eq!(
            row["status"]["state"].as_str(),
            Some(expected_state),
            "case {case:?}: expected the state of ITS synchronisation, got {row:?}"
        );
        if *landed {
            continue;
        }
        assert_eq!(
            (
                row["resourceId"].as_str(),
                row["status"]["error"]["code"].as_str(),
            ),
            (None, Some(SYNC_RESOURCE_ERROR_CODE)),
            "case {case:?}: a resource the workspace would not take has no id in it and says \
             why, got {row:?}"
        );
        assert!(
            row["status"]["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(&taken)),
            "case {case:?}: the reason must name what refused the resource, got {row:?}"
        );
        // Nothing was left behind for it.
        let read = emulator.get_entity(WORKSPACE, external_id).await;
        assert_eq!(
            (read.status, read.error_type.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case:?}: a resource that failed leaves no workspace resource, got {read:?}"
        );
    }

    // The job failed bringing its link up, and says so with the modelled code
    // for exactly that.
    let settled =
        sync_job_in_state(&emulator, WORKSPACE, SYNC_SOURCE_SITEWISE, SYNC_JOB_STATE_ERROR).await;
    assert_eq!(
        settled.member("status")["error"]["code"].as_str(),
        Some(SYNC_JOB_ERROR_CODE),
        "a job that could not land its projection reports the failure code, got {settled:?}"
    );
    assert!(
        settled.member("status")["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(&taken)),
        "the job's reason must be the reason a resource could not be landed, got {settled:?}"
    );
}

/// RULE: `syncSource` reports where a resource CAME FROM, so a resource a caller
/// created carries none — on the entity, on each of its components and on the
/// component type — however many projected resources sit beside it in the same
/// workspace.
#[tokio::test]
async fn a_resource_a_caller_created_reports_no_sync_source() {
    let emulator = emulator_with_workspace().await;
    stage_source(&emulator).await;
    create_sync_job(&emulator).await;

    const OWN_COMPONENT_TYPE: &str = "com.example.pump";
    const OWN_COMPONENT: &str = "pumpComponent";
    assert_eq!(
        emulator
            .create_component_type(
                WORKSPACE,
                OWN_COMPONENT_TYPE,
                json!({"propertyDefinitions": {"rpm": {"dataType": {"type": "DOUBLE"}}}}),
            )
            .await
            .status,
        200,
        "the caller's own component type must be created"
    );
    // `(case, entityId, the components the create carries)`: the rows differ in
    // whether the caller's entity holds a component at all, since the member is
    // reported on the entity and on each of its components.
    let rows: Vec<(&str, &str, Value)> = vec![
        ("an entity with no component", "ownPlainEntity", json!({})),
        (
            "an entity holding a component",
            "ownComponentEntity",
            json!({OWN_COMPONENT: {"componentTypeId": OWN_COMPONENT_TYPE}}),
        ),
    ];
    for (case, entity_id, components) in &rows {
        let created = emulator
            .create_entity(
                WORKSPACE,
                json!({
                    "entityId": entity_id,
                    "entityName": entity_id,
                    "components": components,
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the caller's own entity must be created, got {created:?}"
        );
        let read = emulator.get_entity(WORKSPACE, entity_id).await;
        assert!(
            read.member("syncSource").is_null(),
            "case {case:?}: an entity a caller created is projected from nothing, got {read:?}"
        );
        for (component_name, component) in read
            .member("components")
            .as_object()
            .cloned()
            .unwrap_or_default()
        {
            assert!(
                component["syncSource"].is_null(),
                "case {case:?}: the component {component_name:?} is projected from nothing, \
                 got {component:?}"
            );
        }
    }
    let component_type = emulator
        .get_component_type(WORKSPACE, OWN_COMPONENT_TYPE)
        .await;
    assert!(
        component_type.member("syncSource").is_null(),
        "case \"the caller's own component type\": it is projected from nothing, \
         got {component_type:?}"
    );
    // The service's own built-in, which every workspace is provisioned with, is
    // projected from nothing either.
    let built_in = emulator
        .get_component_type(WORKSPACE, SITEWISE_CONNECTOR_COMPONENT_TYPE_ID)
        .await;
    assert!(
        built_in.member("syncSource").is_null(),
        "case \"the connector built-in\": a component type the service ships is projected from \
         nothing, got {built_in:?}"
    );
}
