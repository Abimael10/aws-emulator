// SPDX-License-Identifier: CC0-1.0

//! `FileInformation`, whose WIRE carries a member this service's on-disk model does not
//! declare.
//!
//! `#[smithy_macros::smithy(shape = ...)]` derives a structure's `Schema`, its serializer
//! and its deserializer from the model's member list, and it admits EXACTLY that list: a
//! Rust field with no member behind it is a compile error. The on-disk
//! `com.amazonaws.omics#FileInformation` declares `totalParts`, `partSize`,
//! `contentLength` and `s3Access`, while a read set file on the wire ALSO carries `md5`,
//! the digest of the file as the service ingested it, which a client uses to confirm the
//! file it reads back is the file it uploaded. So this one shape's schema and codec are
//! written out here instead of derived.
//!
//! The model's own members keep the indices and the order the model gives them (the order
//! `smithy_query(query="shape",shape="com.amazonaws.omics#FileInformation")` lists them
//! in), and `md5` is appended LAST with its shape id in a namespace of its own, so it can
//! neither claim to be modelled nor collide with a member the model may declare later — a
//! later model refresh that adds it only has to move it into place and put the derive
//! back.
//!
//! Deliberately absent, for the same reason the derive's own output would be inert here:
//! the constraint traits (the validating walk runs on operation INPUTS only, and this
//! structure is output-only) and the `@http` chain (baked on only for an operation's
//! input). No member declares a `@jsonName`: each one's wire name IS its member name.

#[allow(unused_imports)]
use super::*;

/// The namespace the member the model does not declare carries its shape id in.
const RECORDED_NAMESPACE: &str = "com.amazonaws.omics.recorded";

/// Details about one file of a read set, as `ReadSetFiles` reports it.
#[derive(Debug, Clone, Default)]
pub struct FileInformation {
    pub total_parts: Option<i32>,
    pub part_size: Option<i64>,
    pub content_length: Option<i64>,
    pub s3_access: Option<ReadSetS3Access>,
    /// RECORDED behaviour, not modelled (see this module's note): the MD5 digest of the
    /// file as the service ingested it.
    pub md5: Option<String>,
}

#[allow(non_upper_case_globals)]
impl FileInformation {
    pub const M_TOTAL_PARTS: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.omics#FileInformation$totalParts",
            "com.amazonaws.omics",
            "totalParts",
        ),
        ::aws_smithy_schema::ShapeType::Integer,
        "totalParts",
        0,
    );

    pub const M_PART_SIZE: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.omics#FileInformation$partSize",
            "com.amazonaws.omics",
            "partSize",
        ),
        ::aws_smithy_schema::ShapeType::Long,
        "partSize",
        1,
    );

    pub const M_CONTENT_LENGTH: ::aws_smithy_schema::Schema =
        ::aws_smithy_schema::Schema::new_member(
            ::aws_smithy_schema::ShapeId::from_static(
                "com.amazonaws.omics#FileInformation$contentLength",
                "com.amazonaws.omics",
                "contentLength",
            ),
            ::aws_smithy_schema::ShapeType::Long,
            "contentLength",
            2,
        );

    pub const M_S3_ACCESS: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.omics#FileInformation$s3Access",
            "com.amazonaws.omics",
            "s3Access",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        "s3Access",
        3,
    );

    /// The member the on-disk model does not declare, in its own namespace and last in
    /// the member list.
    pub const M_MD5: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.omics.recorded#FileInformation$md5",
            RECORDED_NAMESPACE,
            "md5",
        ),
        ::aws_smithy_schema::ShapeType::String,
        "md5",
        4,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.omics#FileInformation",
            "com.amazonaws.omics",
            "FileInformation",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_TOTAL_PARTS,
            &Self::M_PART_SIZE,
            &Self::M_CONTENT_LENGTH,
            &Self::M_S3_ACCESS,
            &Self::M_MD5,
        ],
    );
}

pub static FILE_INFORMATION_SCHEMA: &::aws_smithy_schema::Schema = &FileInformation::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for FileInformation {
    fn serialize_members(
        &self,
        serializer: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        if let ::std::option::Option::Some(total_parts) = self.total_parts.as_ref() {
            serializer.write_integer(&Self::M_TOTAL_PARTS, *total_parts)?;
        }
        if let ::std::option::Option::Some(part_size) = self.part_size.as_ref() {
            serializer.write_long(&Self::M_PART_SIZE, *part_size)?;
        }
        if let ::std::option::Option::Some(content_length) = self.content_length.as_ref() {
            serializer.write_long(&Self::M_CONTENT_LENGTH, *content_length)?;
        }
        if let ::std::option::Option::Some(s3_access) = self.s3_access.as_ref() {
            serializer.write_struct(&Self::M_S3_ACCESS, s3_access)?;
        }
        if let ::std::option::Option::Some(md5) = self.md5.as_ref() {
            serializer.write_string(&Self::M_MD5, md5.as_str())?;
        }
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for FileInformation {
    fn deserialize(
        deserializer: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut total_parts = ::std::option::Option::None;
        let mut part_size = ::std::option::Option::None;
        let mut content_length = ::std::option::Option::None;
        let mut s3_access = ::std::option::Option::None;
        let mut md5 = ::std::option::Option::None;
        deserializer.read_struct(&Self::SCHEMA, &mut |member, member_deserializer| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    total_parts = ::std::option::Option::Some(
                        member_deserializer.read_integer(&Self::M_TOTAL_PARTS)?,
                    );
                }
                ::std::option::Option::Some(1) => {
                    part_size = ::std::option::Option::Some(
                        member_deserializer.read_long(&Self::M_PART_SIZE)?,
                    );
                }
                ::std::option::Option::Some(2) => {
                    content_length = ::std::option::Option::Some(
                        member_deserializer.read_long(&Self::M_CONTENT_LENGTH)?,
                    );
                }
                ::std::option::Option::Some(3) => {
                    s3_access = ::std::option::Option::Some(
                        <ReadSetS3Access as ::aws_smithy_schema_server::DeserializableStruct>::deserialize(
                            member_deserializer,
                        )?,
                    );
                }
                ::std::option::Option::Some(4) => {
                    md5 = ::std::option::Option::Some(
                        member_deserializer.read_string(&Self::M_MD5)?,
                    );
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            total_parts,
            part_size,
            content_length,
            s3_access,
            md5,
        })
    }
}
