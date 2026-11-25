// SPDX-License-Identifier: CC0-1.0

//! ONE paginator for every listing of this service, and the two things a listing decides
//! about its `NextToken`: how the token is SPELLED, and whether serving a page settles
//! "is there more?".
//!
//! Every listing here reports a page of an ordered sequence, so a token is a POSITION in
//! that sequence however it is written — which is what lets one cut, one refusal of a
//! token the service never minted and one round-trip rule serve them all. A token reads
//! back as exactly the text it was handed out as: a position respelled by a caller (signed,
//! zero-padded, re-coded) is no token of the listing's.
//!
//! What differs:
//!
//! * [`Spelling::Position`] — the token is the position itself. [`super::paginate`], the
//!   notification plane's listings.
//! * [`Spelling::Opaque`] — the token is a base64 run the service minted over the position,
//!   coded through the base64 this crate already codes a wire value with
//!   (`aws_smithy_types::base64`), so nothing in it is readable or composable by a caller.
//!
//! * A listing that KNOWS, as it cuts a page, whether another follows hands a token back
//!   only while items remain ([`Continuation::WhileItemsRemain`]).
//! * A listing that does not settle that question when it serves a page hands one back for
//!   every page that reported anything at all ([`Continuation::WhileAPageReportsSomething`]),
//!   and the caller's walk ends on the page that reports nothing — one request more than the
//!   items alone would need. That is what SNS's ACCOUNT-WIDE subscription listing does: a
//!   recorded page carrying the whole of an account's subscriptions came back with a
//!   continuation token all the same, and passing that token back was served. Its own token
//!   therefore always resolves: one minted for a position the sequence has since shrunk past
//!   reports what is there now, which is nothing.

/// How a listing's tokens are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spelling {
    /// The position itself, in decimal.
    Position,
    /// A base64 run minted over the position, readable only by this service.
    Opaque,
}

/// What a token is minted over under [`Spelling::Opaque`], beside the position. Never read
/// by a caller: it is what makes a value a caller composed itself — the position alone,
/// spelled some other way — fail to be one of this listing's tokens.
const OPAQUE_TOKEN_BODY: &str = "after-subscription:";

impl Spelling {
    /// The token for the page starting at `position`.
    fn mint(self, position: usize) -> String {
        match self {
            Self::Position => position.to_string(),
            Self::Opaque => {
                ::aws_smithy_types::base64::encode(format!("{OPAQUE_TOKEN_BODY}{position}"))
            }
        }
    }

    /// The position `token` starts a page at, or `None` when it is not a token this
    /// listing minted.
    fn position(self, token: &str) -> Option<usize> {
        let read = match self {
            Self::Position => token.parse().ok()?,
            Self::Opaque => {
                let decoded = ::aws_smithy_types::base64::decode(token)
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())?;
                decoded.strip_prefix(OPAQUE_TOKEN_BODY)?.parse().ok()?
            }
        };
        // Handed out in one spelling, read back in one spelling.
        (self.mint(read) == token).then_some(read)
    }
}

/// When a listing hands a continuation token back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuation {
    /// Only while the sequence holds items this page did not report.
    WhileItemsRemain,
    /// For every page that reported something, whether or not it exhausted the sequence.
    WhileAPageReportsSomething,
}

/// One page of `items` starting at `token`, plus the token of the page after it.
///
/// `None` for a token this listing never minted, which each caller reports as its own
/// modelled error.
pub fn page<T>(
    items: Vec<T>,
    token: Option<&str>,
    page_size: usize,
    spelling: Spelling,
    continuation: Continuation,
) -> Option<(Vec<T>, Option<String>)> {
    let held = items.len();
    let start = match token {
        Some(token) => spelling.position(token)?,
        None => 0,
    };
    if start > held {
        // A position past the end of the sequence. Under `WhileItemsRemain` no token was
        // ever minted for one, so the value is the caller's own and is refused; the
        // listing that mints one for the end answers the page it promised, which reports
        // what is there now.
        return match continuation {
            Continuation::WhileItemsRemain => None,
            Continuation::WhileAPageReportsSomething => Some((Vec::new(), None)),
        };
    }
    let end = (start + page_size).min(held);
    let page: Vec<T> = items.into_iter().skip(start).take(end - start).collect();
    let follows = match continuation {
        Continuation::WhileItemsRemain => end < held,
        Continuation::WhileAPageReportsSomething => !page.is_empty(),
    };
    Some((page, follows.then(|| spelling.mint(end))))
}

/// One page of the ACCOUNT-WIDE subscription listing: an opaque token on every page that
/// reported a subscription, and the empty page that ends a caller's walk.
pub fn continued_page<T>(
    items: Vec<T>,
    token: Option<&str>,
    page_size: usize,
) -> Option<(Vec<T>, Option<String>)> {
    page(
        items,
        token,
        page_size,
        Spelling::Opaque,
        Continuation::WhileAPageReportsSomething,
    )
}
