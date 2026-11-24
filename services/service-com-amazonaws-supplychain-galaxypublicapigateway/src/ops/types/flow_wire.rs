// SPDX-License-Identifier: CC0-1.0

//! The `DataIntegrationFlow` shape as the flow reads answer it, with the members the WIRE
//! carries and the on-disk model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the model's member list, and it admits
//! EXACTLY that list: a Rust field with no member behind it is a compile error, so a member
//! the service really reports but the on-disk model omits cannot be declared through the
//! derive at all. The flow read answers three such members — `managedBy` and
//! `triggerOptions` on the flow itself, and `datasetId`/`datasetType` on each dataset end of
//! its pipeline — so the schema and the codec for the shapes that carry them are written out
//! here, exactly as the derive would emit them, from a member list that can be longer than
//! the model's. The same module as `types/instance_wire.rs` does for `Instance`.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="com.amazonaws.supplychain#DataIntegrationFlow")`
//! lists them in), and a member the model does not declare is appended LAST with its shape
//! id in a namespace of its own, so it can neither claim to be modelled nor collide with a
//! member the model may declare later — a later model refresh only has to move it into place
//! and put the derive back.
//!
//! WHY THE DATASET ENDS GET THEIR OWN TYPES. The dataset source and target CONFIGURATIONS
//! are input shapes too (`CreateDataIntegrationFlow` and `UpdateDataIntegrationFlow` take
//! them), and the derive is what captures their modelled constraints for the validating walk
//! that runs on inputs. So the modelled types keep their derive and their constraints, and
//! the types below — which carry the two recorded members beside the modelled ones — are
//! used only where a flow is REPORTED. Each one's `SCHEMA` names the modelled shape it is
//! the wire form of, so the bytes on the wire are the shape's, not a new one's.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and every
//! structure below is reported, never accepted), the `@http` chain (baked on only for an
//! operation's input), and `@jsonName`/`@xmlName` (no member below declares either, and this
//! service's sole protocol is restJson1, where a member's wire name IS its Smithy member
//! name).

use super::shared_types::{
    DataIntegrationFlowDatasetOptions, DataIntegrationFlowS3SourceConfiguration,
    DataIntegrationFlowS3TargetConfiguration, DataIntegrationFlowTransformation,
};

/// The service's own namespace: where the members the model DOES declare sit.
const NAMESPACE: &str = "com.amazonaws.supplychain";

/// The namespace a member the model does not declare sits in, so it cannot be mistaken for
/// a member of the shape as the model has it.
const RECORDED_NAMESPACE: &str = "com.amazonaws.supplychain.recorded";

/// The `ShapeType` one member kind names. Mirrors the vendored codegen's
/// `codegen::shape_type_ident`.
macro_rules! scn_shape_type {
    (string) => {
        ::aws_smithy_schema::ShapeType::String
    };
    (timestamp) => {
        ::aws_smithy_schema::ShapeType::Timestamp
    };
    (struct_of) => {
        ::aws_smithy_schema::ShapeType::Structure
    };
    (list_of_struct) => {
        ::aws_smithy_schema::ShapeType::List
    };
}

/// One member's schema, as the vendored codegen emits it: the member's shape id, its shape
/// type, its wire name and its index in the member list.
///
/// `$kind` is the member's shape kind — `string`, `timestamp`, `struct_of(T)` or
/// `list_of_struct(T)` — or `recorded(<kind>)` for a member the model does not declare at
/// all, whose shape id then sits in [`RECORDED_NAMESPACE`] rather than in the service's own.
macro_rules! scn_member_schema {
    ($local:literal, $wire:literal, $idx:literal, recorded($kind:tt $(($inner:ty))?)) => {
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                concat!("com.amazonaws.supplychain.recorded#", $local, "$", $wire),
                RECORDED_NAMESPACE,
                $wire,
            ),
            scn_shape_type!($kind),
            $wire,
            $idx,
        )
    };
    ($local:literal, $wire:literal, $idx:literal, $kind:tt $(($inner:ty))?) => {
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                concat!("com.amazonaws.supplychain#", $local, "$", $wire),
                NAMESPACE,
                $wire,
            ),
            scn_shape_type!($kind),
            $wire,
            $idx,
        )
    };
}

/// Write one member, as the vendored codegen's `codegen::emit_write` does for that kind: a
/// list of structures writes each element against the element shape's own `SCHEMA`.
macro_rules! scn_write_member {
    // A member the model does not declare writes exactly like the kind it wraps; only its
    // shape id differs (see `scn_member_schema!`).
    ($s:ident, $m:expr, $v:expr, recorded($($kind:tt)+)) => {
        scn_write_member!($s, $m, $v, $($kind)+);
    };
    ($s:ident, $m:expr, $v:expr, string) => {
        $s.write_string($m, $v.as_str())?;
    };
    ($s:ident, $m:expr, $v:expr, timestamp) => {
        $s.write_timestamp($m, $v)?;
    };
    ($s:ident, $m:expr, $v:expr, struct_of($inner:ty)) => {
        $s.write_struct($m, $v)?;
    };
    ($s:ident, $m:expr, $v:expr, list_of_struct($inner:ty)) => {
        $s.write_list($m, &|__sl| {
            for __it in $v.iter() {
                __sl.write_struct(&<$inner>::SCHEMA, __it)?;
            }
            Ok(())
        })?;
    };
}

/// Read one member, as the vendored codegen's `codegen::emit_read` does for that kind.
///
/// Element COUNTS are bounded by the request body the server reads with a cap, and nesting
/// DEPTH by the codec itself, which refuses a document nesting deeper than its own maximum.
macro_rules! scn_read_member {
    ($d:ident, $m:expr, recorded($($kind:tt)+)) => {
        scn_read_member!($d, $m, $($kind)+)
    };
    ($d:ident, $m:expr, string) => {
        $d.read_string($m)?
    };
    ($d:ident, $m:expr, timestamp) => {
        $d.read_timestamp($m)?
    };
    ($d:ident, $m:expr, struct_of($inner:ty)) => {
        <$inner as ::aws_smithy_schema_server::DeserializableStruct>::deserialize($d)?
    };
    ($d:ident, $m:expr, list_of_struct($inner:ty)) => {{
        let mut __out: ::std::vec::Vec<$inner> = ::std::vec::Vec::new();
        $d.read_list($m, &mut |__dl| {
            __out.push(
                <$inner as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(__dl)?,
            );
            Ok(())
        })?;
        __out
    }};
}

/// Write a member only when the value is there: an `Option` one when it is `Some`, a
/// non-`Option` one (`@required`) always — the vendored codegen's own `is_option` split.
macro_rules! scn_write_present_member {
    ($s:ident, $this:ident, $m:path, $field:ident, req, $($kind:tt)+) => {
        scn_write_member!($s, &$m, (&$this.$field), $($kind)+);
    };
    ($s:ident, $this:ident, $m:path, $field:ident, opt, $($kind:tt)+) => {
        if let ::std::option::Option::Some(__v) = $this.$field.as_ref() {
            scn_write_member!($s, &$m, __v, $($kind)+);
        }
    };
}

/// The value a read slot contributes to the struct: a `@required` member absent from the
/// wire is refused by name, an optional one stays `None`.
macro_rules! scn_member_value {
    ($slot:ident, $wire:literal, req) => {
        $slot.ok_or_else(|| ::aws_smithy_schema_server::missing_member($wire))?
    };
    ($slot:ident, $wire:literal, opt) => {
        $slot
    };
}

/// The schema, the serializer and the deserializer for one structure, from the member list
/// given here: everything `#[smithy_macros::smithy(shape = ...)]` emits for a reported
/// shape, for a member list that may carry a member the on-disk model does not declare.
///
/// Each member entry is `(CONST, field, "wireName", index, req|opt, kind)`; `req` is a
/// `@required` member (a bare Rust field, refused when absent on the wire) and `opt` an
/// `Option` one. The entries are given in the model's member order — the order
/// `smithy_query(query="shape", shape=...)` lists them in and the order the struct declares
/// its fields in — with the unmodelled members last.
macro_rules! scn_schema_and_codec {
    (
        $ty:ident, $schema_static:ident, $local:literal,
        $( ( $cname:ident, $field:ident, $wire:literal, $idx:literal, $presence:tt, $($kind:tt)+ ) ),+ $(,)?
    ) => {
        #[allow(non_upper_case_globals)]
        impl $ty {
            $(
                pub const $cname: ::aws_smithy_schema::Schema =
                    scn_member_schema!($local, $wire, $idx, $($kind)+);
            )+

            pub const SCHEMA: ::aws_smithy_schema::Schema =
                ::aws_smithy_schema::Schema::new_struct(
                    ::aws_smithy_schema::ShapeId::from_static(
                        concat!("com.amazonaws.supplychain#", $local),
                        NAMESPACE,
                        $local,
                    ),
                    ::aws_smithy_schema::ShapeType::Structure,
                    &[$(&Self::$cname,)+],
                );
        }

        pub static $schema_static: &::aws_smithy_schema::Schema = &$ty::SCHEMA;

        impl ::aws_smithy_schema::serde::SerializableStruct for $ty {
            fn serialize_members(
                &self,
                s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
            ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
                $(
                    scn_write_present_member!(
                        s, self, Self::$cname, $field, $presence, $($kind)+
                    );
                )+
                Ok(())
            }
        }

        impl ::aws_smithy_schema_server::DeserializableStruct for $ty {
            fn deserialize(
                d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
            ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
                $( let mut $field = ::std::option::Option::None; )+
                d.read_struct(&Self::SCHEMA, &mut |member, d| {
                    match member.member_index() {
                        $(
                            ::std::option::Option::Some($idx) => {
                                $field = ::std::option::Option::Some(
                                    scn_read_member!(d, &Self::$cname, $($kind)+),
                                );
                            }
                        )+
                        _ => {}
                    }
                    Ok(())
                })?;
                Ok(Self {
                    $( $field: scn_member_value!($field, $wire, $presence) ),+
                })
            }
        }
    };
}

/// One dataset end of a reported flow, as the wire carries it: the modelled
/// `DataIntegrationFlowDatasetSourceConfiguration` members plus the dataset's identity.
///
/// `datasetId` and `datasetType` are RECORDED, not modelled — see the module note. The
/// store projects them from the dataset the end's `datasetIdentifier` names
/// (`crate::state::ComAmazonawsSupplychainGalaxypublicapigatewayState::flow_to_wire`), so a
/// flow read reports which dataset this end reads and whether it is one of the service's own
/// or a customer's. The id is absent when the lake holds no such dataset, which is the one
/// case where there is no dataset to have one.
#[derive(Debug, Clone, Default)]
pub struct DataIntegrationFlowDatasetSourceWire {
    pub dataset_identifier: String,
    pub options: Option<DataIntegrationFlowDatasetOptions>,
    pub dataset_id: Option<String>,
    pub dataset_type: String,
}

scn_schema_and_codec!(
    DataIntegrationFlowDatasetSourceWire,
    DATA_INTEGRATION_FLOW_DATASET_SOURCE_WIRE_SCHEMA,
    "DataIntegrationFlowDatasetSourceConfiguration",
    (
        M_DATASET_IDENTIFIER,
        dataset_identifier,
        "datasetIdentifier",
        0,
        req,
        string
    ),
    (
        M_OPTIONS,
        options,
        "options",
        1,
        opt,
        struct_of(DataIntegrationFlowDatasetOptions)
    ),
    (M_DATASET_ID, dataset_id, "datasetId", 2, opt, recorded(string)),
    (
        M_DATASET_TYPE,
        dataset_type,
        "datasetType",
        3,
        req,
        recorded(string)
    ),
);

/// The reported form of a flow's dataset TARGET: the same pair of recorded members as
/// [`DataIntegrationFlowDatasetSourceWire`], on the modelled target configuration.
#[derive(Debug, Clone, Default)]
pub struct DataIntegrationFlowDatasetTargetWire {
    pub dataset_identifier: String,
    pub options: Option<DataIntegrationFlowDatasetOptions>,
    pub dataset_id: Option<String>,
    pub dataset_type: String,
}

scn_schema_and_codec!(
    DataIntegrationFlowDatasetTargetWire,
    DATA_INTEGRATION_FLOW_DATASET_TARGET_WIRE_SCHEMA,
    "DataIntegrationFlowDatasetTargetConfiguration",
    (
        M_DATASET_IDENTIFIER,
        dataset_identifier,
        "datasetIdentifier",
        0,
        req,
        string
    ),
    (
        M_OPTIONS,
        options,
        "options",
        1,
        opt,
        struct_of(DataIntegrationFlowDatasetOptions)
    ),
    (M_DATASET_ID, dataset_id, "datasetId", 2, opt, recorded(string)),
    (
        M_DATASET_TYPE,
        dataset_type,
        "datasetType",
        3,
        req,
        recorded(string)
    ),
);

/// One source of a reported flow: the modelled `DataIntegrationFlowSource` members, with the
/// dataset end reported as [`DataIntegrationFlowDatasetSourceWire`]. An S3 source carries
/// the modelled configuration unchanged — only a DATASET end has a dataset identity to
/// report.
#[derive(Debug, Clone, Default)]
pub struct DataIntegrationFlowSourceWire {
    pub source_type: String,
    pub source_name: String,
    pub s3_source: Option<DataIntegrationFlowS3SourceConfiguration>,
    pub dataset_source: Option<DataIntegrationFlowDatasetSourceWire>,
}

scn_schema_and_codec!(
    DataIntegrationFlowSourceWire,
    DATA_INTEGRATION_FLOW_SOURCE_WIRE_SCHEMA,
    "DataIntegrationFlowSource",
    (M_SOURCE_TYPE, source_type, "sourceType", 0, req, string),
    (M_SOURCE_NAME, source_name, "sourceName", 1, req, string),
    (
        M_S3_SOURCE,
        s3_source,
        "s3Source",
        2,
        opt,
        struct_of(DataIntegrationFlowS3SourceConfiguration)
    ),
    (
        M_DATASET_SOURCE,
        dataset_source,
        "datasetSource",
        3,
        opt,
        struct_of(DataIntegrationFlowDatasetSourceWire)
    ),
);

/// The target of a reported flow: the modelled `DataIntegrationFlowTarget` members, with the
/// dataset end reported as [`DataIntegrationFlowDatasetTargetWire`].
#[derive(Debug, Clone, Default)]
pub struct DataIntegrationFlowTargetWire {
    pub target_type: String,
    pub s3_target: Option<DataIntegrationFlowS3TargetConfiguration>,
    pub dataset_target: Option<DataIntegrationFlowDatasetTargetWire>,
}

scn_schema_and_codec!(
    DataIntegrationFlowTargetWire,
    DATA_INTEGRATION_FLOW_TARGET_WIRE_SCHEMA,
    "DataIntegrationFlowTarget",
    (M_TARGET_TYPE, target_type, "targetType", 0, req, string),
    (
        M_S3_TARGET,
        s3_target,
        "s3Target",
        1,
        opt,
        struct_of(DataIntegrationFlowS3TargetConfiguration)
    ),
    (
        M_DATASET_TARGET,
        dataset_target,
        "datasetTarget",
        2,
        opt,
        struct_of(DataIntegrationFlowDatasetTargetWire)
    ),
);

/// How a reported flow is triggered: the `triggerOptions` member of the flow reads.
///
/// RECORDED, not modelled — the whole structure, hence its shape id in
/// [`RECORDED_NAMESPACE`]. The recorded response of a flow created through this API reports
/// `triggerType` SOURCE_CHANGE beside a null schedule configuration and a null schedule
/// state; no operation of this model configures a schedule (neither flow write carries a
/// trigger member), so the schedule members have no value to report and no shape a recording
/// has fixed, and only the trigger type is reported here. A recording of a SCHEDULED flow is
/// what fixes those two.
#[derive(Debug, Clone, Default)]
pub struct DataIntegrationFlowTriggerOptions {
    pub trigger_type: String,
}

#[allow(non_upper_case_globals)]
impl DataIntegrationFlowTriggerOptions {
    pub const M_TRIGGER_TYPE: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.supplychain.recorded#DataIntegrationFlowTriggerOptions$triggerType",
                RECORDED_NAMESPACE,
                "triggerType",
            ),
            ::aws_smithy_schema::ShapeType::String,
            "triggerType",
            0,
        );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.supplychain.recorded#DataIntegrationFlowTriggerOptions",
            RECORDED_NAMESPACE,
            "DataIntegrationFlowTriggerOptions",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[&Self::M_TRIGGER_TYPE],
    );
}

impl ::aws_smithy_schema::serde::SerializableStruct for DataIntegrationFlowTriggerOptions {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        s.write_string(&Self::M_TRIGGER_TYPE, self.trigger_type.as_str())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for DataIntegrationFlowTriggerOptions {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut trigger_type = ::std::option::Option::None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            if member.member_index() == ::std::option::Option::Some(0) {
                trigger_type = ::std::option::Option::Some(d.read_string(&Self::M_TRIGGER_TYPE)?);
            }
            Ok(())
        })?;
        Ok(Self {
            trigger_type: trigger_type
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("triggerType"))?,
        })
    }
}

/// The details of one data-integration flow: the members
/// `smithy_query(query="shape", shape="com.amazonaws.supplychain#DataIntegrationFlow")`
/// lists, in the model's own member order, plus `managedBy` and `triggerOptions` appended
/// last.
///
/// Those two are RECORDED BEHAVIOUR, not modelled: the on-disk shape declares only
/// `instanceId`, `name`, `sources`, `transformation`, `target`, `createdTime` and
/// `lastModifiedTime`, while the recorded response of a flow read also reports who manages
/// the flow and how it is triggered. The model evidently predates them, so they are appended
/// LAST and a later model refresh only has to move them into place; this is also why the
/// schema and the codec are written out in this module rather than derived (see the module
/// note).
#[derive(Debug, Clone)]
pub struct DataIntegrationFlow {
    pub instance_id: String,
    pub name: String,
    pub sources: Vec<DataIntegrationFlowSourceWire>,
    pub transformation: DataIntegrationFlowTransformation,
    pub target: DataIntegrationFlowTargetWire,
    pub created_time: ::aws_smithy_types::DateTime,
    pub last_modified_time: ::aws_smithy_types::DateTime,
    pub managed_by: String,
    pub trigger_options: DataIntegrationFlowTriggerOptions,
}

scn_schema_and_codec!(
    DataIntegrationFlow,
    DATA_INTEGRATION_FLOW_SCHEMA,
    "DataIntegrationFlow",
    (M_INSTANCE_ID, instance_id, "instanceId", 0, req, string),
    (M_NAME, name, "name", 1, req, string),
    (
        M_SOURCES,
        sources,
        "sources",
        2,
        req,
        list_of_struct(DataIntegrationFlowSourceWire)
    ),
    (
        M_TRANSFORMATION,
        transformation,
        "transformation",
        3,
        req,
        struct_of(DataIntegrationFlowTransformation)
    ),
    (
        M_TARGET,
        target,
        "target",
        4,
        req,
        struct_of(DataIntegrationFlowTargetWire)
    ),
    (M_CREATED_TIME, created_time, "createdTime", 5, req, timestamp),
    (
        M_LAST_MODIFIED_TIME,
        last_modified_time,
        "lastModifiedTime",
        6,
        req,
        timestamp
    ),
    (M_MANAGED_BY, managed_by, "managedBy", 7, req, recorded(string)),
    (
        M_TRIGGER_OPTIONS,
        trigger_options,
        "triggerOptions",
        8,
        req,
        recorded(struct_of(DataIntegrationFlowTriggerOptions))
    ),
);
