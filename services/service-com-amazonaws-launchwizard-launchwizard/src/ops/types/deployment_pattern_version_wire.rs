// SPDX-License-Identifier: CC0-1.0

//! The deployment-pattern-VERSION summary every version view answers with, and the
//! capability block it carries that the on-disk model does not declare.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list,
//! and it admits EXACTLY that list: a Rust field with no member behind it is a compile
//! error, so a member the service really reports but the model omits cannot be declared
//! through the derive at all. A deployment pattern version reports, beside its name and
//! description, WHICH OPTIONAL DEPLOYMENT CAPABILITIES it supports — the block a client
//! gates on before it offers to read a deployment's CloudFormation output, to modify an
//! existing deployment, to run post-deployment steps, or to move a deployment onto this
//! version — and
//! `smithy_query(query="shape",shape="DeploymentPatternVersionDataSummary")` lists five
//! members, none of them that block. So this module emits for
//! [`DeploymentPatternVersionDataSummary`] the same items the derive emits: one
//! `M_<MEMBER>` member schema per member (index = its position in the member list), the
//! struct `SCHEMA` naming them in that order, the `<SHAPE>_SCHEMA` static and the
//! `SerializableStruct`/`DeserializableStruct` pair.
//!
//! The model's own members keep the names, the indices and the order the model gives
//! them; the member it does not declare is appended LAST, and its own shape id — and the
//! ids of the structures it targets, which the model does not declare at all — sit in a
//! namespace of their own ([`RECORDED_NAMESPACE`]), so none of them can claim to be
//! modelled or collide with a shape a later model refresh may declare: that refresh then
//! only has to move the member into place and put the derive back. The ids never leave
//! the process — the codec writes a member's NAME as the key.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be
//! inert here: the constraint traits (the validating walk runs on operation INPUTS only,
//! and no operation in this model takes a version summary as input) and the `@http`
//! chain (baked on only for an operation's input). No member below declares a
//! `@jsonName`: each one's wire name IS its member name.
//!
//! Nothing here decides a VALUE: the capability block is projected from the catalogue
//! record the version belongs to
//! (`crate::state::pattern_version_capabilities::feature_flags`), which is also where
//! the rule that decides each capability's status is stated.

/// The namespace the ids of the shapes this service reports and the model does not
/// declare sit in: NOT this service's own, so nothing written here can be mistaken for a
/// modelled shape.
const RECORDED_NAMESPACE: &str = "com.amazonaws.launchwizard.recorded";

/// The namespace of this service's own modelled shapes.
const SERVICE_NAMESPACE: &str = "com.amazonaws.launchwizard";

/// One member's schema, as the vendored codegen emits it: the member's shape id, its
/// shape type, its wire name and its index in the member list.
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

/// How a capability that is configurable is configured: the fields the capability reads
/// or writes.
///
/// A structure the wire carries inside a version's capability block and the on-disk model
/// does not declare at all, so its schema and codec are written out here (see this
/// module's note). `fields` is written even when it is empty, because an empty
/// configuration is what "this capability has no fields to configure" is said with — the
/// member's absence would read as "unknown" instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeploymentPatternVersionCapabilityConfiguration {
    /// The fields the capability is configured through, by name. Empty for a capability
    /// this version configures nothing for.
    pub fields: Vec<String>,
}

impl DeploymentPatternVersionCapabilityConfiguration {
    pub const M_FIELDS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionCapabilityConfiguration$fields",
        RECORDED_NAMESPACE,
        "fields",
        ::aws_smithy_schema::ShapeType::List,
        0,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionCapabilityConfiguration",
            RECORDED_NAMESPACE,
            "DeploymentPatternVersionCapabilityConfiguration",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[&Self::M_FIELDS],
    );
}

pub static DEPLOYMENT_PATTERN_VERSION_CAPABILITY_CONFIGURATION_SCHEMA:
    &::aws_smithy_schema::Schema = &DeploymentPatternVersionCapabilityConfiguration::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct
    for DeploymentPatternVersionCapabilityConfiguration
{
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        s.write_list(&Self::M_FIELDS, &|element| {
            for field in self.fields.iter() {
                element.write_string(&Self::M_FIELDS, field.as_str())?;
            }
            Ok(())
        })?;
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct
    for DeploymentPatternVersionCapabilityConfiguration
{
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut configuration = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            if member.member_index() == ::std::option::Option::Some(0) {
                let mut fields: ::std::vec::Vec<::std::string::String> = ::std::vec::Vec::new();
                d.read_list(&Self::M_FIELDS, &mut |element| {
                    fields.push(element.read_string(&Self::M_FIELDS)?);
                    Ok(())
                })?;
                configuration.fields = fields;
            }
            Ok(())
        })?;
        Ok(configuration)
    }
}

/// ONE optional deployment capability of a deployment pattern version: whether the
/// version supports it, and — where the capability is configurable — what it is
/// configured through.
///
/// A structure the wire carries inside a version's capability block and the on-disk model
/// does not declare at all, so its schema and codec are written out here (see this
/// module's note). `status` is written always: a capability a version does not support
/// says so with a status, never by leaving the member out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeploymentPatternVersionCapability {
    /// What the capability is configured through, for a capability that is configurable
    /// at all; `None` for one that carries no configuration.
    pub configuration: Option<DeploymentPatternVersionCapabilityConfiguration>,
    /// Whether this version supports the capability
    /// (`crate::state::pattern_version_capabilities::CAPABILITY_ENABLED` /
    /// `..._DISABLED`).
    pub status: String,
}

impl DeploymentPatternVersionCapability {
    pub const M_CONFIGURATION: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionCapability$configuration",
        RECORDED_NAMESPACE,
        "configuration",
        ::aws_smithy_schema::ShapeType::Structure,
        0,
    );
    pub const M_STATUS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionCapability$status",
        RECORDED_NAMESPACE,
        "status",
        ::aws_smithy_schema::ShapeType::String,
        1,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionCapability",
            RECORDED_NAMESPACE,
            "DeploymentPatternVersionCapability",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[&Self::M_CONFIGURATION, &Self::M_STATUS],
    );
}

pub static DEPLOYMENT_PATTERN_VERSION_CAPABILITY_SCHEMA: &::aws_smithy_schema::Schema =
    &DeploymentPatternVersionCapability::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeploymentPatternVersionCapability {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        if let ::std::option::Option::Some(configuration) = self.configuration.as_ref() {
            s.write_struct(&Self::M_CONFIGURATION, configuration)?;
        }
        s.write_string(&Self::M_STATUS, self.status.as_str())?;
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for DeploymentPatternVersionCapability {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut capability = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    capability.configuration = ::std::option::Option::Some(
                        <DeploymentPatternVersionCapabilityConfiguration
                            as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?,
                    );
                }
                ::std::option::Option::Some(1) => {
                    capability.status = d.read_string(member)?;
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(capability)
    }
}

/// The optional deployment capabilities ONE deployment pattern version supports: one
/// entry per capability, plus the versions an update can reach this version from.
///
/// A structure the wire carries on a version summary and the on-disk model does not
/// declare at all, so its schema and codec are written out here (see this module's note).
/// Every member is written always — a capability block states what a version supports,
/// and leaving an entry out would read as "unknown" rather than as "not supported".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeploymentPatternVersionFeatureFlags {
    /// Whether a deployment of this version reports the CloudFormation output of the
    /// stack it provisioned, and the fields that output is read through.
    pub cloud_formation_output: DeploymentPatternVersionCapability,
    /// Whether a deployment of this version can be modified after it was created.
    pub modify_deployments: DeploymentPatternVersionCapability,
    /// Whether a deployment of this version runs steps after the deployment itself
    /// finished.
    pub post_deployment_steps: DeploymentPatternVersionCapability,
    /// The versions an update can move a deployment onto this version FROM, newest
    /// first; empty when no version of the pattern can be updated to this one.
    pub update_from_version: Vec<String>,
}

impl DeploymentPatternVersionFeatureFlags {
    pub const M_CLOUD_FORMATION_OUTPUT: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionFeatureFlags$cloudFormationOutput",
        RECORDED_NAMESPACE,
        "cloudFormationOutput",
        ::aws_smithy_schema::ShapeType::Structure,
        0,
    );
    pub const M_MODIFY_DEPLOYMENTS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionFeatureFlags$modifyDeployments",
        RECORDED_NAMESPACE,
        "modifyDeployments",
        ::aws_smithy_schema::ShapeType::Structure,
        1,
    );
    pub const M_POST_DEPLOYMENT_STEPS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionFeatureFlags$postDeploymentSteps",
        RECORDED_NAMESPACE,
        "postDeploymentSteps",
        ::aws_smithy_schema::ShapeType::Structure,
        2,
    );
    pub const M_UPDATE_FROM_VERSION: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionFeatureFlags$updateFromVersion",
        RECORDED_NAMESPACE,
        "updateFromVersion",
        ::aws_smithy_schema::ShapeType::List,
        3,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionFeatureFlags",
            RECORDED_NAMESPACE,
            "DeploymentPatternVersionFeatureFlags",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_CLOUD_FORMATION_OUTPUT,
            &Self::M_MODIFY_DEPLOYMENTS,
            &Self::M_POST_DEPLOYMENT_STEPS,
            &Self::M_UPDATE_FROM_VERSION,
        ],
    );
}

pub static DEPLOYMENT_PATTERN_VERSION_FEATURE_FLAGS_SCHEMA: &::aws_smithy_schema::Schema =
    &DeploymentPatternVersionFeatureFlags::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeploymentPatternVersionFeatureFlags {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        s.write_struct(&Self::M_CLOUD_FORMATION_OUTPUT, &self.cloud_formation_output)?;
        s.write_struct(&Self::M_MODIFY_DEPLOYMENTS, &self.modify_deployments)?;
        s.write_struct(&Self::M_POST_DEPLOYMENT_STEPS, &self.post_deployment_steps)?;
        s.write_list(&Self::M_UPDATE_FROM_VERSION, &|element| {
            for version in self.update_from_version.iter() {
                element.write_string(&Self::M_UPDATE_FROM_VERSION, version.as_str())?;
            }
            Ok(())
        })?;
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for DeploymentPatternVersionFeatureFlags {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut flags = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    flags.cloud_formation_output =
                        <DeploymentPatternVersionCapability
                            as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?;
                }
                ::std::option::Option::Some(1) => {
                    flags.modify_deployments =
                        <DeploymentPatternVersionCapability
                            as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?;
                }
                ::std::option::Option::Some(2) => {
                    flags.post_deployment_steps =
                        <DeploymentPatternVersionCapability
                            as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?;
                }
                ::std::option::Option::Some(3) => {
                    let mut versions: ::std::vec::Vec<::std::string::String> =
                        ::std::vec::Vec::new();
                    d.read_list(&Self::M_UPDATE_FROM_VERSION, &mut |element| {
                        versions.push(element.read_string(&Self::M_UPDATE_FROM_VERSION)?);
                        Ok(())
                    })?;
                    flags.update_from_version = versions;
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(flags)
    }
}

/// Describes a deployment pattern version summary, as BOTH version views answer it
/// (`GetDeploymentPatternVersion` and the page `ListDeploymentPatternVersions` serves),
/// so neither can report a version the other cannot.
///
/// The first five members are `com.amazonaws.launchwizard#DeploymentPatternVersionDataSummary`'s,
/// in the model's member order. `featureFlags` is the one Launch Wizard reports beyond
/// the published model: the optional deployment capabilities the version supports.
#[derive(Debug, Clone, Default)]
pub struct DeploymentPatternVersionDataSummary {
    pub deployment_pattern_version_name: Option<String>,
    pub description: Option<String>,
    pub documentation_url: Option<String>,
    pub workload_name: Option<String>,
    pub deployment_pattern_name: Option<String>,
    /// The member the model does not declare, appended after the modelled ones: which
    /// optional deployment capabilities this version supports.
    pub feature_flags: Option<DeploymentPatternVersionFeatureFlags>,
}

impl DeploymentPatternVersionDataSummary {
    pub const M_DEPLOYMENT_PATTERN_VERSION_NAME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard#DeploymentPatternVersionDataSummary$deploymentPatternVersionName",
        SERVICE_NAMESPACE,
        "deploymentPatternVersionName",
        ::aws_smithy_schema::ShapeType::String,
        0,
    );
    pub const M_DESCRIPTION: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard#DeploymentPatternVersionDataSummary$description",
        SERVICE_NAMESPACE,
        "description",
        ::aws_smithy_schema::ShapeType::String,
        1,
    );
    pub const M_DOCUMENTATION_URL: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard#DeploymentPatternVersionDataSummary$documentationUrl",
        SERVICE_NAMESPACE,
        "documentationUrl",
        ::aws_smithy_schema::ShapeType::String,
        2,
    );
    pub const M_WORKLOAD_NAME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard#DeploymentPatternVersionDataSummary$workloadName",
        SERVICE_NAMESPACE,
        "workloadName",
        ::aws_smithy_schema::ShapeType::String,
        3,
    );
    pub const M_DEPLOYMENT_PATTERN_NAME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard#DeploymentPatternVersionDataSummary$deploymentPatternName",
        SERVICE_NAMESPACE,
        "deploymentPatternName",
        ::aws_smithy_schema::ShapeType::String,
        4,
    );
    /// The member the model does not declare, appended after the modelled ones.
    pub const M_FEATURE_FLAGS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.launchwizard.recorded#DeploymentPatternVersionDataSummary$featureFlags",
        RECORDED_NAMESPACE,
        "featureFlags",
        ::aws_smithy_schema::ShapeType::Structure,
        5,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.launchwizard#DeploymentPatternVersionDataSummary",
            SERVICE_NAMESPACE,
            "DeploymentPatternVersionDataSummary",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_DEPLOYMENT_PATTERN_VERSION_NAME,
            &Self::M_DESCRIPTION,
            &Self::M_DOCUMENTATION_URL,
            &Self::M_WORKLOAD_NAME,
            &Self::M_DEPLOYMENT_PATTERN_NAME,
            &Self::M_FEATURE_FLAGS,
        ],
    );
}

pub static DEPLOYMENT_PATTERN_VERSION_DATA_SUMMARY_SCHEMA: &::aws_smithy_schema::Schema =
    &DeploymentPatternVersionDataSummary::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DeploymentPatternVersionDataSummary {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        // Every member of this shape is optional, so each is written only when the
        // reported version carries it — the derive's own `is_option` split.
        if let ::std::option::Option::Some(name) = self.deployment_pattern_version_name.as_ref() {
            s.write_string(&Self::M_DEPLOYMENT_PATTERN_VERSION_NAME, name.as_str())?;
        }
        if let ::std::option::Option::Some(description) = self.description.as_ref() {
            s.write_string(&Self::M_DESCRIPTION, description.as_str())?;
        }
        if let ::std::option::Option::Some(url) = self.documentation_url.as_ref() {
            s.write_string(&Self::M_DOCUMENTATION_URL, url.as_str())?;
        }
        if let ::std::option::Option::Some(workload) = self.workload_name.as_ref() {
            s.write_string(&Self::M_WORKLOAD_NAME, workload.as_str())?;
        }
        if let ::std::option::Option::Some(pattern) = self.deployment_pattern_name.as_ref() {
            s.write_string(&Self::M_DEPLOYMENT_PATTERN_NAME, pattern.as_str())?;
        }
        if let ::std::option::Option::Some(flags) = self.feature_flags.as_ref() {
            s.write_struct(&Self::M_FEATURE_FLAGS, flags)?;
        }
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for DeploymentPatternVersionDataSummary {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut summary = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    summary.deployment_pattern_version_name =
                        ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(1) => {
                    summary.description = ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(2) => {
                    summary.documentation_url =
                        ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(3) => {
                    summary.workload_name = ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(4) => {
                    summary.deployment_pattern_name =
                        ::std::option::Option::Some(d.read_string(member)?);
                }
                ::std::option::Option::Some(5) => {
                    summary.feature_flags = ::std::option::Option::Some(
                        <DeploymentPatternVersionFeatureFlags
                            as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(d)?,
                    );
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(summary)
    }
}
