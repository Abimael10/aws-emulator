// SPDX-License-Identifier: CC0-1.0

//! The FORM of a subscription confirmation token, and the two sentences
//! `ConfirmSubscription` tells its two refusals apart with.
//!
//! The token is opaque — `smithy_query(query="shape",shape="token")` reports a plain string
//! with no length and no pattern, and `ConfirmSubscriptionInput$Token` documents only
//! "Short-lived token sent to an endpoint during the Subscribe action" — so nothing is parsed
//! out of it and nothing but equality against the minted value confirms anything. What CAN be
//! read off a value without knowing any token is its FORM: a confirmation token is a long run
//! of hexadecimal characters ([`super::CONFIRMATION_TOKEN_HEX_CHARS`] of them, the width this
//! service mints and the width SNS's own tokens carry in the `SubscriptionConfirmation` it
//! posts to an endpoint), and a value of any other shape is not a confirmation token of
//! anybody's.
//!
//! That is the line between the operation's two refusals, which share one status and one
//! error code and differ only in their sentence:
//!
//! * a value that is not of a token's form faults the request MEMBER, and is refused with the
//!   service's bare parameter sentence ([`malformed_confirmation_token_refusal`]);
//! * a value that IS of a token's form but matches no pending confirmation of the named topic
//!   faults the token — an expired, already-superseded or foreign confirmation link — and is
//!   refused with [`super::INVALID_CONFIRMATION_TOKEN_REFUSAL`].
//!
//! The form judgement reads only the value, never the store, so a token this service never
//! minted is still told apart from a value that is no token at all.

use super::*;

/// The member `ConfirmSubscription` carries the confirmation token in, as the model spells
/// it: `smithy_query(query="operation",operation="ConfirmSubscription")` reports the
/// `@required` member `Token`, and an awsQuery member's wire name IS its member name.
pub const CONFIRMATION_TOKEN_MEMBER: &str = "Token";

/// Whether `value` is of a confirmation token's FORM: exactly
/// [`super::CONFIRMATION_TOKEN_HEX_CHARS`] hexadecimal characters, which is what this service
/// mints and the only shape a confirmation token takes.
///
/// Either case of hexadecimal is read, because the width and the alphabet are the form —
/// which half of the alphabet a digit was written in is not something a token's bearer
/// chooses.
pub fn is_confirmation_token_form(value: &str) -> bool {
    value.chars().count() == CONFIRMATION_TOKEN_HEX_CHARS
        && value.chars().all(|character| character.is_ascii_hexdigit())
}

/// The sentence a `ConfirmSubscription` whose `Token` is not of a confirmation token's form
/// is refused with: this service's bare parameter refusal, naming the member whose value was
/// not accepted.
///
/// Wholly fixed but for the member name: the token is a short-lived secret sent to the
/// endpoint, so neither the value nor any reason read off it is reproduced.
pub fn malformed_confirmation_token_refusal() -> String {
    format!("Invalid parameter: {CONFIRMATION_TOKEN_MEMBER}")
}

/// The message a `ConfirmSubscription` carrying `value` as its `Token` is refused with when
/// the value is not of a token's form, or `None` when it is — in which case the operation
/// goes on to look for the pending confirmation it matches.
pub fn confirmation_token_form_refusal(value: &str) -> Option<String> {
    (!is_confirmation_token_form(value)).then(malformed_confirmation_token_refusal)
}
