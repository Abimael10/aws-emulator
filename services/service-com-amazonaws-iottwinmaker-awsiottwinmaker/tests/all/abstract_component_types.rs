// SPDX-License-Identifier: CC0-1.0

//! Abstract component types and SINGLETON ones, end to end over HTTP.
//!
//! Every case drives the real request path — `MultiServiceRouter::dispatch`, the
//! same entry the served emulator uses — so a route, a label, a status code and
//! a serialized body are all exercised, not just a handler body.
//!
//! Two rules a component TYPE decides for the components that instantiate it,
//! which is why they share a suite.
//!
//! The first: a component type that declares an externally stored
//! property and carries no data connector that could read or write one — its
//! own or an inherited one — has no way to serve that property's values, so it
//! is ABSTRACT: `GetComponentType` reports `isAbstract` true for it, and a
//! `CreateEntity` whose component instantiates it is refused with the modelled
//! 400 `ValidationException` before anything is stored. Every other component
//! type is concrete and a component of it is accepted.
//!
//! The rows differ along what the rule generalises over — whether the external
//! property is a time series, which data-connector name backs it, whether the
//! property or the connector is the type's own or inherited, and how many
//! properties the type declares — because those are the dimensions the rule
//! ranges over; the time-series row is the one recorded traffic observed.
//!
//! The second: a component type created or updated with `isSingleton` is one
//! whose member says whether "an entity can have more than one component of
//! this type", so an entity holds at most ONE component of such a type and a
//! write that would leave it holding a further one is refused with the same
//! modelled 400 `ValidationException` before anything is stored. Those rows
//! differ along how many components of the type one request carries, whether
//! they are the entity's own top-level components or composite components below
//! them, which writer carries them (`CreateEntity`, `UpdateEntity`'s
//! `componentUpdates` and `compositeComponentUpdates`) and whether the type is a
//! singleton at all.

use serde_json::{json, Value};
use service_com_amazonaws_iottwinmaker_awsiottwinmaker::state::{
    DATA_CONNECTOR_FUNCTION_NAMES, SCHEMA_INITIALIZER_FUNCTION,
};

use crate::harness::{Emulator, Resp};

/// The workspace every case of a suite is created in.
const WORKSPACE: &str = "abstractWs";

/// A component type that carries a data connector and no property at all, so a
/// type extending it INHERITS the connector.
const CONNECTOR_PARENT: &str = "com.example.connector.parent";
/// A component type that carries an externally stored property and no
/// connector, so a type extending it INHERITS the unservable property — and is
/// itself abstract.
const EXTERNAL_PARENT: &str = "com.example.external.parent";

/// One externally stored property definition: `isStoredExternally` is "a
/// Boolean value that specifies whether the property is stored externally" (the
/// member's own documentation), which is what leaves its values unservable
/// without a connector.
fn external_property(is_time_series: bool) -> Value {
    json!({
        "dataType": {"type": "DOUBLE"},
        "isTimeSeries": is_time_series,
        "isStoredExternally": true,
    })
}

/// One function entry under `function_name`, implemented by a lambda the way
/// `FunctionRequest::implementedBy` (a `DataConnector`) models it.
fn function(function_name: &str) -> Value {
    json!({
        function_name: {
            "scope": "ENTITY",
            "implementedBy": {
                "lambda": {"arn": "arn:aws:lambda:us-west-2:000000000000:function:connector"},
            },
        },
    })
}

/// The message a refused `CreateEntity` carries, read off the error body.
fn message(resp: &Resp) -> String {
    resp.member("message").as_str().unwrap_or_default().to_string()
}

/// The refusal AWS builds from the request for a component of an abstract
/// component type: it names the component type the call asked for.
fn abstract_refusal(component_type_id: &str) -> String {
    format!("Abstract ComponentType {component_type_id} can not be used for creating a component")
}

/// The `entityName`s one `ListEntities` page reports.
fn listed_names(resp: &Resp) -> Vec<String> {
    resp.member("entitySummaries")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| summary["entityName"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Create the workspace and the two parent component types the inheritance rows
/// extend.
async fn seeded() -> Emulator {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_workspace(WORKSPACE).await.status,
        200,
        "the workspace every case is created in must exist"
    );
    assert_eq!(
        emulator
            .create_component_type(
                WORKSPACE,
                CONNECTOR_PARENT,
                json!({
                    "propertyDefinitions": {"Label": {"dataType": {"type": "STRING"}}},
                    "functions": function("dataReader"),
                }),
            )
            .await
            .status,
        200,
        "the connector-carrying parent must be created"
    );
    assert_eq!(
        emulator
            .create_component_type(
                WORKSPACE,
                EXTERNAL_PARENT,
                json!({"propertyDefinitions": {"Inherited": external_property(true)}}),
            )
            .await
            .status,
        200,
        "the external-property parent must be created"
    );
    emulator
}

/// RULE: a component type is abstract exactly when it declares an externally
/// stored property (its own or inherited) and no data connector (its own or
/// inherited) — and an abstract component type cannot be instantiated.
#[tokio::test]
async fn abstractness_is_decided_by_an_unservable_property_and_bars_instantiation() {
    let emulator = seeded().await;

    // (case/componentTypeId, the create body, whether the type is abstract).
    // The connector names come from the service's own catalogue
    // (`state::DATA_CONNECTOR_FUNCTION_NAMES`), so a name added to or dropped
    // from it moves these rows with it — including the ones the issue never
    // cited (`dataWriter`, `dataReaderByComponentType`).
    let mut rows: Vec<(String, Value, bool)> = vec![
        (
            // The recorded shape: one externally stored time-series property
            // and no functions at all.
            "com.example.timeSeriesExternal".to_string(),
            json!({"propertyDefinitions": {"Temperature": external_property(true)}}),
            true,
        ),
        (
            // The same gap with a property that is NOT a time series.
            "com.example.plainExternal".to_string(),
            json!({"propertyDefinitions": {"Serial": external_property(false)}}),
            true,
        ),
        (
            // Several properties, only one of them externally stored: one
            // unservable property is enough.
            "com.example.mixedProperties".to_string(),
            json!({
                "propertyDefinitions": {
                    "Label": {"dataType": {"type": "STRING"}},
                    "Count": {"dataType": {"type": "INTEGER"}},
                    "Temperature": external_property(true),
                },
            }),
            true,
        ),
        (
            // A schema initializer is not a data connector: it initializes the
            // schema and serves no property's values.
            "com.example.schemaInitializerOnly".to_string(),
            json!({
                "propertyDefinitions": {"Temperature": external_property(true)},
                "functions": function(SCHEMA_INITIALIZER_FUNCTION),
            }),
            true,
        ),
        (
            // No externally stored property at all: nothing is unservable.
            "com.example.plainOnly".to_string(),
            json!({"propertyDefinitions": {"Label": {"dataType": {"type": "STRING"}}}}),
            false,
        ),
        (
            // No properties at all.
            "com.example.bare".to_string(),
            json!({}),
            false,
        ),
        (
            // The property is the type's own, the connector INHERITED.
            "com.example.inheritedConnector".to_string(),
            json!({
                "extendsFrom": [CONNECTOR_PARENT],
                "propertyDefinitions": {"Temperature": external_property(true)},
            }),
            false,
        ),
        (
            // The property is INHERITED and no connector reaches it.
            "com.example.inheritedExternalProperty".to_string(),
            json!({"extendsFrom": [EXTERNAL_PARENT]}),
            true,
        ),
        (
            // The property is inherited from one parent and the connector from
            // another.
            "com.example.inheritedBoth".to_string(),
            json!({"extendsFrom": [EXTERNAL_PARENT, CONNECTOR_PARENT]}),
            false,
        ),
    ];
    // One row per data-connector name the service publishes: each of them backs
    // an externally stored property, so each leaves the type concrete.
    for connector in DATA_CONNECTOR_FUNCTION_NAMES {
        rows.push((
            format!("com.example.backedBy.{connector}"),
            json!({
                "propertyDefinitions": {"Temperature": external_property(true)},
                "functions": function(connector),
            }),
            false,
        ));
    }

    for (component_type_id, request, is_abstract) in &rows {
        let created = emulator
            .create_component_type(WORKSPACE, component_type_id, request.clone())
            .await;
        assert_eq!(
            created.status, 200,
            "case {component_type_id:?}: expected 200 from CreateComponentType, got {created:?}"
        );

        let read = emulator
            .get_component_type(WORKSPACE, component_type_id)
            .await;
        assert_eq!(
            read.member("isAbstract").as_bool(),
            Some(*is_abstract),
            "case {component_type_id:?}: expected isAbstract {is_abstract}, got {read:?}"
        );

        // The same component type, instantiated by an entity's component.
        let entity_name = format!("entity for {component_type_id}");
        let create_entity = emulator
            .create_entity(
                WORKSPACE,
                json!({
                    "entityName": entity_name,
                    "components": {"Sensor": {"componentTypeId": component_type_id}},
                }),
            )
            .await;
        if *is_abstract {
            assert_eq!(
                (create_entity.status, create_entity.error_type.as_deref()),
                (400, Some("ValidationException")),
                "case {component_type_id:?}: an abstract component type must be refused, \
                 got {create_entity:?}"
            );
            assert_eq!(
                message(&create_entity),
                abstract_refusal(component_type_id),
                "case {component_type_id:?}: expected the refusal to name the component type, \
                 got {create_entity:?}"
            );
            let listed = emulator.list_entities(WORKSPACE, json!({})).await;
            assert!(
                !listed_names(&listed).contains(&entity_name),
                "case {component_type_id:?}: a refused create must persist no entity, \
                 got {listed:?}"
            );
        } else {
            assert_eq!(
                create_entity.status, 200,
                "case {component_type_id:?}: a concrete component type must be accepted, \
                 got {create_entity:?}"
            );
            // Round trip: the component really was persisted against the type
            // the create named.
            let entity_id = create_entity
                .member("entityId")
                .as_str()
                .unwrap_or_default()
                .to_string();
            let entity = emulator.get_entity(WORKSPACE, &entity_id).await;
            assert_eq!(
                entity.member("components")["Sensor"]["componentTypeId"].as_str(),
                Some(component_type_id.as_str()),
                "case {component_type_id:?}: the stored component must name the created type, \
                 got {entity:?}"
            );
        }
    }

    // Controls: an entity with no components at all, and one whose component
    // names a component type this workspace does not hold, are both accepted —
    // the refusal is about abstractness, not about having components.
    for (case, request) in [
        ("noComponents", json!({"entityName": "no components"})),
        (
            "componentOfAnUnheldType",
            json!({
                "entityName": "component of an unheld type",
                "components": {
                    "Sensor": {"componentTypeId": "com.example.not.in.this.workspace"},
                },
            }),
        ),
    ] {
        let created = emulator.create_entity(WORKSPACE, request).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateEntity, got {created:?}"
        );
    }
}

/// RULE: a COMPOSITE component instantiates the component type the declaration
/// its path resolved to names, so the same abstractness rule refuses it — and a
/// composite component of a concrete type is still accepted.
#[tokio::test]
async fn a_composite_component_of_an_abstract_component_type_is_refused() {
    let emulator = seeded().await;

    // A holder whose two composite declarations differ only in the abstractness
    // of the type they name, so the path that is refused is decided by that
    // alone.
    assert_eq!(
        emulator
            .create_component_type(
                WORKSPACE,
                "com.example.concrete.leaf",
                json!({"propertyDefinitions": {"Label": {"dataType": {"type": "STRING"}}}}),
            )
            .await
            .status,
        200,
        "the concrete leaf type must be created"
    );
    assert_eq!(
        emulator
            .create_component_type(
                WORKSPACE,
                "com.example.holder",
                json!({
                    "compositeComponentTypes": {
                        "abstractLeaf": {"componentTypeId": EXTERNAL_PARENT},
                        "concreteLeaf": {"componentTypeId": "com.example.concrete.leaf"},
                    },
                }),
            )
            .await
            .status,
        200,
        "the holder type must be created"
    );

    // (case, the composite path the create addresses, whether it is refused).
    let rows = [
        ("abstractLeaf", "Holder/abstractLeaf", true),
        ("concreteLeaf", "Holder/concreteLeaf", false),
    ];

    for (case, component_path, refused) in rows {
        let entity_name = format!("composite {case}");
        let created = emulator
            .create_entity(
                WORKSPACE,
                json!({
                    "entityName": entity_name,
                    "components": {"Holder": {"componentTypeId": "com.example.holder"}},
                    "compositeComponents": {component_path: {}},
                }),
            )
            .await;
        if refused {
            assert_eq!(
                (created.status, created.error_type.as_deref()),
                (400, Some("ValidationException")),
                "case {case:?}: an abstract composite type must be refused, got {created:?}"
            );
            assert_eq!(
                message(&created),
                abstract_refusal(EXTERNAL_PARENT),
                "case {case:?}: expected the refusal to name the component type, got {created:?}"
            );
            let listed = emulator.list_entities(WORKSPACE, json!({})).await;
            assert!(
                !listed_names(&listed).contains(&entity_name),
                "case {case:?}: a refused create must persist no entity, got {listed:?}"
            );
        } else {
            assert_eq!(
                created.status, 200,
                "case {case:?}: a concrete composite type must be accepted, got {created:?}"
            );
            let entity_id = created.member("entityId").as_str().unwrap_or_default().to_string();
            let entity = emulator.get_entity(WORKSPACE, &entity_id).await;
            assert_eq!(
                entity.member("components")["Holder"]["compositeComponents"][case]
                    ["componentTypeId"]
                    .as_str(),
                Some("com.example.concrete.leaf"),
                "case {case:?}: the composite component must read back, got {entity:?}"
            );
        }
    }
}

/// RULE: abstractness is decided from what the component type holds, so a patch
/// that adds or removes the data connector moves it — in BOTH directions — and
/// what `CreateEntity` accepts moves with it.
#[tokio::test]
async fn a_patch_that_changes_the_connector_changes_abstractness() {
    let emulator = seeded().await;
    let component_type_id = "com.example.patched";
    assert_eq!(
        emulator
            .create_component_type(
                WORKSPACE,
                component_type_id,
                json!({"propertyDefinitions": {"Temperature": external_property(true)}}),
            )
            .await
            .status,
        200,
        "the component type under patch must be created"
    );

    // (case, the patch, whether the component type is abstract afterwards).
    let rows: Vec<(&str, Option<Value>, bool)> = vec![
        ("asCreated", None, true),
        (
            "connectorAdded",
            Some(json!({"functions": function("dataReader")})),
            false,
        ),
        (
            // The functions map is replaced WHOLE, so naming it empty drops the
            // connector and the property is unservable again.
            "connectorRemoved",
            Some(json!({"functions": {}})),
            true,
        ),
        (
            // The property goes instead of the connector: a type with no
            // externally stored property is concrete however few functions it
            // has.
            "externalPropertyDropped",
            Some(json!({"propertyDefinitions": {"Label": {"dataType": {"type": "STRING"}}}})),
            false,
        ),
    ];

    for (case, patch, is_abstract) in &rows {
        if let Some(patch) = patch {
            let updated = emulator
                .update_component_type(WORKSPACE, component_type_id, patch.clone())
                .await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected 200 from UpdateComponentType, got {updated:?}"
            );
        }
        let read = emulator
            .get_component_type(WORKSPACE, component_type_id)
            .await;
        assert_eq!(
            read.member("isAbstract").as_bool(),
            Some(*is_abstract),
            "case {case:?}: expected isAbstract {is_abstract}, got {read:?}"
        );

        let created = emulator
            .create_entity(
                WORKSPACE,
                json!({
                    "entityName": format!("patched {case}"),
                    "components": {"Sensor": {"componentTypeId": component_type_id}},
                }),
            )
            .await;
        let expected = if *is_abstract { 400 } else { 200 };
        assert_eq!(
            created.status, expected,
            "case {case:?}: expected HTTP {expected} from CreateEntity, got {created:?}"
        );
    }
}

/// A component type created with `isSingleton` true, so an entity holds at most
/// one component of it.
const SINGLETON: &str = "com.example.singleton.motor";
/// The same shape declared with `isSingleton` false: an entity holds as many
/// components of it as it likes.
const MULTIPLE: &str = "com.example.plain.sensor";
/// A singleton component type a holder DECLARES as a composite component type,
/// so an entity reaches a component of it BELOW one of its own components.
const SINGLETON_LEAF: &str = "com.example.singleton.leaf";
/// The holder whose `compositeComponentTypes` name the leaves the composite rows
/// address.
const HOLDER: &str = "com.example.singleton.holder";
/// A `componentTypeId` the workspace holds no component type under: nothing is
/// known about the multiplicity of one that is not stored, so components of it
/// are not counted.
const UNHELD_TYPE: &str = "com.example.not.in.this.workspace";

/// The refusal AWS builds from the request for a further component of a
/// singleton component type: it names the component type the call asked for,
/// exactly as the abstractness refusal above names the one it refused.
fn singleton_refusal(component_type_id: &str) -> String {
    format!(
        "Singleton ComponentType {component_type_id} can not be used for creating more than one \
         component in an entity"
    )
}

/// The workspace with the singleton, non-singleton and holder component types
/// the singleton rows instantiate, each read back through `GetComponentType` so
/// the rule's INPUT — the flag the create declared — is pinned before anything
/// is asked of it.
async fn with_singleton_types() -> Emulator {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_workspace(WORKSPACE).await.status,
        200,
        "the workspace every singleton case works in must exist"
    );
    // (componentTypeId, the create body, the `isSingleton` a read must report).
    let component_types = [
        (
            SINGLETON,
            json!({
                "isSingleton": true,
                "propertyDefinitions": {"rpm": {"dataType": {"type": "DOUBLE"}}},
            }),
            true,
        ),
        (
            MULTIPLE,
            json!({
                "isSingleton": false,
                "propertyDefinitions": {"Label": {"dataType": {"type": "STRING"}}},
            }),
            false,
        ),
        (SINGLETON_LEAF, json!({"isSingleton": true}), true),
        (
            HOLDER,
            json!({
                "compositeComponentTypes": {
                    "leafA": {"componentTypeId": SINGLETON_LEAF},
                    "leafB": {"componentTypeId": SINGLETON_LEAF},
                    "motorLeaf": {"componentTypeId": SINGLETON},
                    "sensorLeafA": {"componentTypeId": MULTIPLE},
                    "sensorLeafB": {"componentTypeId": MULTIPLE},
                },
            }),
            false,
        ),
    ];
    for (component_type_id, body, is_singleton) in component_types {
        let created = emulator
            .create_component_type(WORKSPACE, component_type_id, body)
            .await;
        assert_eq!(
            created.status, 200,
            "case {component_type_id:?}: expected 200 from CreateComponentType, got {created:?}"
        );
        let read = emulator
            .get_component_type(WORKSPACE, component_type_id)
            .await;
        assert_eq!(
            read.member("isSingleton").as_bool(),
            Some(is_singleton),
            "case {component_type_id:?}: expected isSingleton {is_singleton}, got {read:?}"
        );
    }
    emulator
}

/// The `componentTypeId` a read-back entity reports at `component_path`: a
/// top-level component's own, or the one of the composite component a single
/// segment below it.
fn component_type_at_path(entity: &Resp, component_path: &str) -> Option<String> {
    let (component_name, composite_name) = match component_path.split_once('/') {
        Some((component_name, composite_name)) => (component_name, Some(composite_name)),
        None => (component_path, None),
    };
    let component = &entity.member("components")[component_name];
    let reported = match composite_name {
        None => &component["componentTypeId"],
        Some(composite_name) => {
            &component["compositeComponents"][composite_name]["componentTypeId"]
        }
    };
    reported.as_str().map(str::to_string)
}

/// Assert the entity `entity_id` holds exactly the (path, componentTypeId) pairs
/// `expected` names, and nothing at the paths it does not.
async fn assert_components(emulator: &Emulator, case: &str, entity_id: &str, expected: &[(&str, &str)]) {
    let read = emulator.get_entity(WORKSPACE, entity_id).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected 200 from GetEntity, got {read:?}"
    );
    for (component_path, component_type_id) in expected {
        assert_eq!(
            component_type_at_path(&read, component_path).as_deref(),
            Some(*component_type_id),
            "case {case:?}: expected {component_path} to instantiate {component_type_id}, \
             got {read:?}"
        );
    }
    let held = read
        .member("components")
        .as_object()
        .map(|components| components.len())
        .unwrap_or_default();
    let expected_top_level = expected
        .iter()
        .filter(|(component_path, _)| !component_path.contains('/'))
        .count();
    assert_eq!(
        held, expected_top_level,
        "case {case:?}: expected {expected_top_level} top-level components, got {read:?}"
    );
}

/// RULE: an entity holds at most ONE component of a singleton component type —
/// counted over every component it holds, top-level and composite alike — so a
/// create carrying a further one is refused with a `ValidationException` and
/// nothing is stored, while any number of components of a non-singleton type is
/// accepted.
#[tokio::test]
async fn a_create_cannot_give_one_entity_two_components_of_a_singleton_type() {
    let emulator = with_singleton_types().await;

    // (case, which is also the entityId; the `components`; the
    // `compositeComponents`; the component type a refusal names; the
    // (path, componentTypeId) pairs an accepted entity reads back).
    let rows = vec![
        (
            "oneSingletonComponent",
            json!({"motorA": {"componentTypeId": SINGLETON}}),
            json!({}),
            None,
            vec![("motorA", SINGLETON)],
        ),
        (
            // The reported shape: two top-level components naming one
            // singleton component type.
            "twoSingletonComponents",
            json!({
                "motorA": {"componentTypeId": SINGLETON},
                "motorB": {"componentTypeId": SINGLETON},
            }),
            json!({}),
            Some(SINGLETON),
            vec![],
        ),
        (
            "threeSingletonComponents",
            json!({
                "motorA": {"componentTypeId": SINGLETON},
                "motorB": {"componentTypeId": SINGLETON},
                "motorC": {"componentTypeId": SINGLETON},
            }),
            json!({}),
            Some(SINGLETON),
            vec![],
        ),
        (
            // The same multiplicity against a type whose flag is false.
            "twoComponentsOfANonSingletonType",
            json!({
                "sensorA": {"componentTypeId": MULTIPLE},
                "sensorB": {"componentTypeId": MULTIPLE},
            }),
            json!({}),
            None,
            vec![("sensorA", MULTIPLE), ("sensorB", MULTIPLE)],
        ),
        (
            // One of each: the singleton's own count is what decides.
            "oneSingletonBesideTwoOfANonSingletonType",
            json!({
                "motorA": {"componentTypeId": SINGLETON},
                "sensorA": {"componentTypeId": MULTIPLE},
                "sensorB": {"componentTypeId": MULTIPLE},
            }),
            json!({}),
            None,
            vec![
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            // Two COMPOSITE components of one singleton type, below one holder.
            "twoCompositesOfOneSingletonTypeUnderOneHolder",
            json!({"holder": {"componentTypeId": HOLDER}}),
            json!({"holder/leafA": {}, "holder/leafB": {}}),
            Some(SINGLETON_LEAF),
            vec![],
        ),
        (
            // One composite each below two different holders: they are both
            // components of the same entity.
            "compositesOfOneSingletonTypeUnderTwoHolders",
            json!({
                "holderA": {"componentTypeId": HOLDER},
                "holderB": {"componentTypeId": HOLDER},
            }),
            json!({"holderA/leafA": {}, "holderB/leafA": {}}),
            Some(SINGLETON_LEAF),
            vec![],
        ),
        (
            "oneCompositeOfASingletonType",
            json!({"holder": {"componentTypeId": HOLDER}}),
            json!({"holder/leafA": {}}),
            None,
            vec![("holder", HOLDER), ("holder/leafA", SINGLETON_LEAF)],
        ),
        (
            // A top-level component and a composite one of the SAME singleton
            // type: the count is over every path.
            "aTopLevelComponentAndACompositeOfOneSingletonType",
            json!({
                "motorA": {"componentTypeId": SINGLETON},
                "holder": {"componentTypeId": HOLDER},
            }),
            json!({"holder/motorLeaf": {}}),
            Some(SINGLETON),
            vec![],
        ),
        (
            "twoCompositesOfANonSingletonType",
            json!({"holder": {"componentTypeId": HOLDER}}),
            json!({"holder/sensorLeafA": {}, "holder/sensorLeafB": {}}),
            None,
            vec![
                ("holder", HOLDER),
                ("holder/sensorLeafA", MULTIPLE),
                ("holder/sensorLeafB", MULTIPLE),
            ],
        ),
        (
            // A component type the workspace does not hold: the rule knows
            // nothing about its multiplicity, so both components are stored.
            "twoComponentsOfAnUnheldType",
            json!({
                "unheldA": {"componentTypeId": UNHELD_TYPE},
                "unheldB": {"componentTypeId": UNHELD_TYPE},
            }),
            json!({}),
            None,
            vec![("unheldA", UNHELD_TYPE), ("unheldB", UNHELD_TYPE)],
        ),
    ];

    for (case, components, composite_components, refused, read_back) in rows {
        let created = emulator
            .create_entity(
                WORKSPACE,
                json!({
                    "entityId": case,
                    "entityName": case,
                    "components": components,
                    "compositeComponents": composite_components,
                }),
            )
            .await;
        match refused {
            Some(component_type_id) => {
                assert_eq!(
                    (created.status, created.error_type.as_deref()),
                    (400, Some("ValidationException")),
                    "case {case:?}: a further component of a singleton type must be refused, \
                     got {created:?}"
                );
                assert_eq!(
                    message(&created),
                    singleton_refusal(component_type_id),
                    "case {case:?}: expected the refusal to name the component type, \
                     got {created:?}"
                );
                let read = emulator.get_entity(WORKSPACE, case).await;
                assert_eq!(
                    read.status, 404,
                    "case {case:?}: a refused create must persist no entity, got {read:?}"
                );
            }
            None => {
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: expected 200 from CreateEntity, got {created:?}"
                );
                assert_components(&emulator, case, case, &read_back).await;
            }
        }
    }
}

/// RULE: the multiplicity is decided over the RECORD a write would leave
/// behind, so a patch is refused exactly when it would raise the entity's count
/// of components of a singleton type past one — whichever writer carries it —
/// and a refused patch leaves the stored entity as it was.
#[tokio::test]
async fn a_patch_cannot_give_one_entity_two_components_of_a_singleton_type() {
    let emulator = with_singleton_types().await;
    let entity_id = "pump";
    let created = emulator
        .create_entity(
            WORKSPACE,
            json!({
                "entityId": entity_id,
                "entityName": "Pump",
                "components": {"motorA": {"componentTypeId": SINGLETON}},
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the entity under patch must be created, got {created:?}"
    );

    // (case, the patch, the component type a refusal names, the
    // (path, componentTypeId) pairs the entity holds AFTERWARDS). Applied in
    // order to the one entity, so every row is decided against what its
    // predecessors left stored.
    let rows = vec![
        (
            "secondComponentOfTheSingleton",
            json!({
                "componentUpdates": {
                    "motorB": {"componentTypeId": SINGLETON, "updateType": "CREATE"},
                },
            }),
            Some(SINGLETON),
            vec![("motorA", SINGLETON)],
        ),
        (
            // An entry naming no `updateType` creates what the entity does not
            // hold, so it is held to the same rule.
            "secondComponentWithNoUpdateType",
            json!({"componentUpdates": {"motorB": {"componentTypeId": SINGLETON}}}),
            Some(SINGLETON),
            vec![("motorA", SINGLETON)],
        ),
        (
            "componentOfANonSingletonType",
            json!({
                "componentUpdates": {
                    "sensorA": {"componentTypeId": MULTIPLE, "updateType": "CREATE"},
                },
            }),
            None,
            vec![("motorA", SINGLETON), ("sensorA", MULTIPLE)],
        ),
        (
            "secondComponentOfANonSingletonType",
            json!({"componentUpdates": {"sensorB": {"componentTypeId": MULTIPLE}}}),
            None,
            vec![
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            // No component is added: a STORED component is re-pointed onto the
            // singleton type the entity already holds one of.
            "anExistingComponentRepointedOntoTheSingleton",
            json!({
                "componentUpdates": {
                    "sensorB": {"componentTypeId": SINGLETON, "updateType": "UPDATE"},
                },
            }),
            Some(SINGLETON),
            vec![
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            // A patch of the singleton component itself leaves the count where
            // it was, so it is accepted.
            "aPatchOfTheSingletonComponentItself",
            json!({
                "componentUpdates": {
                    "motorA": {"description": "the only motor", "updateType": "UPDATE"},
                },
            }),
            None,
            vec![
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            "theHolderTheCompositeRowsHangOff",
            json!({
                "componentUpdates": {
                    "holder": {"componentTypeId": HOLDER, "updateType": "CREATE"},
                },
            }),
            None,
            vec![
                ("holder", HOLDER),
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            // A COMPOSITE component of the singleton type the entity already
            // holds a top-level component of.
            "aCompositeOfTheSingletonBesideIt",
            json!({"compositeComponentUpdates": {"holder/motorLeaf": {"updateType": "CREATE"}}}),
            Some(SINGLETON),
            vec![
                ("holder", HOLDER),
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            "aCompositeOfANonSingletonType",
            json!({
                "compositeComponentUpdates": {"holder/sensorLeafA": {"updateType": "CREATE"}},
            }),
            None,
            vec![
                ("holder", HOLDER),
                ("holder/sensorLeafA", MULTIPLE),
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            "aCompositeOfASingletonLeaf",
            json!({"compositeComponentUpdates": {"holder/leafA": {"updateType": "CREATE"}}}),
            None,
            vec![
                ("holder", HOLDER),
                ("holder/leafA", SINGLETON_LEAF),
                ("holder/sensorLeafA", MULTIPLE),
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            "aSecondCompositeOfThatSingletonLeaf",
            json!({"compositeComponentUpdates": {"holder/leafB": {"updateType": "CREATE"}}}),
            Some(SINGLETON_LEAF),
            vec![
                ("holder", HOLDER),
                ("holder/leafA", SINGLETON_LEAF),
                ("holder/sensorLeafA", MULTIPLE),
                ("motorA", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            // The count is what the record holds, not what it ever held: once
            // the singleton component is gone another one is accepted.
            "theSingletonComponentRemoved",
            json!({"componentUpdates": {"motorA": {"updateType": "DELETE"}}}),
            None,
            vec![
                ("holder", HOLDER),
                ("holder/leafA", SINGLETON_LEAF),
                ("holder/sensorLeafA", MULTIPLE),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
        (
            "aSingletonComponentAgainOnceTheFirstIsGone",
            json!({
                "componentUpdates": {
                    "motorB": {"componentTypeId": SINGLETON, "updateType": "CREATE"},
                },
            }),
            None,
            vec![
                ("holder", HOLDER),
                ("holder/leafA", SINGLETON_LEAF),
                ("holder/sensorLeafA", MULTIPLE),
                ("motorB", SINGLETON),
                ("sensorA", MULTIPLE),
                ("sensorB", MULTIPLE),
            ],
        ),
    ];

    for (case, patch, refused, held) in rows {
        let updated = emulator.update_entity(WORKSPACE, entity_id, patch).await;
        match refused {
            Some(component_type_id) => {
                assert_eq!(
                    (updated.status, updated.error_type.as_deref()),
                    (400, Some("ValidationException")),
                    "case {case:?}: a further component of a singleton type must be refused, \
                     got {updated:?}"
                );
                assert_eq!(
                    message(&updated),
                    singleton_refusal(component_type_id),
                    "case {case:?}: expected the refusal to name the component type, \
                     got {updated:?}"
                );
            }
            None => assert_eq!(
                updated.status, 200,
                "case {case:?}: expected 200 from UpdateEntity, got {updated:?}"
            ),
        }
        assert_components(&emulator, case, entity_id, &held).await;
    }
}

/// RULE: `isSingleton` is a member of the component TYPE, so a patch may
/// declare a type a singleton while an entity already holds several components
/// of it: that entity stays readable and patchable, and only a write that would
/// raise its count further — or a new entity over the bound — is refused.
#[tokio::test]
async fn a_type_declared_a_singleton_later_still_lets_its_entities_be_patched() {
    let emulator = with_singleton_types().await;
    let entity_id = "twinMotors";
    let created = emulator
        .create_entity(
            WORKSPACE,
            json!({
                "entityId": entity_id,
                "entityName": "Twin motors",
                "components": {
                    "motorA": {"componentTypeId": MULTIPLE},
                    "motorB": {"componentTypeId": MULTIPLE},
                },
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "two components of a non-singleton type must be accepted, got {created:?}"
    );

    let patched = emulator
        .update_component_type(WORKSPACE, MULTIPLE, json!({"isSingleton": true}))
        .await;
    assert_eq!(
        patched.status, 200,
        "the component type must take the flag, got {patched:?}"
    );
    let read = emulator.get_component_type(WORKSPACE, MULTIPLE).await;
    assert_eq!(
        read.member("isSingleton").as_bool(),
        Some(true),
        "the patched component type must report isSingleton true, got {read:?}"
    );

    // The entity the flag was not declared for still holds — and reports — both
    // components.
    assert_components(
        &emulator,
        "afterTheFlagWasDeclared",
        entity_id,
        &[("motorA", MULTIPLE), ("motorB", MULTIPLE)],
    )
    .await;

    // A patch that leaves the count where it is is accepted.
    let described = emulator
        .update_entity(WORKSPACE, entity_id, json!({"description": "two motors"}))
        .await;
    assert_eq!(
        described.status, 200,
        "an unrelated patch of an already-oversubscribed entity must be accepted, \
         got {described:?}"
    );

    // A write that would raise it is refused, on this entity and on a new one.
    let third = emulator
        .update_entity(
            WORKSPACE,
            entity_id,
            json!({
                "componentUpdates": {
                    "motorC": {"componentTypeId": MULTIPLE, "updateType": "CREATE"},
                },
            }),
        )
        .await;
    assert_eq!(
        (third.status, third.error_type.as_deref()),
        (400, Some("ValidationException")),
        "a further component of the now-singleton type must be refused, got {third:?}"
    );
    assert_eq!(
        message(&third),
        singleton_refusal(MULTIPLE),
        "expected the refusal to name the component type, got {third:?}"
    );

    let fresh = emulator
        .create_entity(
            WORKSPACE,
            json!({
                "entityId": "freshTwinMotors",
                "entityName": "Fresh twin motors",
                "components": {
                    "motorA": {"componentTypeId": MULTIPLE},
                    "motorB": {"componentTypeId": MULTIPLE},
                },
            }),
        )
        .await;
    assert_eq!(
        (fresh.status, fresh.error_type.as_deref()),
        (400, Some("ValidationException")),
        "a new entity with two components of the now-singleton type must be refused, \
         got {fresh:?}"
    );
    assert_components(
        &emulator,
        "theEntityIsUnchanged",
        entity_id,
        &[("motorA", MULTIPLE), ("motorB", MULTIPLE)],
    )
    .await;
}
