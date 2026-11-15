// SPDX-License-Identifier: CC0-1.0

//! The FORM a continuation token has to take before a listing reads it.
//!
//! A paginated read of this service answers two different refusals for a `nextToken` it
//! cannot resume from, and they are about different things. Whether this store MINTED the
//! value — the authenticated cursor and the listing scope inside it — is
//! [`super::page_tokens`]'s question, and a value that fails it names no position. Whether
//! the value could be one of this API's tokens AT ALL is this module's: a token Amazon ECS
//! hands out is at least [`MIN_PAGE_TOKEN_LENGTH`] characters long, so a shorter value is
//! refused for its LENGTH, before anything is read out of it, by the listings whose recorded
//! responses show that check.
//!
//! Every token this store mints is longer than the bound — it is the hexadecimal of an
//! authenticator of [`::service_common::page_tokens::AUTHENTICATOR_BYTES`] bytes and the
//! cursor beside it — so the page a caller resumes from can never be refused here.

/// The shortest continuation token Amazon ECS hands out, in characters. RECORDED behaviour:
/// a value shorter than this is not a token of this API, which is what the sentence states.
pub const MIN_PAGE_TOKEN_LENGTH: usize = 48;

/// `InvalidParameterException$message` for a `nextToken` shorter than
/// [`MIN_PAGE_TOKEN_LENGTH`]: the fixed words and the length a token has to reach, with
/// nothing of the value the request carried.
pub fn short_page_token_message() -> String {
    format!("token shorter than {MIN_PAGE_TOKEN_LENGTH}.")
}

/// The refusal a `nextToken` earns for its LENGTH alone, or `None` for a value long enough
/// to be read as one of this API's tokens.
pub fn short_page_token_violation(token: &str) -> Option<String> {
    (token.chars().count() < MIN_PAGE_TOKEN_LENGTH).then(short_page_token_message)
}
