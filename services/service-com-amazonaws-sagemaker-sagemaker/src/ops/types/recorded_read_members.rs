// SPDX-License-Identifier: CC0-1.0

//! The members a SageMaker READ answers with that the on-disk model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list, and
//! it admits EXACTLY that list: a Rust field with no member behind it is a compile error, so
//! a member the service really reports but the model omits cannot be declared through the
//! derive at all. The structures below WRAP the derived ones: each delegates to the derived
//! `serialize_members` for the modelled members and then writes the extra ones from its own
//! member schemas — the same way the framework's own awsJson writer appends `__type` to an
//! error body from a member schema that is in no structure's member list
//! (`aws_smithy_schema_server::rpc`). A JSON member's key is read off the member schema
//! handed to the write (`json::JsonShapeSerializer::emit_member_key`), so a member written
//! this way lands on the wire exactly like a modelled one.
//!
//! A member the model does not declare sits in a namespace of its own
//! ([`RECORDED_NAMESPACE`]), so it can neither claim to be modelled nor collide with a
//! member the model may declare later: a model refresh only has to add the field to the
//! derived structure and drop the wrapper. The ids never leave the process — the codec
//! writes a member's NAME as the key.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and
//! every structure here is output-only) and the `@http` chain (baked on only for an
//! operation's input).
//!
//! Nothing here decides a VALUE: every member is written from what the reading operation
//! passes in (`ops::describe_feature_group`, `ops::describe_training_job`), which reads it
//! off the stored resource. Nor can anything here panic: every write is an `if let` over an
//! `Option`, and no value is indexed, sliced, unwrapped or arithmetic.

use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};

use super::{
    Channel, DescribeFeatureGroupOutput, DescribeTrainingJobOutput, OutputDataConfig,
    ResourceConfig,
};

/// The namespace the members the on-disk model does not declare sit in: NOT this service's
/// own, so a hand-written member can never be mistaken for a modelled one.
const RECORDED_NAMESPACE: &str = "com.amazonaws.sagemaker.recorded";

/// One member of `$local` the on-disk model does not declare: its shape id in
/// [`RECORDED_NAMESPACE`], its shape type, its wire name and the index it would take in
/// `$local`'s member list.
///
/// The namespace is spelled out as a literal as well, because `concat!` takes literals only;
/// it has to stay in step with [`RECORDED_NAMESPACE`] beside it.
macro_rules! recorded_member {
    ($local:literal, $wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!("com.amazonaws.sagemaker.recorded#", $local, "$", $wire),
                RECORDED_NAMESPACE,
                $wire,
            ),
            $shape_type,
            $wire,
            $index,
        )
    };
}

/// `DescribeFeatureGroup`'s answer: the modelled response, plus the three online-store
/// members the recorded service carries for a group that keeps an online store.
///
/// The modelled members keep the indices the model gives them (0..=17); the three recorded
/// ones are appended after them.
#[derive(Debug, Clone)]
pub struct DescribeFeatureGroupRecordedOutput {
    /// Every member `DescribeFeatureGroupResponse` declares.
    pub modelled: DescribeFeatureGroupOutput,
    /// Whether the online store may be read AND written, or only read.
    pub online_store_read_write_type: Option<String>,
    /// The online store's replicas — the stores that mirror it.
    pub online_store_replicas: Option<Vec<String>>,
    /// How many items the online store holds.
    pub online_store_total_item_count: Option<i64>,
}

impl DescribeFeatureGroupRecordedOutput {
    const M_ONLINE_STORE_READ_WRITE_TYPE: Schema = recorded_member!(
        "DescribeFeatureGroupResponse",
        "OnlineStoreReadWriteType",
        ShapeType::String,
        18
    );
    const M_ONLINE_STORE_REPLICAS: Schema = recorded_member!(
        "DescribeFeatureGroupResponse",
        "OnlineStoreReplicas",
        ShapeType::List,
        19
    );
    const M_ONLINE_STORE_TOTAL_ITEM_COUNT: Schema = recorded_member!(
        "DescribeFeatureGroupResponse",
        "OnlineStoreTotalItemCount",
        ShapeType::Long,
        20
    );
    /// The ELEMENT schema of the replica list, which the list write hands each entry — the
    /// `member` sub-schema the derive emits for a list-typed member, under that member's own
    /// shape id.
    const M_ONLINE_STORE_REPLICAS_MEMBER: Schema = recorded_member!(
        "DescribeFeatureGroupResponse$OnlineStoreReplicas",
        "member",
        ShapeType::String,
        0
    );

    /// The modelled response with no recorded member reported yet; the reading operation
    /// fills the ones the group it read has.
    #[must_use]
    pub fn new(modelled: DescribeFeatureGroupOutput) -> Self {
        Self {
            modelled,
            online_store_read_write_type: None,
            online_store_replicas: None,
            online_store_total_item_count: None,
        }
    }
}

impl SerializableStruct for DescribeFeatureGroupRecordedOutput {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        self.modelled.serialize_members(s)?;
        if let Some(mode) = self.online_store_read_write_type.as_ref() {
            s.write_string(&Self::M_ONLINE_STORE_READ_WRITE_TYPE, mode.as_str())?;
        }
        if let Some(replicas) = self.online_store_replicas.as_ref() {
            s.write_list(&Self::M_ONLINE_STORE_REPLICAS, &|element| {
                for replica in replicas {
                    element.write_string(&Self::M_ONLINE_STORE_REPLICAS_MEMBER, replica.as_str())?;
                }
                Ok(())
            })?;
        }
        if let Some(count) = self.online_store_total_item_count {
            s.write_long(&Self::M_ONLINE_STORE_TOTAL_ITEM_COUNT, count)?;
        }
        Ok(())
    }
}

/// One input channel of a training job as a READ reports it: the modelled channel, plus the
/// member the recorded service carries beside it.
#[derive(Debug, Clone)]
pub struct RecordedChannel {
    /// Every member `Channel` declares.
    pub modelled: Channel,
    /// Whether the channel's data is read in FAST FILE mode — streamed from Amazon S3 with
    /// file system access to it — rather than downloaded or piped.
    pub enable_ffm: Option<bool>,
}

impl RecordedChannel {
    const M_ENABLE_FFM: Schema =
        recorded_member!("Channel", "EnableFFM", ShapeType::Boolean, 7);

    /// The modelled channel with no recorded member reported yet.
    #[must_use]
    pub fn new(modelled: Channel) -> Self {
        Self {
            modelled,
            enable_ffm: None,
        }
    }
}

impl SerializableStruct for RecordedChannel {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        self.modelled.serialize_members(s)?;
        if let Some(enabled) = self.enable_ffm {
            s.write_boolean(&Self::M_ENABLE_FFM, enabled)?;
        }
        Ok(())
    }
}

/// A training job's `OutputDataConfig` as a READ reports it: the modelled configuration,
/// plus the two members the recorded service carries beside it.
#[derive(Debug, Clone)]
pub struct RecordedOutputDataConfig {
    /// Every member `OutputDataConfig` declares.
    pub modelled: OutputDataConfig,
    /// Whether the job's model artifacts are NOT uploaded to the output location.
    pub disable_model_upload: Option<bool>,
    /// Whether the job's name is left OUT of the S3 path its artifacts are written under.
    pub remove_job_name_from_s3_output_path: Option<bool>,
}

impl RecordedOutputDataConfig {
    const M_DISABLE_MODEL_UPLOAD: Schema =
        recorded_member!("OutputDataConfig", "DisableModelUpload", ShapeType::Boolean, 3);
    const M_REMOVE_JOB_NAME_FROM_S3_OUTPUT_PATH: Schema = recorded_member!(
        "OutputDataConfig",
        "RemoveJobNameFromS3OutputPath",
        ShapeType::Boolean,
        4
    );

    /// The modelled configuration with no recorded member reported yet.
    #[must_use]
    pub fn new(modelled: OutputDataConfig) -> Self {
        Self {
            modelled,
            disable_model_upload: None,
            remove_job_name_from_s3_output_path: None,
        }
    }
}

impl SerializableStruct for RecordedOutputDataConfig {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        self.modelled.serialize_members(s)?;
        if let Some(disabled) = self.disable_model_upload {
            s.write_boolean(&Self::M_DISABLE_MODEL_UPLOAD, disabled)?;
        }
        if let Some(removed) = self.remove_job_name_from_s3_output_path {
            s.write_boolean(&Self::M_REMOVE_JOB_NAME_FROM_S3_OUTPUT_PATH, removed)?;
        }
        Ok(())
    }
}

/// A training job's `ResourceConfig` as a READ reports it: the modelled configuration, plus
/// the member the recorded service carries beside it.
#[derive(Debug, Clone)]
pub struct RecordedResourceConfig {
    /// Every member `ResourceConfig` declares.
    pub modelled: ResourceConfig,
    /// Whether the job runs on capacity reserved ahead of it rather than on-demand.
    pub use_reserved_capacity: Option<bool>,
}

impl RecordedResourceConfig {
    const M_USE_RESERVED_CAPACITY: Schema =
        recorded_member!("ResourceConfig", "UseReservedCapacity", ShapeType::Boolean, 11);

    /// The modelled configuration with no recorded member reported yet.
    #[must_use]
    pub fn new(modelled: ResourceConfig) -> Self {
        Self {
            modelled,
            use_reserved_capacity: None,
        }
    }
}

impl SerializableStruct for RecordedResourceConfig {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        self.modelled.serialize_members(s)?;
        if let Some(reserved) = self.use_reserved_capacity {
            s.write_boolean(&Self::M_USE_RESERVED_CAPACITY, reserved)?;
        }
        Ok(())
    }
}

/// What a training job's run LEFT BEHIND in Amazon S3, as the recorded service reports it
/// beside the modelled `ModelArtifacts`: the object the run wrote under the job's output
/// location.
#[derive(Debug, Clone)]
pub struct RecordedTrainingJobOutput {
    /// The URI of the object the run wrote under `OutputDataConfig.S3OutputPath`.
    pub s3_training_job_output: Option<String>,
}

impl RecordedTrainingJobOutput {
    const M_S3_TRAINING_JOB_OUTPUT: Schema =
        recorded_member!("TrainingJobOutput", "S3TrainingJobOutput", ShapeType::String, 0);

    /// The output of a job whose run wrote `uri`.
    #[must_use]
    pub fn new(uri: String) -> Self {
        Self {
            s3_training_job_output: Some(uri),
        }
    }
}

impl SerializableStruct for RecordedTrainingJobOutput {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(uri) = self.s3_training_job_output.as_ref() {
            s.write_string(&Self::M_S3_TRAINING_JOB_OUTPUT, uri.as_str())?;
        }
        Ok(())
    }
}

/// `DescribeTrainingJob`'s answer: the modelled response, plus the members the recorded
/// service carries — the job's own `DisableEFA`, and the channels and configurations whose
/// recorded members live inside them.
///
/// The channel list and the two configurations are MOVED out of the modelled response (see
/// [`Self::new`]), so each is written exactly once: the modelled delegation below cannot
/// write a member the modelled response no longer holds.
#[derive(Debug, Clone)]
pub struct DescribeTrainingJobRecordedOutput {
    /// Every member `DescribeTrainingJobResponse` declares, except the channel list and the
    /// two configurations reported below.
    pub modelled: DescribeTrainingJobOutput,
    /// "An array of Channel objects that describes each data input channel", with the
    /// recorded member of each channel.
    pub input_data_config: Option<Vec<RecordedChannel>>,
    /// "The S3 path where model artifacts … are stored", with its recorded members.
    pub output_data_config: Option<RecordedOutputDataConfig>,
    /// The ML compute the job is configured for, with its recorded member.
    pub resource_config: Option<RecordedResourceConfig>,
    /// Whether the Elastic Fabric Adapter is switched off for the job's compute.
    pub disable_efa: Option<bool>,
    /// What the job's run left behind in Amazon S3.
    pub training_job_output: Option<RecordedTrainingJobOutput>,
}

impl DescribeTrainingJobRecordedOutput {
    /// The three modelled members whose recorded members are written by the wrappers above,
    /// at the indices the model gives them in `DescribeTrainingJobResponse`.
    const M_INPUT_DATA_CONFIG: Schema = recorded_member!(
        "DescribeTrainingJobResponse",
        "InputDataConfig",
        ShapeType::List,
        12
    );
    const M_OUTPUT_DATA_CONFIG: Schema = recorded_member!(
        "DescribeTrainingJobResponse",
        "OutputDataConfig",
        ShapeType::Structure,
        13
    );
    const M_RESOURCE_CONFIG: Schema = recorded_member!(
        "DescribeTrainingJobResponse",
        "ResourceConfig",
        ShapeType::Structure,
        14
    );
    /// The one member of the response itself the model does not declare, appended after the
    /// modelled ones (0..=49).
    const M_DISABLE_EFA: Schema =
        recorded_member!("DescribeTrainingJobResponse", "DisableEFA", ShapeType::Boolean, 50);
    const M_TRAINING_JOB_OUTPUT: Schema = recorded_member!(
        "DescribeTrainingJobResponse",
        "TrainingJobOutput",
        ShapeType::Structure,
        51
    );

    /// Take the channel list and the output and resource configurations OUT of the modelled
    /// response, so each is reported through its own wrapper and never twice.
    #[must_use]
    pub fn new(mut modelled: DescribeTrainingJobOutput) -> Self {
        let input_data_config = modelled.input_data_config.take().map(|channels| {
            channels.into_iter().map(RecordedChannel::new).collect()
        });
        let output_data_config = modelled
            .output_data_config
            .take()
            .map(RecordedOutputDataConfig::new);
        let resource_config = modelled
            .resource_config
            .take()
            .map(RecordedResourceConfig::new);
        Self {
            modelled,
            input_data_config,
            output_data_config,
            resource_config,
            disable_efa: None,
            training_job_output: None,
        }
    }
}

impl SerializableStruct for DescribeTrainingJobRecordedOutput {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        self.modelled.serialize_members(s)?;
        if let Some(channels) = self.input_data_config.as_ref() {
            s.write_list(&Self::M_INPUT_DATA_CONFIG, &|element| {
                for channel in channels {
                    element.write_struct(&Channel::SCHEMA, channel)?;
                }
                Ok(())
            })?;
        }
        if let Some(config) = self.output_data_config.as_ref() {
            s.write_struct(&Self::M_OUTPUT_DATA_CONFIG, config)?;
        }
        if let Some(config) = self.resource_config.as_ref() {
            s.write_struct(&Self::M_RESOURCE_CONFIG, config)?;
        }
        if let Some(disabled) = self.disable_efa {
            s.write_boolean(&Self::M_DISABLE_EFA, disabled)?;
        }
        if let Some(output) = self.training_job_output.as_ref() {
            s.write_struct(&Self::M_TRAINING_JOB_OUTPUT, output)?;
        }
        Ok(())
    }
}
