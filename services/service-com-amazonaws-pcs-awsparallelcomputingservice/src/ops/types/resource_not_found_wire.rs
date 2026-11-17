// SPDX-License-Identifier: CC0-1.0

//! The not-found error every lookup of this service refuses with, and the one member the
//! wire carries on it that the on-disk model does not declare: the ARN a request addressed
//! the resource by.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives an error
//! structure's `Schema`, its member list and its serializer from the model's members and
//! admits EXACTLY that list: a Rust field with no member behind it is a compile error, so a
//! member the service really reports but the on-disk model omits cannot be declared through
//! the derive at all. `ResourceNotFoundException` declares three members —
//! `smithy_query(query="shape", shape="ResourceNotFoundException")` lists `message`,
//! `resourceId` and `resourceType`, each `@required` — while a refusal of a request that
//! addressed its subject by ARN reports that ARN back. The items below are the ones the
//! derive emits for an error struct — one member schema per member (index = that member's
//! position in the member list), the `SCHEMA_<SHAPE>` static the `#[operation_error]` enums
//! read, `shape_name`, `Display`, `Error` and `SerializableStruct` — from a member list
//! this file gives, so the extra member can be appended to it.
//!
//! The model's own members keep the names, the order and the indices the model gives them,
//! and the member the model does not declare is appended LAST, with its shape id in a
//! namespace of its own, so it can neither claim to be modelled nor collide with a member
//! the model may declare later — and a later model refresh only has to move it into place
//! and put the derive back.
//!
//! WHAT IS DELIBERATELY ABSENT, for the same reason the derive's own output would be inert
//! here: the constraint traits (the validating walk runs on operation INPUTS only, and no
//! error shape appears in one), the `@http` chain (baked on only for an operation's input)
//! and a deserializer (an error of this service is written, never read). `message` carries
//! the XML element name the derive gives that member, and no member below declares a
//! `@jsonName`: each one's wire name IS its Smithy member name.

/// The namespace this service's own modelled shapes sit in.
const SERVICE_NAMESPACE: &str = "com.amazonaws.pcs";

/// The namespace the id of a member this service reports and the model does not declare
/// sits in, so it can be mistaken neither for a modelled member nor for one the model may
/// declare later.
const RECORDED_NAMESPACE: &str = "com.amazonaws.pcs.recorded";

/// One STRING member's schema, as the vendored codegen emits it: the member's shape id,
/// its wire name and its index in the member list. Every member of this shape targets
/// `String` (`smithy_query(query="shape", shape="ResourceNotFoundException")`), and so does
/// the one the model does not declare, so the shape type is fixed here rather than passed.
const fn string_member_schema(
    shape_id: &'static str,
    namespace: &'static str,
    name: &'static str,
    index: usize,
) -> ::aws_smithy_schema::Schema {
    ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(shape_id, namespace, name),
        ::aws_smithy_schema::ShapeType::String,
        name,
        index,
    )
}

/// The modelled `ResourceNotFoundException`, plus the ARN an ARN-addressed refusal reports.
///
/// The three modelled members are `@required`, so each is a bare `String` and each is
/// always written. The fourth is the member the model does not declare: it is `Option`, and
/// written only by a refusal that HAS an ARN to report — a request that addressed its
/// subject by one. See [`crate::state::not_found`] and [`crate::state::arn_not_found`] for
/// the two refusals and what each fills in.
#[derive(Debug, Clone, Default)]
pub struct ResourceNotFoundException {
    pub message: String,
    pub resource_id: String,
    pub resource_type: String,
    /// The member the model does not declare, appended after the modelled ones: the ARN the
    /// request addressed the resource by, exactly as it supplied it.
    pub resource_arn: Option<String>,
}

impl ResourceNotFoundException {
    pub const M_MESSAGE: ::aws_smithy_schema::Schema = string_member_schema(
        "com.amazonaws.pcs#ResourceNotFoundException$message",
        SERVICE_NAMESPACE,
        "message",
        0,
    )
    .with_xml_name("Message");
    pub const M_RESOURCE_ID: ::aws_smithy_schema::Schema = string_member_schema(
        "com.amazonaws.pcs#ResourceNotFoundException$resourceId",
        SERVICE_NAMESPACE,
        "resourceId",
        1,
    );
    pub const M_RESOURCE_TYPE: ::aws_smithy_schema::Schema = string_member_schema(
        "com.amazonaws.pcs#ResourceNotFoundException$resourceType",
        SERVICE_NAMESPACE,
        "resourceType",
        2,
    );
    /// The member the model does not declare, appended after the modelled ones.
    pub const M_RESOURCE_ARN: ::aws_smithy_schema::Schema = string_member_schema(
        "com.amazonaws.pcs.recorded#ResourceNotFoundException$resourceArn",
        RECORDED_NAMESPACE,
        "resourceArn",
        3,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.pcs#ResourceNotFoundException",
            SERVICE_NAMESPACE,
            "ResourceNotFoundException",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_MESSAGE,
            &Self::M_RESOURCE_ID,
            &Self::M_RESOURCE_TYPE,
            &Self::M_RESOURCE_ARN,
        ],
    );

    /// The wire error code a client branches on, which is the shape's local name.
    pub const fn shape_name() -> &'static str {
        "ResourceNotFoundException"
    }
}

/// The schema every `#[operation_error]` enum of this crate renders this error through.
pub static SCHEMA_RESOURCE_NOT_FOUND_EXCEPTION: ::aws_smithy_schema::Schema =
    ResourceNotFoundException::SCHEMA;

impl ::std::fmt::Display for ResourceNotFoundException {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        write!(f, "{}: {}", Self::shape_name(), self.message)
    }
}

impl ::std::error::Error for ResourceNotFoundException {}

impl ::aws_smithy_schema::serde::SerializableStruct for ResourceNotFoundException {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        s.write_string(&Self::M_MESSAGE, self.message.as_str())?;
        s.write_string(&Self::M_RESOURCE_ID, self.resource_id.as_str())?;
        s.write_string(&Self::M_RESOURCE_TYPE, self.resource_type.as_str())?;
        // The unmodelled member writes like the derive's own `is_option` branch: only when
        // the refusal carries one.
        if let ::std::option::Option::Some(resource_arn) = self.resource_arn.as_ref() {
            s.write_string(&Self::M_RESOURCE_ARN, resource_arn.as_str())?;
        }
        Ok(())
    }
}
