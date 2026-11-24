// SPDX-License-Identifier: CC0-1.0

//! The ONE rule every operation that WRITES a channel's configuration validates
//! that configuration against, BEFORE anything is stored.
//!
//! `CreateChannel` and `UpdateChannel` both write the members a channel encodes
//! and ingests through, so the rule lives here rather than in either handler: the
//! two cannot then accept a configuration the other refuses, exactly as
//! [`super::ComAmazonawsMedialiveMedialiveState::input_configuration_error`] is
//! the one rule `CreateInput` and `UpdateInput` share.
//!
//! What the rule checks:
//!
//! * a channel configuration has to declare the encoder settings the channel
//!   encodes through (`EncoderSettings` is "Encoder Settings"), so a request that
//!   declares none describes no channel this service could run;
//! * every id the write ADDS to the configuration has to resolve to a resource
//!   this account holds — a `ChannelSecurityGroups` entry is one of the "IDs for
//!   all the Input Security Groups attached to the channel", and
//!   `AnywhereSettings` carries "The ID of the cluster for the channel" and "The
//!   ID of the channel placement group for the channel" — and an id no such
//!   resource of this account answers to would
//!   leave the channel claiming a resource no read of the account can show (and
//!   the resource-side derivation of the channels using it —
//!   [`super::ComAmazonawsMedialiveMedialiveState::channels_using_security_group`],
//!   [`super::ComAmazonawsMedialiveMedialiveState::channels_in_cluster`],
//!   [`super::ComAmazonawsMedialiveMedialiveState::channels_in_channel_placement_group`]
//!   — matching nothing);
//! * an `InputAttachment` that names NO input at all
//!   (`InputAttachment$InputId` is "The ID of the input") attaches nothing, so it
//!   declares no ingest the channel could run; the input an attachment DOES name
//!   is resolved beside this rule, as a fact about the request itself
//!   ([`super::ComAmazonawsMedialiveMedialiveState::attached_input_refusal`]);
//! * a reference that is only reachable THROUGH another is resolved against the
//!   resource that one names: a channel placement group is named by
//!   (`ClusterId`, `ChannelPlacementGroupId`) together
//!   ([`super::ComAmazonawsMedialiveMedialiveState::channel_placement_group`]),
//!   so a group id is resolved inside the cluster the same settings name and
//!   settings that name a group but no cluster name a group through no cluster.
//!
//! Each problem found is reported as ONE
//! `UnprocessableEntityException$ValidationErrors` entry ("A collection of
//! validation error responses") naming the element path it was found at
//! (`ValidationError$ElementPath` is "Path to the source of the error") — the
//! channel this service already reports a request it cannot process through
//! (`BatchUpdateSchedule`, `CreateChannelPlacementGroup`).

use super::{
    attached_input_state_message, blank_reference_id_message, missing_reference_id_message,
    not_found_message, required_channel_configuration_message, unknown_reference_message,
    validation_problem, ChannelRecord, ComAmazonawsMedialiveMedialiveState,
};
use crate::ops::types::{AnywhereSettings, EncoderSettings, InputAttachment, ValidationError};

/// The channel configuration a write would leave on a channel.
///
/// Borrowed, and assembled by the CALLER: a create's configuration is the
/// request's own, an update's is the members its request names over the ones the
/// stored channel already had, and the same rule checks both.
#[derive(Debug)]
pub struct ChannelConfiguration<'a> {
    /// `EncoderSettings` — what the channel encodes.
    pub encoder_settings: Option<&'a EncoderSettings>,
    /// `InputAttachments` — "List of input attachments for channel".
    pub input_attachments: &'a [InputAttachment],
    /// `ChannelSecurityGroups` — "A list of IDs for all the Input Security
    /// Groups attached to the channel".
    pub channel_security_groups: &'a [String],
    /// `AnywhereSettings` — where the channel runs: the cluster and the channel
    /// placement group inside it.
    pub anywhere_settings: Option<&'a AnywhereSettings>,
    /// The channel as the store HOLDS it — `None` for a create, which ADDS
    /// every reference it names.
    ///
    /// An id this record already names is part of the configuration the channel
    /// already claims, so the rule resolves only the ids a write ADDS: a write
    /// that leaves a reference list alone is never refused for it, however the
    /// resource it names has fared since.
    pub held: Option<&'a ChannelRecord>,
}

impl ChannelConfiguration<'_> {
    /// Whether the stored channel already attaches the input `id`.
    fn holds_input(&self, id: &str) -> bool {
        self.held.is_some_and(|held| {
            held.input_attachments
                .iter()
                .any(|attachment| attachment.input_id.as_deref() == Some(id))
        })
    }

    /// Whether the stored channel already names the input security group `id`.
    fn holds_security_group(&self, id: &str) -> bool {
        self.held
            .is_some_and(|held| held.channel_security_groups.iter().any(|group| group == id))
    }

    /// The `AnywhereSettings` the stored channel is already placed by, which is
    /// what makes its cluster and its channel placement group ids ones this
    /// write does not add.
    fn held_placement(&self) -> Option<&AnywhereSettings> {
        self.held.and_then(|held| held.anywhere_settings.as_ref())
    }
}

/// The member a channel's encoder settings arrive in, as the wire spells it.
const ENCODER_SETTINGS: &str = "encoderSettings";

/// The member a channel's input attachments arrive in, as the wire spells it.
const INPUT_ATTACHMENTS: &str = "inputAttachments";

/// The family label an `InputAttachment`'s `InputId` names, as every refusal
/// about that family reports it ([`super::not_found_message`]).
const INPUT: &str = "Input";

/// The member a channel's input security groups arrive in, as the wire spells
/// it.
const CHANNEL_SECURITY_GROUPS: &str = "channelSecurityGroups";

/// The family label a `ChannelSecurityGroups` entry names, as every refusal
/// about that family reports it — the input side words the same reference the
/// same way
/// ([`super::ComAmazonawsMedialiveMedialiveState::input_configuration_error`]).
const INPUT_SECURITY_GROUP: &str = "InputSecurityGroup";

/// The member a channel's Elemental Anywhere placement arrives in, as the wire
/// spells it — the prefix of the element path each of its two ids is reported
/// at.
const ANYWHERE_SETTINGS: &str = "anywhereSettings";

/// The `AnywhereSettings` member naming the cluster the channel runs in, as the
/// wire spells it.
const CLUSTER_ID: &str = "clusterId";

/// The family label `AnywhereSettings$ClusterId` names, as every refusal about
/// that family reports it.
const CLUSTER: &str = "Cluster";

/// The `AnywhereSettings` member naming the group the channel is placed in, as
/// the wire spells it.
const CHANNEL_PLACEMENT_GROUP_ID: &str = "channelPlacementGroupId";

/// The family label `AnywhereSettings$ChannelPlacementGroupId` names, as every
/// refusal about that family reports it.
const CHANNEL_PLACEMENT_GROUP: &str = "ChannelPlacementGroup";

impl ComAmazonawsMedialiveMedialiveState {
    /// Every problem `configuration` carries, in the order the configuration
    /// declares its members and its entries — empty for a configuration this
    /// service can run, which is the only one a write stores.
    ///
    /// A reference is resolved through the same lookup every other write that
    /// references the resource goes through — [`Self::live_input`] for an
    /// attachment, [`Self::live_input_security_group`] for a channel security
    /// group, [`Self::live_cluster`] and [`Self::channel_placement_group`] for
    /// the Elemental Anywhere placement (the lookups `CreateNode` and
    /// `CreateChannelPlacementGroup` resolve the same two ids through) — so a
    /// resource whose delete this service has accepted is no longer one of this
    /// account a new reference can name.
    ///
    /// Only the ids the write ADDS are resolved: an id the stored channel
    /// already names ([`ChannelConfiguration::held`]) is part of the
    /// configuration that channel already claims, so a write that leaves a
    /// reference list alone cannot be refused for it.
    pub fn channel_configuration_problems(
        &self,
        configuration: &ChannelConfiguration<'_>,
    ) -> Vec<ValidationError> {
        let mut problems: Vec<ValidationError> = Vec::new();

        if configuration.encoder_settings.is_none() {
            problems.push(validation_problem(
                ENCODER_SETTINGS,
                &required_channel_configuration_message(ENCODER_SETTINGS),
            ));
        }

        for (index, attachment) in configuration.input_attachments.iter().enumerate() {
            // An attachment that names no input at all attaches nothing, so the
            // configuration declares an ingest it could not run, reported the
            // way every other reference list reports an entry with no id. The
            // input an attachment DOES name is resolved by
            // [`Self::attached_input_refusal`] instead, as a problem with the
            // REQUEST, so it is never also an entry here.
            if attachment
                .input_id
                .as_deref()
                .is_none_or(|id| id.is_empty())
            {
                problems.push(validation_problem(
                    &format!("{INPUT_ATTACHMENTS}/{index}/inputId"),
                    &missing_reference_id_message(INPUT_ATTACHMENTS),
                ));
            }
        }

        for (index, group) in configuration.channel_security_groups.iter().enumerate() {
            let path = format!("{CHANNEL_SECURITY_GROUPS}/{index}");
            if group.is_empty() {
                problems.push(validation_problem(
                    &path,
                    &missing_reference_id_message(CHANNEL_SECURITY_GROUPS),
                ));
            } else if self.live_input_security_group(group).is_none()
                && !configuration.holds_security_group(group)
            {
                problems.push(validation_problem(
                    &path,
                    &unknown_reference_message(INPUT_SECURITY_GROUP, group),
                ));
            }
        }

        if let Some(settings) = configuration.anywhere_settings {
            self.collect_anywhere_problems(configuration, settings, &mut problems);
        }

        problems
    }

    /// The refusal the inputs `configuration`'s attachments name earn, worded as
    /// this service words a problem with the REQUEST — `None` when every
    /// attachment names an input a channel may be fed by.
    ///
    /// An `InputAttachment`'s `InputId` is "The ID of the input", so the
    /// attachment list is resolved as part of the request itself: an id this
    /// account holds no input for names no input at all
    /// ([`super::not_found_message`]), and an id it DOES hold a record for whose
    /// delete this service has ACCEPTED names a real, identified input in a
    /// state no channel can ingest through
    /// ([`super::attached_input_state_message`]). Both are facts about the
    /// request rather than about the configuration it would store, so both are
    /// answered as the operation's modelled `BadRequestException`, naming the
    /// input the caller supplied and what is wrong with it, rather than as one
    /// more `ValidationErrors` entry: a caller branching on the status is told
    /// to fix the request instead of being handed a semantic validation report.
    ///
    /// Only the attachments the write ADDS are resolved, for the same reason
    /// [`Self::channel_configuration_problems`] resolves only those: an
    /// attachment the stored channel already holds is part of the configuration
    /// that channel already claims, so an edit of another member is not refused
    /// for it.
    ///
    /// ONE helper, so `CreateChannel` and `UpdateChannel` — which attach inputs
    /// under the same rule — cannot answer the same request two ways.
    pub fn attached_input_refusal(
        &self,
        configuration: &ChannelConfiguration<'_>,
    ) -> Option<String> {
        configuration.input_attachments.iter().find_map(|attachment| {
            let id = attachment.input_id.as_deref().filter(|id| !id.is_empty())?;
            if configuration.holds_input(id) {
                return None;
            }
            match self.inputs.get(id) {
                None => Some(not_found_message(INPUT, id)),
                Some(input) if input.is_delete_accepted() => Some(attached_input_state_message(
                    id,
                    self.input_state(input),
                )),
                Some(_) => None,
            }
        })
    }

    /// Every problem the `AnywhereSettings` of a channel configuration carries,
    /// appended to `problems`.
    ///
    /// The cluster is resolved first, through the same
    /// [`Self::live_cluster`] every write addressed to a cluster goes through,
    /// and the group is then resolved INSIDE it through the same
    /// [`Self::channel_placement_group`] a read of that group goes through — a
    /// group is named by both ids together, so a group of another cluster
    /// resolves no better than one the account never created, and a group whose
    /// delete this service has accepted is no longer one a new placement can
    /// name. A group id whose cluster did not resolve is not reported a second
    /// time: the cluster problem already names what the caller must change.
    ///
    /// Only the placement a write ADDS is resolved: the pair the stored channel
    /// is already placed by
    /// ([`ChannelConfiguration::held_placement`]) is part of the configuration
    /// the channel already claims.
    fn collect_anywhere_problems(
        &self,
        configuration: &ChannelConfiguration<'_>,
        settings: &AnywhereSettings,
        problems: &mut Vec<ValidationError>,
    ) {
        let named_cluster = settings.cluster_id.as_deref();
        let named_group = settings.channel_placement_group_id.as_deref();
        let held = configuration.held_placement();
        let holds_cluster =
            |id: &str| held.and_then(|held| held.cluster_id.as_deref()) == Some(id);
        let holds_group = |cluster_id: &str, group_id: &str| {
            holds_cluster(cluster_id)
                && held.and_then(|held| held.channel_placement_group_id.as_deref())
                    == Some(group_id)
        };
        let cluster_path = format!("{ANYWHERE_SETTINGS}/{CLUSTER_ID}");

        let cluster = match named_cluster.filter(|id| !id.is_empty()) {
            // Settings that carry a blank cluster id, or that place the channel
            // in a group without naming the cluster that group lives in, name
            // no cluster at all — and a channel with no cluster is no channel
            // this account's clusters can report.
            None if named_cluster.is_some() || named_group.is_some() => {
                problems.push(validation_problem(
                    &cluster_path,
                    &blank_reference_id_message(CLUSTER_ID),
                ));
                None
            }
            None => None,
            Some(id) if self.live_cluster(id).is_none() && !holds_cluster(id) => {
                problems.push(validation_problem(
                    &cluster_path,
                    &unknown_reference_message(CLUSTER, id),
                ));
                None
            }
            Some(id) => Some(id),
        };

        let Some(group_id) = named_group else {
            return;
        };
        let group_path = format!("{ANYWHERE_SETTINGS}/{CHANNEL_PLACEMENT_GROUP_ID}");
        if group_id.is_empty() {
            problems.push(validation_problem(
                &group_path,
                &blank_reference_id_message(CHANNEL_PLACEMENT_GROUP_ID),
            ));
            return;
        }
        let Some(cluster_id) = cluster else {
            return;
        };
        if self
            .channel_placement_group(cluster_id, group_id)
            .is_none_or(|group| group.is_delete_accepted())
            && !holds_group(cluster_id, group_id)
        {
            problems.push(validation_problem(
                &group_path,
                &unknown_reference_message(CHANNEL_PLACEMENT_GROUP, group_id),
            ));
        }
    }
}
