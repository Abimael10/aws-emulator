// SPDX-License-Identifier: CC0-1.0

//! The MATERIAL a subscription confirmation token is written in, and the two sentences
//! `ConfirmSubscription` tells its two refusals apart with.
//!
//! The token is opaque — `smithy_query(query="shape",shape="token")` reports a plain string
//! with no length and no pattern, and `ConfirmSubscriptionInput$Token` documents only
//! "Short-lived token sent to an endpoint during the Subscribe action" — so nothing is parsed
//! out of it and nothing but equality against the minted value confirms anything. What CAN be
//! read off a value without knowing any token is the MATERIAL it is written in: a confirmation
//! token is a run of hexadecimal characters of one width
//! ([`super::CONFIRMATION_TOKEN_HEX_CHARS`], the width SNS's own tokens carry in the
//! `SubscriptionConfirmation` it posts to an endpoint and the width this service mints).
//!
//! That is the line between the operation's two refusals, which share one status and one
//! error code and differ only in their sentence:
//!
//! * a value written in a token's own material but NOT at the width one is issued at is a
//!   confirmation token that was damaged on its way back — a confirmation link truncated or
//!   run together by whatever carried it — so the fault is in the request MEMBER, and it is
//!   refused with the service's bare parameter sentence
//!   ([`malformed_confirmation_token_refusal`]);
//! * every other value carries no confirmation material this service can match — a token of
//!   the issued width that confirms nothing (expired, already superseded, or a foreign
//!   topic's), and equally a word, a URL or an ARN presented where a token belongs — and is
//!   refused as the token that confirms nothing
//!   ([`super::INVALID_CONFIRMATION_TOKEN_REFUSAL`]).
//!
//! The judgement reads only the value, never the store, so a token this service never minted
//! is still told apart from a damaged one.

use super::*;

/// The member `ConfirmSubscription` carries the confirmation token in, as the model spells
/// it: `smithy_query(query="operation",operation="ConfirmSubscription")` reports the
/// `@required` member `Token`, and an awsQuery member's wire name IS its member name.
pub const CONFIRMATION_TOKEN_MEMBER: &str = "Token";

/// Whether `value` is written at the width a confirmation token is issued at, in a
/// confirmation token's material: exactly [`super::CONFIRMATION_TOKEN_HEX_CHARS`] hexadecimal
/// characters.
///
/// Either case of hexadecimal is read: which half of the alphabet a digit was written in is
/// not something a token's bearer chooses.
pub fn is_confirmation_token_form(value: &str) -> bool {
    value.chars().count() == CONFIRMATION_TOKEN_HEX_CHARS && is_confirmation_token_material(value)
}

/// Whether `value` is written in the MATERIAL a confirmation token is made of: a non-empty run
/// of hexadecimal characters, and nothing else.
///
/// Where the two refusals divide. Recorded evidence of this operation has a run of hexadecimal
/// characters one digest wide — this material, at a width no token is issued at — refused with
/// the bare parameter sentence, and a value carrying no such material refused as the token that
/// confirms nothing; so a value of this material that is not of the issued width
/// ([`is_confirmation_token_form`]) is a token that was damaged, which is the one condition
/// `ConfirmSubscription` faults the request member for, and everything else — an empty `Token`
/// included, since an empty value carries no confirmation material either — goes on to be
/// looked for among the topic's pending confirmations and, failing that, refused with
/// [`super::INVALID_CONFIRMATION_TOKEN_REFUSAL`].
pub fn is_confirmation_token_material(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_hexdigit())
}

/// The sentence a `ConfirmSubscription` whose `Token` is a damaged confirmation token is
/// refused with: this service's bare parameter refusal, naming the member whose value was
/// not accepted.
///
/// Wholly fixed but for the member name: the token is a short-lived secret sent to the
/// endpoint, so neither the value nor any reason read off it is reproduced.
pub fn malformed_confirmation_token_refusal() -> String {
    format!("Invalid parameter: {CONFIRMATION_TOKEN_MEMBER}")
}

/// The message a `ConfirmSubscription` carrying `value` as its `Token` is refused with when
/// the value is a DAMAGED token — a token's own material at a width no token is issued at —
/// or `None` for every other value, which goes on to be looked for among the topic's pending
/// confirmations and, failing that, refused as the token that confirms nothing.
pub fn confirmation_token_form_refusal(value: &str) -> Option<String> {
    (is_confirmation_token_material(value) && !is_confirmation_token_form(value))
        .then(malformed_confirmation_token_refusal)
}
