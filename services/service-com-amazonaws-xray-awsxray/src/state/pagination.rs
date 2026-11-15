// SPDX-License-Identifier: CC0-1.0

//! The crate's ONE wording for a pagination token this service cannot interpret.
//!
//! Every `@paginated` read of this service takes its `NextToken` as "the
//! pagination token returned by a previous request"
//! (`smithy_query(query="doc",shape="GetTraceSummariesRequest",member="NextToken")`),
//! so a value no previous response handed out — unparseable, naming nothing in
//! the keyspace being walked, or past its end — is not a cursor this read can
//! resume from. Such a token is refused with the operation's modelled
//! client-input error, and the message that refusal carries is written down ONCE
//! here: the fixed sentence plus the offending token, taken verbatim from the
//! request, so a caller reading the message can pick the token it sent back out
//! of it and two readers of this service cannot word one rule two ways.

/// The refusal message for a pagination token this service cannot interpret.
///
/// The fixed text is the whole message bar the token, and `token` is appended
/// exactly as the request supplied it — unquoted, with nothing after it, so the
/// message both names the rule and reports the offending value.
pub fn invalid_next_token_message(token: &str) -> String {
    format!("Invalid token {token}")
}

/// The refusal message for a `NextToken` supplied to a read whose modelled
/// pagination this service does not serve.
///
/// A read that hands out no cursor can resume from none, so ANY token is refused
/// — the sentence states that the paging is unavailable rather than that this
/// token was not recognised, because the two mean opposite things to a client: a
/// caller told its token was unknown re-derives one and pages again, while this
/// tells it to stop paginating the call. It is FIXED text: the submitted token
/// is not echoed, because the value is not what is wrong with the request.
pub const PAGINATION_UNSUPPORTED_MESSAGE: &str = "NextToken is currently not supported";
