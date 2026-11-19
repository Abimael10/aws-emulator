// SPDX-License-Identifier: CC0-1.0

//! The wire form of `ResourceSharePermissionSummary`: the members the on-disk model
//! declares, plus the one RAM reports on every summary it serves.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list,
//! and it admits EXACTLY that list: a Rust field with no member behind it is a compile
//! error, so a member the service really reports but the model omits cannot be declared
//! through the derive at all. RAM reports, beside the twelve members
//! `smithy_query(query="shape",shape="ResourceSharePermissionSummary",model_path="services/service-com-amazonaws-ram-amazonresourcesharing/smithy-model/com-amazonaws-ram-amazonresourcesharing.json")`
//! declares, a boolean saying whether the managed permission the summary describes may
//! govern only a service-linked resource share — so this module emits for the shape the
//! same items the derive emits: one member schema per member (index = its position in the
//! member list), the struct `SCHEMA` naming them in that order, the
//! [`RESOURCE_SHARE_PERMISSION_SUMMARY_SCHEMA`] static and the
//! `SerializableStruct`/`DeserializableStruct` pair. It is the same arrangement
//! [`super::tag_wire`] makes for the tag entries a tag mutation reports.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="ResourceSharePermissionSummary")` lists them in);
//! the member it does not declare is appended LAST, with its shape id in a namespace of
//! its own ([`RECORDED_NAMESPACE`]), so it can neither claim to be modelled nor collide
//! with a member the model may declare later — a model refresh only has to move it into
//! place and put the derive back. The ids never leave the process: the codec writes a
//! member's NAME as the key.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be
//! inert here: the constraint traits (the validating walk runs on operation INPUTS only,
//! and this structure is output-only), the `@http` chain (baked on only for an
//! operation's input) and `@jsonName` (no member here declares one: each one's wire name
//! IS its member name).
//!
//! Nothing here decides a VALUE: every member is written from the stored permission the
//! summary was projected from ([`crate::state::PermissionRecord::summary`]).

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
///
/// The shape id is spelled from the namespace and the structure's own name, so a member
/// the model declares sits in this service's namespace ([`NAMESPACE`]) and the one it does
/// not sits in [`RECORDED_NAMESPACE`], where it can neither claim to be modelled nor
/// collide with a member the model may declare later.
macro_rules! member_schema {
    ($namespace:expr, $shape_id:literal, $wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!($shape_id, "$", $wire),
                $namespace,
                $wire,
            ),
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
            "com.amazonaws.ram#ResourceSharePermissionSummary",
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
            "com.amazonaws.ram.recorded#ResourceSharePermissionSummary",
            $wire,
            $shape_type,
            $index
        )
    };
}

impl ResourceSharePermissionSummary {
    pub const M_ARN: Schema = modelled_member!("arn", ShapeType::String, 0);
    pub const M_VERSION: Schema = modelled_member!("version", ShapeType::String, 1);
    pub const M_DEFAULT_VERSION: Schema =
        modelled_member!("defaultVersion", ShapeType::Boolean, 2);
    pub const M_NAME: Schema = modelled_member!("name", ShapeType::String, 3);
    pub const M_RESOURCE_TYPE: Schema = modelled_member!("resourceType", ShapeType::String, 4);
    pub const M_STATUS: Schema = modelled_member!("status", ShapeType::String, 5);
    pub const M_CREATION_TIME: Schema =
        modelled_member!("creationTime", ShapeType::Timestamp, 6);
    pub const M_LAST_UPDATED_TIME: Schema =
        modelled_member!("lastUpdatedTime", ShapeType::Timestamp, 7);
    pub const M_IS_RESOURCE_TYPE_DEFAULT: Schema =
        modelled_member!("isResourceTypeDefault", ShapeType::Boolean, 8);
    pub const M_PERMISSION_TYPE: Schema =
        modelled_member!("permissionType", ShapeType::String, 9);
    pub const M_FEATURE_SET: Schema = modelled_member!("featureSet", ShapeType::String, 10);
    pub const M_TAGS: Schema = modelled_member!("tags", ShapeType::List, 11);

    pub const M_RESTRICT_TO_SERVICE_LINKED_SHARES: Schema =
        recorded_member!("restrictToServiceLinkedShares", ShapeType::Boolean, 12);

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.ram#ResourceSharePermissionSummary",
            NAMESPACE,
            "ResourceSharePermissionSummary",
        ),
        ShapeType::Structure,
        &[
            &Self::M_ARN,
            &Self::M_VERSION,
            &Self::M_DEFAULT_VERSION,
            &Self::M_NAME,
            &Self::M_RESOURCE_TYPE,
            &Self::M_STATUS,
            &Self::M_CREATION_TIME,
            &Self::M_LAST_UPDATED_TIME,
            &Self::M_IS_RESOURCE_TYPE_DEFAULT,
            &Self::M_PERMISSION_TYPE,
            &Self::M_FEATURE_SET,
            &Self::M_TAGS,
            &Self::M_RESTRICT_TO_SERVICE_LINKED_SHARES,
        ],
    );
}

pub static RESOURCE_SHARE_PERMISSION_SUMMARY_SCHEMA: &Schema =
    &ResourceSharePermissionSummary::SCHEMA;

impl SerializableStruct for ResourceSharePermissionSummary {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(value) = self.arn.as_ref() {
            s.write_string(&Self::M_ARN, value.as_str())?;
        }
        if let Some(value) = self.version.as_ref() {
            s.write_string(&Self::M_VERSION, value.as_str())?;
        }
        if let Some(value) = self.default_version.as_ref() {
            s.write_boolean(&Self::M_DEFAULT_VERSION, *value)?;
        }
        if let Some(value) = self.name.as_ref() {
            s.write_string(&Self::M_NAME, value.as_str())?;
        }
        if let Some(value) = self.resource_type.as_ref() {
            s.write_string(&Self::M_RESOURCE_TYPE, value.as_str())?;
        }
        if let Some(value) = self.status.as_ref() {
            s.write_string(&Self::M_STATUS, value.as_str())?;
        }
        if let Some(value) = self.creation_time.as_ref() {
            s.write_timestamp(&Self::M_CREATION_TIME, value)?;
        }
        if let Some(value) = self.last_updated_time.as_ref() {
            s.write_timestamp(&Self::M_LAST_UPDATED_TIME, value)?;
        }
        if let Some(value) = self.is_resource_type_default.as_ref() {
            s.write_boolean(&Self::M_IS_RESOURCE_TYPE_DEFAULT, *value)?;
        }
        if let Some(value) = self.permission_type.as_ref() {
            s.write_string(&Self::M_PERMISSION_TYPE, value.as_str())?;
        }
        if let Some(value) = self.feature_set.as_ref() {
            s.write_string(&Self::M_FEATURE_SET, value.as_str())?;
        }
        if let Some(value) = self.tags.as_ref() {
            s.write_list(&Self::M_TAGS, &|element_serializer| {
                for tag in value.iter() {
                    element_serializer.write_struct(&Tag::SCHEMA, tag)?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.restrict_to_service_linked_shares.as_ref() {
            s.write_boolean(&Self::M_RESTRICT_TO_SERVICE_LINKED_SHARES, *value)?;
        }
        Ok(())
    }
}

impl DeserializableStruct for ResourceSharePermissionSummary {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut summary = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => summary.arn = Some(d.read_string(&Self::M_ARN)?),
                Some(1) => summary.version = Some(d.read_string(&Self::M_VERSION)?),
                Some(2) => {
                    summary.default_version = Some(d.read_boolean(&Self::M_DEFAULT_VERSION)?)
                }
                Some(3) => summary.name = Some(d.read_string(&Self::M_NAME)?),
                Some(4) => {
                    summary.resource_type = Some(d.read_string(&Self::M_RESOURCE_TYPE)?)
                }
                Some(5) => summary.status = Some(d.read_string(&Self::M_STATUS)?),
                Some(6) => {
                    summary.creation_time = Some(d.read_timestamp(&Self::M_CREATION_TIME)?)
                }
                Some(7) => {
                    summary.last_updated_time =
                        Some(d.read_timestamp(&Self::M_LAST_UPDATED_TIME)?)
                }
                Some(8) => {
                    summary.is_resource_type_default =
                        Some(d.read_boolean(&Self::M_IS_RESOURCE_TYPE_DEFAULT)?)
                }
                Some(9) => {
                    summary.permission_type = Some(d.read_string(&Self::M_PERMISSION_TYPE)?)
                }
                Some(10) => summary.feature_set = Some(d.read_string(&Self::M_FEATURE_SET)?),
                Some(11) => {
                    let mut tags: Vec<Tag> = Vec::new();
                    d.read_list(&Self::M_TAGS, &mut |element| {
                        tags.push(Tag::deserialize(element)?);
                        Ok(())
                    })?;
                    summary.tags = Some(tags);
                }
                Some(12) => {
                    summary.restrict_to_service_linked_shares =
                        Some(d.read_boolean(&Self::M_RESTRICT_TO_SERVICE_LINKED_SHARES)?)
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(summary)
    }
}
