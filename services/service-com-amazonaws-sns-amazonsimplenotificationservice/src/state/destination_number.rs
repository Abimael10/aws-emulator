// SPDX-License-Identifier: CC0-1.0

//! What makes a value a DESTINATION phone number an SMS message could be sent to, and
//! how the SMS service SNS hands such a number to refuses one that is not.
//!
//! Two questions live here, because they are two readings of one value:
//!
//! * WHY a value is not a destination number — it is not written in E.164 form at all,
//!   or it is written in E.164 form but opens with a country code E.164 assigns to no
//!   country. [`super::is_e164`] is the yes/no form of the same verdict, so the clauses
//!   have ONE source for every operation that carries a phone-number member.
//! * WHICH sentence the refusal states. An operation that only reads or clears this
//!   account's own record of a number answers with SNS's own member refusal
//!   ([`super::invalid_phone_number_message`]). An operation that asks for a MESSAGE to
//!   be sent to the number hands it on to the SMS service that would carry it, as that
//!   service's own `destinationPhoneNumber` argument, and surfaces ITS refusal verbatim
//!   inside SNS's `Invalid parameter:` frame — so the message names the inner service,
//!   the inner member and the inner call's own request id, and never SNS's member name
//!   or the (sensitive) number the caller sent.

use super::*;

/// Why a value is not a destination phone number — the two faults the SMS service
/// distinguishes, which is why they are kept apart rather than collapsed into one
/// "malformed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationNumberFault {
    /// The value is not written in E.164 form at all: no leading `+`, something other
    /// than digits after it, or a digit count outside the [`MIN_E164_DIGITS`] to
    /// [`MAX_E164_DIGITS`] span an E.164 number is written in. Bare national digits and
    /// a short code are this fault — the modelled pattern of the shared phone-number
    /// shape admits them, and a destination an SMS could reach is not one of them.
    Form,
    /// The value IS written in E.164 form, but the country code it opens with is one
    /// E.164 assigns to no country, so it addresses no destination anywhere. ITU-T
    /// E.164 reserves the digit `0` as the first digit of the international prefix and
    /// assigns no country code beginning with it.
    CountryCode,
}

/// Whether the digits of an E.164 number open with a country code E.164 assigns.
///
/// Only the ASSIGNMENT of the leading digit is judged: ITU-T E.164 numbers a country
/// code `1`..`9`, never `0`. Being stricter than the published rule here would refuse
/// numbers the SMS service accepts, which is the opposite fault.
fn opens_with_assigned_country_code(digits: &str) -> bool {
    !digits.starts_with('0')
}

/// The fault that keeps `value` from being a destination phone number, or `None` when
/// it is one: a `+`, then an assigned country code, then digits only, between
/// [`MIN_E164_DIGITS`] and [`MAX_E164_DIGITS`] of them in all.
///
/// ARGUMENT validation — the phone-number counterpart of [`arn_shape_reason`]: a value
/// that could be no destination number anywhere is bad input, judged BEFORE the store
/// is consulted, whichever operation carried it.
pub fn destination_number_fault(value: &str) -> Option<DestinationNumberFault> {
    let Some(digits) = value.strip_prefix('+') else {
        return Some(DestinationNumberFault::Form);
    };
    if !(MIN_E164_DIGITS..=MAX_E164_DIGITS).contains(&digits.len())
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Some(DestinationNumberFault::Form);
    }
    (!opens_with_assigned_country_code(digits)).then_some(DestinationNumberFault::CountryCode)
}

/// The SMS service a destination number is handed to for the message to be carried, as
/// it names itself in the refusals SNS surfaces.
pub const SMS_SERVICE: &str = "PinpointSmsVoiceV2";

/// The argument that service carries the destination number as — the member its own
/// refusals name, which is never SNS's own `PhoneNumber`.
pub const DESTINATION_NUMBER_ARGUMENT: &str = "destinationPhoneNumber";

/// The HTTP status the SMS service refused the destination number with, as its message
/// states it. A client error of the inner call, which SNS in turn reports as its own
/// `InvalidParameterException` (HTTP 400).
pub const SMS_SERVICE_STATUS: u16 = 400;

/// The tail every refusal the SMS service states carries: which service answered, the
/// status it answered with, and the id of THAT call — a correlation id of the inner
/// request, so it is minted per call and is not the id of the SNS response carrying it.
fn sms_service_tail(request_id: &str) -> String {
    format!(" (Service: {SMS_SERVICE}, Status Code: {SMS_SERVICE_STATUS}, Request ID: {request_id})")
}

/// The sentence the SMS service refuses `fault` with, framed as SNS reports it and
/// tailed with `request_id` — the inner call's id.
///
/// The two faults take two different sentences, because that text is the only thing
/// telling them apart under one error code: a value that is not a number at all fails
/// the inner service's argument validation, while a well-formed number addressing no
/// assigned country is an argument it cannot use.
pub fn sms_service_refusal(fault: DestinationNumberFault, request_id: &str) -> String {
    let stated = match fault {
        DestinationNumberFault::Form => format!(
            "Validation Error Occurred - Reason=\"INVALID_PARAMETER\" \
             Fields=\"{DESTINATION_NUMBER_ARGUMENT}\""
        ),
        DestinationNumberFault::CountryCode => format!(
            "The following parameter is null, empty or invalid: ['{DESTINATION_NUMBER_ARGUMENT}']"
        ),
    };
    format!(
        "Invalid parameter: {stated}{tail}",
        tail = sms_service_tail(request_id)
    )
}

/// The message an operation that asks for an SMS to be SENT to `value` refuses it with,
/// or `None` when `value` is a destination number the SMS service would take.
///
/// The number travels on to the SMS service, so the refusal is that service's
/// ([`sms_service_refusal`]) rather than SNS's own member sentence, and each call
/// carries its own inner request id ([`new_request_id`]).
pub fn sent_to_refusal(value: &str) -> Option<String> {
    destination_number_fault(value)
        .map(|fault| sms_service_refusal(fault, &new_request_id()))
}
