// SPDX-License-Identifier: CC0-1.0

//! Whether this account has anything to send an SMS message FROM.
//!
//! An SMS message leaves AWS from an ORIGINATION ENTITY — a short code, a toll-free
//! number, a ten-digit long code or another sender identity the account holds. The
//! inventory of the account's entities lives in
//! [`super::ComAmazonawsSnsAmazonsimplenotificationserviceState::origination_numbers`],
//! is acquired OUT OF BAND (no SNS API adds one, as
//! [`super::OriginationNumberRecord`] sets out) and is therefore EMPTY unless something
//! put a number there — which is what `ListOriginationNumbers` reports.
//!
//! An account holding no entity able to carry a text message has nothing to send one
//! from, so the SEND an operation asks for fails before it is attempted, and SNS reports
//! that failure to the caller as its modelled user error with the sentence
//! [`NO_ORIGINATION_ENTITIES`]. Only the SEND fails: whatever else the operation does
//! stands, because the message was the only part of it that needed an entity.
//!
//! This module holds what the inventory's records MEAN, so the reading does not depend
//! on which call asked.

/// The capability an origination entity has to carry for SNS to send an SMS message
/// from it: `SMS`, one of the values the modelled `NumberCapability` enum accepts
/// (`crate::ops::types::NUMBER_CAPABILITY_VALUES`). A number that can only place a
/// voice call or carry MMS cannot send a text message.
pub const SMS_CAPABILITY: &str = "SMS";

/// The sentence SNS states when it is asked to carry an SMS message for an account that
/// holds no origination entity able to send one.
///
/// WHOLLY FIXED text: nothing in it is taken from the request or from state — the
/// destination number of such a send is `@sensitive` and is not echoed, and the entity
/// that is missing has no name to give.
pub const NO_ORIGINATION_ENTITIES: &str = "No origination entities available to send";

/// Whether `record` can carry an SMS message: its capabilities name the
/// [`SMS_CAPABILITY`].
pub fn sends_sms(record: &super::OriginationNumberRecord) -> bool {
    record
        .number_capabilities
        .iter()
        .any(|capability| capability == SMS_CAPABILITY)
}

/// Whether this account holds ANY origination entity an SMS message could leave from.
///
/// Read from the inventory as it stands, so an entity acquired out of band is enough on
/// the next call and one taken away stops being: nothing here caches the answer.
pub fn can_send_sms(store: &super::ComAmazonawsSnsAmazonsimplenotificationserviceState) -> bool {
    store.origination_numbers.values().any(sends_sms)
}
