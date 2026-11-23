// SPDX-License-Identifier: CC0-1.0

//! The network record a read reports, in the form this service's wire carries it: the two
//! shapes `GetNetwork` and `ListNetworks` answer with, each with its schema and its codec
//! written out instead of derived.
//!
//! # Why this module exists
//!
//! `#[smithy_macros::smithy(shape = ...)]` derives a structure's `Schema`, its serializer
//! and its deserializer from the model's member list and admits EXACTLY that list: a Rust
//! field with no member behind it is a compile error, so a member the service really
//! reports and the on-disk model omits cannot be declared through the derive at all. Two
//! things about a network record are beyond that list:
//!
//! * the network's TAG SET. Neither `GetNetworkResponse` nor `Network` declares a `tags`
//!   member (`smithy_query(query="shape",shape="Network")` lists nine members, none of
//!   them a tag set), and a read of a network answers one — the service-set entry naming
//!   the network's own ARN among them ([`crate::state::NETWORK_TAG_RESOURCE_ARN`]).
//! * the members the LIST form serializes as an explicit null where the network has no
//!   value for them, two of which (`translationStatus` and `translationIamRoleArn`) the
//!   model does not declare either. A member of a derived shape is written only when it
//!   holds a value, so "present and null" cannot be expressed through the derive.
//!
//! The items below are the ones the derive emits — one `M_<MEMBER>` member schema per
//! member (index = that member's position in the member list), a `SCHEMA` naming them in
//! that order, the `<STRUCT>_SCHEMA` static and the `SerializableStruct` /
//! `DeserializableStruct` pair — from a member list this file gives, so the extra members
//! can be appended to it. The model's own members keep the names and the order the model
//! gives them, and a member the model does not declare is appended LAST with its shape id
//! in [`RECORDED_NAMESPACE`] rather than in this service's own, so it can neither claim to
//! be modelled nor collide with a member a later model refresh may declare — a refresh then
//! only has to move it into place and put the derive back.
//!
//! # What is deliberately absent
//!
//! For the same reason the derive's own output would be inert here: the constraint traits
//! (the validating walk runs on operation INPUTS only, and both shapes below are
//! output-only), the `@http` chain (baked on only for an operation's input), `@jsonName`
//! (no member below declares one: each one's wire name IS its Smithy member name) and the
//! tag map's key/value sub-member schemas (only the XML codecs read those, and this service
//! is `restJson1`).

use ::std::collections::BTreeMap;

use super::{member_schema, RECORDED_NAMESPACE, SERVICE_NAMESPACE};

/// Write `tags` as the tag set member of `member`: each key and then its value, against the
/// map member's own schema, which is where the JSON codec reads the entry layout from.
fn write_tags(
    s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    member: &::aws_smithy_schema::Schema,
    tags: &BTreeMap<String, String>,
) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
    s.write_map(member, &|entries| {
        for (key, value) in tags {
            entries.write_string(member, key.as_str())?;
            entries.write_string(member, value.as_str())?;
        }
        Ok(())
    })
}

/// Read the tag set member `member` holds.
fn read_tags(
    d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    member: &::aws_smithy_schema::Schema,
) -> ::std::result::Result<BTreeMap<String, String>, ::aws_smithy_schema::serde::SerdeError> {
    let mut tags = BTreeMap::new();
    d.read_map(member, &mut |key, value| {
        tags.insert(key, value.read_string(member)?);
        Ok(())
    })?;
    Ok(tags)
}

/// `GetNetwork`'s answer: one network as a read of it by id reports it.
///
/// The modelled members in the model's own order, then the tag set the wire carries and the
/// model does not declare (see this module's note).
#[derive(Debug, Clone, Default)]
pub struct GetNetworkOutput {
    pub network_id: String,
    pub network_name: String,
    pub access_level: String,
    pub aws_account_id: String,
    pub network_arn: String,
    pub standing: Option<i32>,
    pub free_trial_expiration: Option<String>,
    pub migration_state: Option<i32>,
    pub encryption_key_arn: Option<String>,
    /// The network's tags, which the model does not declare on this shape.
    pub tags: BTreeMap<String, String>,
}

impl GetNetworkOutput {    pub const M_NETWORK_ID: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$networkId",
        SERVICE_NAMESPACE,
        "networkId",
        ::aws_smithy_schema::ShapeType::String,
        0,
    );
    pub const M_NETWORK_NAME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$networkName",
        SERVICE_NAMESPACE,
        "networkName",
        ::aws_smithy_schema::ShapeType::String,
        1,
    );
    pub const M_ACCESS_LEVEL: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$accessLevel",
        SERVICE_NAMESPACE,
        "accessLevel",
        // An enum-targeted member takes the String shape type, as the derive does for one.
        ::aws_smithy_schema::ShapeType::String,
        2,
    );
    pub const M_AWS_ACCOUNT_ID: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$awsAccountId",
        SERVICE_NAMESPACE,
        "awsAccountId",
        ::aws_smithy_schema::ShapeType::String,
        3,
    );
    pub const M_NETWORK_ARN: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$networkArn",
        SERVICE_NAMESPACE,
        "networkArn",
        ::aws_smithy_schema::ShapeType::String,
        4,
    );
    pub const M_STANDING: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$standing",
        SERVICE_NAMESPACE,
        "standing",
        ::aws_smithy_schema::ShapeType::Integer,
        5,
    );
    pub const M_FREE_TRIAL_EXPIRATION: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$freeTrialExpiration",
        SERVICE_NAMESPACE,
        "freeTrialExpiration",
        ::aws_smithy_schema::ShapeType::String,
        6,
    );
    pub const M_MIGRATION_STATE: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$migrationState",
        SERVICE_NAMESPACE,
        "migrationState",
        ::aws_smithy_schema::ShapeType::Integer,
        7,
    );
    pub const M_ENCRYPTION_KEY_ARN: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#GetNetworkResponse$encryptionKeyArn",
        SERVICE_NAMESPACE,
        "encryptionKeyArn",
        ::aws_smithy_schema::ShapeType::String,
        8,
    );
    /// The member the model does not declare, appended after the modelled ones.
    pub const M_TAGS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr.recorded#GetNetworkResponse$tags",
        RECORDED_NAMESPACE,
        "tags",
        ::aws_smithy_schema::ShapeType::Map,
        9,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.wickr#GetNetworkResponse",
            SERVICE_NAMESPACE,
            "GetNetworkResponse",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_NETWORK_ID,
            &Self::M_NETWORK_NAME,
            &Self::M_ACCESS_LEVEL,
            &Self::M_AWS_ACCOUNT_ID,
            &Self::M_NETWORK_ARN,
            &Self::M_STANDING,
            &Self::M_FREE_TRIAL_EXPIRATION,
            &Self::M_MIGRATION_STATE,
            &Self::M_ENCRYPTION_KEY_ARN,
            &Self::M_TAGS,
        ],
    );
}

pub static GET_NETWORK_OUTPUT_SCHEMA: &::aws_smithy_schema::Schema = &GetNetworkOutput::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for GetNetworkOutput {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        // A `@required` member is written always, an optional one only when it holds a
        // value — the derive's own `is_option` split.
        s.write_string(&Self::M_NETWORK_ID, self.network_id.as_str())?;
        s.write_string(&Self::M_NETWORK_NAME, self.network_name.as_str())?;
        s.write_string(&Self::M_ACCESS_LEVEL, self.access_level.as_str())?;
        s.write_string(&Self::M_AWS_ACCOUNT_ID, self.aws_account_id.as_str())?;
        s.write_string(&Self::M_NETWORK_ARN, self.network_arn.as_str())?;
        if let Some(standing) = self.standing {
            s.write_integer(&Self::M_STANDING, standing)?;
        }
        if let Some(expiration) = self.free_trial_expiration.as_ref() {
            s.write_string(&Self::M_FREE_TRIAL_EXPIRATION, expiration.as_str())?;
        }
        if let Some(migration_state) = self.migration_state {
            s.write_integer(&Self::M_MIGRATION_STATE, migration_state)?;
        }
        if let Some(key_arn) = self.encryption_key_arn.as_ref() {
            s.write_string(&Self::M_ENCRYPTION_KEY_ARN, key_arn.as_str())?;
        }
        write_tags(s, &Self::M_TAGS, &self.tags)
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for GetNetworkOutput {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut network_id = None;
        let mut network_name = None;
        let mut access_level = None;
        let mut aws_account_id = None;
        let mut network_arn = None;
        let mut read = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => network_id = Some(d.read_string(member)?),
                Some(1) => network_name = Some(d.read_string(member)?),
                Some(2) => access_level = Some(d.read_string(member)?),
                Some(3) => aws_account_id = Some(d.read_string(member)?),
                Some(4) => network_arn = Some(d.read_string(member)?),
                Some(5) => read.standing = Some(d.read_integer(member)?),
                Some(6) => read.free_trial_expiration = Some(d.read_string(member)?),
                Some(7) => read.migration_state = Some(d.read_integer(member)?),
                Some(8) => read.encryption_key_arn = Some(d.read_string(member)?),
                Some(9) => read.tags = read_tags(d, member)?,
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            network_id: network_id
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("networkId"))?,
            network_name: network_name
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("networkName"))?,
            access_level: access_level
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("accessLevel"))?,
            aws_account_id: aws_account_id
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("awsAccountId"))?,
            network_arn: network_arn
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("networkArn"))?,
            ..read
        })
    }
}

/// One network as a LISTING of them reports it.
///
/// The modelled members in the model's own order, then the three the model does not declare
/// (see this module's note). The list form carries a FIXED member set: the members this
/// shape always serializes are written whatever the network holds, as an explicit null
/// where it holds nothing, so a client that tells "present and null" from "absent" reads
/// the same record shape for every network.
#[derive(Debug, Clone, Default)]
pub struct Network {
    pub network_id: String,
    pub network_name: String,
    pub access_level: String,
    pub aws_account_id: String,
    pub network_arn: String,
    pub standing: Option<i32>,
    /// Written always, as an explicit null for a network with no free trial.
    pub free_trial_expiration: Option<String>,
    pub migration_state: Option<i32>,
    pub encryption_key_arn: Option<String>,
    /// The network's tags, which the model does not declare on this shape.
    pub tags: BTreeMap<String, String>,
    /// How far the network's message translation has got, which the model does not declare
    /// on this shape. Written always, as an explicit null while nothing has configured
    /// translation for the network — which no operation of this API does.
    pub translation_status: Option<String>,
    /// The role the network's message translation assumes, which the model does not declare
    /// on this shape. Written always, as an explicit null on the same terms.
    pub translation_iam_role_arn: Option<String>,
}

impl Network {
    pub const M_NETWORK_ID: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$networkId",
        SERVICE_NAMESPACE,
        "networkId",
        ::aws_smithy_schema::ShapeType::String,
        0,
    );
    pub const M_NETWORK_NAME: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$networkName",
        SERVICE_NAMESPACE,
        "networkName",
        ::aws_smithy_schema::ShapeType::String,
        1,
    );
    pub const M_ACCESS_LEVEL: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$accessLevel",
        SERVICE_NAMESPACE,
        "accessLevel",
        // An enum-targeted member takes the String shape type, as the derive does for one.
        ::aws_smithy_schema::ShapeType::String,
        2,
    );
    pub const M_AWS_ACCOUNT_ID: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$awsAccountId",
        SERVICE_NAMESPACE,
        "awsAccountId",
        ::aws_smithy_schema::ShapeType::String,
        3,
    );
    pub const M_NETWORK_ARN: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$networkArn",
        SERVICE_NAMESPACE,
        "networkArn",
        ::aws_smithy_schema::ShapeType::String,
        4,
    );
    pub const M_STANDING: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$standing",
        SERVICE_NAMESPACE,
        "standing",
        ::aws_smithy_schema::ShapeType::Integer,
        5,
    );
    pub const M_FREE_TRIAL_EXPIRATION: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$freeTrialExpiration",
        SERVICE_NAMESPACE,
        "freeTrialExpiration",
        ::aws_smithy_schema::ShapeType::String,
        6,
    );
    pub const M_MIGRATION_STATE: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$migrationState",
        SERVICE_NAMESPACE,
        "migrationState",
        ::aws_smithy_schema::ShapeType::Integer,
        7,
    );
    pub const M_ENCRYPTION_KEY_ARN: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr#Network$encryptionKeyArn",
        SERVICE_NAMESPACE,
        "encryptionKeyArn",
        ::aws_smithy_schema::ShapeType::String,
        8,
    );
    /// The first of the members the model does not declare, appended after the modelled
    /// ones.
    pub const M_TAGS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr.recorded#Network$tags",
        RECORDED_NAMESPACE,
        "tags",
        ::aws_smithy_schema::ShapeType::Map,
        9,
    );
    pub const M_TRANSLATION_STATUS: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr.recorded#Network$translationStatus",
        RECORDED_NAMESPACE,
        "translationStatus",
        ::aws_smithy_schema::ShapeType::String,
        10,
    );
    pub const M_TRANSLATION_IAM_ROLE_ARN: ::aws_smithy_schema::Schema = member_schema(
        "com.amazonaws.wickr.recorded#Network$translationIamRoleArn",
        RECORDED_NAMESPACE,
        "translationIamRoleArn",
        ::aws_smithy_schema::ShapeType::String,
        11,
    );

    pub const SCHEMA: ::aws_smithy_schema::Schema = ::aws_smithy_schema::Schema::new_struct(
        ::aws_smithy_schema::ShapeId::from_static(
            "com.amazonaws.wickr#Network",
            SERVICE_NAMESPACE,
            "Network",
        ),
        ::aws_smithy_schema::ShapeType::Structure,
        &[
            &Self::M_NETWORK_ID,
            &Self::M_NETWORK_NAME,
            &Self::M_ACCESS_LEVEL,
            &Self::M_AWS_ACCOUNT_ID,
            &Self::M_NETWORK_ARN,
            &Self::M_STANDING,
            &Self::M_FREE_TRIAL_EXPIRATION,
            &Self::M_MIGRATION_STATE,
            &Self::M_ENCRYPTION_KEY_ARN,
            &Self::M_TAGS,
            &Self::M_TRANSLATION_STATUS,
            &Self::M_TRANSLATION_IAM_ROLE_ARN,
        ],
    );
}

pub static NETWORK_SCHEMA: &::aws_smithy_schema::Schema = &Network::SCHEMA;

impl ::aws_smithy_schema::serde::SerializableStruct for Network {
    fn serialize_members(
        &self,
        s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    ) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
        s.write_string(&Self::M_NETWORK_ID, self.network_id.as_str())?;
        s.write_string(&Self::M_NETWORK_NAME, self.network_name.as_str())?;
        s.write_string(&Self::M_ACCESS_LEVEL, self.access_level.as_str())?;
        s.write_string(&Self::M_AWS_ACCOUNT_ID, self.aws_account_id.as_str())?;
        s.write_string(&Self::M_NETWORK_ARN, self.network_arn.as_str())?;
        if let Some(standing) = self.standing {
            s.write_integer(&Self::M_STANDING, standing)?;
        }
        write_or_null(s, &Self::M_FREE_TRIAL_EXPIRATION, &self.free_trial_expiration)?;
        if let Some(migration_state) = self.migration_state {
            s.write_integer(&Self::M_MIGRATION_STATE, migration_state)?;
        }
        if let Some(key_arn) = self.encryption_key_arn.as_ref() {
            s.write_string(&Self::M_ENCRYPTION_KEY_ARN, key_arn.as_str())?;
        }
        write_tags(s, &Self::M_TAGS, &self.tags)?;
        write_or_null(s, &Self::M_TRANSLATION_STATUS, &self.translation_status)?;
        write_or_null(
            s,
            &Self::M_TRANSLATION_IAM_ROLE_ARN,
            &self.translation_iam_role_arn,
        )
    }
}

/// Write `member` whatever the record holds: its value, or an explicit null where it holds
/// none — the half of the record's shape a derived member cannot express, since a derived
/// member is written only when it has a value.
fn write_or_null(
    s: &mut dyn ::aws_smithy_schema::serde::ShapeSerializer,
    member: &::aws_smithy_schema::Schema,
    held: &Option<String>,
) -> ::std::result::Result<(), ::aws_smithy_schema::serde::SerdeError> {
    match held {
        Some(value) => s.write_string(member, value.as_str()),
        None => s.write_null(member),
    }
}

impl ::aws_smithy_schema_server::DeserializableStruct for Network {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> ::std::result::Result<Self, ::aws_smithy_schema::serde::SerdeError> {
        let mut network_id = None;
        let mut network_name = None;
        let mut access_level = None;
        let mut aws_account_id = None;
        let mut network_arn = None;
        let mut read = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => network_id = Some(d.read_string(member)?),
                Some(1) => network_name = Some(d.read_string(member)?),
                Some(2) => access_level = Some(d.read_string(member)?),
                Some(3) => aws_account_id = Some(d.read_string(member)?),
                Some(4) => network_arn = Some(d.read_string(member)?),
                Some(5) => read.standing = Some(d.read_integer(member)?),
                Some(6) => read.free_trial_expiration = Some(d.read_string(member)?),
                Some(7) => read.migration_state = Some(d.read_integer(member)?),
                Some(8) => read.encryption_key_arn = Some(d.read_string(member)?),
                Some(9) => read.tags = read_tags(d, member)?,
                Some(10) => read.translation_status = Some(d.read_string(member)?),
                Some(11) => read.translation_iam_role_arn = Some(d.read_string(member)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            network_id: network_id
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("networkId"))?,
            network_name: network_name
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("networkName"))?,
            access_level: access_level
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("accessLevel"))?,
            aws_account_id: aws_account_id
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("awsAccountId"))?,
            network_arn: network_arn
                .ok_or_else(|| ::aws_smithy_schema_server::missing_member("networkArn"))?,
            ..read
        })
    }
}
