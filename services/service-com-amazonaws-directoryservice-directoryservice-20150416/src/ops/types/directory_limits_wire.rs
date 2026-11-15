// SPDX-License-Identifier: CC0-1.0

//! `DirectoryLimits`: the structure `GetDirectoryLimits` answers with, whose schema and
//! codec are written out here rather than derived.
//!
//! WHY. A Directory Service limits answer carries one flag beyond the nine members the
//! on-disk model declares for this structure — `IdentityPoolDirectoriesLimitReached`, a
//! boolean reported alongside the other `...LimitReached` flags — so a client that reads
//! every limit flag off the answer to decide whether another directory can be created
//! finds an explicit boolean rather than an absent member.
//! `#[smithy_macros::smithy(shape = ...)]` derives a structure's `Schema`, its serializer
//! and its deserializer from the model's member list and admits EXACTLY that list (a Rust
//! field with no member behind it is a compile error), so a member the service reports and
//! the model omits cannot be declared through the derive at all. The items below are the
//! ones the derive emits — one member schema per member (index = that member's position in
//! the member list), the `SCHEMA` naming them in that order, the `DIRECTORY_LIMITS_SCHEMA`
//! static and the `SerializableStruct` / `DeserializableStruct` pair — from a member list
//! given here, so the extra flag can be appended to it.
//!
//! The model's own members keep the names, order and indices the model gives them
//! (`smithy_query(query="shape", shape="DirectoryLimits")`), and the unmodelled flag is
//! appended LAST, so a later model refresh that declares it only has to move it into place
//! and put the derive back. No constraint trait is carried: the validating walk reads an
//! operation's INPUT schema only, and this structure appears in no operation input.
//!
//! The same machinery is hand-written for the same reason in several sibling service
//! crates (`service-com-amazonaws-mq-mq`,
//! `service-com-amazonaws-imagebuilder-imagebuilder`,
//! `service-com-amazonaws-athena-amazonathena`); the gap it compensates for is in
//! `vendor/smithy-macros`, which is read-only from here. Once the generator takes a
//! recorded member, delete this module and put the derive back on the structure;
//! `tests/all/directory_limit_members.rs` pins the wire form either way.

/// One member schema of this structure, at `index` in its member list: the member's shape
/// id, its shape type and its wire name, exactly as the vendored codegen emits them.
const fn member_schema(
    shape_id: &'static str,
    name: &'static str,
    shape_type: ::aws_smithy_schema::ShapeType,
    index: usize,
) -> ::aws_smithy_schema::Schema {
    ::aws_smithy_schema::Schema::new_member(
        ::aws_smithy_schema::ShapeId::from_static(shape_id, "com.amazonaws.directoryservice", name),
        shape_type,
        name,
        index,
    )
}

/// Contains directory limit information for a Region: each category's own quota, how many
/// directories of it the account holds and whether that quota is used up.
///
/// Derived in
/// [`crate::state::ComAmazonawsDirectoryserviceDirectoryservice20150416State::directory_limits`],
/// which is the one place every member of it is computed.
#[derive(Debug, Clone, Default)]
pub struct DirectoryLimits {
    pub cloud_only_directories_limit: Option<i32>,
    pub cloud_only_directories_current_count: Option<i32>,
    pub cloud_only_directories_limit_reached: bool,
    pub cloud_only_microsoft_ad_limit: Option<i32>,
    pub cloud_only_microsoft_ad_current_count: Option<i32>,
    pub cloud_only_microsoft_ad_limit_reached: bool,
    pub connected_directories_limit: Option<i32>,
    pub connected_directories_current_count: Option<i32>,
    pub connected_directories_limit_reached: bool,
    /// `IdentityPoolDirectoriesLimitReached`: the member a Directory Service answer carries
    /// and the model does not declare. Not an `Option`, because the service reports it on
    /// every answer, beside the three flags the model does declare.
    pub identity_pool_directories_limit_reached: bool,
}

impl DirectoryLimits {
    pub const M_CLOUD_ONLY_DIRECTORIES_LIMIT: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$CloudOnlyDirectoriesLimit",
        "CloudOnlyDirectoriesLimit",
        ::aws_smithy_schema::ShapeType::Integer,
        0,
    );
    pub const M_CLOUD_ONLY_DIRECTORIES_CURRENT_COUNT: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$CloudOnlyDirectoriesCurrentCount",
        "CloudOnlyDirectoriesCurrentCount",
        ::aws_smithy_schema::ShapeType::Integer,
        1,
    );
    pub const M_CLOUD_ONLY_DIRECTORIES_LIMIT_REACHED: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$CloudOnlyDirectoriesLimitReached",
        "CloudOnlyDirectoriesLimitReached",
        ::aws_smithy_schema::ShapeType::Boolean,
        2,
    );
    pub const M_CLOUD_ONLY_MICROSOFT_AD_LIMIT: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$CloudOnlyMicrosoftADLimit",
        "CloudOnlyMicrosoftADLimit",
        ::aws_smithy_schema::ShapeType::Integer,
        3,
    );
    pub const M_CLOUD_ONLY_MICROSOFT_AD_CURRENT_COUNT: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$CloudOnlyMicrosoftADCurrentCount",
        "CloudOnlyMicrosoftADCurrentCount",
        ::aws_smithy_schema::ShapeType::Integer,
        4,
    );
    pub const M_CLOUD_ONLY_MICROSOFT_AD_LIMIT_REACHED: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$CloudOnlyMicrosoftADLimitReached",
        "CloudOnlyMicrosoftADLimitReached",
        ::aws_smithy_schema::ShapeType::Boolean,
        5,
    );
    pub const M_CONNECTED_DIRECTORIES_LIMIT: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$ConnectedDirectoriesLimit",
        "ConnectedDirectoriesLimit",
        ::aws_smithy_schema::ShapeType::Integer,
        6,
    );
    pub const M_CONNECTED_DIRECTORIES_CURRENT_COUNT: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$ConnectedDirectoriesCurrentCount",
        "ConnectedDirectoriesCurrentCount",
        ::aws_smithy_schema::ShapeType::Integer,
        7,
    );
    pub const M_CONNECTED_DIRECTORIES_LIMIT_REACHED: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.directoryservice#DirectoryLimits$ConnectedDirectoriesLimitReached",
        "ConnectedDirectoriesLimitReached",
        ::aws_smithy_schema::ShapeType::Boolean,
        8,
    );
    /// The member the model does not declare, appended after the modelled ones.
    pub const M_IDENTITY_POOL_DIRECTORIES_LIMIT_REACHED: ::aws_smithy_schema::Schema =
        member_schema(
            "com.amazonaws.directoryservice#DirectoryLimits$IdentityPoolDirectoriesLimitReached",
            "IdentityPoolDirectoriesLimitReached",
            ::aws_smithy_schema::ShapeType::Boolean,
            9,
        );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.directoryservice#DirectoryLimits",
            "com.amazonaws.directoryservice",
            "DirectoryLimits",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_CLOUD_ONLY_DIRECTORIES_LIMIT,
            &Self::M_CLOUD_ONLY_DIRECTORIES_CURRENT_COUNT,
            &Self::M_CLOUD_ONLY_DIRECTORIES_LIMIT_REACHED,
            &Self::M_CLOUD_ONLY_MICROSOFT_AD_LIMIT,
            &Self::M_CLOUD_ONLY_MICROSOFT_AD_CURRENT_COUNT,
            &Self::M_CLOUD_ONLY_MICROSOFT_AD_LIMIT_REACHED,
            &Self::M_CONNECTED_DIRECTORIES_LIMIT,
            &Self::M_CONNECTED_DIRECTORIES_CURRENT_COUNT,
            &Self::M_CONNECTED_DIRECTORIES_LIMIT_REACHED,
            &Self::M_IDENTITY_POOL_DIRECTORIES_LIMIT_REACHED,
        ],
    );
}

pub static DIRECTORY_LIMITS_SCHEMA: &::aws_smithy_schema::Schema = &DirectoryLimits::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for DirectoryLimits {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        // Each counter is written when it is there, and each flag unconditionally — the
        // derive's own `is_option` split, which puts a member the model gives a `@default`
        // on the second branch. The unmodelled flag is a flag, so it writes with them.
        if let ::std::option::Option::Some(limit) = self.cloud_only_directories_limit {
            s.write_integer(&Self::M_CLOUD_ONLY_DIRECTORIES_LIMIT, limit)?;
        }
        if let ::std::option::Option::Some(count) = self.cloud_only_directories_current_count {
            s.write_integer(&Self::M_CLOUD_ONLY_DIRECTORIES_CURRENT_COUNT, count)?;
        }
        s.write_boolean(
            &Self::M_CLOUD_ONLY_DIRECTORIES_LIMIT_REACHED,
            self.cloud_only_directories_limit_reached,
        )?;
        if let ::std::option::Option::Some(limit) = self.cloud_only_microsoft_ad_limit {
            s.write_integer(&Self::M_CLOUD_ONLY_MICROSOFT_AD_LIMIT, limit)?;
        }
        if let ::std::option::Option::Some(count) = self.cloud_only_microsoft_ad_current_count {
            s.write_integer(&Self::M_CLOUD_ONLY_MICROSOFT_AD_CURRENT_COUNT, count)?;
        }
        s.write_boolean(
            &Self::M_CLOUD_ONLY_MICROSOFT_AD_LIMIT_REACHED,
            self.cloud_only_microsoft_ad_limit_reached,
        )?;
        if let ::std::option::Option::Some(limit) = self.connected_directories_limit {
            s.write_integer(&Self::M_CONNECTED_DIRECTORIES_LIMIT, limit)?;
        }
        if let ::std::option::Option::Some(count) = self.connected_directories_current_count {
            s.write_integer(&Self::M_CONNECTED_DIRECTORIES_CURRENT_COUNT, count)?;
        }
        s.write_boolean(
            &Self::M_CONNECTED_DIRECTORIES_LIMIT_REACHED,
            self.connected_directories_limit_reached,
        )?;
        s.write_boolean(
            &Self::M_IDENTITY_POOL_DIRECTORIES_LIMIT_REACHED,
            self.identity_pool_directories_limit_reached,
        )?;
        Ok(())
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for DirectoryLimits {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut limits = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                ::std::option::Option::Some(0) => {
                    limits.cloud_only_directories_limit =
                        ::std::option::Option::Some(d.read_integer(member)?);
                }
                ::std::option::Option::Some(1) => {
                    limits.cloud_only_directories_current_count =
                        ::std::option::Option::Some(d.read_integer(member)?);
                }
                ::std::option::Option::Some(2) => {
                    limits.cloud_only_directories_limit_reached = d.read_boolean(member)?;
                }
                ::std::option::Option::Some(3) => {
                    limits.cloud_only_microsoft_ad_limit =
                        ::std::option::Option::Some(d.read_integer(member)?);
                }
                ::std::option::Option::Some(4) => {
                    limits.cloud_only_microsoft_ad_current_count =
                        ::std::option::Option::Some(d.read_integer(member)?);
                }
                ::std::option::Option::Some(5) => {
                    limits.cloud_only_microsoft_ad_limit_reached = d.read_boolean(member)?;
                }
                ::std::option::Option::Some(6) => {
                    limits.connected_directories_limit =
                        ::std::option::Option::Some(d.read_integer(member)?);
                }
                ::std::option::Option::Some(7) => {
                    limits.connected_directories_current_count =
                        ::std::option::Option::Some(d.read_integer(member)?);
                }
                ::std::option::Option::Some(8) => {
                    limits.connected_directories_limit_reached = d.read_boolean(member)?;
                }
                ::std::option::Option::Some(9) => {
                    limits.identity_pool_directories_limit_reached = d.read_boolean(member)?;
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(limits)
    }
}
