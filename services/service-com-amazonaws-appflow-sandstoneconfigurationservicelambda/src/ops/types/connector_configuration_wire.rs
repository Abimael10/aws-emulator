// SPDX-License-Identifier: CC0-1.0

//! The `ConnectorConfiguration` AppFlow reports for one connector, as the WIRE carries it.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list, and
//! it admits EXACTLY that list: a Rust field with no member behind it is a compile error, so
//! a member the service really reports but the published model omits cannot be declared
//! through the derive at all. AppFlow reports one such member on every connector
//! configuration — `isSupportedApiVersionsRequired`, the flag that tells a caller whether it
//! must pin one of the connector's API versions — so this module emits for
//! `ConnectorConfiguration` the same items the derive emits: one member schema per member
//! (index = its position in the member list), the struct `SCHEMA` naming them in that order,
//! the `CONNECTOR_CONFIGURATION_SCHEMA` static and the `SerializableStruct` /
//! `DeserializableStruct` pair. The run record beside it needs the same treatment for the
//! same reason (see [`super::execution_record_wire`]).
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="ConnectorConfiguration")` lists them in); the member it
//! does not declare is appended LAST, with its shape id in a namespace of its own
//! ([`RECORDED_NAMESPACE`]), so it can neither claim to be modelled nor collide with a member
//! the model may declare later — a model refresh only has to move it into place and put the
//! derive back. The ids never leave the process: the JSON codec writes a member's NAME as the
//! object key.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and this
//! structure is output-only), the `@http` chain (baked on only for an operation's input) and
//! `@jsonName` (no member here declares one: each one's wire name IS its member name).
//!
//! Nothing here decides a VALUE: every member is written from the record the configuration
//! was projected from (`crate::state::VendedConnector::to_connector_configuration` for a
//! connector AppFlow vends, `crate::state::registered_connector_configuration` for one an
//! account registered).

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_schema_server::DeserializableStruct;
use ::aws_smithy_types::DateTime;

use super::{
    AuthenticationConfig, ConnectorMetadata, ConnectorProvisioningConfig, ConnectorRuntimeSetting,
    DataTransferApi,
};

/// The namespace the model's own shapes sit in.
const MODEL_NAMESPACE: &str = "com.amazonaws.appflow";

/// The namespace a member the published model does not declare sits in: NOT this service's
/// own, so a hand-written member can never be mistaken for a modelled one.
const RECORDED_NAMESPACE: &str = "com.amazonaws.appflow.recorded";

/// The configuration settings related to a given connector.
///
/// The first 28 members are `com.amazonaws.appflow#ConnectorConfiguration`'s, in the model's
/// member order. The last one is the member AppFlow reports beyond the published model:
///
/// * `isSupportedApiVersionsRequired` — whether a caller must name one of the connector's
///   API versions when it builds a connector profile or a flow. Reported for every
///   connector, like the other configuration flags, so a client never has to tell `false`
///   from "not reported".
#[derive(Debug, Clone, Default)]
pub struct ConnectorConfiguration {
    pub can_use_as_source: bool,
    pub can_use_as_destination: bool,
    pub supported_destination_connectors: Option<Vec<String>>,
    pub supported_scheduling_frequencies: Option<Vec<String>>,
    pub is_private_link_enabled: bool,
    pub is_private_link_endpoint_url_required: bool,
    pub supported_trigger_types: Option<Vec<String>>,
    pub connector_metadata: Option<ConnectorMetadata>,
    pub connector_type: Option<String>,
    pub connector_label: Option<String>,
    pub connector_description: Option<String>,
    pub connector_owner: Option<String>,
    pub connector_name: Option<String>,
    pub connector_version: Option<String>,
    pub connector_arn: Option<String>,
    pub connector_modes: Option<Vec<String>>,
    pub authentication_config: Option<AuthenticationConfig>,
    pub connector_runtime_settings: Option<Vec<ConnectorRuntimeSetting>>,
    pub supported_api_versions: Option<Vec<String>>,
    pub supported_operators: Option<Vec<String>>,
    pub supported_write_operations: Option<Vec<String>>,
    pub connector_provisioning_type: Option<String>,
    pub connector_provisioning_config: Option<ConnectorProvisioningConfig>,
    pub logo_url: Option<String>,
    pub registered_at: Option<DateTime>,
    pub registered_by: Option<String>,
    pub supported_data_transfer_types: Option<Vec<String>>,
    pub supported_data_transfer_apis: Option<Vec<DataTransferApi>>,
    pub is_supported_api_versions_required: bool,
}

impl ConnectorConfiguration {
    /// One member the model declares, as the codegen emits a member schema: its shape id,
    /// its shape type, its wire name and its index in the member list.
    const fn member(
        id: &'static str,
        name: &'static str,
        shape_type: ShapeType,
        index: usize,
    ) -> Schema {
        Schema::new_member(
            ShapeId::from_static(id, MODEL_NAMESPACE, name),
            shape_type,
            name,
            index,
        )
    }

    /// One member the published model does not declare: the same schema, with its shape id
    /// in [`RECORDED_NAMESPACE`].
    const fn recorded_member(
        id: &'static str,
        name: &'static str,
        shape_type: ShapeType,
        index: usize,
    ) -> Schema {
        Schema::new_member(
            ShapeId::from_static(id, RECORDED_NAMESPACE, name),
            shape_type,
            name,
            index,
        )
    }

    pub const M_CAN_USE_AS_SOURCE: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$canUseAsSource",
        "canUseAsSource",
        ShapeType::Boolean,
        0,
    );
    pub const M_CAN_USE_AS_DESTINATION: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$canUseAsDestination",
        "canUseAsDestination",
        ShapeType::Boolean,
        1,
    );
    pub const M_SUPPORTED_DESTINATION_CONNECTORS: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$supportedDestinationConnectors",
        "supportedDestinationConnectors",
        ShapeType::List,
        2,
    );
    pub const M_SUPPORTED_SCHEDULING_FREQUENCIES: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$supportedSchedulingFrequencies",
        "supportedSchedulingFrequencies",
        ShapeType::List,
        3,
    );
    pub const M_IS_PRIVATE_LINK_ENABLED: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$isPrivateLinkEnabled",
        "isPrivateLinkEnabled",
        ShapeType::Boolean,
        4,
    );
    pub const M_IS_PRIVATE_LINK_ENDPOINT_URL_REQUIRED: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$isPrivateLinkEndpointUrlRequired",
        "isPrivateLinkEndpointUrlRequired",
        ShapeType::Boolean,
        5,
    );
    pub const M_SUPPORTED_TRIGGER_TYPES: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$supportedTriggerTypes",
        "supportedTriggerTypes",
        ShapeType::List,
        6,
    );
    pub const M_CONNECTOR_METADATA: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorMetadata",
        "connectorMetadata",
        ShapeType::Structure,
        7,
    );
    pub const M_CONNECTOR_TYPE: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorType",
        "connectorType",
        ShapeType::String,
        8,
    );
    pub const M_CONNECTOR_LABEL: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorLabel",
        "connectorLabel",
        ShapeType::String,
        9,
    );
    pub const M_CONNECTOR_DESCRIPTION: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorDescription",
        "connectorDescription",
        ShapeType::String,
        10,
    );
    pub const M_CONNECTOR_OWNER: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorOwner",
        "connectorOwner",
        ShapeType::String,
        11,
    );
    pub const M_CONNECTOR_NAME: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorName",
        "connectorName",
        ShapeType::String,
        12,
    );
    pub const M_CONNECTOR_VERSION: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorVersion",
        "connectorVersion",
        ShapeType::String,
        13,
    );
    pub const M_CONNECTOR_ARN: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorArn",
        "connectorArn",
        ShapeType::String,
        14,
    );
    pub const M_CONNECTOR_MODES: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorModes",
        "connectorModes",
        ShapeType::List,
        15,
    );
    pub const M_AUTHENTICATION_CONFIG: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$authenticationConfig",
        "authenticationConfig",
        ShapeType::Structure,
        16,
    );
    pub const M_CONNECTOR_RUNTIME_SETTINGS: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorRuntimeSettings",
        "connectorRuntimeSettings",
        ShapeType::List,
        17,
    );
    pub const M_SUPPORTED_API_VERSIONS: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$supportedApiVersions",
        "supportedApiVersions",
        ShapeType::List,
        18,
    );
    pub const M_SUPPORTED_OPERATORS: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$supportedOperators",
        "supportedOperators",
        ShapeType::List,
        19,
    );
    pub const M_SUPPORTED_WRITE_OPERATIONS: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$supportedWriteOperations",
        "supportedWriteOperations",
        ShapeType::List,
        20,
    );
    pub const M_CONNECTOR_PROVISIONING_TYPE: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorProvisioningType",
        "connectorProvisioningType",
        ShapeType::String,
        21,
    );
    pub const M_CONNECTOR_PROVISIONING_CONFIG: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$connectorProvisioningConfig",
        "connectorProvisioningConfig",
        ShapeType::Structure,
        22,
    );
    pub const M_LOGO_URL: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$logoURL",
        "logoURL",
        ShapeType::String,
        23,
    );
    pub const M_REGISTERED_AT: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$registeredAt",
        "registeredAt",
        ShapeType::Timestamp,
        24,
    );
    pub const M_REGISTERED_BY: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$registeredBy",
        "registeredBy",
        ShapeType::String,
        25,
    );
    pub const M_SUPPORTED_DATA_TRANSFER_TYPES: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$supportedDataTransferTypes",
        "supportedDataTransferTypes",
        ShapeType::List,
        26,
    );
    pub const M_SUPPORTED_DATA_TRANSFER_APIS: Schema = Self::member(
        "com.amazonaws.appflow#ConnectorConfiguration$supportedDataTransferApis",
        "supportedDataTransferApis",
        ShapeType::List,
        27,
    );
    pub const M_IS_SUPPORTED_API_VERSIONS_REQUIRED: Schema = Self::recorded_member(
        "com.amazonaws.appflow.recorded#ConnectorConfiguration$isSupportedApiVersionsRequired",
        "isSupportedApiVersionsRequired",
        ShapeType::Boolean,
        28,
    );

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.appflow#ConnectorConfiguration",
            MODEL_NAMESPACE,
            "ConnectorConfiguration",
        ),
        ShapeType::Structure,
        &[
            &Self::M_CAN_USE_AS_SOURCE,
            &Self::M_CAN_USE_AS_DESTINATION,
            &Self::M_SUPPORTED_DESTINATION_CONNECTORS,
            &Self::M_SUPPORTED_SCHEDULING_FREQUENCIES,
            &Self::M_IS_PRIVATE_LINK_ENABLED,
            &Self::M_IS_PRIVATE_LINK_ENDPOINT_URL_REQUIRED,
            &Self::M_SUPPORTED_TRIGGER_TYPES,
            &Self::M_CONNECTOR_METADATA,
            &Self::M_CONNECTOR_TYPE,
            &Self::M_CONNECTOR_LABEL,
            &Self::M_CONNECTOR_DESCRIPTION,
            &Self::M_CONNECTOR_OWNER,
            &Self::M_CONNECTOR_NAME,
            &Self::M_CONNECTOR_VERSION,
            &Self::M_CONNECTOR_ARN,
            &Self::M_CONNECTOR_MODES,
            &Self::M_AUTHENTICATION_CONFIG,
            &Self::M_CONNECTOR_RUNTIME_SETTINGS,
            &Self::M_SUPPORTED_API_VERSIONS,
            &Self::M_SUPPORTED_OPERATORS,
            &Self::M_SUPPORTED_WRITE_OPERATIONS,
            &Self::M_CONNECTOR_PROVISIONING_TYPE,
            &Self::M_CONNECTOR_PROVISIONING_CONFIG,
            &Self::M_LOGO_URL,
            &Self::M_REGISTERED_AT,
            &Self::M_REGISTERED_BY,
            &Self::M_SUPPORTED_DATA_TRANSFER_TYPES,
            &Self::M_SUPPORTED_DATA_TRANSFER_APIS,
            &Self::M_IS_SUPPORTED_API_VERSIONS_REQUIRED,
        ],
    );
}

pub static CONNECTOR_CONFIGURATION_SCHEMA: &Schema = &ConnectorConfiguration::SCHEMA;

/// Write one list of strings, the way the codegen writes a `list<string>` member: every
/// element against the member's own schema.
fn write_strings(
    s: &mut dyn ShapeSerializer,
    member: &Schema,
    values: &[String],
) -> Result<(), SerdeError> {
    s.write_list(member, &|element| {
        for value in values.iter() {
            element.write_string(member, value.as_str())?;
        }
        Ok(())
    })
}

/// Read one list of strings, the way the codegen reads a `list<string>` member.
fn read_strings(d: &mut dyn ShapeDeserializer, member: &Schema) -> Result<Vec<String>, SerdeError> {
    let mut values: Vec<String> = Vec::new();
    d.read_list(member, &mut |element| {
        values.push(element.read_string(member)?);
        Ok(())
    })?;
    Ok(values)
}

impl SerializableStruct for ConnectorConfiguration {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        // A member with a `@default` is written whatever its value, the way the codegen
        // writes a non-`Option` field; an optional one only when it is there.
        s.write_boolean(&Self::M_CAN_USE_AS_SOURCE, self.can_use_as_source)?;
        s.write_boolean(&Self::M_CAN_USE_AS_DESTINATION, self.can_use_as_destination)?;
        if let Some(value) = self.supported_destination_connectors.as_ref() {
            write_strings(s, &Self::M_SUPPORTED_DESTINATION_CONNECTORS, value)?;
        }
        if let Some(value) = self.supported_scheduling_frequencies.as_ref() {
            write_strings(s, &Self::M_SUPPORTED_SCHEDULING_FREQUENCIES, value)?;
        }
        s.write_boolean(&Self::M_IS_PRIVATE_LINK_ENABLED, self.is_private_link_enabled)?;
        s.write_boolean(
            &Self::M_IS_PRIVATE_LINK_ENDPOINT_URL_REQUIRED,
            self.is_private_link_endpoint_url_required,
        )?;
        if let Some(value) = self.supported_trigger_types.as_ref() {
            write_strings(s, &Self::M_SUPPORTED_TRIGGER_TYPES, value)?;
        }
        if let Some(value) = self.connector_metadata.as_ref() {
            s.write_struct(&Self::M_CONNECTOR_METADATA, value)?;
        }
        if let Some(value) = self.connector_type.as_ref() {
            s.write_string(&Self::M_CONNECTOR_TYPE, value.as_str())?;
        }
        if let Some(value) = self.connector_label.as_ref() {
            s.write_string(&Self::M_CONNECTOR_LABEL, value.as_str())?;
        }
        if let Some(value) = self.connector_description.as_ref() {
            s.write_string(&Self::M_CONNECTOR_DESCRIPTION, value.as_str())?;
        }
        if let Some(value) = self.connector_owner.as_ref() {
            s.write_string(&Self::M_CONNECTOR_OWNER, value.as_str())?;
        }
        if let Some(value) = self.connector_name.as_ref() {
            s.write_string(&Self::M_CONNECTOR_NAME, value.as_str())?;
        }
        if let Some(value) = self.connector_version.as_ref() {
            s.write_string(&Self::M_CONNECTOR_VERSION, value.as_str())?;
        }
        if let Some(value) = self.connector_arn.as_ref() {
            s.write_string(&Self::M_CONNECTOR_ARN, value.as_str())?;
        }
        if let Some(value) = self.connector_modes.as_ref() {
            write_strings(s, &Self::M_CONNECTOR_MODES, value)?;
        }
        if let Some(value) = self.authentication_config.as_ref() {
            s.write_struct(&Self::M_AUTHENTICATION_CONFIG, value)?;
        }
        if let Some(value) = self.connector_runtime_settings.as_ref() {
            s.write_list(&Self::M_CONNECTOR_RUNTIME_SETTINGS, &|element| {
                for setting in value.iter() {
                    element.write_struct(&ConnectorRuntimeSetting::SCHEMA, setting)?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.supported_api_versions.as_ref() {
            write_strings(s, &Self::M_SUPPORTED_API_VERSIONS, value)?;
        }
        if let Some(value) = self.supported_operators.as_ref() {
            write_strings(s, &Self::M_SUPPORTED_OPERATORS, value)?;
        }
        if let Some(value) = self.supported_write_operations.as_ref() {
            write_strings(s, &Self::M_SUPPORTED_WRITE_OPERATIONS, value)?;
        }
        if let Some(value) = self.connector_provisioning_type.as_ref() {
            s.write_string(&Self::M_CONNECTOR_PROVISIONING_TYPE, value.as_str())?;
        }
        if let Some(value) = self.connector_provisioning_config.as_ref() {
            s.write_struct(&Self::M_CONNECTOR_PROVISIONING_CONFIG, value)?;
        }
        if let Some(value) = self.logo_url.as_ref() {
            s.write_string(&Self::M_LOGO_URL, value.as_str())?;
        }
        if let Some(value) = self.registered_at.as_ref() {
            s.write_timestamp(&Self::M_REGISTERED_AT, value)?;
        }
        if let Some(value) = self.registered_by.as_ref() {
            s.write_string(&Self::M_REGISTERED_BY, value.as_str())?;
        }
        if let Some(value) = self.supported_data_transfer_types.as_ref() {
            write_strings(s, &Self::M_SUPPORTED_DATA_TRANSFER_TYPES, value)?;
        }
        if let Some(value) = self.supported_data_transfer_apis.as_ref() {
            s.write_list(&Self::M_SUPPORTED_DATA_TRANSFER_APIS, &|element| {
                for api in value.iter() {
                    element.write_struct(&DataTransferApi::SCHEMA, api)?;
                }
                Ok(())
            })?;
        }
        s.write_boolean(
            &Self::M_IS_SUPPORTED_API_VERSIONS_REQUIRED,
            self.is_supported_api_versions_required,
        )?;
        Ok(())
    }
}

impl DeserializableStruct for ConnectorConfiguration {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut configuration = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => {
                    configuration.can_use_as_source = d.read_boolean(&Self::M_CAN_USE_AS_SOURCE)?
                }
                Some(1) => {
                    configuration.can_use_as_destination =
                        d.read_boolean(&Self::M_CAN_USE_AS_DESTINATION)?
                }
                Some(2) => {
                    configuration.supported_destination_connectors =
                        Some(read_strings(d, &Self::M_SUPPORTED_DESTINATION_CONNECTORS)?)
                }
                Some(3) => {
                    configuration.supported_scheduling_frequencies =
                        Some(read_strings(d, &Self::M_SUPPORTED_SCHEDULING_FREQUENCIES)?)
                }
                Some(4) => {
                    configuration.is_private_link_enabled =
                        d.read_boolean(&Self::M_IS_PRIVATE_LINK_ENABLED)?
                }
                Some(5) => {
                    configuration.is_private_link_endpoint_url_required =
                        d.read_boolean(&Self::M_IS_PRIVATE_LINK_ENDPOINT_URL_REQUIRED)?
                }
                Some(6) => {
                    configuration.supported_trigger_types =
                        Some(read_strings(d, &Self::M_SUPPORTED_TRIGGER_TYPES)?)
                }
                Some(7) => {
                    configuration.connector_metadata = Some(ConnectorMetadata::deserialize(d)?)
                }
                Some(8) => {
                    configuration.connector_type = Some(d.read_string(&Self::M_CONNECTOR_TYPE)?)
                }
                Some(9) => {
                    configuration.connector_label = Some(d.read_string(&Self::M_CONNECTOR_LABEL)?)
                }
                Some(10) => {
                    configuration.connector_description =
                        Some(d.read_string(&Self::M_CONNECTOR_DESCRIPTION)?)
                }
                Some(11) => {
                    configuration.connector_owner = Some(d.read_string(&Self::M_CONNECTOR_OWNER)?)
                }
                Some(12) => {
                    configuration.connector_name = Some(d.read_string(&Self::M_CONNECTOR_NAME)?)
                }
                Some(13) => {
                    configuration.connector_version =
                        Some(d.read_string(&Self::M_CONNECTOR_VERSION)?)
                }
                Some(14) => {
                    configuration.connector_arn = Some(d.read_string(&Self::M_CONNECTOR_ARN)?)
                }
                Some(15) => {
                    configuration.connector_modes = Some(read_strings(d, &Self::M_CONNECTOR_MODES)?)
                }
                Some(16) => {
                    configuration.authentication_config =
                        Some(AuthenticationConfig::deserialize(d)?)
                }
                Some(17) => {
                    let mut settings: Vec<ConnectorRuntimeSetting> = Vec::new();
                    d.read_list(&Self::M_CONNECTOR_RUNTIME_SETTINGS, &mut |element| {
                        settings.push(ConnectorRuntimeSetting::deserialize(element)?);
                        Ok(())
                    })?;
                    configuration.connector_runtime_settings = Some(settings);
                }
                Some(18) => {
                    configuration.supported_api_versions =
                        Some(read_strings(d, &Self::M_SUPPORTED_API_VERSIONS)?)
                }
                Some(19) => {
                    configuration.supported_operators =
                        Some(read_strings(d, &Self::M_SUPPORTED_OPERATORS)?)
                }
                Some(20) => {
                    configuration.supported_write_operations =
                        Some(read_strings(d, &Self::M_SUPPORTED_WRITE_OPERATIONS)?)
                }
                Some(21) => {
                    configuration.connector_provisioning_type =
                        Some(d.read_string(&Self::M_CONNECTOR_PROVISIONING_TYPE)?)
                }
                Some(22) => {
                    configuration.connector_provisioning_config =
                        Some(ConnectorProvisioningConfig::deserialize(d)?)
                }
                Some(23) => configuration.logo_url = Some(d.read_string(&Self::M_LOGO_URL)?),
                Some(24) => {
                    configuration.registered_at = Some(d.read_timestamp(&Self::M_REGISTERED_AT)?)
                }
                Some(25) => {
                    configuration.registered_by = Some(d.read_string(&Self::M_REGISTERED_BY)?)
                }
                Some(26) => {
                    configuration.supported_data_transfer_types =
                        Some(read_strings(d, &Self::M_SUPPORTED_DATA_TRANSFER_TYPES)?)
                }
                Some(27) => {
                    let mut apis: Vec<DataTransferApi> = Vec::new();
                    d.read_list(&Self::M_SUPPORTED_DATA_TRANSFER_APIS, &mut |element| {
                        apis.push(DataTransferApi::deserialize(element)?);
                        Ok(())
                    })?;
                    configuration.supported_data_transfer_apis = Some(apis);
                }
                Some(28) => {
                    configuration.is_supported_api_versions_required =
                        d.read_boolean(&Self::M_IS_SUPPORTED_API_VERSIONS_REQUIRED)?
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(configuration)
    }
}
