// SPDX-License-Identifier: CC0-1.0

//! The report plan a read reports, and the schedule block it carries that the on-disk
//! model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the model's member list and admits
//! EXACTLY that list: a Rust field with no member behind it is a compile error, so a member
//! the service really reports but the on-disk model omits cannot be declared through the
//! derive at all. A recorded Backup answer for a report plan carries the plan's report
//! TIMING — the frequency the report runs at and the hour of the day it starts — which the
//! checked-in model's `ReportPlan` does not declare
//! (`smithy_query(query="shape", shape="ReportPlan")` lists nine members, none of them a
//! timing block). The items below are the ones the derive emits — one `M_<MEMBER>` member
//! schema per member (index = that member's position in the member list), a `SCHEMA` naming
//! them in that order, the `<SHAPE>_SCHEMA` static and the `SerializableStruct` /
//! `DeserializableStruct` pair — from a member list this file gives, so the extra member can
//! be appended to it.
//!
//! The model's own members keep the names and the order the model gives them, and the member
//! the model does not declare is appended LAST, with its shape id (and the shape id of the
//! structure it targets) in a namespace of its own, so it can neither claim to be modelled
//! nor collide with a member the model may declare later — and a later model refresh only has
//! to move it into place and put the derive back.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and
//! `ReportPlan` appears in no operation input) and the `@http` chain (baked on only for an
//! operation's input). No member below carries a `@jsonName`: each one's wire name IS its
//! Smithy member name, and each timestamp keeps the rendering the protocol gives every
//! timestamp this service writes.

use super::{ReportDeliveryChannel, ReportSetting};

/// The namespace the ids of the shapes this service reports and the model does not
/// declare sit in, so none of them can be mistaken for a modelled shape or collide with
/// one the model may declare later.
const RECORDED_NAMESPACE: &str = "com.amazonaws.backup.recorded";

/// The namespace of this service's own modelled shapes.
const SERVICE_NAMESPACE: &str = "com.amazonaws.backup";

/// One member's schema, as the vendored codegen emits it: the member's shape id, its shape
/// type, its wire name and its index in the member list.
const fn member_schema(
    shape_id: &'static str,
    namespace: &'static str,
    name: &'static str,
    shape_type: ::aws_smithy_schema::ShapeType,
    index: usize,
) -> ::aws_smithy_schema::Schema {
    ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(shape_id, namespace, name),
        shape_type,
        name,
        index,
    )
}

/// When the report of a plan runs: how many hours apart two runs are, and the hour of the
/// day the first of them starts at.
///
/// A structure the recorded wire carries on a report plan and the on-disk model does not
/// declare at all, so its schema and codec are written out here (see this module's note).
#[derive(Debug, Clone, Default)]
pub struct ReportTiming {
    /// How many hours apart the report's runs are.
    pub report_frequency_hours: i32,
    /// The hour of the day, in UTC, the report's first run of a day starts at.
    pub report_start_hour: i32,
}

impl ReportTiming {
    pub const M_REPORT_FREQUENCY_HOURS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup.recorded#ReportTiming$ReportFrequencyHours",
        RECORDED_NAMESPACE,
        "ReportFrequencyHours",
        ::aws_smithy_schema::ShapeType::Integer,
        0,
    );
    pub const M_REPORT_START_HOUR: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup.recorded#ReportTiming$ReportStartHour",
        RECORDED_NAMESPACE,
        "ReportStartHour",
        ::aws_smithy_schema::ShapeType::Integer,
        1,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.backup.recorded#ReportTiming",
            RECORDED_NAMESPACE,
            "ReportTiming",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[&Self::M_REPORT_FREQUENCY_HOURS, &Self::M_REPORT_START_HOUR],
    );
}

pub static REPORT_TIMING_SCHEMA: &::aws_smithy_schema::Schema = &ReportTiming::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for ReportTiming {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        s.write_integer(&Self::M_REPORT_FREQUENCY_HOURS, self.report_frequency_hours)?;
        s.write_integer(&Self::M_REPORT_START_HOUR, self.report_start_hour)?;
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for ReportTiming {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut report_frequency_hours = ::std::option::Option::None;
        let mut report_start_hour = ::std::option::Option::None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    report_frequency_hours = ::std::option::Option::Some(d.read_integer(member)?);
                }
                ::std::option::Option::Some(1) => {
                    report_start_hour = ::std::option::Option::Some(d.read_integer(member)?);
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            // Absent on the wire reads as the zero the schema's own integer default
            // gives, which is what the derive does for a member with no value.
            report_frequency_hours: report_frequency_hours.unwrap_or_default(),
            report_start_hour: report_start_hour.unwrap_or_default(),
        })
    }
}

/// A report plan, as every read of one reports it.
///
/// The modelled members in the model's own order, then the one member the recorded wire
/// carries and the model does not declare (see this module's note).
#[derive(Debug, Clone, Default)]
pub struct ReportPlan {
    pub report_plan_arn: Option<String>,
    pub report_plan_name: Option<String>,
    pub report_plan_description: Option<String>,
    pub report_setting: Option<ReportSetting>,
    pub report_delivery_channel: Option<ReportDeliveryChannel>,
    pub deployment_status: Option<String>,
    pub creation_time: Option<::aws_smithy_types::DateTime>,
    pub last_attempted_execution_time: Option<::aws_smithy_types::DateTime>,
    pub last_successful_execution_time: Option<::aws_smithy_types::DateTime>,
    /// The member the model does not declare, appended after the modelled ones: when the
    /// plan's report runs.
    pub report_timing: Option<ReportTiming>,
}

impl ReportPlan {
    pub const M_REPORT_PLAN_ARN: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$ReportPlanArn",
        SERVICE_NAMESPACE,
        "ReportPlanArn",
        ::aws_smithy_schema::ShapeType::String,
        0,
    );
    pub const M_REPORT_PLAN_NAME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$ReportPlanName",
        SERVICE_NAMESPACE,
        "ReportPlanName",
        ::aws_smithy_schema::ShapeType::String,
        1,
    );
    pub const M_REPORT_PLAN_DESCRIPTION: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$ReportPlanDescription",
        SERVICE_NAMESPACE,
        "ReportPlanDescription",
        ::aws_smithy_schema::ShapeType::String,
        2,
    );
    pub const M_REPORT_SETTING: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$ReportSetting",
        SERVICE_NAMESPACE,
        "ReportSetting",
        ::aws_smithy_schema::ShapeType::Structure,
        3,
    );
    pub const M_REPORT_DELIVERY_CHANNEL: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$ReportDeliveryChannel",
        SERVICE_NAMESPACE,
        "ReportDeliveryChannel",
        ::aws_smithy_schema::ShapeType::Structure,
        4,
    );
    pub const M_DEPLOYMENT_STATUS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$DeploymentStatus",
        SERVICE_NAMESPACE,
        "DeploymentStatus",
        ::aws_smithy_schema::ShapeType::String,
        5,
    );
    pub const M_CREATION_TIME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$CreationTime",
        SERVICE_NAMESPACE,
        "CreationTime",
        ::aws_smithy_schema::ShapeType::Timestamp,
        6,
    );
    pub const M_LAST_ATTEMPTED_EXECUTION_TIME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$LastAttemptedExecutionTime",
        SERVICE_NAMESPACE,
        "LastAttemptedExecutionTime",
        ::aws_smithy_schema::ShapeType::Timestamp,
        7,
    );
    pub const M_LAST_SUCCESSFUL_EXECUTION_TIME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup#ReportPlan$LastSuccessfulExecutionTime",
        SERVICE_NAMESPACE,
        "LastSuccessfulExecutionTime",
        ::aws_smithy_schema::ShapeType::Timestamp,
        8,
    );
    /// The member the model does not declare, appended after the modelled ones.
    pub const M_REPORT_TIMING: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.backup.recorded#ReportPlan$ReportTiming",
        RECORDED_NAMESPACE,
        "ReportTiming",
        ::aws_smithy_schema::ShapeType::Structure,
        9,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.backup#ReportPlan",
            SERVICE_NAMESPACE,
            "ReportPlan",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_REPORT_PLAN_ARN,
            &Self::M_REPORT_PLAN_NAME,
            &Self::M_REPORT_PLAN_DESCRIPTION,
            &Self::M_REPORT_SETTING,
            &Self::M_REPORT_DELIVERY_CHANNEL,
            &Self::M_DEPLOYMENT_STATUS,
            &Self::M_CREATION_TIME,
            &Self::M_LAST_ATTEMPTED_EXECUTION_TIME,
            &Self::M_LAST_SUCCESSFUL_EXECUTION_TIME,
            &Self::M_REPORT_TIMING,
        ],
    );
}

pub static REPORT_PLAN_SCHEMA: &::aws_smithy_schema::Schema = &ReportPlan::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for ReportPlan {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        // Every member of this shape is optional, so each is written only when the plan
        // carries it — the derive's own `is_option` split.
        if let ::std::option::Option::Some(arn) = self.report_plan_arn.as_ref() {
            s.write_string(&Self::M_REPORT_PLAN_ARN, arn.as_str())?;
        }
        if let ::std::option::Option::Some(name) = self.report_plan_name.as_ref() {
            s.write_string(&Self::M_REPORT_PLAN_NAME, name.as_str())?;
        }
        if let ::std::option::Option::Some(description) = self.report_plan_description.as_ref() {
            s.write_string(&Self::M_REPORT_PLAN_DESCRIPTION, description.as_str())?;
        }
        if let ::std::option::Option::Some(setting) = self.report_setting.as_ref() {
            s.write_struct(&Self::M_REPORT_SETTING, setting)?;
        }
        if let ::std::option::Option::Some(channel) = self.report_delivery_channel.as_ref() {
            s.write_struct(&Self::M_REPORT_DELIVERY_CHANNEL, channel)?;
        }
        if let ::std::option::Option::Some(status) = self.deployment_status.as_ref() {
            s.write_string(&Self::M_DEPLOYMENT_STATUS, status.as_str())?;
        }
        if let ::std::option::Option::Some(created) = self.creation_time.as_ref() {
            s.write_timestamp(&Self::M_CREATION_TIME, created)?;
        }
        if let ::std::option::Option::Some(attempted) = self.last_attempted_execution_time.as_ref()
        {
            s.write_timestamp(&Self::M_LAST_ATTEMPTED_EXECUTION_TIME, attempted)?;
        }
        if let ::std::option::Option::Some(succeeded) =
            self.last_successful_execution_time.as_ref()
        {
            s.write_timestamp(&Self::M_LAST_SUCCESSFUL_EXECUTION_TIME, succeeded)?;
        }
        if let ::std::option::Option::Some(timing) = self.report_timing.as_ref() {
            s.write_struct(&Self::M_REPORT_TIMING, timing)?;
        }
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for ReportPlan {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut plan = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    plan.report_plan_arn = ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(1) => {
                    plan.report_plan_name = ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(2) => {
                    plan.report_plan_description =
                        ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(3) => {
                    plan.report_setting = ::std::option::Option::Some(
                        <ReportSetting as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?,
                    );
                }
                ::std::option::Option::Some(4) => {
                    plan.report_delivery_channel = ::std::option::Option::Some(
                        <ReportDeliveryChannel as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?,
                    );
                }
                ::std::option::Option::Some(5) => {
                    plan.deployment_status = ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(6) => {
                    plan.creation_time = ::std::option::Option::Some(d.read_timestamp(member)?);
                }
                ::std::option::Option::Some(7) => {
                    plan.last_attempted_execution_time =
                        ::std::option::Option::Some(d.read_timestamp(member)?);
                }
                ::std::option::Option::Some(8) => {
                    plan.last_successful_execution_time =
                        ::std::option::Option::Some(d.read_timestamp(member)?);
                }
                ::std::option::Option::Some(9) => {
                    plan.report_timing = ::std::option::Option::Some(
                        <ReportTiming as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?,
                    );
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(plan)
    }
}
