// SPDX-License-Identifier: CC0-1.0

//! The ONE page walk this service's paginated reads take over their records.
//!
//! Every listing of this service orders its records by the unique id it pages by and
//! names the page boundary with a cursor the token codec beside this module renders,
//! so the walk itself — how a stated `maxResults` becomes a page size, where a
//! decoded cursor resumes, where the page ends and when a `nextToken` is handed out
//! — is stated once here rather than once per listing: two listings then cannot
//! drift apart about what a page is, and a rule fixed for one walk is fixed for
//! every walk.

use super::encode_next_token;

/// The page size a request that states no usable `maxResults` gets.
///
/// Where the model declares that member's own default (`range(min: 1, max: 10)`,
/// default 10) this is that default; where it declares none, it is this service's
/// one page size, so one walk of this service does not page unlike the next.
pub const DEFAULT_PAGE_SIZE: usize = 10;

/// The largest page a listing whose `maxResults` the model bounds returns, from
/// that member's modelled range (`range(min: 1, max: 10)`).
pub const MAX_PAGE_SIZE: i32 = 10;

/// The page size one call reports, from the `maxResults` the caller stated.
///
/// `cap` is the largest page the operation's own modelled `maxResults` range
/// admits ([`MAX_PAGE_SIZE`]), or `None` for a listing whose `maxResults` the model
/// bounds nowhere: a stated upper limit is then honoured as stated, since what a
/// page can hold is bounded anyway by the records the store is allowed to hold. A
/// request that states no page size, or one at or below zero — which asks for
/// nothing a walk could continue from, and which the boundary validator already
/// refuses wherever the model bounds the member — takes [`DEFAULT_PAGE_SIZE`]
/// rather than a page of one.
pub fn page_size(requested: Option<i32>, cap: Option<i32>) -> usize {
    match requested {
        Some(requested) if requested > 0 => {
            let asked = cap.map_or(requested, |cap| requested.min(cap));
            usize::try_from(asked).unwrap_or(DEFAULT_PAGE_SIZE)
        }
        _ => DEFAULT_PAGE_SIZE,
    }
}

/// Take one page out of `ordered`, and the `nextToken` the call after it resumes
/// from.
///
/// `ordered` is the operation's records sorted by the id it pages by, which is
/// unique and so a total order over them: a page boundary is stable across calls,
/// and a walk neither repeats a record nor skips one. `cursor_of` reads that id out
/// of a record, `resume_after` is the cursor a caller's token decoded to — the walk
/// continues strictly after it — and the returned token is issued only while
/// records remain after this page, so a caller stops paging exactly when the
/// enumeration is exhausted.
pub fn page_of<'records, Record>(
    operation: &str,
    ordered: &'records [Record],
    cursor_of: impl Fn(&Record) -> &str,
    resume_after: Option<&str>,
    page_size: usize,
    now: ::std::time::SystemTime,
) -> (&'records [Record], Option<String>) {
    let start = match resume_after {
        Some(cursor) => ordered.partition_point(|record| cursor_of(record) <= cursor),
        None => 0,
    };
    let end = start.saturating_add(page_size).min(ordered.len());
    let page = &ordered[start..end];
    let next_token = (end < ordered.len())
        .then(|| {
            page.last()
                .map(|last| encode_next_token(operation, cursor_of(last), now))
        })
        .flatten();
    (page, next_token)
}

#[cfg(test)]
mod tests {
    use super::super::decode_next_token;
    use super::{page_of, page_size, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
    use ::std::time::{Duration, SystemTime};

    /// The operation a walk is scoped to, as a handler passes it.
    const OPERATION: &str = "ListResponderGateways";

    /// The instant a token is issued at, fixed so nothing here depends on the wall
    /// clock.
    fn issued_at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600)
    }

    /// What a stated `maxResults` becomes, at each published bound of the modelled
    /// range and outside it, for a listing whose member the model bounds and for one
    /// it does not.
    #[test]
    fn a_stated_page_size_is_honoured_up_to_the_modelled_maximum() {
        for (case, requested, cap, expected) in [
            (
                "no member at all",
                None,
                Some(MAX_PAGE_SIZE),
                DEFAULT_PAGE_SIZE,
            ),
            ("the range's minimum", Some(1), Some(MAX_PAGE_SIZE), 1),
            ("inside the range", Some(4), Some(MAX_PAGE_SIZE), 4),
            (
                "the range's maximum",
                Some(MAX_PAGE_SIZE),
                Some(MAX_PAGE_SIZE),
                10,
            ),
            ("above the range", Some(50), Some(MAX_PAGE_SIZE), 10),
            ("zero", Some(0), Some(MAX_PAGE_SIZE), DEFAULT_PAGE_SIZE),
            ("negative", Some(-7), Some(MAX_PAGE_SIZE), DEFAULT_PAGE_SIZE),
            ("unbounded, stated large", Some(50), None, 50),
            ("unbounded, stated none", None, None, DEFAULT_PAGE_SIZE),
            ("unbounded, stated zero", Some(0), None, DEFAULT_PAGE_SIZE),
        ] {
            assert_eq!(
                page_size(requested, cap),
                expected,
                "case {case:?}: expected maxResults={requested:?} under cap {cap:?} to page by {expected}"
            );
        }
    }

    /// A walk reports every record exactly once, in order, and hands out a token
    /// only while records remain after the page it answered.
    #[test]
    fn a_walk_reports_every_record_once_and_stops_when_it_is_exhausted() {
        let records: Vec<String> = (0..7).map(|n| format!("rtb-gw-{n}")).collect();

        for (case, stored, per_page) in [
            ("an empty store", 0usize, 3usize),
            ("one short page", 2, 3),
            ("exactly one page", 3, 3),
            ("pages with a short last one", 7, 3),
            ("a page larger than the store", 4, 10),
            ("one record per page", 3, 1),
        ] {
            let ordered = &records[..stored];
            let mut walked: Vec<String> = Vec::new();
            let mut resume_after: Option<String> = None;
            let mut pages = 0usize;
            loop {
                let (reported, next_token) = page_of(
                    OPERATION,
                    ordered,
                    |id: &String| id.as_str(),
                    resume_after.as_deref(),
                    per_page,
                    issued_at(),
                );
                pages += 1;
                assert!(
                    reported.len() <= per_page,
                    "case {case:?}: a page holds at most {per_page} record(s), got {reported:?}"
                );
                walked.extend(reported.iter().cloned());
                match next_token {
                    Some(token) => {
                        resume_after = Some(
                            decode_next_token(OPERATION, &token, issued_at())
                                .expect("a token this walk issued is one it resumes from"),
                        );
                    }
                    None => break,
                }
                assert!(
                    pages <= 32,
                    "case {case:?}: the walk did not terminate, {walked:?} so far"
                );
            }
            assert_eq!(
                walked, *ordered,
                "case {case:?}: expected the walk to report every record once in order, got {walked:?}"
            );
        }
    }
}
