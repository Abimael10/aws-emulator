// SPDX-License-Identifier: CC0-1.0

//! The values the service APPLIES to a channel's configuration as it STORES it,
//! so every channel-shaped response reports a complete configuration.
//!
//! A channel is stored as the service completes it, not as one request spelled
//! it: the members a request left out but the service has a value for are filled
//! in HERE, by the operations that write a channel's configuration
//! (`CreateChannel`, `UpdateChannel`). Every projection of the stored record —
//! the `Channel` [`super::ComAmazonawsMedialiveMedialiveState::channel_description`]
//! builds for the describe, the delete, the create, the update, the start, the
//! stop and the pipeline restart, and the
//! [`super::ComAmazonawsMedialiveMedialiveState::channel_summary`] `ListChannels`
//! reports — then reports those values without each one re-deriving them.
//!
//! Two kinds of member are completed:
//!
//! * `LogLevel`, "The log level being written to CloudWatch Logs"
//!   (`smithy_query(query="shape", shape="Channel")`). A channel whose request
//!   chose no level writes none, and writing none is the `DISABLED` the enum
//!   declares (`smithy_query(query="shape", shape="LogLevel")`: DEBUG | DISABLED
//!   | ERROR | INFO | WARNING), so that is the level the service applies — a
//!   channel always reports the level it is writing at, rather than leaving a
//!   client that reads the member to guess whether logging is on.
//! * the COLLECTION members of the configuration shapes a channel projection
//!   carries. A collection the request supplied nothing for is stored EMPTY
//!   rather than unset, so a client that iterates
//!   `encoderSettings.captionDescriptions` or
//!   `inputAttachments[i].logicalInterfaceNames` walks a collection with no
//!   entries instead of a null one.
//!
//! The collections are named member by member rather than filled in shape by
//! shape: which of a shape's collections a channel projection carries is a fact
//! about the member, and the shapes here also carry collections the projection
//! does NOT report as collections (`OutputDestination$MediaConnectRouterSettings`
//! is one — the recorded AWS channel projection carries the three collections
//! beside it as empty lists and that one not at all), so only the members below
//! are completed.

use super::ChannelRecord;
use crate::ops::types::{EncoderSettings, InputAttachment, Output, OutputDestination, OutputGroup};

/// The log level a channel whose request chose none is stored with: it writes no
/// log, which is the level `LogLevel` declares for writing none.
pub const DEFAULT_LOG_LEVEL: &str = "DISABLED";

/// Complete the configuration of `channel` with the values the service applies
/// to every channel it stores.
///
/// Call this from the operations that WRITE a channel's configuration, AFTER the
/// members the request supplied are on the record: the completion is part of
/// storing the configuration, so no reporting operation does it and no reader
/// has to.
///
/// Idempotent — it only fills a member that holds nothing, so a later write that
/// goes through it again neither drops nor re-chooses a value the channel
/// already has.
pub fn apply_channel_configuration_defaults(channel: &mut ChannelRecord) {
    if channel.log_level.is_none() {
        channel.log_level = Some(DEFAULT_LOG_LEVEL.to_string());
    }
    for attachment in &mut channel.input_attachments {
        complete_input_attachment(attachment);
    }
    for destination in &mut channel.destinations {
        complete_destination(destination);
    }
    if let Some(encoder_settings) = channel.encoder_settings.as_mut() {
        complete_encoder_settings(encoder_settings);
    }
}

/// `InputAttachment$LogicalInterfaceNames` — "Optional assignment of an input to
/// a logical interface on the Node": an attachment that assigns none is attached
/// to no logical interface, which is an empty assignment rather than no member.
fn complete_input_attachment(attachment: &mut InputAttachment) {
    empty_rather_than_unset(&mut attachment.logical_interface_names);
}

/// The collections of `OutputDestination` a channel projection carries:
/// `MediaPackageSettings`, `Settings`, `SrtSettings` and
/// `LogicalInterfaceNames` — a destination that names none of a kind of setting
/// has an empty list of them.
fn complete_destination(destination: &mut OutputDestination) {
    empty_rather_than_unset(&mut destination.media_package_settings);
    empty_rather_than_unset(&mut destination.settings);
    empty_rather_than_unset(&mut destination.srt_settings);
    empty_rather_than_unset(&mut destination.logical_interface_names);
}

/// `EncoderSettings$CaptionDescriptions` — "Settings for caption decriptions" —
/// and, one level down, the collections of each output group. The encoder's
/// other collections (`AudioDescriptions`, `OutputGroups`,
/// `VideoDescriptions`) are required members the request always supplies, so
/// they are lists already.
fn complete_encoder_settings(settings: &mut EncoderSettings) {
    empty_rather_than_unset(&mut settings.caption_descriptions);
    for group in &mut settings.output_groups {
        complete_output_group(group);
    }
}

/// The collections of one output group: the two of its HLS group settings
/// (`AdMarkers`, "Choose one or more ad marker types to pass SCTE35 signals
/// through to this group of Apple HLS outputs", and
/// `CaptionLanguageMappings`, "Mapping of up to 4 caption channels to caption
/// languages"), and the collections of each of its outputs.
fn complete_output_group(group: &mut OutputGroup) {
    if let Some(hls) = group.output_group_settings.hls_group_settings.as_mut() {
        empty_rather_than_unset(&mut hls.ad_markers);
        empty_rather_than_unset(&mut hls.caption_language_mappings);
    }
    for output in &mut group.outputs {
        complete_output(output);
    }
}

/// The collections of one output: `AudioDescriptionNames` ("The names of the
/// AudioDescriptions used as audio sources for this output") and
/// `CaptionDescriptionNames` ("The names of the CaptionDescriptions used as
/// caption sources for this output") — an output that sources from none of
/// either names an empty list of them.
fn complete_output(output: &mut Output) {
    empty_rather_than_unset(&mut output.audio_description_names);
    empty_rather_than_unset(&mut output.caption_description_names);
}

/// Store an unset collection member as the EMPTY collection, and leave one that
/// already holds entries exactly as it is.
fn empty_rather_than_unset<T>(member: &mut Option<Vec<T>>) {
    if member.is_none() {
        *member = Some(Vec::new());
    }
}
