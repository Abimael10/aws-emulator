// SPDX-License-Identifier: CC0-1.0

//! The origination identity a one-time passcode is sent FROM.
//!
//! `SendOTPMessageRequestParameters$OriginationIdentity` is "the origination identity
//! used to send OTP from", and such an identity is not a resource of this service: a
//! number is leased to the account through the SMS and voice API, which owns the
//! account's inventory of them. This module is the one place that reads that inventory,
//! so an OTP send's verdict follows what the owning service actually holds instead of a
//! second copy kept here — a number released there stops being sendable from here.

use service_com_amazonaws_pinpointsmsvoicev2_pinpointsmsvoicev2 as sms_voice;

/// What this service reports for the destination of an OTP send whose
/// `OriginationIdentity` is not one the calling account can send from.
///
/// The sentence has NO variable part: it names neither the origination identity, nor
/// the destination, nor the application, so every refusal of this class reads exactly
/// the same — it classifies the fault as the number the request named rather than as
/// anything about the application's own configuration.
pub const ORIGINATION_IDENTITY_NOT_USABLE_MESSAGE: &str =
    "The provided number does not exist or does not belong to the account, or its \
     resource policy does not allow Pinpoint to use it";

/// Whether `identity` names an origination identity the calling account holds, and so
/// one this service may send a message from.
///
/// An identity is resolved the way the service that leases it resolves one: by its
/// identifier, by its ARN, or by the number itself (with or without its leading `+`).
/// Nothing is written: this only reads the inventory.
///
/// `None` means that inventory could not be reached at all, which is a server fault and
/// not an answer about the identity.
pub fn account_can_send_from<Eng: Send + Sync + 'static>(
    engine: &Eng,
    identity: &str,
) -> Option<bool> {
    let inventory = sms_voice::state::service_state(engine)?;
    let inventory = inventory.lock();
    Some(inventory.find_phone_number(identity).is_some())
}
