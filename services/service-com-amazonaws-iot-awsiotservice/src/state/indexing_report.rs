// SPDX-License-Identifier: CC0-1.0

//! What a READER of the account's fleet-indexing configuration reports on top of what
//! the last writer stored: the fields the Fleet Indexing service manages itself, and
//! the filter structure every thing-indexing configuration has.
//!
//! The store keeps exactly what `UpdateIndexingConfiguration` was given
//! ([`super::IndexingConfiguration`]) — "You can't modify managed fields by updating
//! fleet indexing configuration"
//! (`smithy_query(query="doc",shape="ThingIndexingConfiguration",member="managedFields")`),
//! so a caller's own `managedFields` list is dropped on the way in and the service's
//! own is added on the way out. The addition is a PROJECTION of the stored
//! configuration: it reads the stored modes and nothing else, writes nothing, and is
//! therefore the same for two reads with no update between them.
//!
//! Two members of the reported configuration are the service's, not the caller's:
//!
//! * `managedFields` — "Contains fields that are indexed and whose types are already
//!   known by the Fleet Indexing service" — which a section whose indexing is
//!   disabled has none of, because nothing is indexed for it; the enabled DATA
//!   SOURCES decide the list ([`REGISTRY_THING_FIELDS`],
//!   [`CONNECTIVITY_THING_FIELDS`], [`THING_GROUP_FIELDS`]);
//! * `filter` — "Provides additional selections for named shadows and geolocation
//!   data" — which every thing-indexing configuration HAS: it is reported as the
//!   structure it is rather than omitted, so a client reading the configuration to
//!   edit it and write it back (the read-modify-write `UpdateIndexingConfiguration`
//!   documents) finds the member and can tell "no selections" from "not modelled".
//!   An enabled index reports each selection, and a selection nothing was added to is
//!   the empty one; a disabled index has no selections at all.

use crate::ops::types::{
    ConnectivityFilter, Field, IndexingFilter, ThingGroupIndexingConfiguration,
    ThingIndexingConfiguration,
};

use super::fleet_metrics::{FIELD_TYPE_BOOLEAN, FIELD_TYPE_NUMBER, FIELD_TYPE_STRING};
use super::INDEXING_MODE_OFF;

/// The managed fields of the THING index the REGISTRY data source contributes: the
/// thing's identity and the registry members every thing carries, with the
/// `FieldType` the Fleet Indexing service knows each by.
///
/// They are the managed-field table AWS IoT fleet indexing publishes for the registry
/// section of the thing index, in the order the service lists them. The same members
/// are what this emulator's own index documents carry for a thing
/// (`super::fleet_search`), which is why an aggregation may name one without the
/// account declaring anything ([`super::fleet_metrics::MANAGED_THING_FIELDS`]).
pub const REGISTRY_THING_FIELDS: &[(&str, &str)] = &[
    ("registry.version", FIELD_TYPE_NUMBER),
    ("thingName", FIELD_TYPE_STRING),
    ("thingId", FIELD_TYPE_STRING),
    ("registry.thingTypeName", FIELD_TYPE_STRING),
    ("registry.thingGroupNames", FIELD_TYPE_STRING),
];

/// The managed fields of the THING index the CONNECTIVITY data source contributes —
/// where a thing's connection stands, when it last changed and why it ended.
///
/// They are in the index only while connectivity indexing is on: "STATUS – Your thing
/// index contains connectivity status"
/// (`smithy_query(query="doc",shape="ThingIndexingConfiguration",member="thingConnectivityIndexingMode")`),
/// so a configuration with that mode at [`INDEXING_MODE_OFF`] reports none of them.
pub const CONNECTIVITY_THING_FIELDS: &[(&str, &str)] = &[
    ("connectivity.connected", FIELD_TYPE_BOOLEAN),
    ("connectivity.timestamp", FIELD_TYPE_NUMBER),
    ("connectivity.disconnectReason", FIELD_TYPE_STRING),
];

/// The managed fields of the THING GROUP index: the group's identity, its place in the
/// hierarchy, its description and its registry version, in the order the service lists
/// them.
///
/// The group index has one data source — the thing-group registry — so these are its
/// managed fields whenever group indexing is on.
pub const THING_GROUP_FIELDS: &[(&str, &str)] = &[
    ("version", FIELD_TYPE_NUMBER),
    ("thingGroupId", FIELD_TYPE_STRING),
    ("parentGroupNames", FIELD_TYPE_STRING),
    ("description", FIELD_TYPE_STRING),
    ("thingGroupName", FIELD_TYPE_STRING),
];

/// Whether `mode` is one of the "this kind of indexing is disabled" values.
fn enabled(mode: &str) -> bool {
    mode != INDEXING_MODE_OFF
}

/// One catalog entry as the modelled `Field` a response carries.
fn field((name, field_type): &(&str, &str)) -> Field {
    Field {
        name: Some((*name).to_string()),
        r#type: Some((*field_type).to_string()),
    }
}

impl super::IndexingConfiguration {
    /// The configuration as a READER hands it back: the stored sections plus the
    /// members the service itself fills in (see this module's note).
    ///
    /// A projection, not a change: the store is left exactly as it was, so two reads
    /// with no update between them report the same configuration.
    pub fn reported(&self) -> Self {
        Self {
            thing: reported_thing(&self.thing),
            thing_group: reported_thing_group(&self.thing_group),
        }
    }
}

/// The thing section as a reader hands it back: what was stored, with `managedFields`
/// and `filter` filled in by the service.
fn reported_thing(stored: &ThingIndexingConfiguration) -> ThingIndexingConfiguration {
    ThingIndexingConfiguration {
        managed_fields: thing_managed_fields(stored),
        filter: Some(reported_filter(stored)),
        ..stored.clone()
    }
}

/// The thing group section as a reader hands it back, under the same rule — the group
/// index has no filter of its own, so only `managedFields` is added.
fn reported_thing_group(stored: &ThingGroupIndexingConfiguration) -> ThingGroupIndexingConfiguration {
    ThingGroupIndexingConfiguration {
        managed_fields: thing_group_managed_fields(stored),
        ..stored.clone()
    }
}

/// The managed fields of the THING index under `configuration`: the fields its ENABLED
/// data sources contribute, or none at all while thing indexing is disabled.
fn thing_managed_fields(configuration: &ThingIndexingConfiguration) -> Option<Vec<Field>> {
    if !enabled(&configuration.thing_indexing_mode) {
        return None;
    }
    let connectivity = configuration
        .thing_connectivity_indexing_mode
        .as_deref()
        .unwrap_or(INDEXING_MODE_OFF);
    let mut fields: Vec<Field> = REGISTRY_THING_FIELDS.iter().map(field).collect();
    if enabled(connectivity) {
        fields.extend(CONNECTIVITY_THING_FIELDS.iter().map(field));
    }
    Some(fields)
}

/// The managed fields of the THING GROUP index under `configuration`, under the same
/// rule: the group registry's fields while group indexing is on, none while it is off.
fn thing_group_managed_fields(
    configuration: &ThingGroupIndexingConfiguration,
) -> Option<Vec<Field>> {
    enabled(&configuration.thing_group_indexing_mode)
        .then(|| THING_GROUP_FIELDS.iter().map(field).collect())
}

/// The `filter` of the THING index under `configuration`: the selections the stored
/// configuration makes.
///
/// An index that is being built reports each of the three selections, so a selection
/// nothing was added to is the EMPTY selection rather than an absent member; a
/// disabled index indexes nothing, so it makes no selections at all.
fn reported_filter(configuration: &ThingIndexingConfiguration) -> IndexingFilter {
    let stored = configuration.filter.clone().unwrap_or_default();
    if !enabled(&configuration.thing_indexing_mode) {
        return stored;
    }
    IndexingFilter {
        named_shadow_names: Some(stored.named_shadow_names.unwrap_or_default()),
        geo_locations: Some(stored.geo_locations.unwrap_or_default()),
        connectivity: Some(ConnectivityFilter {
            include_socket_information: Some(
                stored
                    .connectivity
                    .and_then(|connectivity| connectivity.include_socket_information)
                    .unwrap_or_default(),
            ),
        }),
    }
}
