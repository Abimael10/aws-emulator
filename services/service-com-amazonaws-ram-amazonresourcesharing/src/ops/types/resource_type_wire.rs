// SPDX-License-Identifier: CC0-1.0

//! The wire form of `ServiceNameAndResourceType`, one entry of the shareable
//! resource-type catalog: the members the on-disk model declares, plus the one RAM
//! reports on every entry it serves.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list,
//! and it admits EXACTLY that list: a Rust field with no member behind it is a compile
//! error, so a member the service really reports but the model omits cannot be declared
//! through the derive at all. RAM reports, beside the three members
//! `smithy_query(query="shape",shape="ServiceNameAndResourceType",model_path="services/service-com-amazonaws-ram-amazonresourcesharing/smithy-model/com-amazonaws-ram-amazonresourcesharing.json")`
//! declares, a boolean saying whether resources of the type can be shared only through
//! service-linked resource shares — so this module emits for the shape the same items the
//! derive emits: one member schema per member (index = its position in the member list),
//! the struct `SCHEMA` naming them in that order, the
//! [`SERVICE_NAME_AND_RESOURCE_TYPE_SCHEMA`] static and the
//! `SerializableStruct`/`DeserializableStruct` pair. It is the same arrangement
//! [`super::permission_summary_wire`] makes for the managed-permission summaries, whose
//! own module docs state the reasoning in full.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="ServiceNameAndResourceType")` lists them in); the
//! member it does not declare is appended LAST, with its shape id in a namespace of its
//! own ([`RECORDED_NAMESPACE`]), so it can neither claim to be modelled nor collide with a
//! member the model may declare later — a model refresh only has to move it into place and
//! put the derive back. The ids never leave the process: the codec writes a member's NAME
//! as the key.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and
//! this structure is output-only), the `@http` chain (baked on only for an operation's
//! input) and `@jsonName` (no member here declares one: each one's wire name IS its member
//! name).
//!
//! Nothing here decides a VALUE: every member is written from the catalog entry the
//! handler projected ([`crate::ops::list_resource_types`]).

use super::*;

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_schema_server::DeserializableStruct;

/// This service's Smithy namespace, which every modelled member's shape id carries.
const NAMESPACE: &str = "com.amazonaws.ram";

/// The namespace the member the on-disk model does not declare sits in: NOT this
/// service's own, so a hand-written member can never be mistaken for a modelled one.
const RECORDED_NAMESPACE: &str = "com.amazonaws.ram.recorded";

/// One member's schema, as the derive emits it for a modelled member: its shape id, its
/// shape type, its wire name and its index in the member list.
macro_rules! member_schema {
    ($namespace:expr, $shape_id:literal, $wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(concat!($shape_id, "$", $wire), $namespace, $wire),
            $shape_type,
            $wire,
            $index,
        )
    };
}

/// One member this service's model DOES declare.
macro_rules! modelled_member {
    ($wire:literal, $shape_type:expr, $index:literal) => {
        member_schema!(
            NAMESPACE,
            "com.amazonaws.ram#ServiceNameAndResourceType",
            $wire,
            $shape_type,
            $index
        )
    };
}

/// The member the model does NOT declare.
macro_rules! recorded_member {
    ($wire:literal, $shape_type:expr, $index:literal) => {
        member_schema!(
            RECORDED_NAMESPACE,
            "com.amazonaws.ram.recorded#ServiceNameAndResourceType",
            $wire,
            $shape_type,
            $index
        )
    };
}

impl ServiceNameAndResourceType {
    pub const M_RESOURCE_TYPE: Schema = modelled_member!("resourceType", ShapeType::String, 0);
    pub const M_SERVICE_NAME: Schema = modelled_member!("serviceName", ShapeType::String, 1);
    /// `resourceRegionScope` targets the `ResourceRegionScope` enum, whose members are
    /// carried as the strings they are on the wire — the way every other enum-targeted
    /// member of this service's hand-written wire forms is.
    pub const M_RESOURCE_REGION_SCOPE: Schema =
        modelled_member!("resourceRegionScope", ShapeType::String, 2);

    pub const M_RESTRICT_TO_SERVICE_LINKED_SHARES: Schema =
        recorded_member!("restrictToServiceLinkedShares", ShapeType::Boolean, 3);

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.ram#ServiceNameAndResourceType",
            NAMESPACE,
            "ServiceNameAndResourceType",
        ),
        ShapeType::Structure,
        &[
            &Self::M_RESOURCE_TYPE,
            &Self::M_SERVICE_NAME,
            &Self::M_RESOURCE_REGION_SCOPE,
            &Self::M_RESTRICT_TO_SERVICE_LINKED_SHARES,
        ],
    );
}

pub static SERVICE_NAME_AND_RESOURCE_TYPE_SCHEMA: &Schema = &ServiceNameAndResourceType::SCHEMA;

impl SerializableStruct for ServiceNameAndResourceType {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(value) = self.resource_type.as_ref() {
            s.write_string(&Self::M_RESOURCE_TYPE, value.as_str())?;
        }
        if let Some(value) = self.service_name.as_ref() {
            s.write_string(&Self::M_SERVICE_NAME, value.as_str())?;
        }
        if let Some(value) = self.resource_region_scope.as_ref() {
            s.write_string(&Self::M_RESOURCE_REGION_SCOPE, value.as_str())?;
        }
        if let Some(value) = self.restrict_to_service_linked_shares.as_ref() {
            s.write_boolean(&Self::M_RESTRICT_TO_SERVICE_LINKED_SHARES, *value)?;
        }
        Ok(())
    }
}

impl DeserializableStruct for ServiceNameAndResourceType {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut entry = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => entry.resource_type = Some(d.read_string(&Self::M_RESOURCE_TYPE)?),
                Some(1) => entry.service_name = Some(d.read_string(&Self::M_SERVICE_NAME)?),
                Some(2) => {
                    entry.resource_region_scope =
                        Some(d.read_string(&Self::M_RESOURCE_REGION_SCOPE)?)
                }
                Some(3) => {
                    entry.restrict_to_service_linked_shares =
                        Some(d.read_boolean(&Self::M_RESTRICT_TO_SERVICE_LINKED_SHARES)?)
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(entry)
    }
}
