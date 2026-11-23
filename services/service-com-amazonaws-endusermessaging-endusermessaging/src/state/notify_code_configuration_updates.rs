// SPDX-License-Identifier: CC0-1.0

//! Applying an `UpdateNotifyCodeConfiguration` request to a stored configuration.
//!
//! The update shapes of this API are LOOSE twins of the shapes a create takes: every
//! member is optional, and the string and map members admit an empty value the create
//! refuses. That empty value is this API's clear sentinel, so one supplied structure
//! says three different things member by member — absent preserves the stored value, the
//! empty value clears it, and any other value replaces it. The merge lives here rather
//! than in the handler so the rule is stated once, for every channel and every member
//! that carries it.

use std::collections::HashMap;
use std::time::SystemTime;

use super::NotifyCodeConfigurationRecord;
use crate::ops::types::{
    ChannelParameters, CodeConfigurationParameters, NotifyParameters, TextParameters,
    UpdateChannelParameters, UpdateCodeConfigurationParameters, UpdateNotifyCodeConfigurationInput,
    UpdateNotifyParameters, UpdateTextParameters, UpdateVoiceParameters, UpdateWhatsAppParameters,
    VoiceParameters, WhatsAppParameters,
};

/// Apply `requested` to `record`.
///
/// Only the members the request supplies are considered, and the identifier it was
/// addressed by is not one of them: it is resolved before this is called. A call that
/// leaves every member at the value it already had changes nothing at all — `updatedAt`
/// included, since that instant reports when the configuration last changed rather than
/// when it was last written to.
///
/// A request carrying no updatable member at all is served and changes nothing: this API
/// documents the update as changing only the fields that you supply, so supplying none
/// changes none. That is why this update, unlike the brand-profile one
/// ([`super::nothing_to_update`]), does not refuse such a request: two of a notify code
/// configuration's updatable members are loose structures whose own members are all
/// optional, so carrying one of them is not in itself a request to change anything.
pub fn apply_notify_code_configuration_update(
    record: &mut NotifyCodeConfigurationRecord,
    requested: UpdateNotifyCodeConfigurationInput,
    now: SystemTime,
) {
    let mut changed = false;

    // The name is bounded `@length(1..64)` on the update as it is on the create, so it
    // carries no clear sentinel: a supplied name replaces the stored one.
    if let Some(name) = requested.notify_code_configuration_name {
        changed |= replaced(&mut record.name, name);
    }
    if let Some(enabled) = requested.deletion_protection_enabled {
        changed |= replaced(&mut record.deletion_protection_enabled, enabled);
    }
    if let Some(parameters) = requested.code_configuration_parameters {
        let merged = merged_code_configuration(
            record.code_configuration_parameters.as_ref(),
            parameters,
        );
        changed |= replaced(&mut record.code_configuration_parameters, merged);
    }
    if let Some(parameters) = requested.channel_parameters {
        let merged = merged_channels(record.channel_parameters.as_ref(), parameters);
        changed |= replaced(&mut record.channel_parameters, merged);
    }

    if changed {
        record.updated_at = now;
    }
}

/// Write `value` over `stored`, answering whether that was a change.
fn replaced<T: PartialEq>(stored: &mut T, value: T) -> bool {
    if *stored == value {
        return false;
    }
    *stored = value;
    true
}

/// The passcode policy a stored configuration holds once `requested` is applied.
///
/// Every member of the policy is a number or an enum value, so none of them carries a
/// clear sentinel: a supplied member replaces the stored value and an omitted one
/// preserves it. A supplied structure with no member set therefore asks for no change,
/// and leaves the stored policy exactly as it is — absent included.
fn merged_code_configuration(
    stored: Option<&CodeConfigurationParameters>,
    requested: UpdateCodeConfigurationParameters,
) -> Option<CodeConfigurationParameters> {
    let mut merged = stored.cloned().unwrap_or_default();
    if let Some(code_type) = requested.code_type {
        merged.code_type = Some(code_type);
    }
    if let Some(code_length) = requested.code_length {
        merged.code_length = Some(code_length);
    }
    if let Some(validity_period_minutes) = requested.validity_period_minutes {
        merged.validity_period_minutes = Some(validity_period_minutes);
    }
    if let Some(max_attempts) = requested.max_attempts {
        merged.max_attempts = Some(max_attempts);
    }
    // A policy with no member set is not a policy at all: a supplied structure that
    // leaves every member absent asks for no change, so the stored value stands, absent
    // included.
    carrying_a_value(merged).or_else(|| stored.cloned())
}

/// The channel parameters a stored configuration holds once `requested` is applied.
///
/// A channel the request omits is left as it is. A channel it supplies is merged member
/// by member, and a channel whose every member ends up cleared holds nothing to render
/// and is dropped — as is the whole structure once its last channel is.
fn merged_channels(
    stored: Option<&ChannelParameters>,
    requested: UpdateChannelParameters,
) -> Option<ChannelParameters> {
    let mut merged = stored.cloned().unwrap_or_default();
    if let Some(text) = requested.text {
        merged.text = merged_text(merged.text, text);
    }
    if let Some(voice) = requested.voice {
        merged.voice = merged_voice(merged.voice, voice);
    }
    if let Some(notify) = requested.notify {
        merged.notify = merged_notify(merged.notify, notify);
    }
    if let Some(whats_app) = requested.whats_app {
        merged.whats_app = merged_whats_app(merged.whats_app, whats_app);
    }
    // The structure goes with its last channel: once every channel is dropped there is
    // nothing left to report.
    carrying_a_value(merged)
}

/// The text channel's parameters once `requested` is applied, or `None` once every
/// member of them is cleared.
fn merged_text(
    stored: Option<TextParameters>,
    requested: UpdateTextParameters,
) -> Option<TextParameters> {
    let mut merged = stored.unwrap_or_default();
    merge_member(&mut merged.inline_template_body, requested.inline_template_body);
    merge_member(
        &mut merged.destination_country_parameters,
        requested.destination_country_parameters,
    );
    carrying_a_value(merged)
}

/// The voice channel's parameters once `requested` is applied, or `None` once every
/// member of them is cleared.
fn merged_voice(
    stored: Option<VoiceParameters>,
    requested: UpdateVoiceParameters,
) -> Option<VoiceParameters> {
    let mut merged = stored.unwrap_or_default();
    merge_member(&mut merged.inline_template_body, requested.inline_template_body);
    merge_member(&mut merged.language_code, requested.language_code);
    merge_member(&mut merged.voice_id, requested.voice_id);
    merge_member(
        &mut merged.voice_message_body_text_type,
        requested.voice_message_body_text_type,
    );
    carrying_a_value(merged)
}

/// The preapproved notify route's parameters once `requested` is applied, or `None`
/// once every member of them is cleared.
fn merged_notify(
    stored: Option<NotifyParameters>,
    requested: UpdateNotifyParameters,
) -> Option<NotifyParameters> {
    let mut merged = stored.unwrap_or_default();
    merge_member(&mut merged.notify_template_id, requested.notify_template_id);
    merge_member(&mut merged.voice_id, requested.voice_id);
    carrying_a_value(merged)
}

/// The WhatsApp channel's parameters once `requested` is applied, or `None` once every
/// member of them is cleared.
fn merged_whats_app(
    stored: Option<WhatsAppParameters>,
    requested: UpdateWhatsAppParameters,
) -> Option<WhatsAppParameters> {
    let mut merged = stored.unwrap_or_default();
    merge_member(
        &mut merged.whats_app_template_name,
        requested.whats_app_template_name,
    );
    merge_member(&mut merged.language_code, requested.language_code);
    carrying_a_value(merged)
}

/// `merged`, or `None` when it holds no member at all — which is what the structure's
/// own default is.
fn carrying_a_value<T: PartialEq + Default>(merged: T) -> Option<T> {
    (merged != T::default()).then_some(merged)
}

/// Apply one member of a supplied channel to the value stored for it: an absent value
/// preserves the stored one, this API's empty sentinel clears it, and any other value
/// replaces it.
fn merge_member<T: ClearSentinel>(stored: &mut Option<T>, requested: Option<T>) {
    if let Some(value) = requested {
        *stored = (!value.is_clear_sentinel()).then_some(value);
    }
}

/// A value an update can carry in place of a member it wants cleared.
///
/// The update-only twin of each channel structure widens its string members to
/// `@length(0..)` and its map member to `@length(0..)`, and that empty value is the only
/// thing those twins admit that the create's shapes do not — so it is the sentinel, for
/// every member of every channel.
trait ClearSentinel {
    fn is_clear_sentinel(&self) -> bool;
}

impl ClearSentinel for String {
    fn is_clear_sentinel(&self) -> bool {
        self.is_empty()
    }
}

impl ClearSentinel for HashMap<String, String> {
    fn is_clear_sentinel(&self) -> bool {
        self.is_empty()
    }
}
