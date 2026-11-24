// SPDX-License-Identifier: CC0-1.0

//! The schema and the codec for `DataIntegrationEventDatasetTargetDetails`, the dataset load
//! an event read reports — the one structure of this service whose WIRE form leaves out a
//! member the on-disk model declares `@required`.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the model's member list, and a
//! `@required` member is a bare Rust field there: the derive writes it always and refuses it
//! when the wire carries none. `datasetIdentifier` is the dataset ARN a CLIENT addressed,
//! and no client addresses the dataset the service keeps for an entity event type — the
//! recorded event read of such an event reports the load with that member absent. So the
//! field is an `Option` here and the items below are exactly what the derive emits for this
//! shape, written out so that one member can be optional. The same module as
//! `types/instance_wire.rs` and `types/flow_wire.rs` are for their shapes.
//!
//! The structure is REPORTED, never accepted: no operation of this service takes it as an
//! input (a send names its target through `DataIntegrationEventDatasetTargetConfiguration`),
//! which is why the derive's constraint traits are deliberately absent — the validating walk
//! that enforces them runs on operation inputs only. So are the `@http` chain (baked on only
//! for an input) and `@jsonName`/`@xmlName`: no member below declares either, and this
//! service's sole protocol is restJson1, where a member's wire name IS its Smithy member
//! name.

use super::shared_types::DataIntegrationEventDatasetLoadExecutionDetails;

/// The service's own namespace: where all three members of this shape sit.
const NAMESPACE: &str = "com.amazonaws.supplychain";

/// The dataset load one data-integration event drove, as the event reads report it.
///
/// `datasetIdentifier` is the dataset the load wrote, as the ARN a client names a dataset
/// by — present for an event that named its own target, absent for one loaded into the
/// dataset the service keeps for its event type (see
/// [`crate::state::entity_event_dataset_load`]). The model declares it `@required`, which
/// the recorded response of such an event contradicts; see the module note.
#[derive(Debug, Clone, Default)]
pub struct DataIntegrationEventDatasetTargetDetails {
    pub dataset_identifier: Option<String>,
    pub operation_type: String,
    pub dataset_load_execution: DataIntegrationEventDatasetLoadExecutionDetails,
}

#[allow(non_upper_case_globals)]
impl DataIntegrationEventDatasetTargetDetails {
    pub const M_DATASET_IDENTIFIER: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#DataIntegrationEventDatasetTargetDetails$datasetIdentifier",
                NAMESPACE,
                "datasetIdentifier",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "datasetIdentifier",
            0,
        );

    pub const M_OPERATION_TYPE: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#DataIntegrationEventDatasetTargetDetails$operationType",
                NAMESPACE,
                "operationType",
            ),
            // `DataIntegrationEventDatasetOperationType` is an `enum` shape, which is
            // string-encoded on the wire.
            ::aws_smithy_schema::ShapeType::String,
            "operationType",
            1,
        );

    pub const M_DATASET_LOAD_EXECUTION: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain#DataIntegrationEventDatasetTargetDetails$datasetLoadExecution",
                NAMESPACE,
                "datasetLoadExecution",
            ),
            ::aws_smithy_schema::ShapeType::Structure,
            "datasetLoadExecution",
            2,
        );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.supplychain#DataIntegrationEventDatasetTargetDetails",
            NAMESPACE,
            "DataIntegrationEventDatasetTargetDetails",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_DATASET_IDENTIFIER,
            &Self::M_OPERATION_TYPE,
            &Self::M_DATASET_LOAD_EXECUTION,
        ],
    );
}

/// This shape's schema, under the name the derive would have given it, so a caller that
/// names the shape's schema reaches the same one the codec uses.
pub static DATA_INTEGRATION_EVENT_DATASET_TARGET_DETAILS_SCHEMA: &::aws_smithy_schema::Schema =
    &DataIntegrationEventDatasetTargetDetails::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DataIntegrationEventDatasetTargetDetails {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        // The one optional member is written only when the load wrote a dataset a client
        // addressed; the other two are written always, as the derive writes a `@required`
        // one.
        if let Some(value) = self.dataset_identifier.as_ref() {
            s.write_string(&Self::M_DATASET_IDENTIFIER, value.as_str())?;
        }
        s.write_string(&Self::M_OPERATION_TYPE, self.operation_type.as_str())?;
        s.write_struct(
            &Self::M_DATASET_LOAD_EXECUTION,
            &self.dataset_load_execution,
        )
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for DataIntegrationEventDatasetTargetDetails {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut dataset_identifier = ::std::option::Option::None;
        let mut operation_type = ::std::option::Option::None;
        let mut dataset_load_execution = ::std::option::Option::None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    dataset_identifier =
                        ::std::option::Option::Some(d.read_string(&Self::M_DATASET_IDENTIFIER)?);
                }
                ::std::option::Option::Some(1) => {
                    operation_type =
                        ::std::option::Option::Some(d.read_string(&Self::M_OPERATION_TYPE)?);
                }
                ::std::option::Option::Some(2) => {
                    dataset_load_execution = ::std::option::Option::Some(
                        <DataIntegrationEventDatasetLoadExecutionDetails as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?,
                    );
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            dataset_identifier,
            // The two members the model requires and the wire always carries are refused by
            // name when it does not, as the derive refuses them.
            operation_type: operation_type
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("operationType"))?,
            dataset_load_execution: dataset_load_execution.ok_or_else(|| {
                ::aws_smithy_schema_server::missing_member("datasetLoadExecution")
            })?,
        })
    }
}
