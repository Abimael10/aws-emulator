// SPDX-License-Identifier: CC0-1.0

//! The `Command` every reader of a Run Command request answers with, and the five members
//! of it the on-disk model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list, and
//! it admits EXACTLY that list: a Rust field with no member behind it is a compile error,
//! so a member the service really reports but the model omits cannot be declared through
//! the derive at all. Systems Manager reports five such members on an accepted command —
//! the client name and client source id a command was issued under, whether it carries a
//! send or a cancel signature, and whether it is an interactive one — so this module emits
//! for `Command` the same items the derive emits: one member schema per member (index = its
//! position in the member list), the struct `SCHEMA` naming them in that order, the
//! `COMMAND_SCHEMA` static and the `SerializableStruct`/`DeserializableStruct` pair. It is
//! the same arrangement other services in this tree make for a member their own model omits
//! (`services/service-com-amazonaws-appstream-photonadminproxyservice/src/ops/types/
//! theme_wire.rs`).
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="Command")` lists them in); the members it does not
//! declare are appended LAST, with their shape ids in a namespace of their own
//! ([`RECORDED_NAMESPACE`]), so they can neither claim to be modelled nor collide with
//! members the model may declare later — a model refresh only has to move them into place
//! and put the derive back. The ids never leave the process: the codec writes a member's
//! NAME as the key.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and this
//! structure is output-only, which is also why `@sensitive` on `Parameters` has nothing to
//! hide here), the `@http` chain (baked on only for an operation's input) and `@jsonName`
//! (no member here declares one: each one's wire name IS its member name).
//!
//! Nothing here decides a VALUE: every member is written from what the stored command holds
//! (`crate::state::commands::CommandRecord::to_command`).

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_schema_server::DeserializableStruct;
use ::aws_smithy_types::DateTime;
use ::std::collections::HashMap;

use super::{AlarmConfiguration, AlarmStateInformation, CloudWatchOutputConfig,
    NotificationConfig, Target};

/// The namespace the members the on-disk model does not declare sit in: NOT this service's
/// own, so a hand-written member can never be mistaken for a modelled one.
const RECORDED_NAMESPACE: &str = "com.amazonaws.ssm.recorded";

/// A command request, as every operation that reports one answers it.
///
/// The first twenty-six members are `com.amazonaws.ssm#Command`'s, in the model's member
/// order. The five after them are the ones Systems Manager reports beyond the published
/// model.
#[derive(Debug, Clone, Default)]
pub struct Command {
    pub command_id: Option<String>,
    pub document_name: Option<String>,
    pub document_version: Option<String>,
    pub comment: Option<String>,
    pub expires_after: Option<DateTime>,
    pub parameters: Option<HashMap<String, Vec<String>>>,
    pub instance_ids: Option<Vec<String>>,
    pub targets: Option<Vec<Target>>,
    pub requested_date_time: Option<DateTime>,
    pub status: Option<String>,
    pub status_details: Option<String>,
    pub output_s3_region: Option<String>,
    pub output_s3_bucket_name: Option<String>,
    pub output_s3_key_prefix: Option<String>,
    pub max_concurrency: Option<String>,
    pub max_errors: Option<String>,
    pub target_count: i32,
    pub completed_count: i32,
    pub error_count: i32,
    pub delivery_timed_out_count: i32,
    pub service_role: Option<String>,
    pub notification_config: Option<NotificationConfig>,
    pub cloud_watch_output_config: Option<CloudWatchOutputConfig>,
    pub timeout_seconds: Option<i32>,
    pub alarm_configuration: Option<AlarmConfiguration>,
    pub triggered_alarms: Option<Vec<AlarmStateInformation>>,
    /// The name of the client the command was issued by.
    pub client_name: Option<String>,
    /// The id of the source that issued it.
    pub client_source_id: Option<String>,
    /// Whether the command carries a send-command signature.
    pub has_send_command_signature: Option<bool>,
    /// Whether it carries a cancel-command signature.
    pub has_cancel_command_signature: Option<bool>,
    /// Whether it is an interactive command.
    pub interactive: Option<bool>,
}

/// One member this service's model DOES declare: its shape id in the service's own
/// namespace, its shape type, its wire name and its index in the member list — the member
/// schema the derive would emit for it.
macro_rules! modelled_member {
    ($wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!("com.amazonaws.ssm#Command$", $wire),
                "com.amazonaws.ssm",
                $wire,
            ),
            $shape_type,
            $wire,
            $index,
        )
    };
}

/// One member the model does NOT declare, in [`RECORDED_NAMESPACE`] so it cannot be
/// mistaken for a modelled one.
macro_rules! recorded_member {
    ($wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!("com.amazonaws.ssm.recorded#Command$", $wire),
                RECORDED_NAMESPACE,
                $wire,
            ),
            $shape_type,
            $wire,
            $index,
        )
    };
}

impl Command {
    pub const M_COMMAND_ID: Schema = modelled_member!("CommandId", ShapeType::String, 0);
    pub const M_DOCUMENT_NAME: Schema = modelled_member!("DocumentName", ShapeType::String, 1);
    pub const M_DOCUMENT_VERSION: Schema =
        modelled_member!("DocumentVersion", ShapeType::String, 2);
    pub const M_COMMENT: Schema = modelled_member!("Comment", ShapeType::String, 3);
    pub const M_EXPIRES_AFTER: Schema =
        modelled_member!("ExpiresAfter", ShapeType::Timestamp, 4);
    pub const M_PARAMETERS: Schema = modelled_member!("Parameters", ShapeType::Map, 5);
    pub const M_INSTANCE_IDS: Schema = modelled_member!("InstanceIds", ShapeType::List, 6);
    pub const M_TARGETS: Schema = modelled_member!("Targets", ShapeType::List, 7);
    pub const M_REQUESTED_DATE_TIME: Schema =
        modelled_member!("RequestedDateTime", ShapeType::Timestamp, 8);
    pub const M_STATUS: Schema = modelled_member!("Status", ShapeType::String, 9);
    pub const M_STATUS_DETAILS: Schema =
        modelled_member!("StatusDetails", ShapeType::String, 10);
    pub const M_OUTPUT_S3_REGION: Schema =
        modelled_member!("OutputS3Region", ShapeType::String, 11);
    pub const M_OUTPUT_S3_BUCKET_NAME: Schema =
        modelled_member!("OutputS3BucketName", ShapeType::String, 12);
    pub const M_OUTPUT_S3_KEY_PREFIX: Schema =
        modelled_member!("OutputS3KeyPrefix", ShapeType::String, 13);
    pub const M_MAX_CONCURRENCY: Schema =
        modelled_member!("MaxConcurrency", ShapeType::String, 14);
    pub const M_MAX_ERRORS: Schema = modelled_member!("MaxErrors", ShapeType::String, 15);
    pub const M_TARGET_COUNT: Schema = modelled_member!("TargetCount", ShapeType::Integer, 16);
    pub const M_COMPLETED_COUNT: Schema =
        modelled_member!("CompletedCount", ShapeType::Integer, 17);
    pub const M_ERROR_COUNT: Schema = modelled_member!("ErrorCount", ShapeType::Integer, 18);
    pub const M_DELIVERY_TIMED_OUT_COUNT: Schema =
        modelled_member!("DeliveryTimedOutCount", ShapeType::Integer, 19);
    pub const M_SERVICE_ROLE: Schema = modelled_member!("ServiceRole", ShapeType::String, 20);
    pub const M_NOTIFICATION_CONFIG: Schema =
        modelled_member!("NotificationConfig", ShapeType::Structure, 21);
    pub const M_CLOUD_WATCH_OUTPUT_CONFIG: Schema =
        modelled_member!("CloudWatchOutputConfig", ShapeType::Structure, 22);
    pub const M_TIMEOUT_SECONDS: Schema =
        modelled_member!("TimeoutSeconds", ShapeType::Integer, 23);
    pub const M_ALARM_CONFIGURATION: Schema =
        modelled_member!("AlarmConfiguration", ShapeType::Structure, 24);
    pub const M_TRIGGERED_ALARMS: Schema =
        modelled_member!("TriggeredAlarms", ShapeType::List, 25);

    pub const M_CLIENT_NAME: Schema = recorded_member!("ClientName", ShapeType::String, 26);
    pub const M_CLIENT_SOURCE_ID: Schema =
        recorded_member!("ClientSourceId", ShapeType::String, 27);
    pub const M_HAS_SEND_COMMAND_SIGNATURE: Schema =
        recorded_member!("HasSendCommandSignature", ShapeType::Boolean, 28);
    pub const M_HAS_CANCEL_COMMAND_SIGNATURE: Schema =
        recorded_member!("HasCancelCommandSignature", ShapeType::Boolean, 29);
    pub const M_INTERACTIVE: Schema = recorded_member!("Interactive", ShapeType::Boolean, 30);

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static("com.amazonaws.ssm#Command", "com.amazonaws.ssm", "Command"),
        ShapeType::Structure,
        &[
            &Self::M_COMMAND_ID,
            &Self::M_DOCUMENT_NAME,
            &Self::M_DOCUMENT_VERSION,
            &Self::M_COMMENT,
            &Self::M_EXPIRES_AFTER,
            &Self::M_PARAMETERS,
            &Self::M_INSTANCE_IDS,
            &Self::M_TARGETS,
            &Self::M_REQUESTED_DATE_TIME,
            &Self::M_STATUS,
            &Self::M_STATUS_DETAILS,
            &Self::M_OUTPUT_S3_REGION,
            &Self::M_OUTPUT_S3_BUCKET_NAME,
            &Self::M_OUTPUT_S3_KEY_PREFIX,
            &Self::M_MAX_CONCURRENCY,
            &Self::M_MAX_ERRORS,
            &Self::M_TARGET_COUNT,
            &Self::M_COMPLETED_COUNT,
            &Self::M_ERROR_COUNT,
            &Self::M_DELIVERY_TIMED_OUT_COUNT,
            &Self::M_SERVICE_ROLE,
            &Self::M_NOTIFICATION_CONFIG,
            &Self::M_CLOUD_WATCH_OUTPUT_CONFIG,
            &Self::M_TIMEOUT_SECONDS,
            &Self::M_ALARM_CONFIGURATION,
            &Self::M_TRIGGERED_ALARMS,
            &Self::M_CLIENT_NAME,
            &Self::M_CLIENT_SOURCE_ID,
            &Self::M_HAS_SEND_COMMAND_SIGNATURE,
            &Self::M_HAS_CANCEL_COMMAND_SIGNATURE,
            &Self::M_INTERACTIVE,
        ],
    );
}

pub static COMMAND_SCHEMA: &Schema = &Command::SCHEMA;

impl SerializableStruct for Command {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(value) = self.command_id.as_ref() {
            s.write_string(&Self::M_COMMAND_ID, value.as_str())?;
        }
        if let Some(value) = self.document_name.as_ref() {
            s.write_string(&Self::M_DOCUMENT_NAME, value.as_str())?;
        }
        if let Some(value) = self.document_version.as_ref() {
            s.write_string(&Self::M_DOCUMENT_VERSION, value.as_str())?;
        }
        if let Some(value) = self.comment.as_ref() {
            s.write_string(&Self::M_COMMENT, value.as_str())?;
        }
        if let Some(value) = self.expires_after.as_ref() {
            s.write_timestamp(&Self::M_EXPIRES_AFTER, value)?;
        }
        if let Some(value) = self.parameters.as_ref() {
            s.write_map(&Self::M_PARAMETERS, &|entries| {
                for (name, values) in value.iter() {
                    entries.write_string(&Self::M_PARAMETERS, name.as_str())?;
                    entries.write_list(&Self::M_PARAMETERS, &|element| {
                        for held in values.iter() {
                            element.write_string(&Self::M_PARAMETERS, held.as_str())?;
                        }
                        Ok(())
                    })?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.instance_ids.as_ref() {
            s.write_list(&Self::M_INSTANCE_IDS, &|element| {
                for held in value.iter() {
                    element.write_string(&Self::M_INSTANCE_IDS, held.as_str())?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.targets.as_ref() {
            s.write_list(&Self::M_TARGETS, &|element| {
                for held in value.iter() {
                    element.write_struct(&Target::SCHEMA, held)?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.requested_date_time.as_ref() {
            s.write_timestamp(&Self::M_REQUESTED_DATE_TIME, value)?;
        }
        if let Some(value) = self.status.as_ref() {
            s.write_string(&Self::M_STATUS, value.as_str())?;
        }
        if let Some(value) = self.status_details.as_ref() {
            s.write_string(&Self::M_STATUS_DETAILS, value.as_str())?;
        }
        if let Some(value) = self.output_s3_region.as_ref() {
            s.write_string(&Self::M_OUTPUT_S3_REGION, value.as_str())?;
        }
        if let Some(value) = self.output_s3_bucket_name.as_ref() {
            s.write_string(&Self::M_OUTPUT_S3_BUCKET_NAME, value.as_str())?;
        }
        if let Some(value) = self.output_s3_key_prefix.as_ref() {
            s.write_string(&Self::M_OUTPUT_S3_KEY_PREFIX, value.as_str())?;
        }
        if let Some(value) = self.max_concurrency.as_ref() {
            s.write_string(&Self::M_MAX_CONCURRENCY, value.as_str())?;
        }
        if let Some(value) = self.max_errors.as_ref() {
            s.write_string(&Self::M_MAX_ERRORS, value.as_str())?;
        }
        // The four counts carry the model's `@default` of 0, so they are written whatever
        // they hold, exactly as the derive writes a non-`Option` member.
        s.write_integer(&Self::M_TARGET_COUNT, self.target_count)?;
        s.write_integer(&Self::M_COMPLETED_COUNT, self.completed_count)?;
        s.write_integer(&Self::M_ERROR_COUNT, self.error_count)?;
        s.write_integer(
            &Self::M_DELIVERY_TIMED_OUT_COUNT,
            self.delivery_timed_out_count,
        )?;
        if let Some(value) = self.service_role.as_ref() {
            s.write_string(&Self::M_SERVICE_ROLE, value.as_str())?;
        }
        if let Some(value) = self.notification_config.as_ref() {
            s.write_struct(&Self::M_NOTIFICATION_CONFIG, value)?;
        }
        if let Some(value) = self.cloud_watch_output_config.as_ref() {
            s.write_struct(&Self::M_CLOUD_WATCH_OUTPUT_CONFIG, value)?;
        }
        if let Some(value) = self.timeout_seconds.as_ref() {
            s.write_integer(&Self::M_TIMEOUT_SECONDS, *value)?;
        }
        if let Some(value) = self.alarm_configuration.as_ref() {
            s.write_struct(&Self::M_ALARM_CONFIGURATION, value)?;
        }
        if let Some(value) = self.triggered_alarms.as_ref() {
            s.write_list(&Self::M_TRIGGERED_ALARMS, &|element| {
                for held in value.iter() {
                    element.write_struct(&AlarmStateInformation::SCHEMA, held)?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.client_name.as_ref() {
            s.write_string(&Self::M_CLIENT_NAME, value.as_str())?;
        }
        if let Some(value) = self.client_source_id.as_ref() {
            s.write_string(&Self::M_CLIENT_SOURCE_ID, value.as_str())?;
        }
        if let Some(value) = self.has_send_command_signature.as_ref() {
            s.write_boolean(&Self::M_HAS_SEND_COMMAND_SIGNATURE, *value)?;
        }
        if let Some(value) = self.has_cancel_command_signature.as_ref() {
            s.write_boolean(&Self::M_HAS_CANCEL_COMMAND_SIGNATURE, *value)?;
        }
        if let Some(value) = self.interactive.as_ref() {
            s.write_boolean(&Self::M_INTERACTIVE, *value)?;
        }
        Ok(())
    }
}

impl DeserializableStruct for Command {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut command = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => command.command_id = Some(d.read_string(&Self::M_COMMAND_ID)?),
                Some(1) => command.document_name = Some(d.read_string(&Self::M_DOCUMENT_NAME)?),
                Some(2) => {
                    command.document_version = Some(d.read_string(&Self::M_DOCUMENT_VERSION)?)
                }
                Some(3) => command.comment = Some(d.read_string(&Self::M_COMMENT)?),
                Some(4) => {
                    command.expires_after = Some(d.read_timestamp(&Self::M_EXPIRES_AFTER)?)
                }
                Some(5) => {
                    let mut parameters: HashMap<String, Vec<String>> = HashMap::new();
                    d.read_map(&Self::M_PARAMETERS, &mut |name, entry| {
                        let mut values: Vec<String> = Vec::new();
                        entry.read_list(&Self::M_PARAMETERS, &mut |element| {
                            values.push(element.read_string(&Self::M_PARAMETERS)?);
                            Ok(())
                        })?;
                        parameters.insert(name, values);
                        Ok(())
                    })?;
                    command.parameters = Some(parameters);
                }
                Some(6) => {
                    let mut ids: Vec<String> = Vec::new();
                    d.read_list(&Self::M_INSTANCE_IDS, &mut |element| {
                        ids.push(element.read_string(&Self::M_INSTANCE_IDS)?);
                        Ok(())
                    })?;
                    command.instance_ids = Some(ids);
                }
                Some(7) => {
                    let mut targets: Vec<Target> = Vec::new();
                    d.read_list(&Self::M_TARGETS, &mut |element| {
                        targets.push(Target::deserialize(element)?);
                        Ok(())
                    })?;
                    command.targets = Some(targets);
                }
                Some(8) => {
                    command.requested_date_time =
                        Some(d.read_timestamp(&Self::M_REQUESTED_DATE_TIME)?)
                }
                Some(9) => command.status = Some(d.read_string(&Self::M_STATUS)?),
                Some(10) => {
                    command.status_details = Some(d.read_string(&Self::M_STATUS_DETAILS)?)
                }
                Some(11) => {
                    command.output_s3_region = Some(d.read_string(&Self::M_OUTPUT_S3_REGION)?)
                }
                Some(12) => {
                    command.output_s3_bucket_name =
                        Some(d.read_string(&Self::M_OUTPUT_S3_BUCKET_NAME)?)
                }
                Some(13) => {
                    command.output_s3_key_prefix =
                        Some(d.read_string(&Self::M_OUTPUT_S3_KEY_PREFIX)?)
                }
                Some(14) => {
                    command.max_concurrency = Some(d.read_string(&Self::M_MAX_CONCURRENCY)?)
                }
                Some(15) => command.max_errors = Some(d.read_string(&Self::M_MAX_ERRORS)?),
                Some(16) => command.target_count = d.read_integer(&Self::M_TARGET_COUNT)?,
                Some(17) => command.completed_count = d.read_integer(&Self::M_COMPLETED_COUNT)?,
                Some(18) => command.error_count = d.read_integer(&Self::M_ERROR_COUNT)?,
                Some(19) => {
                    command.delivery_timed_out_count =
                        d.read_integer(&Self::M_DELIVERY_TIMED_OUT_COUNT)?
                }
                Some(20) => command.service_role = Some(d.read_string(&Self::M_SERVICE_ROLE)?),
                Some(21) => {
                    command.notification_config = Some(NotificationConfig::deserialize(d)?)
                }
                Some(22) => {
                    command.cloud_watch_output_config =
                        Some(CloudWatchOutputConfig::deserialize(d)?)
                }
                Some(23) => {
                    command.timeout_seconds = Some(d.read_integer(&Self::M_TIMEOUT_SECONDS)?)
                }
                Some(24) => {
                    command.alarm_configuration = Some(AlarmConfiguration::deserialize(d)?)
                }
                Some(25) => {
                    let mut alarms: Vec<AlarmStateInformation> = Vec::new();
                    d.read_list(&Self::M_TRIGGERED_ALARMS, &mut |element| {
                        alarms.push(AlarmStateInformation::deserialize(element)?);
                        Ok(())
                    })?;
                    command.triggered_alarms = Some(alarms);
                }
                Some(26) => command.client_name = Some(d.read_string(&Self::M_CLIENT_NAME)?),
                Some(27) => {
                    command.client_source_id = Some(d.read_string(&Self::M_CLIENT_SOURCE_ID)?)
                }
                Some(28) => {
                    command.has_send_command_signature =
                        Some(d.read_boolean(&Self::M_HAS_SEND_COMMAND_SIGNATURE)?)
                }
                Some(29) => {
                    command.has_cancel_command_signature =
                        Some(d.read_boolean(&Self::M_HAS_CANCEL_COMMAND_SIGNATURE)?)
                }
                Some(30) => command.interactive = Some(d.read_boolean(&Self::M_INTERACTIVE)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(command)
    }
}
