// SPDX-License-Identifier: CC0-1.0

//! The `Referer` the request this thread is serving STATES, and the condition key a policy is
//! decided against it by.
//!
//! "aws:Referer — use this key to compare who referred the request in the client browser with
//! the referer specified in the policy. … The `aws:Referer` request context value is provided
//! by the caller in an HTTP header" (AWS General Reference, *Global condition context keys*).
//! So the key's value is the header the caller sent and nothing else: a request that states no
//! `Referer` carries no value for it, which is itself part of a decision — a grant written over
//! the key admits only the requests stating what it names, and the refusal stated the other way
//! round refuses exactly the rest.
//!
//! It is the key a bucket's PUBLIC read is narrowed to one front door with: AWS's own guidance
//! for a static website served through a distribution restricts `s3:GetObject` to the requests
//! that state a secret `Referer`, which the front door adds to every origin request it makes.
//! Both halves of such a configuration have to be answered for the site to be readable at all,
//! and the half this service owns is this one.
//!
//! Not state: a request-scoped note, for the same reason [`crate::state::access_points`]'s is.
//! The decision a bucket policy is taken by is reached deep inside this store
//! ([`crate::state::Bucket`]), from a request the store never sees — the gate is handed the
//! action, the resource and the caller — so the one fact about the REQUEST that a policy may be
//! written over travels here. The discipline is that module's:
//!
//! * [`remember`] is called for EVERY request this service claims, from the routing seam the
//!   framework reaches with the whole request (`crate::service::EmulatedService::override_operation`),
//!   so a request stating no `Referer` records `None` rather than leaving the previous
//!   request's note behind;
//! * [`stated`] READS the note rather than taking it, because one request's authorization may
//!   be decided more than once (an operation that authorizes an action per resource), and the
//!   note is replaced by the next request's own.

use std::cell::RefCell;

/// The request header the caller states the site it was referred from in, which is what
/// [`CONDITION_KEY`] answers.
pub(crate) const REFERER_HEADER: &str = "referer";

/// The global condition key that header answers (see the module docs). The ONE spelling of it,
/// read by every decision of this service that is taken over a policy document — the one the
/// store takes for the anonymous caller and the one the request gate takes for a credentialed
/// one — so the two cannot answer the key differently.
pub(crate) const CONDITION_KEY: &str = "aws:Referer";

thread_local! {
    /// The `Referer` the request this thread is serving states, for the length of that one
    /// request.
    static STATED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Record `referer` as the `Referer` the request this thread is serving states, `None` for a
/// request that states none.
///
/// Called for EVERY request this service claims, so the note a decision reads is always its
/// own request's.
pub(crate) fn remember(referer: Option<&str>) {
    let recorded = referer
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    STATED.with(|note| {
        *note.borrow_mut() = recorded;
    });
}

/// The `Referer` recorded for the request being served, READ rather than taken (see the module
/// docs).
pub(crate) fn stated() -> Option<String> {
    STATED.with(|note| note.borrow().clone())
}

/// The condition context the stated `Referer` contributes: one pair for a request that states
/// one, and nothing at all for a request that states none — which is the value's own absence,
/// decided rather than unknown because the key is PUBLISHED beside the context.
pub(crate) fn conditions() -> Vec<(String, String)> {
    stated()
        .map(|referer| (CONDITION_KEY.to_string(), referer))
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the note answers the condition key with the header the request stated, and
    /// answers nothing at all for a request that stated none — including one that stated it
    /// empty, which carries no value.
    #[test]
    fn the_stated_referer_is_the_value_the_condition_key_carries() {
        for (case, stated, expected) in [
            ("a request that states none", None, None),
            ("a request that states one", Some("site-secret"), Some("site-secret")),
            ("a request that states another", Some("other"), Some("other")),
            ("a request that states it empty", Some(""), None),
        ] {
            remember(stated);
            let answered = conditions();
            let actual = answered
                .iter()
                .find(|(key, _)| key == CONDITION_KEY)
                .map(|(_, value)| value.as_str());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {CONDITION_KEY} to carry {expected:?}, got {actual:?}"
            );
            assert_eq!(
                answered.len(),
                usize::from(expected.is_some()),
                "case {case:?}: expected {} pair(s), got {answered:?}",
                usize::from(expected.is_some())
            );
        }
        // The note belongs to the request that recorded it: the next request's own value
        // replaces it rather than being read beside it.
        remember(None);
        assert!(
            conditions().is_empty(),
            "a request that states no referer must answer the key with nothing, got {:?}",
            conditions()
        );
    }
}
