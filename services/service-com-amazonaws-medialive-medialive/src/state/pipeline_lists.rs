// SPDX-License-Identifier: CC0-1.0

//! The ONE rule every write of a PER-PIPELINE list validates that list against,
//! before anything is stored.
//!
//! MediaLive runs one encoding pipeline per pipeline of a resource's class — the
//! `ChannelClass` doc: "A standard channel has two encoding pipelines and a
//! single pipeline channel only has one", and the `InputClass` doc: "A standard
//! input has two sources and a single pipeline input only has one" — so a list
//! the model documents as carrying ONE ENTRY PER PIPELINE is bounded by that
//! count and by nothing the caller chooses:
//!
//! * a channel destination's `Settings` is "Destination settings for a standard
//!   output; one destination for each redundant encoder", so it carries exactly
//!   one entry per pipeline of the channel's class — a count neither the member
//!   itself nor any other single-member constraint can express, because it
//!   depends on `ChannelClass`
//!   ([`channel_destination_settings_error`]);
//! * a pushed-to input's `Destinations` is the ingest endpoint each pipeline is
//!   pushed to, so it carries at most the [`PIPELINES_PER_STANDARD_CLASS`]
//!   entries the two-pipeline class has ([`push_destination_count_error`]).
//!
//! Both are cross-field rules the model states no constraint for, and both are
//! checked by the operations that WRITE the list, before anything is stored, so a
//! request that declares more per-pipeline entries than the resource has
//! pipelines creates nothing. The destinations a MediaPackage or a multiplex
//! output declares are deliberately NOT bounded this way: the model documents
//! each of them as "one destination for both encoders", so their count does not
//! follow the pipeline count. The other per-encoder output families a destination
//! can declare (`SrtSettings`, `MediaConnectRouterSettings`) are left alone for a
//! reason of wording rather than of counting: the sentence this service answers a
//! miscounted destination with
//! ([`destination_settings_message`]) names no member, so stating it for a
//! different member would put a count on a member the refusal does not name.

use super::{class_pipelines, push_ingest_protocol, InputRecord};
use crate::ops::types::OutputDestination;

/// The pipelines the two-pipeline class has, which is the most pipelines any
/// resource of this service runs: the `STANDARD` class of
/// [`super::channel_class`] and of [`super::input_class`], whose docs are what
/// put two encoding pipelines (and two ingest sources) on it.
///
/// The same count [`class_pipelines`] answers for that class; a `const` here
/// because it is the bound a list is checked against before any class is known.
pub const PIPELINES_PER_STANDARD_CLASS: usize = 2;

/// The message a channel write refused because one of its destinations declares
/// a number of per-encoder settings other than the channel's pipeline count
/// carries: the count the class requires and the destination's own `Id` ("User-
/// specified id. This is used in an output group or an output"), both out of the
/// request.
pub fn destination_settings_message(required: usize, destination_id: &str) -> String {
    format!("Exactly {required} destinations settings required for destination {destination_id}")
}

/// The message a write refused because the pushed-to input it would leave behind
/// declares a number of ingest destinations the pipelines cannot be fed through
/// carries.
///
/// FIXED text: the refusal states the bound a push input's `Destinations` has,
/// and neither the count the request carried nor the destinations in it change
/// what has to be true.
pub const PUSH_DESTINATION_COUNT_MESSAGE: &str =
    "At least one destination and at most two destinations required";

/// The refusal the `Destinations` of a channel of `channel_class` earns, or
/// `None` when every destination declares one per-encoder setting per pipeline
/// of that class.
///
/// A destination that declares NO `Settings` declares no standard output, so the
/// per-encoder count says nothing about it: its output travels in one of the
/// other members (`MediaPackageSettings`, `MultiplexSettings`, …), each of which
/// the model gives a count of its own. A destination that DOES declare the member
/// is checked against the pipeline count, in both directions — a single-pipeline
/// channel whose destination carries two settings feeds a pipeline it does not
/// have, and a standard channel whose destination carries one leaves its second
/// pipeline with nowhere to send its output.
pub fn channel_destination_settings_error(
    channel_class: &str,
    destinations: &[OutputDestination],
) -> Option<String> {
    let required = class_pipelines(channel_class).count();
    destinations.iter().find_map(|destination| {
        let settings = destination.settings.as_deref().unwrap_or_default();
        (!settings.is_empty() && settings.len() != required).then(|| {
            destination_settings_message(
                required,
                destination.id.as_deref().unwrap_or_default(),
            )
        })
    })
}

/// The refusal the `Destinations` of the input `input` would leave behind earns,
/// or `None` when that list is one the input's pipelines can be fed through.
///
/// Only a pushed-to input has such a list — `Destinations` is "Destination
/// settings for PUSH type inputs", one ingest endpoint per pipeline
/// ([`super::PUSH_INGEST_PROTOCOLS`] is the table of the types that are pushed
/// to) — so an input of any other type names no per-pipeline destination and is
/// not bounded here.
pub fn push_destination_count_error(input: &InputRecord) -> Option<String> {
    push_ingest_protocol(input.input_type.as_deref())?;
    (input.destinations.len() > PIPELINES_PER_STANDARD_CLASS)
        .then(|| PUSH_DESTINATION_COUNT_MESSAGE.to_string())
}
