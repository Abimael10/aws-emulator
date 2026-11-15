// SPDX-License-Identifier: CC0-1.0

//! What a `SITEWISE` sync job SYNCHRONISES: reading the sync source's asset
//! model hierarchy and projecting it into the workspace that holds the job.
//!
//! A sync job is a workspace's standing link to IoT SiteWise, and what the link
//! does is defined by the source's own resources: the service reads the asset
//! models and assets of the account and creates the corresponding component
//! types and entities in the workspace, each carrying the `syncSource` of the job
//! that projected it. That mapping is what this module performs:
//!
//! * one COMPONENT TYPE per asset model, extending the IoT SiteWise connector
//!   built-in ([`SITEWISE_CONNECTOR_COMPONENT_TYPE_ID`]) the service ships for
//!   exactly this link, and declaring one property per property the model
//!   declares;
//! * one ENTITY per asset, named as the asset is named, hanging off the entity
//!   its parent asset was projected to, and holding one component of the
//!   component type its asset model was projected to — whose two inherited
//!   external-id properties ([`SITEWISE_ASSET_ID_PROPERTY`],
//!   [`SITEWISE_ASSET_MODEL_ID_PROPERTY`]) name the source resources the entity
//!   stands for.
//!
//! This is a real read and a real write, not a promise. The source is read out of
//! the IoT SiteWise service's OWN store in the same shared engine
//! ([`sitewise_state`]) — the records that service's creates wrote — and the
//! projection is landed by the one writer every bulk write of this service's
//! workspaces goes through ([`write_document_into_workspace`]), so a resource the
//! control plane would have refused is not one a sync job puts in a workspace,
//! and a resource the job reports as synchronised is a record `GetEntity` and
//! `GetComponentType` read back afterwards.
//!
//! WHEN it happens: in the call that ACCEPTS the job, which is the one place
//! holding both stores; the STATES of bringing the link up are what the clock
//! owes the job ([`SYNC_JOB_SETTLE_SPAN`], [`SYNC_JOB_ACTIVE_SPAN`]), so a caller
//! that reads the job back in the same breath as the create still sees
//! `CREATING`, one that polls sees `INITIALIZING` and then the standing `ACTIVE`,
//! and a job whose projection could not be landed settles in the modelled
//! failure state carrying [`SYNC_JOB_ERROR_CODE`] and the reason the resource
//! that could not be landed produced.
//!
//! HOW MUCH it does: one pass over the source's asset models and assets, both of
//! which that service bounds the count of
//! (`service_com_amazonaws_iotsitewise_awsiotsitewise::state::MAX_ASSET_MODELS`
//! and `MAX_ASSETS`, which its own creates refuse past), landing one entry each
//! through a writer that itself refuses past this service's
//! [`MAX_COMPONENT_TYPES_PER_WORKSPACE`] and [`MAX_ENTITIES_PER_WORKSPACE`]. So
//! the work one create performs is linear in a bounded number of resources, and
//! what it can add to a workspace is bounded by what that workspace may hold.

use super::*;

/// The built-in component type a projected component type extends.
///
/// The service ships it for this link: it is the IoT SiteWise connector of the
/// user guide's built-in component types, and its two property definitions are
/// the external ids that name the asset and the asset model a component reads
/// its values from (see `BUILT_IN_COMPONENT_TYPES`), which is exactly what a
/// projected entity's component carries.
pub const SITEWISE_CONNECTOR_COMPONENT_TYPE_ID: &str = "com.amazon.iotsitewise.connector";

/// The connector built-in's property naming the asset model a component reads
/// from. See [`SITEWISE_CONNECTOR_COMPONENT_TYPE_ID`].
pub const SITEWISE_ASSET_MODEL_ID_PROPERTY: &str = "sitewiseAssetModelId";

/// The connector built-in's property naming the asset a component reads from.
/// See [`SITEWISE_CONNECTOR_COMPONENT_TYPE_ID`].
pub const SITEWISE_ASSET_ID_PROPERTY: &str = "sitewiseAssetId";

/// The name the one component a projected entity holds is called by.
///
/// An entity's components are keyed by a `Name`, so the projection has to name
/// the component it creates; one name for every projected entity means a client
/// reads the link's component off any synchronised entity the same way, and the
/// name says which link put it there.
pub const SITEWISE_COMPONENT_NAME: &str = "sitewiseBase";

/// The id prefix a component type projected from an asset model carries, before
/// the asset model's own id.
///
/// The projected id is derived from the source resource's identity so that one
/// asset model is projected to one component type however often the link is
/// brought up, and it is derived WITHIN the modelled `Id` shape
/// (`^[a-zA-Z_0-9][a-zA-Z_\-0-9]*[a-zA-Z0-9]+$`, `@length(1..128)`) rather than
/// merely within `ComponentTypeId`: `SyncResourceSummary$resourceId` — the member
/// `ListSyncResources` reports a synchronised resource's workspace id on — is an
/// `Id`, so an id outside that shape could not be reported for the resource it
/// names. The prefix lies outside the namespace the service reserves for its own
/// built-ins ([`BUILT_IN_COMPONENT_TYPE_ID_PREFIX`]), because a projected
/// component type is a resource of the workspace rather than one the service
/// ships.
pub const SITEWISE_COMPONENT_TYPE_ID_PREFIX: &str = "iotsitewise_assetmodel_";

/// The `AssetModelType` an ordinary IoT SiteWise asset model carries — the kind
/// of model an asset is an instance of, and so the kind a component type is
/// projected from. A `COMPONENT_MODEL` is reused inside another model's
/// composite models and an `INTERFACE` is applied to one, so neither is a model
/// any asset is an instance of.
pub const SITEWISE_ASSET_MODEL_TYPE: &str = "ASSET_MODEL";

/// The `SyncResourceType` values a projected resource is reported under
/// (`ops::types::SYNC_RESOURCE_TYPE_VALUES`).
pub const SYNC_RESOURCE_TYPE_ENTITY: &str = "ENTITY";
/// See [`SYNC_RESOURCE_TYPE_ENTITY`].
pub const SYNC_RESOURCE_TYPE_COMPONENT_TYPE: &str = "COMPONENT_TYPE";

/// The `SyncResourceState` a resource the projection landed in the workspace is
/// in: the workspace resource matches the source resource it was projected from
/// (`ops::types::SYNC_RESOURCE_STATE_VALUES`).
pub const SYNC_RESOURCE_STATE_IN_SYNC: &str = "IN_SYNC";
/// The `SyncResourceState` a resource the projection could NOT land is in. See
/// [`SYNC_RESOURCE_STATE_IN_SYNC`].
pub const SYNC_RESOURCE_STATE_ERROR: &str = "ERROR";

/// The `ErrorCode` a resource the projection could not create in the workspace
/// carries (`ops::types::ERROR_CODE_VALUES`): the code for a resource whose
/// creation under the sync failed, as distinct from
/// [`SYNC_JOB_ERROR_CODE`], which the JOB carries for having failed to bring
/// the link up.
pub const SYNC_RESOURCE_ERROR_CODE: &str = "SYNC_CREATING_ERROR";

/// The greatest number of characters the modelled `Id` admits — the shape a
/// synchronised resource's `resourceId` is reported on
/// (`SyncResourceSummary$resourceId`, `@length(1..128)`), and so the bound the
/// id of a projected component type is derived within
/// ([`sitewise_component_type_id`]).
const ID_MAX_CHARS: usize = 128;

/// The greatest number of characters the modelled `Name` admits — the shape an
/// entity's component names and a component type's property names are keyed by
/// (`@length(1..256)`, `^[a-zA-Z_\-0-9]+$`).
const NAME_MAX_CHARS: usize = 256;

/// The reason a projected resource carries when the run reported nothing about
/// it at all, which is a resource the writer neither wrote nor refused.
const NOT_WRITTEN_MESSAGE: &str = "the resource was not written to the workspace";

/// One resource the projection describes, at BOTH its ends: its identity in the
/// sync source and the id it is projected to in the workspace.
///
/// Carried beside the document so that, once the document has been landed, each
/// source resource can be reported as the [`SyncResource`] the job holds for it
/// — which is what `ListSyncResources` lists.
#[derive(Debug, Clone)]
pub struct ProjectedResource {
    /// [`SYNC_RESOURCE_TYPE_ENTITY`] or [`SYNC_RESOURCE_TYPE_COMPONENT_TYPE`].
    pub resource_type: &'static str,
    /// The resource's identity in the sync SOURCE: the `assetId` or
    /// `assetModelId` it was projected from.
    pub external_id: String,
    /// The id the resource is projected to in the workspace.
    pub resource_id: String,
}

impl ProjectedResource {
    /// Which kind of entry the workspace writer reports this resource as, so the
    /// row below is built from what that writer did with it.
    fn entry_kind(&self) -> &'static str {
        match self.resource_type {
            SYNC_RESOURCE_TYPE_COMPONENT_TYPE => ENTRY_KIND_COMPONENT_TYPE,
            _ => ENTRY_KIND_ENTITY,
        }
    }
}

/// What one reading of the sync source came to: the workspace resources it
/// describes, and the source resource each of them stands for.
#[derive(Debug, Clone, Default)]
pub struct SyncProjection {
    /// The component types and entities the source's resources are projected to,
    /// in the one intermediate form every bulk write of a workspace goes through.
    pub document: MetadataDocument,
    /// One entry per resource the document describes, in document order:
    /// component types first, then entities.
    pub resources: Vec<ProjectedResource>,
}

/// Read the IoT SiteWise store and describe what a `SITEWISE` sync job projects
/// into its workspace.
///
/// A pure READ of the source: it writes nothing and takes no lock of this
/// service's own store, so the caller holds the source's guard only for as long
/// as this takes and then writes its own store with the result.
///
/// Asset models and assets are walked in ID order, so two runs over one source
/// describe the same resources in the same order; an asset whose model is not a
/// model any asset is an instance of ([`SITEWISE_ASSET_MODEL_TYPE`]) is left out
/// along with its model, because the component type its component would
/// instantiate is not projected either.
pub fn sitewise_projection(sitewise: &SiteWiseState) -> SyncProjection {
    let mut projection = SyncProjection::default();
    let mut models: Vec<&::service_com_amazonaws_iotsitewise_awsiotsitewise::state::AssetModelRecord> =
        sitewise
            .asset_models
            .values()
            .filter(|model| model.model_type == SITEWISE_ASSET_MODEL_TYPE)
            .collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    let projected_models: BTreeSet<&str> = models.iter().map(|model| model.id.as_str()).collect();
    for model in &models {
        let component_type_id = sitewise_component_type_id(&model.id);
        projection.document.component_types.push(ComponentTypeEntry {
            component_type_id: component_type_id.clone(),
            // The asset model's name, which is a friendly name rather than an
            // id in either service: `assetModelName` is what the source calls
            // the model, and `componentTypeName` is the member a component type
            // carries the same thing on.
            component_type_name: Some(model.name.clone()),
            description: model.description.clone(),
            extends_from: vec![SITEWISE_CONNECTOR_COMPONENT_TYPE_ID.to_string()],
            is_singleton: false,
            property_definitions: projected_property_definitions(&model.properties),
            ..ComponentTypeEntry::default()
        });
        projection.resources.push(ProjectedResource {
            resource_type: SYNC_RESOURCE_TYPE_COMPONENT_TYPE,
            external_id: model.id.clone(),
            resource_id: component_type_id,
        });
    }
    let mut assets: Vec<&::service_com_amazonaws_iotsitewise_awsiotsitewise::state::AssetRecord> =
        sitewise
            .assets
            .values()
            .filter(|asset| projected_models.contains(asset.asset_model_id.as_str()))
            .collect();
    assets.sort_by(|left, right| left.id.cmp(&right.id));
    for asset in assets {
        let component_type_id = sitewise_component_type_id(&asset.asset_model_id);
        // The source's hierarchies ARE the workspace's parent/child
        // relationships: the asset's parent in the source is the entity its own
        // entity hangs off, and an asset that is nobody's child is projected to
        // an entity at the root of the workspace's entity tree.
        let parent_entity_id = sitewise
            .hierarchy_parent(&asset.id)
            .map(|(parent_asset_id, _)| parent_asset_id.to_string());
        projection.document.entities.push(EntityEntry {
            // The asset's own id: an asset is projected to the entity that
            // stands for it, so the entity is addressable by the id the source
            // knows the asset by.
            entity_id: asset.id.clone(),
            entity_name: asset.name.clone(),
            description: asset.description.clone(),
            parent_entity_id,
            components: BTreeMap::from([(
                SITEWISE_COMPONENT_NAME.to_string(),
                ComponentEntry {
                    component_type_id: Some(component_type_id.clone()),
                    properties: BTreeMap::from([
                        (
                            SITEWISE_ASSET_ID_PROPERTY.to_string(),
                            string_property(&asset.id),
                        ),
                        (
                            SITEWISE_ASSET_MODEL_ID_PROPERTY.to_string(),
                            string_property(&asset.asset_model_id),
                        ),
                    ]),
                    ..ComponentEntry::default()
                },
            )]),
            ..EntityEntry::default()
        });
        projection.resources.push(ProjectedResource {
            resource_type: SYNC_RESOURCE_TYPE_ENTITY,
            external_id: asset.id.clone(),
            resource_id: asset.id.clone(),
        });
    }
    projection
}

/// Land `projection` in the workspace `workspace_id` of `state` as the work of
/// the sync job `sync_source` holds, and report what the job then holds: one
/// [`SyncResource`] per source resource, keyed by its identity in the source,
/// and the failure (if any) the job settles in.
///
/// Every resource is written by [`write_document_into_workspace`], the one
/// writer that holds a bulk write to the rules `CreateComponentType` and
/// `CreateEntity` hold a caller's own create to, with the job's `syncSource`
/// stamped on each record it wrote. A resource the workspace ALREADY holds under
/// the projected id is in sync — the workspace resource the link stands for is
/// there — and one the writer refused is reported `ERROR` carrying the reason,
/// which is also what the job reports for having failed to bring the link up.
pub fn project_sync_source(
    state: &mut RegionalState,
    region: &str,
    workspace_id: &str,
    sync_source: &str,
    projection: &SyncProjection,
    now: SystemTime,
) -> (BTreeMap<String, SyncResource>, Option<crate::ops::types::ErrorDetails>) {
    let report = write_document_into_workspace(
        state,
        region,
        workspace_id,
        &projection.document,
        now,
        Some(sync_source),
    );
    let mut resources: BTreeMap<String, SyncResource> = BTreeMap::new();
    let mut first_failure: Option<String> = None;
    for projected in &projection.resources {
        let entry = report
            .entries
            .iter()
            .find(|entry| entry.kind == projected.entry_kind() && entry.id == projected.resource_id);
        let failure = match entry {
            Some(entry) if entry.outcome == EntryOutcome::Failed => Some(
                entry
                    .reason
                    .clone()
                    .unwrap_or_else(|| NOT_WRITTEN_MESSAGE.to_string()),
            ),
            Some(_) => None,
            None => Some(NOT_WRITTEN_MESSAGE.to_string()),
        };
        if let Some(reason) = &failure {
            first_failure = first_failure.or_else(|| Some(reason.clone()));
        }
        resources.insert(
            projected.external_id.clone(),
            SyncResource {
                resource_type: projected.resource_type.to_string(),
                external_id: projected.external_id.clone(),
                // The id in the WORKSPACE, which a resource the run could not
                // create there has none of.
                resource_id: failure.is_none().then(|| projected.resource_id.clone()),
                state: match failure.is_none() {
                    true => SYNC_RESOURCE_STATE_IN_SYNC.to_string(),
                    false => SYNC_RESOURCE_STATE_ERROR.to_string(),
                },
                error: failure.map(|reason| crate::ops::types::ErrorDetails {
                    code: Some(SYNC_RESOURCE_ERROR_CODE.to_string()),
                    message: Some(clamped_message(reason, ERROR_MESSAGE_MAX_CHARS)),
                }),
                update_date_time: now,
            },
        );
    }
    let failure = first_failure.map(|reason| crate::ops::types::ErrorDetails {
        code: Some(SYNC_JOB_ERROR_CODE.to_string()),
        message: Some(clamped_message(reason, ERROR_MESSAGE_MAX_CHARS)),
    });
    (resources, failure)
}

/// The `componentTypeId` the asset model `asset_model_id` is projected to. See
/// [`SITEWISE_COMPONENT_TYPE_ID_PREFIX`].
///
/// Within [`ID_MAX_CHARS`], the maximum of the member a synchronised resource's
/// workspace id is reported on: a source id long enough to carry the derived id
/// past it keeps its LEADING characters, so two models still project to two ids
/// wherever their ids differ in what the bound admits.
#[must_use]
pub fn sitewise_component_type_id(asset_model_id: &str) -> String {
    let derived = format!("{SITEWISE_COMPONENT_TYPE_ID_PREFIX}{asset_model_id}");
    match derived.chars().count() <= ID_MAX_CHARS {
        true => derived,
        // Cut in CHARACTERS, as Smithy measures a string's length, and cut back
        // to the last character the shape admits at the end of an id (its
        // pattern ends `[a-zA-Z0-9]+`), so the cut cannot leave a trailing `_`
        // or `-` the model would not accept.
        false => {
            let kept: String = derived.chars().take(ID_MAX_CHARS).collect();
            kept.trim_end_matches(['_', '-']).to_string()
        }
    }
}

/// One property of a projected entity's component: the string value that names a
/// source resource.
fn string_property(value: &str) -> crate::ops::types::PropertyRequest {
    crate::ops::types::PropertyRequest {
        // No definition of its own: the property is one the connector built-in
        // declares and the projected component type inherits, so the value is
        // judged against — and reported through — that definition.
        value: Some(crate::ops::types::DataValue {
            string_value: Some(value.to_string()),
            ..crate::ops::types::DataValue::default()
        }),
        ..crate::ops::types::PropertyRequest::default()
    }
}

/// The property definitions a component type projected from an asset model
/// declares: one per property the model declares, keyed by the name the modelled
/// `Name` shape admits for it ([`projected_property_name`]).
///
/// A property whose projected name is already taken by an earlier property is
/// keyed by its own id in the source instead, which is unique within the model,
/// so no property of the source is dropped and none overwrites another.
fn projected_property_definitions(
    properties: &[::service_com_amazonaws_iotsitewise_awsiotsitewise::ops::types::AssetModelProperty],
) -> BTreeMap<String, crate::ops::types::PropertyDefinitionRequest> {
    let mut definitions: BTreeMap<String, crate::ops::types::PropertyDefinitionRequest> =
        BTreeMap::new();
    for property in properties {
        let name = projected_property_name(&property.name);
        let key = match (definitions.contains_key(&name), &property.id) {
            (true, Some(id)) => projected_property_name(id),
            _ => name,
        };
        definitions.insert(
            key,
            crate::ops::types::PropertyDefinitionRequest {
                data_type: Some(crate::ops::types::DataType {
                    r#type: projected_data_type(&property.data_type).to_string(),
                    ..crate::ops::types::DataType::default()
                }),
                // "A Boolean value that specifies whether the property consists
                // of time series data": an attribute of an asset carries one
                // value, while a measurement, a transform and a metric are
                // series of values over time.
                is_time_series: Some(property.r#type.attribute.is_none()),
                // The source's own name for the property, which the projected
                // name is derived from and may differ from: a friendly name is
                // what `displayName` carries.
                display_name: Some(property.name.clone()),
                ..crate::ops::types::PropertyDefinitionRequest::default()
            },
        );
    }
    definitions
}

/// `name` as the modelled `Name` shape admits it — the shape a component type's
/// property definitions are keyed by (`^[a-zA-Z_\-0-9]+$`,
/// `@length(1..256)`).
///
/// A source property's name is a free-form label, so it may carry characters the
/// key of a projected definition may not (a space, a dot, a parenthesis): each
/// of those becomes an underscore, so the projected name still reads as the
/// source's, and the result is cut to the member's maximum. A name that is empty
/// once projected — which no source name of at least one admissible character
/// produces — is keyed by a single underscore, so the key is never outside the
/// shape's minimum.
#[must_use]
pub fn projected_property_name(name: &str) -> String {
    let projected: String = name
        .chars()
        .take(NAME_MAX_CHARS)
        .map(|character| match character.is_ascii_alphanumeric() || character == '-' {
            true => character,
            false => '_',
        })
        .collect();
    match projected.is_empty() {
        true => "_".to_string(),
        false => projected,
    }
}

/// The modelled `Type` a property of the source's `PropertyDataType` is
/// projected to (`ops::types::TYPE_VALUES`).
///
/// The data types BOTH services define under the same name — `STRING`,
/// `INTEGER`, `DOUBLE` and `BOOLEAN` — are projected to that type. The source's
/// remaining data types (its structured and media ones) name no type this
/// service defines, so a property of one is projected as `STRING`, the type this
/// service carries opaque text on.
#[must_use]
pub fn projected_data_type(data_type: &str) -> &'static str {
    match data_type {
        "INTEGER" => "INTEGER",
        "DOUBLE" => "DOUBLE",
        "BOOLEAN" => "BOOLEAN",
        _ => "STRING",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a projected property name is the source's name with every
    /// character the modelled `Name` shape does not admit replaced, within that
    /// shape's own bounds.
    #[test]
    fn a_projected_property_name_satisfies_the_modelled_name_shape() {
        // `(case, the source's property name, the projected key)`.
        let rows: &[(&str, &str, &str)] = &[
            ("already admissible", "Temperature", "Temperature"),
            ("an underscore and digits", "temp_2_max", "temp_2_max"),
            ("a hyphen", "temp-max", "temp-max"),
            ("spaces", "Max Temperature", "Max_Temperature"),
            ("a dot and a colon", "line.1:temp", "line_1_temp"),
            ("parentheses and a slash", "Flow (m3/s)", "Flow__m3_s_"),
            ("a uuid, as an id is", "a1b2c3d4-0000", "a1b2c3d4-0000"),
            ("non-ascii", "température", "temp_rature"),
        ];
        for (case, name, expected) in rows {
            let actual = projected_property_name(name);
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            assert!(
                (1..=NAME_MAX_CHARS).contains(&actual.chars().count())
                    && actual
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric()
                            || character == '_'
                            || character == '-'),
                "case {case:?}: the key must satisfy the Name shape, got {actual:?}"
            );
        }
        // The shape's maximum holds however long the source's name is.
        let long = projected_property_name(&"x y".repeat(500));
        assert_eq!(
            long.chars().count(),
            NAME_MAX_CHARS,
            "case \"a name past the maximum\": expected {NAME_MAX_CHARS} characters, got {}",
            long.chars().count()
        );
    }

    /// RULE: a source data type this service also defines is projected to the
    /// type of that name, and every other to the type this service carries
    /// opaque text on — and the answer is always a modelled `Type`.
    #[test]
    fn a_projected_data_type_is_always_a_modelled_type() {
        // Every value of the source's `PropertyDataType`
        // (`service_com_amazonaws_iotsitewise_awsiotsitewise::ops::types::PROPERTY_DATA_TYPE_VALUES`),
        // plus a value no version of it declares.
        let rows: &[(&str, &str)] = &[
            ("STRING", "STRING"),
            ("INTEGER", "INTEGER"),
            ("DOUBLE", "DOUBLE"),
            ("BOOLEAN", "BOOLEAN"),
            ("STRUCT", "STRING"),
            ("JSON", "STRING"),
            ("ANNOTATION", "STRING"),
            ("VIDEO", "STRING"),
            ("AN_UNDECLARED_TYPE", "STRING"),
        ];
        for (data_type, expected) in rows {
            let actual = projected_data_type(data_type);
            assert_eq!(
                &actual, expected,
                "case {data_type:?}: expected {expected:?}, got {actual:?}"
            );
            assert!(
                crate::ops::types::TYPE_VALUES.contains(&actual),
                "case {data_type:?}: the projected type must be a modelled Type, got {actual:?}"
            );
        }
        for data_type in
            ::service_com_amazonaws_iotsitewise_awsiotsitewise::ops::types::PROPERTY_DATA_TYPE_VALUES
        {
            assert!(
                crate::ops::types::TYPE_VALUES.contains(&projected_data_type(data_type)),
                "case {data_type:?}: every source data type must project to a modelled Type"
            );
        }
    }

    /// RULE: a component type is projected to an id derived from the asset
    /// model's own, within the modelled `Id` shape — the shape
    /// `ListSyncResources` reports a synchronised resource's workspace id on —
    /// and outside the namespace the service reserves for its own built-ins.
    #[test]
    fn a_projected_component_type_id_satisfies_the_modelled_id_shape() {
        // `(case, the source's assetModelId, whether the derived id carries the
        //  source's own id whole)`. The `Id` bounds, from
        // `smithy_query(query="shape",shape="SyncResourceSummary")`:
        // `@length(1..128)` and `^[a-zA-Z_0-9][a-zA-Z_\-0-9]*[a-zA-Z0-9]+$`. The
        // rows differ in the shape and the LENGTH of the source id, including
        // ones long enough to carry the derived id past that maximum.
        let longest_source = "a".repeat(ID_MAX_CHARS - SITEWISE_COMPONENT_TYPE_ID_PREFIX.len());
        let over_the_bound = "b".repeat(ID_MAX_CHARS);
        let trailing_separators = format!("{}_-_-", "c".repeat(ID_MAX_CHARS));
        let rows: Vec<(&str, &str, bool)> = vec![
            ("a minted uuid", "d4e5f6a7-1234-4567-89ab-0123456789ab", true),
            ("a short id", "m1", true),
            ("an id of digits", "0123456789", true),
            ("an id with an underscore", "plant_line_model", true),
            ("the longest source id that still fits", &longest_source, true),
            ("a source id past the maximum", &over_the_bound, false),
            (
                "a source id past it whose cut lands on separators",
                &trailing_separators,
                false,
            ),
        ];
        for (case, asset_model_id, carries_the_source) in &rows {
            let actual = sitewise_component_type_id(asset_model_id);
            assert_eq!(
                actual.ends_with(asset_model_id),
                *carries_the_source,
                "case {case:?}: the id carries the source's own id exactly while the bound \
                 admits it, got {actual:?}"
            );
            assert!(
                actual.starts_with(SITEWISE_COMPONENT_TYPE_ID_PREFIX),
                "case {case:?}: the id must be derived from the source's own, got {actual:?}"
            );
            let admissible = actual
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || character == '_'
                    || character == '-');
            assert!(
                admissible
                    && actual.chars().count() <= ID_MAX_CHARS
                    && actual.ends_with(|last: char| last.is_ascii_alphanumeric()),
                "case {case:?}: the id must satisfy the Id shape, got {actual:?}"
            );
            assert!(
                !built_in_component_type_id(&actual),
                "case {case:?}: a projected component type is not one the service ships, \
                 got {actual:?}"
            );
        }
    }
}
