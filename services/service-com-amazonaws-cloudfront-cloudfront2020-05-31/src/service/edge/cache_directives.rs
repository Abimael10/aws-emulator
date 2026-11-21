// SPDX-License-Identifier: CC0-1.0

//! What the ORIGIN said about storing its own answer, and how long the edge holds that answer
//! because of it.
//!
//! One rule for every answer an origin produces: an answer whose own `Cache-Control` carries a
//! directive that forbids a shared cache from storing it is not held in the edge cache at all,
//! and one that carries none is held for the span the distribution's configuration states for it.
//! The exception is stated by the cache behaviour itself — "if your minimum TTL is greater than
//! 0, CloudFront will cache content for at least the duration specified in the cache policy's
//! minimum TTL, even if the `Cache-Control: no-cache`, `no-store`, or `private` directives are
//! present in the origin headers" (`CacheBehavior`) — so a behaviour whose minimum TTL is a span
//! holds such an answer for that span. The exception is about CONTENT: it covers the statuses the
//! edge caches as objects ([`super::is_cacheable_status`]) and not an answer that is an error,
//! which is held only for what the distribution's custom error responses state about it
//! ([`super::custom_errors`]).
//!
//! The three directives are the ones that sentence names, and they are read as PRESENT or absent
//! — "the … directives are present in the origin headers" — so a directive stating what it
//! applies to is the directive being present. Nothing else of the header is read here: for how
//! long an answer the origin admits to a cache is held is the configuration's to say, which is
//! where [`held_for`] takes it from.

use ::std::time::Duration;

/// The response header an origin states a cache directive in.
const CACHE_CONTROL: &str = "cache-control";

/// The directives that forbid a shared cache from storing the answer they are on, as
/// `CacheBehavior` names them: `no-cache`, `no-store` and `private`.
const FORBID_STORAGE: [&str; 3] = ["no-store", "no-cache", "private"];

/// One answer the edge has to decide the residence of: the status it carries, what the origin
/// said about storing it, and the spans the distribution's configuration states.
#[derive(Debug)]
pub struct Residence<'a> {
    /// The status the origin answered with.
    pub status: u16,
    /// The origin's own response headers, lowercased, as the edge serves them.
    pub headers: &'a [(String, String)],
    /// `ErrorCachingMinTTL` for this status, when the distribution's custom error responses name
    /// it ([`super::custom_errors::caching_ttl`]).
    pub error_ttl: Option<Duration>,
    /// How long an answer the behaviour may cache is held, from its cache policy.
    pub ttl: Option<Duration>,
    /// The behaviour's MINIMUM TTL, which is what holds content the origin asked no cache to
    /// store.
    pub min_ttl: Option<Duration>,
}

/// How long the edge holds `residence` in the distribution's cache, or `None` when it holds it
/// not at all.
pub fn held_for(residence: &Residence<'_>) -> Option<Duration> {
    if forbids_storage(residence.headers) {
        // The origin asked that this answer not be stored, so the only thing that still holds it
        // is the behaviour's own minimum TTL, and only for content.
        return residence
            .min_ttl
            .filter(|_| super::is_cacheable_status(residence.status));
    }
    residence
        .error_ttl
        .or_else(|| residence.ttl.filter(|_| super::is_cacheable_status(residence.status)))
}

/// Whether the origin's own headers forbid a shared cache from storing the answer they describe:
/// a `Cache-Control` carrying one of [`FORBID_STORAGE`].
///
/// The value is a list of directives, each of which may state what it applies to after an `=`, so
/// the name is what is read. A header the origin stated more than once states one list too.
pub fn forbids_storage(headers: &[(String, String)]) -> bool {
    headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(CACHE_CONTROL))
        .flat_map(|(_, value)| value.split(','))
        .map(|directive| directive.split('=').next().unwrap_or_default().trim())
        .any(|name| {
            FORBID_STORAGE
                .iter()
                .any(|forbidding| name.eq_ignore_ascii_case(forbidding))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One header set, as an origin answer carries it.
    fn headers(stated: &[(&str, &str)]) -> Vec<(String, String)> {
        stated
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    /// WHICH directives forbid storage: each of the three the behaviour's own documentation names,
    /// spelled bare, beside others, in another case and with an argument, against directives that
    /// forbid nothing, a header that is not a cache directive at all, and no header.
    #[test]
    fn the_directives_that_forbid_storage_are_the_three_the_service_names() {
        // (case, the origin's headers, whether they forbid storage)
        type Stated = (&'static str, &'static [(&'static str, &'static str)], bool);
        let cases: [Stated; 12] = [
            ("no header at all", &[], false),
            (
                "a freshness lifetime",
                &[("cache-control", "max-age=120")],
                false,
            ),
            (
                "a shared freshness lifetime",
                &[("cache-control", "s-maxage=60, public")],
                false,
            ),
            ("no-store", &[("cache-control", "no-store")], true),
            ("no-cache", &[("cache-control", "no-cache")], true),
            ("private", &[("cache-control", "private")], true),
            (
                "no-store beside other directives",
                &[("cache-control", "max-age=0, no-store, must-revalidate")],
                true,
            ),
            (
                "a directive in upper case",
                &[("cache-control", "NO-STORE")],
                true,
            ),
            (
                "a qualified private, which is the directive being present",
                &[("cache-control", "private=\"set-cookie\"")],
                true,
            ),
            (
                "the header name in upper case",
                &[("Cache-Control", "no-store")],
                true,
            ),
            (
                "a header that is not a cache directive",
                &[("x-cache-control-note", "no-store")],
                false,
            ),
            (
                "a directive that merely resembles one",
                &[("cache-control", "no-store-note")],
                false,
            ),
        ];
        for (case, stated, expected) in cases {
            let actual = forbids_storage(&headers(stated));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected forbids_storage == {expected}, got {actual}"
            );
        }
    }

    /// How long an answer is held: the span the configuration states for an answer the origin
    /// admits to a cache, nothing for one it asks no cache to store, and the behaviour's minimum
    /// TTL for such an answer when it states one — which holds CONTENT and never an error.
    #[test]
    fn an_answer_the_origin_asks_no_cache_to_store_is_held_only_by_a_minimum_ttl() {
        let hour = Duration::from_secs(3_600);
        let minute = Duration::from_secs(60);
        // (case, status, the origin's headers, the error TTL, the policy TTL, the minimum TTL,
        //  how long the edge holds it)
        type Row = (
            &'static str,
            u16,
            &'static [(&'static str, &'static str)],
            Option<Duration>,
            Option<Duration>,
            Option<Duration>,
            Option<Duration>,
        );
        let cases: [Row; 9] = [
            (
                "content the origin stated nothing about",
                200,
                &[],
                None,
                Some(hour),
                None,
                Some(hour),
            ),
            (
                "content under a freshness lifetime",
                200,
                &[("cache-control", "max-age=120")],
                None,
                Some(hour),
                None,
                Some(hour),
            ),
            (
                "content the origin asks no cache to store",
                200,
                &[("cache-control", "no-store")],
                None,
                Some(hour),
                None,
                None,
            ),
            (
                "the same content under a behaviour whose minimum TTL is a span",
                200,
                &[("cache-control", "no-store")],
                None,
                Some(hour),
                Some(minute),
                Some(minute),
            ),
            (
                "content a private directive covers",
                200,
                &[("cache-control", "private")],
                None,
                Some(hour),
                None,
                None,
            ),
            (
                "an error the configuration names",
                502,
                &[],
                Some(hour),
                None,
                None,
                Some(hour),
            ),
            (
                "an error the configuration names, the origin asking no cache to store it",
                502,
                &[("cache-control", "no-store")],
                Some(hour),
                None,
                None,
                None,
            ),
            (
                "the same error under a behaviour whose minimum TTL is a span",
                502,
                &[("cache-control", "no-store")],
                Some(hour),
                Some(hour),
                Some(minute),
                None,
            ),
            (
                "an error the configuration does not name",
                502,
                &[],
                None,
                Some(hour),
                None,
                None,
            ),
        ];
        for (case, status, stated, error_ttl, ttl, min_ttl, expected) in cases {
            let stated = headers(stated);
            let actual = held_for(&Residence {
                status,
                headers: &stated,
                error_ttl,
                ttl,
                min_ttl,
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the edge to hold it for {expected:?}, got {actual:?}"
            );
        }
    }
}
