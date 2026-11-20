// SPDX-License-Identifier: CC0-1.0

//! The continuation tokens Parameter Store's paginated reads hand out.
//!
//! A `NextToken` is opaque: the position travels INSIDE the token, and there is no page
//! kept on the side to look it up in. A read that only checks the token's SHAPE therefore
//! honours any value of that shape — a caller that corrupts, truncates or reuses a stale
//! token gets a page it was never offered and silently restarts the scan — while what the
//! API documents for such a value is `InvalidNextToken`.
//!
//! So a token carries an authenticator over its own cursor, computed under a secret this
//! store draws from the system's randomness and never reveals
//! ([`::service_common::page_tokens::PageTokenKey`], the workspace's one reading of that):
//! the cursor stays readable by the read that minted it, and no caller can produce a
//! cursor the key authenticates. The cursor also names WHICH read minted it, so a token a
//! hierarchy read handed out does not resume a metadata describe at the same name.

pub use ::service_common::page_tokens::PageTokenKey;

/// The byte a cursor puts between the read it was minted for and the position it names. A
/// unit separator is in no parameter name (a name carries only `a-zA-Z0-9_.-` and the `/`
/// separator) and in no version number, so it splits a cursor exactly once.
const CURSOR_SEPARATOR: char = '\u{1f}';

/// Which paginated read of Parameter Store a token was minted for.
///
/// Each read walks its own sequence — the parameters of a hierarchy, the parameters of a
/// Region, the versions of one parameter — so a position means something different in each
/// and a token is honoured only by the read that issued it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagedRead {
    /// `GetParametersByPath`, which resumes at a parameter name.
    Hierarchy,
    /// `DescribeParameters`, which resumes at a parameter name.
    Metadata,
    /// `GetParameterHistory`, which resumes at a version number.
    History,
}

impl PagedRead {
    /// How this read names itself inside a cursor: the operation's own name, so two reads
    /// never agree on a cursor by accident.
    const fn scope(self) -> &'static str {
        match self {
            Self::Hierarchy => "GetParametersByPath",
            Self::Metadata => "DescribeParameters",
            Self::History => "GetParameterHistory",
        }
    }
}

/// The `NextToken` that resumes `read` AFTER `resume_after`: the authenticated bytes of the
/// cursor, hex-encoded.
///
/// `resume_after` is the LAST entry the page this token is handed out with served, never the
/// one that was waiting behind it ([`already_served`]).
///
/// The position is not hidden — a continuation token is opaque, not confidential — and
/// nothing about it has to be: what a caller cannot do is produce the authenticator for a
/// position of its own.
pub fn page_token(key: &PageTokenKey, read: PagedRead, resume_after: &str) -> String {
    ::hex::encode(key.mint(&format!("{}{CURSOR_SEPARATOR}{resume_after}", read.scope())))
}

/// Whether `position` is one a resume after `resume_after` has already served, so the walk
/// steps over it.
///
/// A page's token names the last entry THAT page served, so the next page continues
/// STRICTLY after it: the walk resumes from the cut the previous page left rather than from
/// a position fixed while that page was served. An entry the store gained between the two
/// calls, at a position after that cut, is therefore reported rather than stepped over,
/// while nothing the previous pages reported is reported twice. `None` resumes nothing, so
/// every position is still waiting — the first page of the read.
///
/// The one reading of what a resume has behind it, shared by the paginated parameter reads,
/// whichever total order each walks: a parameter name for the metadata describe and the
/// hierarchy read, a version number for the version history.
pub fn already_served<T: PartialOrd + ?Sized>(resume_after: Option<&T>, position: &T) -> bool {
    resume_after.is_some_and(|cut| position <= cut)
}

/// The position a `NextToken` resumes `read` at, or `None` for any value THIS store did not
/// mint for THAT read — one that is not hex, one too short to carry an authenticator, one
/// whose authenticator does not match the cursor it travels with, and one minted for
/// another read.
pub fn page_cursor(key: &PageTokenKey, read: PagedRead, token: &str) -> Option<String> {
    let cursor = key.cursor_of(&::hex::decode(token).ok()?)?;
    let (scope, position) = cursor.split_once(CURSOR_SEPARATOR)?;
    (scope == read.scope()).then(|| position.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A position round-trips through the read that minted it, and through nothing else:
    /// not another read, not another store's key, and not a value a caller assembled.
    #[test]
    fn a_position_round_trips_only_through_the_read_that_minted_it() {
        let key = PageTokenKey::new();
        let other_store = PageTokenKey::new();
        let reads = [PagedRead::Hierarchy, PagedRead::Metadata, PagedRead::History];
        // The forms a Parameter Store cursor carries: a top-level name, a hierarchy path,
        // a version number, and the empty position.
        for position in ["MyParameter", "Dev/DBServer", "7", ""] {
            for read in reads {
                let token = page_token(&key, read, position);
                assert_eq!(
                    page_cursor(&key, read, &token).as_deref(),
                    Some(position),
                    "case {read:?} at {position:?}: did not round-trip"
                );
                assert_eq!(
                    page_cursor(&other_store, read, &token),
                    None,
                    "case {read:?} at {position:?}: another store's key must not decode it"
                );
                for another in reads.iter().filter(|another| **another != read) {
                    assert_eq!(
                        page_cursor(&key, *another, &token),
                        None,
                        "case {read:?} at {position:?}: {another:?} must not resume from it"
                    );
                }
            }
        }

        // Values of a caller's own making, including the position in the clear and a
        // minted token with one byte changed.
        let minted = page_token(&key, PagedRead::Hierarchy, "Dev/DBServer");
        let mut tampered = minted.clone();
        tampered.replace_range(0..1, if minted.starts_with('a') { "b" } else { "a" });
        for (case, token) in [
            ("empty", String::new()),
            ("prose", "not-a-real-token".to_string()),
            ("the position in the clear", "Dev/DBServer".to_string()),
            ("hex that carries no authenticator", "abcdef".to_string()),
            ("a minted token with a byte changed", tampered),
        ] {
            assert_eq!(
                page_cursor(&key, PagedRead::Hierarchy, &token),
                None,
                "case {case:?}: expected no cursor for {token:?}"
            );
        }
    }

    /// A resume has the entries up to and including the cut behind it, and everything after
    /// the cut still waiting — in both total orders a parameter read walks.
    #[test]
    fn a_resume_continues_strictly_after_the_entry_its_cut_names() {
        // (case, the cut, the position, whether the position is already served)
        let names: &[(&str, Option<&str>, &str, bool)] = &[
            ("no cut at all", None, "Dev/DBServer", false),
            ("the cut itself", Some("Dev/DBServer"), "Dev/DBServer", true),
            ("before the cut", Some("Dev/DBServer"), "Dev/A", true),
            ("after the cut", Some("Dev/DBServer"), "Dev/E", false),
            ("a deeper name under the cut", Some("Dev"), "Dev/A", false),
        ];
        for (case, cut, position, served) in names {
            assert_eq!(
                already_served(*cut, *position),
                *served,
                "case {case:?}: expected {position:?} after cut {cut:?} to be served={served}"
            );
        }

        // (case, the cut, the version, whether the version is already served)
        let versions: &[(&str, Option<i64>, i64, bool)] = &[
            ("no cut at all", None, 1, false),
            ("the cut itself", Some(2), 2, true),
            ("before the cut", Some(2), 1, true),
            ("after the cut", Some(2), 3, false),
        ];
        for (case, cut, version, served) in versions {
            assert_eq!(
                already_served(cut.as_ref(), version),
                *served,
                "case {case:?}: expected version {version} after cut {cut:?} to be \
                 served={served}"
            );
        }
    }
}
