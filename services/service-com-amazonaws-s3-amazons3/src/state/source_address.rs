// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS the request being served was made from, and the condition key a clause about
//! the requester's network is decided against.
//!
//! "`aws:SourceIp` — the requester's IP address" (AWS General Reference, *Global condition
//! context keys*) is a fact of the CONNECTION rather than of anything the request's parameters
//! say, and a policy clause over it is what confines a grant to a set of addresses: the
//! statement that publishes a bucket to one office network, and the one that publishes it to
//! the health checkers whose blocks a service publishes for exactly that purpose. This service
//! therefore ANSWERS the key for every request it decides — a clause over it is DECIDED rather
//! than left undecided, which is what makes such a grant enforceable at all — and the value is
//! the one every other service of this tree reads for the same key
//! ([`super::notifications::request_source_address`]): the address the request states it was
//! forwarded for, and otherwise the loopback host every connection to this emulator is made
//! from.
//!
//! The address is a request-scoped NOTE rather than state ([`remember`]), under the same
//! discipline as the other facts of a request a decision deep in this store needs
//! (`super::access_points`): the seams that decide a bucket policy — the anonymous read's
//! ([`super::policy_decision_for_anonymous`]), the credentialed caller's
//! (`super::Bucket::caller_is_denied`) and the full authorization gate's
//! (`crate::service::authorization`) — are handed the store and not the request, and one
//! request's address must never be read as another's. It is recorded for EVERY request this
//! service serves, and a decision taken where nothing recorded one reads the loopback host,
//! which is where a connection that reached this emulator came from.

use ::std::cell::RefCell;

/// The condition key carrying the address the request was made from: "`aws:SourceIp` — the
/// requester's IP address", compared under IAM's `IpAddress` / `NotIpAddress` operators, which
/// read both sides as addresses so a clause stating a CIDR block holds for every address in
/// it.
pub const SOURCE_IP_KEY: &str = "aws:SourceIp";

/// The header a request states the address it was made FROM in, whose FIRST entry is the
/// client's own — the same reading every other service in this tree takes of a request's
/// source address.
pub const FORWARDED_FOR_HEADER: &str = "x-forwarded-for";

/// The address a request carries when nothing stated the hop it was forwarded for: this
/// emulator serves its API over the loopback interface, so a connection that reached it came
/// from the local host.
const LOOPBACK_SOURCE_ADDRESS: &str = "127.0.0.1";

/// The address a request stating `forwarded_for` as the hops it was forwarded through was made
/// from — the ONE reading of that question, which the decision below and the published event
/// that reports the same address ([`super::notifications::request_source_address`]) both ask.
///
/// The FIRST entry of the chain is the client's own, and the value is published only when it
/// IS an address: a header carrying anything else states no address the request could have
/// been made from and is read as the loopback host, which also bounds the value to one parsed
/// address however long a header the transport accepted.
#[must_use]
pub fn stated_source_address(forwarded_for: Option<&str>) -> String {
    forwarded_for
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .and_then(|address| address.parse::<::std::net::IpAddr>().ok())
        .map_or_else(
            || LOOPBACK_SOURCE_ADDRESS.to_string(),
            |address| address.to_string(),
        )
}

thread_local! {
    /// The address the request this thread is serving was made from, for the length of that one
    /// request. See the module docs for the discipline.
    static SOURCE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Record the address the request this thread is serving was made from, read off the
/// `forwarded_for` header value it carried ([`stated_source_address`]).
///
/// Called for EVERY request this service serves, so the note a decision reads is always its
/// own request's rather than the previous one's.
pub(crate) fn remember(forwarded_for: Option<&str>) {
    let address = stated_source_address(forwarded_for);
    SOURCE.with(|note| {
        *note.borrow_mut() = Some(address);
    });
}

/// The address recorded for the request being served, READ rather than taken: one request's
/// decision may be taken more than once (an operation that authorizes an action per resource),
/// and the note is replaced by the next request's own.
///
/// [`LOOPBACK_SOURCE_ADDRESS`] where nothing recorded one, which is the address a connection
/// that reached this emulator was made from.
#[must_use]
pub fn source_address() -> String {
    SOURCE.with(|note| {
        note.borrow()
            .clone()
            .unwrap_or_else(|| LOOPBACK_SOURCE_ADDRESS.to_string())
    })
}

/// What the request's own network answers in a condition context: one pair, under the key this
/// service publishes it as ([`SOURCE_IP_KEY`]).
///
/// ONE place, so every decision taken about one request — the anonymous read's, the
/// credentialed caller's and the full authorization gate's — is decided against one reading of
/// the connection it arrived on.
#[must_use]
pub fn source_ip_conditions() -> Vec<(String, String)> {
    vec![(SOURCE_IP_KEY.to_string(), source_address())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_address_is_the_one_the_request_was_made_from() {
        // The rows differ along what the request states: nothing at all, one address, a chain
        // whose first entry is the client's own, a chain written with spaces, an IPv6 address,
        // and a value that names no address at all.
        let rows: [(&str, Option<&str>, &str); 6] = [
            ("a request that states no hop", None, LOOPBACK_SOURCE_ADDRESS),
            ("one stated address", Some("15.177.2.1"), "15.177.2.1"),
            (
                "a chain, whose first entry is the client's own",
                Some("15.177.2.1,203.0.113.9"),
                "15.177.2.1",
            ),
            (
                "a chain written with spaces",
                Some(" 192.0.2.10 , 203.0.113.9 "),
                "192.0.2.10",
            ),
            ("an IPv6 address", Some("2001:db8::1"), "2001:db8::1"),
            (
                "a value that names no address",
                Some("not-an-address"),
                LOOPBACK_SOURCE_ADDRESS,
            ),
        ];
        for (case, forwarded_for, expected) in rows {
            remember(forwarded_for);
            let recorded = source_address();
            assert_eq!(
                recorded, expected,
                "case {case:?}: expected the decision to be taken against {expected:?}, got \
                 {recorded:?}"
            );
            let conditions = source_ip_conditions();
            assert_eq!(
                conditions,
                vec![(SOURCE_IP_KEY.to_string(), expected.to_string())],
                "case {case:?}: expected one pair under {SOURCE_IP_KEY}, got {conditions:?}"
            );
        }
        // And the note is the request's own: the next request's address replaces it rather
        // than being read as the previous one's.
        remember(None);
        let recorded = source_address();
        assert_eq!(
            recorded, LOOPBACK_SOURCE_ADDRESS,
            "case \"a later request stating no hop\": expected the note to be replaced, got \
             {recorded:?}"
        );
    }
}
