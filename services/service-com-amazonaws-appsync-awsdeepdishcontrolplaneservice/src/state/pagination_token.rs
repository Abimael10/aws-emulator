// SPDX-License-Identifier: CC0-1.0

//! The `nextToken` a list operation resumes from, and the refusals for one it cannot read.
//!
//! A pagination token is OPAQUE to the caller: it carries a wrapped payload the service
//! wrote, so a value the service did not write is not a resumption point but a payload the
//! token decoder cannot read. Either way it is `BadRequestException` (HTTP 400), the one
//! client error every list operation of this service models for a request that is not well
//! formed — but the two generations of this service's API state it differently, which is
//! what the recorded refusals of the two show:
//!
//! * the `/v1/…` control plane states ONE fixed sentence
//!   ([`INVALID_PAGINATION_TOKEN_MESSAGE`]) and nothing of the token or the call;
//! * the `/v2/apis…` surface — the `Api` (Event API) operations, the generation whose
//!   own `name` refusal also differs from the v1 one ([`crate::state::api_name_refusal`])
//!   — surfaces the token decoder's own failure instead
//!   ([`pagination_token_refusal`]): it names the OPERATION the token was submitted to, so
//!   a caller paging several collections can tell which call's token was refused, and ends
//!   with the decoder's diagnosis of the payload, so it can also see WHY.
//!
//! The decoder reads the payload's WRAPPING SCHEMA first: a token is a base64 payload
//! whose leading 16 bits say which schema wraps the cursor inside it, and a token this
//! service cannot resume from is diagnosed by that value — the one thing the decoder got
//! far enough to read. The payload is decoded with the `base64` crate, the one base64
//! reading this crate has, under a configuration that matches how a decoder reads a
//! PREFIX of a longer payload: characters outside the alphabet carry no payload bits and
//! are passed over, the bits past the schema id belong to the cursor behind it rather than
//! being padding to validate, and a payload shorter than the schema id is read with its
//! missing bits unset — so every token the `@pattern` of `PaginationToken` admits has
//! exactly one diagnosis.

use ::base64::engine::{general_purpose, DecodePaddingMode, Engine as _};

/// The v1 control plane's fixed, request-independent text for a `nextToken` a list
/// operation cannot resume from, carried by `BadRequestException` (HTTP 400).
pub const INVALID_PAGINATION_TOKEN_MESSAGE: &str = "Invalid pagination token given.";

/// How many base64 characters the payload's leading 16 bits are spread over: each
/// character carries six bits, so three of them cover the first sixteen (and two bits of
/// what follows).
const SCHEMA_CHARS: usize = 3;

/// The engine the payload's leading bytes are decoded with: RFC 4648 base64, no padding,
/// and the bits past the last whole byte left alone — those belong to the cursor behind
/// the schema id, so they are not padding whose canonical form a prefix could satisfy.
const PAYLOAD_DECODER: general_purpose::GeneralPurpose = general_purpose::GeneralPurpose::new(
    &::base64::alphabet::STANDARD,
    general_purpose::GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireNone),
);

/// The wrapping schema `token`'s payload states: the leading 16 bits of the payload its
/// base64 characters spell, read as a signed value the way the decoder reads the schema
/// id it compares against its own.
///
/// A payload shorter than the schema id is read out with `A`, the alphabet's zero, so its
/// missing bits are unset rather than borrowed from anywhere.
pub fn pagination_token_wrapping_schema(token: &str) -> i16 {
    let alphabet = ::base64::alphabet::STANDARD.as_str();
    let leading: String = token
        .chars()
        .filter(|character| alphabet.contains(*character))
        .take(SCHEMA_CHARS)
        .collect();
    let payload = format!("{leading:A<width$}", width = SCHEMA_CHARS);
    let bytes = PAYLOAD_DECODER.decode(payload).unwrap_or_default();
    i16::from_be_bytes([
        bytes.first().copied().unwrap_or_default(),
        bytes.get(1).copied().unwrap_or_default(),
    ])
}

/// The text `BadRequestException` (HTTP 400) carries on the `/v2/apis…` surface for a
/// `nextToken` that `operation` cannot resume from: the operation the token was submitted
/// to, and the token decoder's own diagnosis of the payload.
///
/// A call that submitted NO token has no payload to diagnose — and a page resolution
/// cannot fail without one, since a listing that names no cursor starts at the beginning —
/// so that arm states the sentence that names neither the call nor a payload
/// ([`INVALID_PAGINATION_TOKEN_MESSAGE`]) rather than a schema id invented for a token
/// that was never sent.
pub fn pagination_token_refusal(operation: &str, token: Option<&str>) -> String {
    match token {
        Some(token) => format!(
            "Pagination token exception in operation '{operation}': invalid payload wrapping \
             schema {}",
            pagination_token_wrapping_schema(token)
        ),
        None => INVALID_PAGINATION_TOKEN_MESSAGE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the diagnosis states the payload's leading 16 bits as a signed value,
    /// whatever the token holds.
    #[test]
    fn the_diagnosis_states_the_payloads_leading_schema_id() {
        // (the token, the schema id its payload states) — each id is the first sixteen
        // bits of the payload the token's base64 characters spell.
        for (token, schema) in [
            ("not-a-valid-token", -24949),
            ("notavalidtoken", -24949),
            ("AAAA", 0),
            ("garbage", -32342),
            ("zzz", -12484),
            // Fewer characters than the schema id spans, and a token of characters the
            // alphabet does not hold at all: both still have exactly one diagnosis.
            ("A", 0),
            ("--", 0),
        ] {
            assert_eq!(
                pagination_token_wrapping_schema(token),
                schema,
                "case {token:?}: expected the payload's schema id {schema}, got {}",
                pagination_token_wrapping_schema(token)
            );
        }
    }

    /// RULE: the sentence names the operation the token was submitted to and ends with the
    /// diagnosis; a call that submitted no token takes the sentence that names neither.
    #[test]
    fn the_sentence_names_the_operation_and_the_diagnosis() {
        for operation in ["ListApis", "ListDataSources"] {
            let stated = pagination_token_refusal(operation, Some("not-a-valid-token"));
            assert_eq!(
                stated,
                format!(
                    "Pagination token exception in operation '{operation}': invalid payload \
                     wrapping schema -24949"
                ),
                "case {operation:?}: got {stated:?}"
            );
            let stated = pagination_token_refusal(operation, None);
            assert_eq!(
                stated, INVALID_PAGINATION_TOKEN_MESSAGE,
                "case {operation:?} with no token at all: got {stated:?}"
            );
        }
    }
}
