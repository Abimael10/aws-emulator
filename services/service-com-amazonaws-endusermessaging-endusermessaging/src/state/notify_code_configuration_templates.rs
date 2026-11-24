// SPDX-License-Identifier: CC0-1.0

//! What a supplied one-time-passcode template body has to carry.
//!
//! Both freeform channels of this API — the text channel and the voice channel —
//! document their `inlineTemplateBody` as "the freeform message template used to render
//! the one-time passcode", which "must contain the code placeholder". A body without
//! that placeholder can never render a passcode, so it is refused where it is supplied
//! rather than stored and left to fail at send time.
//!
//! The rule is the MEMBER's, not one operation's: the same body arrives on a create, on
//! an update and on a send's channel overrides, so the gate is stated once here and
//! every handler that accepts channel parameters calls it before writing anything.

use crate::ops::types::{ChannelParameters, UpdateChannelParameters};

/// The placeholder a template body renders the one-time passcode at, as
/// `TextParameters$inlineTemplateBody` and `VoiceParameters$inlineTemplateBody` document
/// ("the code placeholder").
pub const CODE_PLACEHOLDER: &str = "{{code}}";

/// The member a `CreateNotifyCodeConfiguration` or `UpdateNotifyCodeConfiguration`
/// request carries its channel parameters on.
pub const CHANNEL_PARAMETERS_MEMBER: &str = "channelParameters";

/// The member a `SendNotifyCodeVerification` request carries its channel-parameter
/// overrides on.
pub const OVERRIDE_CHANNEL_PARAMETERS_MEMBER: &str = "overrideChannelParameters";

/// The channel member each freeform template body arrives under, as the wire names it.
const TEXT_CHANNEL: &str = "text";
const VOICE_CHANNEL: &str = "voice";

/// The member of `inlineTemplateBody`'s own structures, as the wire names it.
const INLINE_TEMPLATE_BODY: &str = "inlineTemplateBody";

/// Refuse `parameters` when a template body it supplies cannot render a passcode.
///
/// `member` is the input member the structure arrived on, so the refusal names the
/// member at fault wherever it was supplied from.
pub fn channel_templates_render_a_code(
    member: &str,
    parameters: &ChannelParameters,
) -> Result<(), crate::ops::types::ValidationException> {
    rendering_a_code(
        member,
        [
            (
                TEXT_CHANNEL,
                parameters
                    .text
                    .as_ref()
                    .and_then(|text| text.inline_template_body.as_deref()),
            ),
            (
                VOICE_CHANNEL,
                parameters
                    .voice
                    .as_ref()
                    .and_then(|voice| voice.inline_template_body.as_deref()),
            ),
        ],
    )
}

/// Refuse an update's `parameters` when a template body it supplies cannot render a
/// passcode.
///
/// The update twins of the channel structures widen the body to `@length(0..)`, and that
/// empty value is this API's clear sentinel: it stores no template at all, so there is no
/// body for the placeholder to be missing from and it is not refused here.
pub fn update_channel_templates_render_a_code(
    member: &str,
    parameters: &UpdateChannelParameters,
) -> Result<(), crate::ops::types::ValidationException> {
    rendering_a_code(
        member,
        [
            (
                TEXT_CHANNEL,
                parameters
                    .text
                    .as_ref()
                    .and_then(|text| text.inline_template_body.as_deref()),
            ),
            (
                VOICE_CHANNEL,
                parameters
                    .voice
                    .as_ref()
                    .and_then(|voice| voice.inline_template_body.as_deref()),
            ),
        ],
    )
}

/// The one gate both forms of the channel structures reach: every supplied body has to
/// carry [`CODE_PLACEHOLDER`], and the channels are judged in the order the structure
/// declares them so one request has one refusal.
fn rendering_a_code(
    member: &str,
    bodies: [(&str, Option<&str>); 2],
) -> Result<(), crate::ops::types::ValidationException> {
    for (channel, body) in bodies {
        let Some(body) = body else { continue };
        if body.is_empty() || body.contains(CODE_PLACEHOLDER) {
            continue;
        }
        return Err(template_without_a_code(member, channel));
    }
    Ok(())
}

/// `ValidationException` for a supplied template body that carries no code placeholder.
///
/// Entirely fixed text: what it reports is that the body does not carry the one thing
/// every passcode template has to, so it names the placeholder and nothing of the
/// caller's — the body is `@sensitive`, and the member it came in on is what the caller
/// needs to correct it. `fieldList` names that member as the JSON pointer of the nested
/// member in the request document, which is what `ValidationExceptionField$path`
/// documents and how this service names every nested member it refuses.
fn template_without_a_code(member: &str, channel: &str) -> crate::ops::types::ValidationException {
    let stated = format!("template body must contain {CODE_PLACEHOLDER} placeholder");
    crate::ops::types::ValidationException {
        message: stated.clone(),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: ::service_common::json_pointer::json_pointer(&format!(
                "{member}.{channel}.{INLINE_TEMPLATE_BODY}"
            )),
            message: stated,
        }]),
    }
}
