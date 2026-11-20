// SPDX-License-Identifier: CC0-1.0

//! What a thing type's PROPERTIES may carry, and how a write of them changes a type the
//! registry already holds.
//!
//! `ThingTypeProperties` is the one shape BOTH writers of a thing type carry —
//! `CreateThingType` and `UpdateThingType` — so the ceiling on how many entries one of
//! its lists may hold ([`super::MAX_THING_TYPE_PROPERTY_ENTRIES`]) is stated once here and
//! read by both: a record keeps what the writer sent so a reader can hand it back, and
//! neither list has a modelled maximum, so without the ceiling one request would choose
//! how large a single record is.
//!
//! `UpdateThingType` "Updates a thing type". It addresses the type by the name it was
//! created under — the `@httpLabel` of `/thing-types/{thingTypeName}` — and is bound to
//! HTTP `PATCH` with `thingTypeProperties` and every member of it OPTIONAL
//! (`smithy_query(query="operation",operation="UpdateThingType")`,
//! `smithy_query(query="shape",shape="ThingTypeProperties")`), so each member the request
//! PROVIDES is what the type carries afterwards and each member it OMITS leaves the stored
//! one exactly as it was. That is why the update is kept as [`ThingTypeUpdate`] — a record
//! of what was provided — rather than as the properties themselves: a whole-properties
//! write could not tell "no description given" from "no description wanted", and a request
//! that states only the MQTT 5 configuration has to leave the description and the
//! searchable attributes alone.
//!
//! The write lives here rather than in the handler for the same reason
//! [`super::ComAmazonawsIotAwsiotserviceState::update_thing_group`]'s does: it is the
//! registry's own rule about which changes a type record takes, and a handler maps a
//! refusal onto the error ITS model declares.

use super::{
    not_found, ComAmazonawsIotAwsiotserviceState, PropagatingAttributeRecord,
    ThingTypeConfiguration, MAX_THING_TYPE_PROPERTY_ENTRIES,
};
use crate::ops::types::ThingTypeProperties;

/// The sentence a write of a thing type is refused with when one of the property lists it
/// carries holds more entries than [`MAX_THING_TYPE_PROPERTY_ENTRIES`], or `None` when
/// every list is within the ceiling.
///
/// Both writers of a thing type read it, so the two cannot disagree about which lists are
/// bounded or about how the refusal names them: the member whose list is over the ceiling
/// (in the modelled member path a client sent it under), the ceiling, and how many entries
/// the request actually carried. The lists are checked in the order the shape declares
/// them, so a request that is over on both is refused about the first.
pub fn over_long_thing_type_property_list(properties: &ThingTypeProperties) -> Option<String> {
    let entry_counts = [
        (
            "searchableAttributes",
            properties.searchable_attributes.as_ref().map_or(0, Vec::len),
        ),
        (
            "mqtt5Configuration.propagatingAttributes",
            properties
                .mqtt5_configuration
                .as_ref()
                .and_then(|configuration| configuration.propagating_attributes.as_ref())
                .map_or(0, Vec::len),
        ),
    ];
    entry_counts
        .into_iter()
        .find(|(_, count)| *count > MAX_THING_TYPE_PROPERTY_ENTRIES)
        .map(|(member, count)| {
            format!(
                "a thing type's {member} may carry at most {MAX_THING_TYPE_PROPERTY_ENTRIES} \
                 entries; this request carries {count}"
            )
        })
}

/// What an update asks the registry to change about one thing type: the members the
/// request PROVIDED, each in the form the record keeps it.
///
/// `None` is "the request said nothing about this", which leaves the stored value alone;
/// `Some` is the value the type carries afterwards — including an empty list, which is a
/// request that states the type has no such entries any more.
#[derive(Debug, Clone, Default)]
pub struct ThingTypeUpdate {
    /// `ThingTypeProperties$thingTypeDescription`, when the request carried one.
    pub description: Option<String>,
    /// `ThingTypeProperties$searchableAttributes`, when the request carried the list.
    pub searchable_attributes: Option<Vec<String>>,
    /// The `propagatingAttributes` of `ThingTypeProperties$mqtt5Configuration`, when the
    /// request carried that list. A request carrying an `mqtt5Configuration` that does not
    /// carry the list itself states nothing about the propagating attributes, the same way
    /// a request carrying no `mqtt5Configuration` at all does: the member is how the
    /// entries are carried.
    pub propagating_attributes: Option<Vec<PropagatingAttributeRecord>>,
}

impl From<ThingTypeProperties> for ThingTypeUpdate {
    fn from(properties: ThingTypeProperties) -> Self {
        Self {
            description: properties.thing_type_description,
            searchable_attributes: properties.searchable_attributes,
            propagating_attributes: properties
                .mqtt5_configuration
                .and_then(|configuration| configuration.propagating_attributes)
                .map(|attributes| {
                    attributes
                        .into_iter()
                        .map(PropagatingAttributeRecord::from)
                        .collect()
                }),
        }
    }
}

impl ThingTypeUpdate {
    /// Apply what was provided to `configuration`, leaving every member the request did
    /// not state as it stands.
    pub fn apply_to(self, configuration: &mut ThingTypeConfiguration) {
        if let Some(description) = self.description {
            configuration.description = Some(description);
        }
        if let Some(searchable_attributes) = self.searchable_attributes {
            configuration.searchable_attributes = searchable_attributes;
        }
        if let Some(propagating_attributes) = self.propagating_attributes {
            configuration.propagating_attributes = propagating_attributes;
        }
    }
}

/// Why a thing type cannot take an update — what
/// [`ComAmazonawsIotAwsiotserviceState::update_thing_type`] answers instead of the updated
/// record.
///
/// It is the registry's reason, not a wire error: the handler maps it onto the error its
/// own model declares, and the wording lives here so one rule is stated once.
#[derive(Debug, Clone)]
pub enum ThingTypeUpdateRefusal {
    /// `thingTypeName` names no thing type this account holds.
    NotRegistered(String),
}

impl ThingTypeUpdateRefusal {
    /// What the refusal reports, in the wording this service states it in.
    pub fn message(&self) -> String {
        match self {
            Self::NotRegistered(thing_type_name) => {
                not_found::missing_thing_type_message(thing_type_name)
            }
        }
    }
}

impl ComAmazonawsIotAwsiotserviceState {
    /// Apply `update` to the thing type registered as `thing_type_name`: the ONE registry
    /// write that changes a type's configuration after it was created.
    ///
    /// A name the account holds no type under is refused and writes nothing, so the
    /// registry never grows a record an update addressed. The type's IDENTITY — its name,
    /// its id and its ARN — and its creation and deprecation instants are not part of an
    /// update: the model carries no member for any of them, so the record keeps what it
    /// was created (and deprecated) with, and only the configuration a later
    /// `DescribeThingType` reports as `thingTypeProperties` moves.
    pub fn update_thing_type(
        &mut self,
        thing_type_name: &str,
        update: ThingTypeUpdate,
    ) -> Result<(), ThingTypeUpdateRefusal> {
        let thing_type = self.thing_types.get_mut(thing_type_name).ok_or_else(|| {
            ThingTypeUpdateRefusal::NotRegistered(thing_type_name.to_string())
        })?;
        update.apply_to(&mut thing_type.configuration);
        Ok(())
    }
}
