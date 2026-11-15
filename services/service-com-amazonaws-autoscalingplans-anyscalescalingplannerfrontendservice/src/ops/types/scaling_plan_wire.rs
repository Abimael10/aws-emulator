// SPDX-License-Identifier: CC0-1.0

//! The `ScalingPlan` one stored plan is reported as, and the one member of it this
//! service's published model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list, and
//! it admits EXACTLY that list: a Rust field with no member behind it is a compile error,
//! so a member the service really reports but the published model omits cannot be
//! declared through the derive at all. `DescribeScalingPlans` reports one such member on
//! every plan it returns — `LastMutatingRequestTime`, the instant of the last request
//! that wrote the plan — so this module emits for `ScalingPlan` the same items the derive
//! emits: one member schema per member (index = its position in the member list), the
//! struct `SCHEMA` naming them in that order, and the
//! `SerializableStruct`/`DeserializableStruct` pair.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape",shape="ScalingPlan")` lists them in: `ScalingPlanName`,
//! `ScalingPlanVersion`, `ApplicationSource`, `ScalingInstructions`, `StatusCode`,
//! `StatusMessage`, `StatusStartTime`, `CreationTime`); the member it does not declare is
//! appended LAST, with a shape id in a namespace of its own ([`RECORDED_NAMESPACE`]), so
//! it can neither claim to be modelled nor collide with a member the model may declare
//! later — a model refresh only has to move it into place and put the derive back. The
//! ids never leave the process: the JSON codec writes a member's NAME as the object key,
//! and the model file itself stays exactly as shipped.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be
//! inert here: the constraint traits (the validating walk runs on operation INPUTS only,
//! and this structure is output-only), the `@http` chain (baked on only for an
//! operation's input) and `@jsonName` (no member here declares one: each one's wire name
//! IS its member name).
//!
//! Nothing here decides a VALUE: every member is written from what the stored plan holds
//! (see `crate::ops::describe_scaling_plans`).

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_schema_server::DeserializableStruct;
use ::aws_smithy_types::DateTime;

use super::{ApplicationSource, ScalingInstruction};

/// This service's own namespace, which the members the model DOES declare sit in.
const NAMESPACE: &str = "com.amazonaws.autoscalingplans";

/// The namespace the member the published model does not declare sits in: NOT this
/// service's own, so a hand-written member can never be mistaken for a modelled one.
const RECORDED_NAMESPACE: &str = "com.amazonaws.autoscalingplans.recorded";

/// One member the MODEL declares, as the codegen emits a member schema: its shape id in
/// this service's namespace (`namespace#Local$member`, which `ShapeId::from_static` wants
/// as a literal), its shape type, its wire name and its index in the member list.
macro_rules! modelled_member {
    ($name:literal, $shape_type:expr, $index:expr) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!("com.amazonaws.autoscalingplans#ScalingPlan$", $name),
                NAMESPACE,
                $name,
            ),
            $shape_type,
            $name,
            $index,
        )
    };
}

/// One scaling plan, as `DescribeScalingPlans` reports it.
///
/// The first eight members are `com.amazonaws.autoscalingplans#ScalingPlan`'s, in the
/// model's member order. The last one is the member AWS reports beyond the published
/// model: `LastMutatingRequestTime`, which is what lets a caller polling the plan tell a
/// read that has seen its own write from a pre-write snapshot.
#[derive(Debug, Clone, Default)]
pub struct ScalingPlan {
    pub scaling_plan_name: String,
    pub scaling_plan_version: i64,
    pub application_source: ApplicationSource,
    pub scaling_instructions: Vec<ScalingInstruction>,
    pub status_code: String,
    pub status_message: Option<String>,
    pub status_start_time: Option<DateTime>,
    pub creation_time: Option<DateTime>,
    pub last_mutating_request_time: Option<DateTime>,
}

impl ScalingPlan {
    pub const M_SCALING_PLAN_NAME: Schema =
        modelled_member!("ScalingPlanName", ShapeType::String, 0);
    pub const M_SCALING_PLAN_VERSION: Schema =
        modelled_member!("ScalingPlanVersion", ShapeType::Long, 1);
    pub const M_APPLICATION_SOURCE: Schema =
        modelled_member!("ApplicationSource", ShapeType::Structure, 2);
    pub const M_SCALING_INSTRUCTIONS: Schema =
        modelled_member!("ScalingInstructions", ShapeType::List, 3);
    pub const M_STATUS_CODE: Schema = modelled_member!("StatusCode", ShapeType::String, 4);
    pub const M_STATUS_MESSAGE: Schema = modelled_member!("StatusMessage", ShapeType::String, 5);
    pub const M_STATUS_START_TIME: Schema =
        modelled_member!("StatusStartTime", ShapeType::Timestamp, 6);
    pub const M_CREATION_TIME: Schema =
        modelled_member!("CreationTime", ShapeType::Timestamp, 7);
    pub const M_LAST_MUTATING_REQUEST_TIME: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.autoscalingplans.recorded#ScalingPlan$LastMutatingRequestTime",
            RECORDED_NAMESPACE,
            "LastMutatingRequestTime",
        ),
        ShapeType::Timestamp,
        "LastMutatingRequestTime",
        8,
    );

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.autoscalingplans#ScalingPlan",
            NAMESPACE,
            "ScalingPlan",
        ),
        ShapeType::Structure,
        &[
            &Self::M_SCALING_PLAN_NAME,
            &Self::M_SCALING_PLAN_VERSION,
            &Self::M_APPLICATION_SOURCE,
            &Self::M_SCALING_INSTRUCTIONS,
            &Self::M_STATUS_CODE,
            &Self::M_STATUS_MESSAGE,
            &Self::M_STATUS_START_TIME,
            &Self::M_CREATION_TIME,
            &Self::M_LAST_MUTATING_REQUEST_TIME,
        ],
    );
}

pub static SCALING_PLAN_SCHEMA: &Schema = &ScalingPlan::SCHEMA;

impl SerializableStruct for ScalingPlan {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&Self::M_SCALING_PLAN_NAME, self.scaling_plan_name.as_str())?;
        s.write_long(&Self::M_SCALING_PLAN_VERSION, self.scaling_plan_version)?;
        s.write_struct(&Self::M_APPLICATION_SOURCE, &self.application_source)?;
        s.write_list(&Self::M_SCALING_INSTRUCTIONS, &|element| {
            for instruction in self.scaling_instructions.iter() {
                element.write_struct(&ScalingInstruction::SCHEMA, instruction)?;
            }
            Ok(())
        })?;
        s.write_string(&Self::M_STATUS_CODE, self.status_code.as_str())?;
        if let Some(value) = self.status_message.as_ref() {
            s.write_string(&Self::M_STATUS_MESSAGE, value.as_str())?;
        }
        if let Some(value) = self.status_start_time.as_ref() {
            s.write_timestamp(&Self::M_STATUS_START_TIME, value)?;
        }
        if let Some(value) = self.creation_time.as_ref() {
            s.write_timestamp(&Self::M_CREATION_TIME, value)?;
        }
        if let Some(value) = self.last_mutating_request_time.as_ref() {
            s.write_timestamp(&Self::M_LAST_MUTATING_REQUEST_TIME, value)?;
        }
        Ok(())
    }
}

impl DeserializableStruct for ScalingPlan {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut plan = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => plan.scaling_plan_name = d.read_string(&Self::M_SCALING_PLAN_NAME)?,
                Some(1) => plan.scaling_plan_version = d.read_long(&Self::M_SCALING_PLAN_VERSION)?,
                Some(2) => plan.application_source = ApplicationSource::deserialize(d)?,
                Some(3) => {
                    let mut instructions: Vec<ScalingInstruction> = Vec::new();
                    d.read_list(&Self::M_SCALING_INSTRUCTIONS, &mut |element| {
                        instructions.push(ScalingInstruction::deserialize(element)?);
                        Ok(())
                    })?;
                    plan.scaling_instructions = instructions;
                }
                Some(4) => plan.status_code = d.read_string(&Self::M_STATUS_CODE)?,
                Some(5) => plan.status_message = Some(d.read_string(&Self::M_STATUS_MESSAGE)?),
                Some(6) => {
                    plan.status_start_time = Some(d.read_timestamp(&Self::M_STATUS_START_TIME)?)
                }
                Some(7) => plan.creation_time = Some(d.read_timestamp(&Self::M_CREATION_TIME)?),
                Some(8) => {
                    plan.last_mutating_request_time =
                        Some(d.read_timestamp(&Self::M_LAST_MUTATING_REQUEST_TIME)?)
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(plan)
    }
}
