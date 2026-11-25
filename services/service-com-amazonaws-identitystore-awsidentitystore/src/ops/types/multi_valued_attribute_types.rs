// SPDX-License-Identifier: CC0-1.0

//! The elements of a user's multi-valued attributes: `Email`, `Address`,
//! `PhoneNumber`, `Photo` and `Role`.
//!
//! Every sub-attribute of such an element is reported only when the element actually
//! carries one — including `Primary`, which the model gives a `@default` of `false`
//! (`smithy_query(query="shape",shape="Email,Address,PhoneNumber,Photo,Role")`). The
//! generated binding of a `@default` member is a NON-optional field, which cannot tell
//! an element whose caller supplied `Primary: false` from one that supplied no
//! `Primary` at all, so each element below declares `Primary` as an `Option<bool>` and
//! writes it only when it is present, exactly as it does for the sub-attributes the
//! model leaves optional.
//!
//! The `Modeled…` struct beside each element is the generated binding of the same
//! shape, kept for its member SCHEMAS alone: the model's `@length`, `@pattern` and
//! `@sensitive` traits ride on those, and the validating walk at the request boundary
//! reads them from there — so this file restates no constraint of the model, and the
//! wire name, JSON key and shape id of every member stay the generated ones.
#![allow(unused_imports)]

use super::*;

use ::aws_smithy_schema::serde::{
    SerdeError, SerializableStruct, ShapeDeserializer, ShapeSerializer,
};
use ::aws_smithy_schema::Schema;
use ::aws_smithy_schema_server::{missing_member, within_member, DeserializableStruct};

smithy_macros::smithy_model!("smithy-model/com-amazonaws-identitystore-awsidentitystore.json");

/// Write one string sub-attribute, which stays out of the response when the element
/// carries none.
fn write_text(
    s: &mut dyn ShapeSerializer,
    member: &Schema,
    value: Option<&str>,
) -> Result<(), SerdeError> {
    if let Some(text) = value {
        s.write_string(member, text)?;
    }
    Ok(())
}

/// Write the `Primary` sub-attribute, which stays out of the response when the element
/// carries none.
fn write_flag(
    s: &mut dyn ShapeSerializer,
    member: &Schema,
    value: Option<bool>,
) -> Result<(), SerdeError> {
    if let Some(flag) = value {
        s.write_boolean(member, flag)?;
    }
    Ok(())
}

/// Read one string sub-attribute, naming the member on a failure inside it.
fn read_text(d: &mut dyn ShapeDeserializer, member: &Schema) -> Result<String, SerdeError> {
    let name = member.member_name().unwrap_or_default();
    d.read_string(member)
        .map_err(|error| within_member(error, name))
}

/// Read the `Primary` sub-attribute, naming the member on a failure inside it.
fn read_flag(d: &mut dyn ShapeDeserializer, member: &Schema) -> Result<bool, SerdeError> {
    let name = member.member_name().unwrap_or_default();
    d.read_boolean(member)
        .map_err(|error| within_member(error, name))
}

/// The generated binding of `Email`, kept for the member schemas [`Email`] serializes
/// its own members through.
#[smithy_macros::smithy(shape = "com.amazonaws.identitystore#Email")]
#[derive(Debug, Clone, Default)]
struct ModeledEmail {
    value: Option<String>,
    r#type: Option<String>,
    primary: bool,
}

/// The email address associated with a user.
#[derive(Debug, Clone, Default)]
pub struct Email {
    pub value: Option<String>,
    pub r#type: Option<String>,
    /// `Some` only for an element that carried a `Primary`.
    pub primary: Option<bool>,
}

impl Email {
    /// The modelled shape's own schema, its members and their traits included.
    pub const SCHEMA: Schema = ModeledEmail::SCHEMA;
}

impl SerializableStruct for Email {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        write_text(s, &ModeledEmail::M_VALUE, self.value.as_deref())?;
        write_text(s, &ModeledEmail::M_TYPE, self.r#type.as_deref())?;
        write_flag(s, &ModeledEmail::M_PRIMARY, self.primary)
    }
}

impl DeserializableStruct for Email {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut entry = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_name() {
                Some("Value") => entry.value = Some(read_text(d, member)?),
                Some("Type") => entry.r#type = Some(read_text(d, member)?),
                Some("Primary") => entry.primary = Some(read_flag(d, member)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(entry)
    }
}

/// The generated binding of `Address`, kept for the member schemas [`Address`]
/// serializes its own members through.
#[smithy_macros::smithy(shape = "com.amazonaws.identitystore#Address")]
#[derive(Debug, Clone, Default)]
struct ModeledAddress {
    street_address: Option<String>,
    locality: Option<String>,
    region: Option<String>,
    postal_code: Option<String>,
    country: Option<String>,
    formatted: Option<String>,
    r#type: Option<String>,
    primary: bool,
}

/// The address associated with a user.
#[derive(Debug, Clone, Default)]
pub struct Address {
    pub street_address: Option<String>,
    pub locality: Option<String>,
    pub region: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub formatted: Option<String>,
    pub r#type: Option<String>,
    /// `Some` only for an element that carried a `Primary`.
    pub primary: Option<bool>,
}

impl Address {
    /// The modelled shape's own schema, its members and their traits included.
    pub const SCHEMA: Schema = ModeledAddress::SCHEMA;
}

impl SerializableStruct for Address {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        write_text(
            s,
            &ModeledAddress::M_STREET_ADDRESS,
            self.street_address.as_deref(),
        )?;
        write_text(s, &ModeledAddress::M_LOCALITY, self.locality.as_deref())?;
        write_text(s, &ModeledAddress::M_REGION, self.region.as_deref())?;
        write_text(
            s,
            &ModeledAddress::M_POSTAL_CODE,
            self.postal_code.as_deref(),
        )?;
        write_text(s, &ModeledAddress::M_COUNTRY, self.country.as_deref())?;
        write_text(s, &ModeledAddress::M_FORMATTED, self.formatted.as_deref())?;
        write_text(s, &ModeledAddress::M_TYPE, self.r#type.as_deref())?;
        write_flag(s, &ModeledAddress::M_PRIMARY, self.primary)
    }
}

impl DeserializableStruct for Address {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut entry = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_name() {
                Some("StreetAddress") => entry.street_address = Some(read_text(d, member)?),
                Some("Locality") => entry.locality = Some(read_text(d, member)?),
                Some("Region") => entry.region = Some(read_text(d, member)?),
                Some("PostalCode") => entry.postal_code = Some(read_text(d, member)?),
                Some("Country") => entry.country = Some(read_text(d, member)?),
                Some("Formatted") => entry.formatted = Some(read_text(d, member)?),
                Some("Type") => entry.r#type = Some(read_text(d, member)?),
                Some("Primary") => entry.primary = Some(read_flag(d, member)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(entry)
    }
}

/// The generated binding of `PhoneNumber`, kept for the member schemas [`PhoneNumber`]
/// serializes its own members through.
#[smithy_macros::smithy(shape = "com.amazonaws.identitystore#PhoneNumber")]
#[derive(Debug, Clone, Default)]
struct ModeledPhoneNumber {
    value: Option<String>,
    r#type: Option<String>,
    primary: bool,
}

/// The phone number associated with a user.
#[derive(Debug, Clone, Default)]
pub struct PhoneNumber {
    pub value: Option<String>,
    pub r#type: Option<String>,
    /// `Some` only for an element that carried a `Primary`.
    pub primary: Option<bool>,
}

impl PhoneNumber {
    /// The modelled shape's own schema, its members and their traits included.
    pub const SCHEMA: Schema = ModeledPhoneNumber::SCHEMA;
}

impl SerializableStruct for PhoneNumber {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        write_text(s, &ModeledPhoneNumber::M_VALUE, self.value.as_deref())?;
        write_text(s, &ModeledPhoneNumber::M_TYPE, self.r#type.as_deref())?;
        write_flag(s, &ModeledPhoneNumber::M_PRIMARY, self.primary)
    }
}

impl DeserializableStruct for PhoneNumber {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut entry = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_name() {
                Some("Value") => entry.value = Some(read_text(d, member)?),
                Some("Type") => entry.r#type = Some(read_text(d, member)?),
                Some("Primary") => entry.primary = Some(read_flag(d, member)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(entry)
    }
}

/// The generated binding of `Photo`, kept for the member schemas [`Photo`] serializes
/// its own members through.
#[smithy_macros::smithy(shape = "com.amazonaws.identitystore#Photo")]
#[derive(Debug, Clone, Default)]
struct ModeledPhoto {
    value: String,
    r#type: Option<String>,
    display: Option<String>,
    primary: bool,
}

/// One of a user's photos. `Photo$Value` is the shape's only `@required` member ("this
/// field is required for all photo entries").
#[derive(Debug, Clone, Default)]
pub struct Photo {
    pub value: String,
    pub r#type: Option<String>,
    pub display: Option<String>,
    /// `Some` only for an element that carried a `Primary`.
    pub primary: Option<bool>,
}

impl Photo {
    /// The modelled shape's own schema, its members and their traits included.
    pub const SCHEMA: Schema = ModeledPhoto::SCHEMA;
}

impl SerializableStruct for Photo {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&ModeledPhoto::M_VALUE, self.value.as_str())?;
        write_text(s, &ModeledPhoto::M_TYPE, self.r#type.as_deref())?;
        write_text(s, &ModeledPhoto::M_DISPLAY, self.display.as_deref())?;
        write_flag(s, &ModeledPhoto::M_PRIMARY, self.primary)
    }
}

impl DeserializableStruct for Photo {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut value: Option<String> = None;
        let mut entry = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_name() {
                Some("Value") => value = Some(read_text(d, member)?),
                Some("Type") => entry.r#type = Some(read_text(d, member)?),
                Some("Display") => entry.display = Some(read_text(d, member)?),
                Some("Primary") => entry.primary = Some(read_flag(d, member)?),
                _ => {}
            }
            Ok(())
        })?;
        entry.value = value.ok_or_else(|| missing_member("Value"))?;
        Ok(entry)
    }
}

/// The generated binding of `Role`, kept for the member schemas [`Role`] serializes its
/// own members through.
#[smithy_macros::smithy(shape = "com.amazonaws.identitystore#Role")]
#[derive(Debug, Clone, Default)]
struct ModeledRole {
    value: Option<String>,
    r#type: Option<String>,
    primary: bool,
}

/// The role associated with a user.
#[derive(Debug, Clone, Default)]
pub struct Role {
    pub value: Option<String>,
    pub r#type: Option<String>,
    /// `Some` only for an element that carried a `Primary`.
    pub primary: Option<bool>,
}

impl Role {
    /// The modelled shape's own schema, its members and their traits included.
    pub const SCHEMA: Schema = ModeledRole::SCHEMA;
}

impl SerializableStruct for Role {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        write_text(s, &ModeledRole::M_VALUE, self.value.as_deref())?;
        write_text(s, &ModeledRole::M_TYPE, self.r#type.as_deref())?;
        write_flag(s, &ModeledRole::M_PRIMARY, self.primary)
    }
}

impl DeserializableStruct for Role {
    fn deserialize(d: &mut dyn ShapeDeserializer) -> Result<Self, SerdeError> {
        let mut entry = Self::default();
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_name() {
                Some("Value") => entry.value = Some(read_text(d, member)?),
                Some("Type") => entry.r#type = Some(read_text(d, member)?),
                Some("Primary") => entry.primary = Some(read_flag(d, member)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(entry)
    }
}
