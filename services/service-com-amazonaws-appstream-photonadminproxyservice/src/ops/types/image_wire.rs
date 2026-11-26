// SPDX-License-Identifier: CC0-1.0

//! The `Image` every reader of an image answers with, and the seven members of it the
//! on-disk model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list, and
//! it admits EXACTLY that list: a Rust field with no member behind it is a compile error,
//! so a member the service really reports but the model omits cannot be declared through
//! the derive at all. An image reports, beside the members the model declares, the instance
//! family it runs on, the instance type groups it serves, whether its licence is brought by
//! the account, the configuration of its root volume, whether its application catalog is
//! dynamic, and the ARN and the release instant of the published recipe it was built from —
//! seven members the shape's modelled member list does not mention — so this module emits for `Image` the same items the
//! derive emits: one member schema per member (index = its position in the member list),
//! the struct `SCHEMA` naming them in that order, the `IMAGE_SCHEMA` static and the
//! `SerializableStruct`/`DeserializableStruct` pair. It is the same arrangement
//! [`super::stack_wire`] and [`super::theme_wire`] make for their own unmodelled members.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape", shape="Image")` lists them in); the members it does not
//! declare are appended LAST, with their shape ids in a namespace of their own
//! ([`RECORDED_NAMESPACE`]), so they can neither claim to be modelled nor collide with
//! members the model may declare later — a model refresh only has to move them into place
//! and put the derive back. The ids never leave the process: the codec writes a member's
//! NAME as the key.
//!
//! The root volume's own structure is written out here for the same reason: the model
//! declares `VolumeConfig$VolumeSizeInGb` and nothing else, while the volume an image
//! reports also states the volume TYPE.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and
//! this structure is output-only), the `@http` chain (baked on only for an operation's
//! input) and `@jsonName` (no member here declares one: each one's wire name IS its member
//! name).
//!
//! Nothing here decides a VALUE: every member is written from what the stored image holds
//! (`crate::state::ImageRecord::to_image`).

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_schema_server::DeserializableStruct;
use ::aws_smithy_types::DateTime;

use super::{
    Application, ImagePermissions, ImageSoftwareMetadata, ImageStateChangeReason, ResourceError,
};

/// The namespace the members the on-disk model does not declare sit in: NOT this service's
/// own, so a hand-written member can never be mistaken for a modelled one.
const RECORDED_NAMESPACE: &str = "com.amazonaws.appstream.recorded";

/// The configuration of an image's root volume, as an image reports it.
///
/// The size is `com.amazonaws.appstream#VolumeConfig`'s one modelled member; the volume
/// type is the member the on-disk model does not declare.
#[derive(Debug, Clone, Default)]
pub struct ImageVolumeConfig {
    pub volume_size_in_gb: Option<i32>,
    pub volume_type: Option<String>,
}

impl ImageVolumeConfig {
    pub const M_VOLUME_SIZE_IN_GB: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.appstream#VolumeConfig$VolumeSizeInGb",
            "com.amazonaws.appstream",
            "VolumeSizeInGb",
        ),
        ShapeType::Integer,
        "VolumeSizeInGb",
        0,
    );

    /// The member the model does not declare, appended last and in [`RECORDED_NAMESPACE`].
    pub const M_VOLUME_TYPE: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.appstream.recorded#VolumeConfig$VolumeType",
            RECORDED_NAMESPACE,
            "VolumeType",
        ),
        ShapeType::String,
        "VolumeType",
        1,
    );

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.appstream#VolumeConfig",
            "com.amazonaws.appstream",
            "VolumeConfig",
        ),
        ShapeType::Structure,
        &[&Self::M_VOLUME_SIZE_IN_GB, &Self::M_VOLUME_TYPE],
    );
}

impl SerializableStruct for ImageVolumeConfig {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(value) = self.volume_size_in_gb {
            s.write_integer(&Self::M_VOLUME_SIZE_IN_GB, value)?;
        }
        if let Some(value) = self.volume_type.as_ref() {
            s.write_string(&Self::M_VOLUME_TYPE, value.as_str())?;
        }
        Ok(())
    }
}

impl DeserializableStruct for ImageVolumeConfig {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut volume = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => {
                    volume.volume_size_in_gb = Some(d.read_integer(&Self::M_VOLUME_SIZE_IN_GB)?)
                }
                Some(1) => volume.volume_type = Some(d.read_string(&Self::M_VOLUME_TYPE)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(volume)
    }
}

/// Describes an image, as every operation that reports one answers it.
///
/// The first twenty-four members are `com.amazonaws.appstream#Image`'s, in the model's
/// member order. The seven after them are the ones AppStream reports beyond the published
/// model.
#[derive(Debug, Clone, Default)]
pub struct Image {
    pub name: String,
    pub arn: Option<String>,
    pub base_image_arn: Option<String>,
    pub display_name: Option<String>,
    pub state: Option<String>,
    pub visibility: Option<String>,
    pub image_builder_supported: Option<bool>,
    pub image_builder_name: Option<String>,
    pub platform: Option<String>,
    pub description: Option<String>,
    pub state_change_reason: Option<ImageStateChangeReason>,
    pub applications: Option<Vec<Application>>,
    pub created_time: Option<DateTime>,
    pub public_base_image_released_date: Option<DateTime>,
    pub appstream_agent_version: Option<String>,
    pub image_permissions: Option<ImagePermissions>,
    pub image_errors: Option<Vec<ResourceError>>,
    pub latest_appstream_agent_version: Option<String>,
    pub supported_instance_families: Option<Vec<String>>,
    pub dynamic_app_providers_enabled: Option<String>,
    pub image_shared_with_others: Option<String>,
    pub managed_software_included: Option<bool>,
    pub image_type: Option<String>,
    pub image_software_metadata: Option<ImageSoftwareMetadata>,
    /// The instance family the image runs on, the first of the members the on-disk model
    /// does not declare.
    pub instance_family: Option<String>,
    /// Whether the account brings the image's operating-system licence.
    pub byol: Option<bool>,
    /// The configuration of the image's root volume.
    pub root_volume_config: Option<ImageVolumeConfig>,
    /// Whether the image serves a dynamic application catalog.
    pub dynamic_application_catalog_enabled: Option<bool>,
    /// The ARN of the published recipe the image was built from.
    pub appstream_recipe_arn: Option<String>,
    /// When that recipe was released.
    pub appstream_recipe_release_timestamp: Option<DateTime>,
    /// The instance type groups the image serves, reported as a list that is PRESENT and
    /// empty when the image states none.
    pub supported_instance_type_groups: Option<Vec<String>>,
}

/// One member this service's model DOES declare: its shape id in the service's own
/// namespace, its shape type, its wire name and its index in the member list — the member
/// schema the derive would emit for it.
macro_rules! modelled_member {
    ($wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!("com.amazonaws.appstream#Image$", $wire),
                "com.amazonaws.appstream",
                $wire,
            ),
            $shape_type,
            $wire,
            $index,
        )
    };
}

/// One member the model does NOT declare: the same schema, in [`RECORDED_NAMESPACE`].
macro_rules! recorded_member {
    ($wire:literal, $shape_type:expr, $index:literal) => {
        Schema::new_member(
            ShapeId::from_static(
                concat!("com.amazonaws.appstream.recorded#Image$", $wire),
                RECORDED_NAMESPACE,
                $wire,
            ),
            $shape_type,
            $wire,
            $index,
        )
    };
}

impl Image {
    pub const M_NAME: Schema = modelled_member!("Name", ShapeType::String, 0);
    pub const M_ARN: Schema = modelled_member!("Arn", ShapeType::String, 1);
    pub const M_BASE_IMAGE_ARN: Schema = modelled_member!("BaseImageArn", ShapeType::String, 2);
    pub const M_DISPLAY_NAME: Schema = modelled_member!("DisplayName", ShapeType::String, 3);
    pub const M_STATE: Schema = modelled_member!("State", ShapeType::String, 4);
    pub const M_VISIBILITY: Schema = modelled_member!("Visibility", ShapeType::String, 5);
    pub const M_IMAGE_BUILDER_SUPPORTED: Schema =
        modelled_member!("ImageBuilderSupported", ShapeType::Boolean, 6);
    pub const M_IMAGE_BUILDER_NAME: Schema =
        modelled_member!("ImageBuilderName", ShapeType::String, 7);
    pub const M_PLATFORM: Schema = modelled_member!("Platform", ShapeType::String, 8);
    pub const M_DESCRIPTION: Schema = modelled_member!("Description", ShapeType::String, 9);
    pub const M_STATE_CHANGE_REASON: Schema =
        modelled_member!("StateChangeReason", ShapeType::Structure, 10);
    pub const M_APPLICATIONS: Schema = modelled_member!("Applications", ShapeType::List, 11);
    pub const M_CREATED_TIME: Schema = modelled_member!("CreatedTime", ShapeType::Timestamp, 12);
    pub const M_PUBLIC_BASE_IMAGE_RELEASED_DATE: Schema =
        modelled_member!("PublicBaseImageReleasedDate", ShapeType::Timestamp, 13);
    pub const M_APPSTREAM_AGENT_VERSION: Schema =
        modelled_member!("AppstreamAgentVersion", ShapeType::String, 14);
    pub const M_IMAGE_PERMISSIONS: Schema =
        modelled_member!("ImagePermissions", ShapeType::Structure, 15);
    pub const M_IMAGE_ERRORS: Schema = modelled_member!("ImageErrors", ShapeType::List, 16);
    pub const M_LATEST_APPSTREAM_AGENT_VERSION: Schema =
        modelled_member!("LatestAppstreamAgentVersion", ShapeType::String, 17);
    pub const M_SUPPORTED_INSTANCE_FAMILIES: Schema =
        modelled_member!("SupportedInstanceFamilies", ShapeType::List, 18);
    pub const M_DYNAMIC_APP_PROVIDERS_ENABLED: Schema =
        modelled_member!("DynamicAppProvidersEnabled", ShapeType::String, 19);
    pub const M_IMAGE_SHARED_WITH_OTHERS: Schema =
        modelled_member!("ImageSharedWithOthers", ShapeType::String, 20);
    pub const M_MANAGED_SOFTWARE_INCLUDED: Schema =
        modelled_member!("ManagedSoftwareIncluded", ShapeType::Boolean, 21);
    pub const M_IMAGE_TYPE: Schema = modelled_member!("ImageType", ShapeType::String, 22);
    pub const M_IMAGE_SOFTWARE_METADATA: Schema =
        modelled_member!("ImageSoftwareMetadata", ShapeType::Structure, 23);

    pub const M_INSTANCE_FAMILY: Schema = recorded_member!("InstanceFamily", ShapeType::String, 24);
    pub const M_BYOL: Schema = recorded_member!("Byol", ShapeType::Boolean, 25);
    pub const M_ROOT_VOLUME_CONFIG: Schema =
        recorded_member!("RootVolumeConfig", ShapeType::Structure, 26);
    pub const M_DYNAMIC_APPLICATION_CATALOG_ENABLED: Schema =
        recorded_member!("DynamicApplicationCatalogEnabled", ShapeType::Boolean, 27);
    pub const M_APPSTREAM_RECIPE_ARN: Schema =
        recorded_member!("appStreamRecipeArn", ShapeType::String, 28);
    pub const M_APPSTREAM_RECIPE_RELEASE_TIMESTAMP: Schema =
        recorded_member!("appStreamRecipeReleaseTimestamp", ShapeType::Timestamp, 29);
    pub const M_SUPPORTED_INSTANCE_TYPE_GROUPS: Schema =
        recorded_member!("SupportedInstanceTypeGroups", ShapeType::List, 30);

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.appstream#Image",
            "com.amazonaws.appstream",
            "Image",
        ),
        ShapeType::Structure,
        &[
            &Self::M_NAME,
            &Self::M_ARN,
            &Self::M_BASE_IMAGE_ARN,
            &Self::M_DISPLAY_NAME,
            &Self::M_STATE,
            &Self::M_VISIBILITY,
            &Self::M_IMAGE_BUILDER_SUPPORTED,
            &Self::M_IMAGE_BUILDER_NAME,
            &Self::M_PLATFORM,
            &Self::M_DESCRIPTION,
            &Self::M_STATE_CHANGE_REASON,
            &Self::M_APPLICATIONS,
            &Self::M_CREATED_TIME,
            &Self::M_PUBLIC_BASE_IMAGE_RELEASED_DATE,
            &Self::M_APPSTREAM_AGENT_VERSION,
            &Self::M_IMAGE_PERMISSIONS,
            &Self::M_IMAGE_ERRORS,
            &Self::M_LATEST_APPSTREAM_AGENT_VERSION,
            &Self::M_SUPPORTED_INSTANCE_FAMILIES,
            &Self::M_DYNAMIC_APP_PROVIDERS_ENABLED,
            &Self::M_IMAGE_SHARED_WITH_OTHERS,
            &Self::M_MANAGED_SOFTWARE_INCLUDED,
            &Self::M_IMAGE_TYPE,
            &Self::M_IMAGE_SOFTWARE_METADATA,
            &Self::M_INSTANCE_FAMILY,
            &Self::M_BYOL,
            &Self::M_ROOT_VOLUME_CONFIG,
            &Self::M_DYNAMIC_APPLICATION_CATALOG_ENABLED,
            &Self::M_APPSTREAM_RECIPE_ARN,
            &Self::M_APPSTREAM_RECIPE_RELEASE_TIMESTAMP,
            &Self::M_SUPPORTED_INSTANCE_TYPE_GROUPS,
        ],
    );
}

pub static IMAGE_SCHEMA: &Schema = &Image::SCHEMA;

impl SerializableStruct for Image {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&Self::M_NAME, self.name.as_str())?;
        if let Some(value) = self.arn.as_ref() {
            s.write_string(&Self::M_ARN, value.as_str())?;
        }
        if let Some(value) = self.base_image_arn.as_ref() {
            s.write_string(&Self::M_BASE_IMAGE_ARN, value.as_str())?;
        }
        if let Some(value) = self.display_name.as_ref() {
            s.write_string(&Self::M_DISPLAY_NAME, value.as_str())?;
        }
        if let Some(value) = self.state.as_ref() {
            s.write_string(&Self::M_STATE, value.as_str())?;
        }
        if let Some(value) = self.visibility.as_ref() {
            s.write_string(&Self::M_VISIBILITY, value.as_str())?;
        }
        if let Some(value) = self.image_builder_supported {
            s.write_boolean(&Self::M_IMAGE_BUILDER_SUPPORTED, value)?;
        }
        if let Some(value) = self.image_builder_name.as_ref() {
            s.write_string(&Self::M_IMAGE_BUILDER_NAME, value.as_str())?;
        }
        if let Some(value) = self.platform.as_ref() {
            s.write_string(&Self::M_PLATFORM, value.as_str())?;
        }
        if let Some(value) = self.description.as_ref() {
            s.write_string(&Self::M_DESCRIPTION, value.as_str())?;
        }
        if let Some(value) = self.state_change_reason.as_ref() {
            s.write_struct(&Self::M_STATE_CHANGE_REASON, value)?;
        }
        if let Some(value) = self.applications.as_ref() {
            s.write_list(&Self::M_APPLICATIONS, &|element| {
                for application in value.iter() {
                    element.write_struct(&Application::SCHEMA, application)?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.created_time.as_ref() {
            s.write_timestamp(&Self::M_CREATED_TIME, value)?;
        }
        if let Some(value) = self.public_base_image_released_date.as_ref() {
            s.write_timestamp(&Self::M_PUBLIC_BASE_IMAGE_RELEASED_DATE, value)?;
        }
        if let Some(value) = self.appstream_agent_version.as_ref() {
            s.write_string(&Self::M_APPSTREAM_AGENT_VERSION, value.as_str())?;
        }
        if let Some(value) = self.image_permissions.as_ref() {
            s.write_struct(&Self::M_IMAGE_PERMISSIONS, value)?;
        }
        if let Some(value) = self.image_errors.as_ref() {
            s.write_list(&Self::M_IMAGE_ERRORS, &|element| {
                for error in value.iter() {
                    element.write_struct(&ResourceError::SCHEMA, error)?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.latest_appstream_agent_version.as_ref() {
            s.write_string(&Self::M_LATEST_APPSTREAM_AGENT_VERSION, value.as_str())?;
        }
        if let Some(value) = self.supported_instance_families.as_ref() {
            s.write_list(&Self::M_SUPPORTED_INSTANCE_FAMILIES, &|element| {
                for family in value.iter() {
                    element.write_string(&Self::M_SUPPORTED_INSTANCE_FAMILIES, family.as_str())?;
                }
                Ok(())
            })?;
        }
        if let Some(value) = self.dynamic_app_providers_enabled.as_ref() {
            s.write_string(&Self::M_DYNAMIC_APP_PROVIDERS_ENABLED, value.as_str())?;
        }
        if let Some(value) = self.image_shared_with_others.as_ref() {
            s.write_string(&Self::M_IMAGE_SHARED_WITH_OTHERS, value.as_str())?;
        }
        if let Some(value) = self.managed_software_included {
            s.write_boolean(&Self::M_MANAGED_SOFTWARE_INCLUDED, value)?;
        }
        if let Some(value) = self.image_type.as_ref() {
            s.write_string(&Self::M_IMAGE_TYPE, value.as_str())?;
        }
        if let Some(value) = self.image_software_metadata.as_ref() {
            s.write_struct(&Self::M_IMAGE_SOFTWARE_METADATA, value)?;
        }
        if let Some(value) = self.instance_family.as_ref() {
            s.write_string(&Self::M_INSTANCE_FAMILY, value.as_str())?;
        }
        if let Some(value) = self.byol {
            s.write_boolean(&Self::M_BYOL, value)?;
        }
        if let Some(value) = self.root_volume_config.as_ref() {
            s.write_struct(&Self::M_ROOT_VOLUME_CONFIG, value)?;
        }
        if let Some(value) = self.dynamic_application_catalog_enabled {
            s.write_boolean(&Self::M_DYNAMIC_APPLICATION_CATALOG_ENABLED, value)?;
        }
        if let Some(value) = self.appstream_recipe_arn.as_ref() {
            s.write_string(&Self::M_APPSTREAM_RECIPE_ARN, value.as_str())?;
        }
        if let Some(value) = self.appstream_recipe_release_timestamp.as_ref() {
            s.write_timestamp(&Self::M_APPSTREAM_RECIPE_RELEASE_TIMESTAMP, value)?;
        }
        if let Some(value) = self.supported_instance_type_groups.as_ref() {
            s.write_list(&Self::M_SUPPORTED_INSTANCE_TYPE_GROUPS, &|element| {
                for group in value.iter() {
                    element.write_string(&Self::M_SUPPORTED_INSTANCE_TYPE_GROUPS, group.as_str())?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

impl DeserializableStruct for Image {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut image = Self::default();
        let mut name: Option<String> = None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => name = Some(d.read_string(&Self::M_NAME)?),
                Some(1) => image.arn = Some(d.read_string(&Self::M_ARN)?),
                Some(2) => image.base_image_arn = Some(d.read_string(&Self::M_BASE_IMAGE_ARN)?),
                Some(3) => image.display_name = Some(d.read_string(&Self::M_DISPLAY_NAME)?),
                Some(4) => image.state = Some(d.read_string(&Self::M_STATE)?),
                Some(5) => image.visibility = Some(d.read_string(&Self::M_VISIBILITY)?),
                Some(6) => {
                    image.image_builder_supported =
                        Some(d.read_boolean(&Self::M_IMAGE_BUILDER_SUPPORTED)?)
                }
                Some(7) => {
                    image.image_builder_name = Some(d.read_string(&Self::M_IMAGE_BUILDER_NAME)?)
                }
                Some(8) => image.platform = Some(d.read_string(&Self::M_PLATFORM)?),
                Some(9) => image.description = Some(d.read_string(&Self::M_DESCRIPTION)?),
                Some(10) => {
                    image.state_change_reason = Some(ImageStateChangeReason::deserialize(d)?)
                }
                Some(11) => {
                    let mut applications: Vec<Application> = Vec::new();
                    d.read_list(&Self::M_APPLICATIONS, &mut |element| {
                        applications.push(Application::deserialize(element)?);
                        Ok(())
                    })?;
                    image.applications = Some(applications);
                }
                Some(12) => image.created_time = Some(d.read_timestamp(&Self::M_CREATED_TIME)?),
                Some(13) => {
                    image.public_base_image_released_date =
                        Some(d.read_timestamp(&Self::M_PUBLIC_BASE_IMAGE_RELEASED_DATE)?)
                }
                Some(14) => {
                    image.appstream_agent_version =
                        Some(d.read_string(&Self::M_APPSTREAM_AGENT_VERSION)?)
                }
                Some(15) => image.image_permissions = Some(ImagePermissions::deserialize(d)?),
                Some(16) => {
                    let mut errors: Vec<ResourceError> = Vec::new();
                    d.read_list(&Self::M_IMAGE_ERRORS, &mut |element| {
                        errors.push(ResourceError::deserialize(element)?);
                        Ok(())
                    })?;
                    image.image_errors = Some(errors);
                }
                Some(17) => {
                    image.latest_appstream_agent_version =
                        Some(d.read_string(&Self::M_LATEST_APPSTREAM_AGENT_VERSION)?)
                }
                Some(18) => {
                    let mut families: Vec<String> = Vec::new();
                    d.read_list(&Self::M_SUPPORTED_INSTANCE_FAMILIES, &mut |element| {
                        families.push(element.read_string(&Self::M_SUPPORTED_INSTANCE_FAMILIES)?);
                        Ok(())
                    })?;
                    image.supported_instance_families = Some(families);
                }
                Some(19) => {
                    image.dynamic_app_providers_enabled =
                        Some(d.read_string(&Self::M_DYNAMIC_APP_PROVIDERS_ENABLED)?)
                }
                Some(20) => {
                    image.image_shared_with_others =
                        Some(d.read_string(&Self::M_IMAGE_SHARED_WITH_OTHERS)?)
                }
                Some(21) => {
                    image.managed_software_included =
                        Some(d.read_boolean(&Self::M_MANAGED_SOFTWARE_INCLUDED)?)
                }
                Some(22) => image.image_type = Some(d.read_string(&Self::M_IMAGE_TYPE)?),
                Some(23) => {
                    image.image_software_metadata = Some(ImageSoftwareMetadata::deserialize(d)?)
                }
                Some(24) => image.instance_family = Some(d.read_string(&Self::M_INSTANCE_FAMILY)?),
                Some(25) => image.byol = Some(d.read_boolean(&Self::M_BYOL)?),
                Some(26) => image.root_volume_config = Some(ImageVolumeConfig::deserialize(d)?),
                Some(27) => {
                    image.dynamic_application_catalog_enabled =
                        Some(d.read_boolean(&Self::M_DYNAMIC_APPLICATION_CATALOG_ENABLED)?)
                }
                Some(28) => {
                    image.appstream_recipe_arn = Some(d.read_string(&Self::M_APPSTREAM_RECIPE_ARN)?)
                }
                Some(29) => {
                    image.appstream_recipe_release_timestamp =
                        Some(d.read_timestamp(&Self::M_APPSTREAM_RECIPE_RELEASE_TIMESTAMP)?)
                }
                Some(30) => {
                    let mut groups: Vec<String> = Vec::new();
                    d.read_list(&Self::M_SUPPORTED_INSTANCE_TYPE_GROUPS, &mut |element| {
                        groups.push(element.read_string(&Self::M_SUPPORTED_INSTANCE_TYPE_GROUPS)?);
                        Ok(())
                    })?;
                    image.supported_instance_type_groups = Some(groups);
                }
                _ => {}
            }
            Ok(())
        })?;
        image.name = name.ok_or_else(|| ::aws_smithy_schema_server::missing_member("Name"))?;
        Ok(image)
    }
}
