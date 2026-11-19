// SPDX-License-Identifier: CC0-1.0

//! The `X-Forwarded-For` header a TARGET behind an Application Load Balancer receives, and
//! the attribute that decides what the load balancer does to it.
//!
//! `LoadBalancerAttribute$Key` carries the switch:
//! "`routing.http.xff_header_processing.mode` - Enables you to modify, preserve, or remove the
//! `X-Forwarded-For` header in the HTTP request before the Application Load Balancer sends
//! the request to the target. The possible values are `append`, `preserve`, and `remove`. The
//! default is `append`." So each of the three is a different header at the target:
//!
//! * `append` — the load balancer ADDS the address the request came from to whatever list the
//!   client supplied, which is what makes the last entry of the list the one hop the load
//!   balancer itself saw rather than anything the client claimed;
//! * `preserve` — the client's own header travels to the target exactly as it arrived, and a
//!   request that carried none reaches the target carrying none;
//! * `remove` — the header is stripped, so the target sees no forwarded-for list at all.
//!
//! The processing is the LOAD BALANCER's, read per load balancer exactly as the sibling
//! attribute that drops invalid header fields is ([`super::invalid_header_fields`]), and it is
//! applied where the request LEAVES the load balancer for a target
//! ([`super::data_plane::serve`]) — after its listener's rules have been evaluated against the
//! request as the client sent it, which is what a rule written on that header is matched
//! against.

use crate::state::LoadBalancerRecord;

/// The attribute that decides what happens to the header:
/// `LoadBalancerAttribute$Key`'s `routing.http.xff_header_processing.mode`.
const XFF_HEADER_PROCESSING_MODE_ATTRIBUTE: &str = "routing.http.xff_header_processing.mode";

/// The three values that attribute takes, as its own documentation spells them.
const PRESERVE_MODE: &str = "preserve";
const REMOVE_MODE: &str = "remove";

/// The header itself, lower-cased as the request this plane serves holds its names.
pub(super) const FORWARDED_FOR_HEADER: &str = "x-forwarded-for";

/// How the entries of the header are separated, and so how an appended one is joined to the
/// list the client supplied.
const ENTRY_SEPARATOR: &str = ", ";

/// The address a request is attributed to where this emulator observed none of its own: the
/// core does not hand a handler the peer of a connection it accepted, and every connection to
/// a loopback listener comes from loopback, so that is the address such a request came from —
/// the same reading every other plane of this emulator that reports the address a request came
/// from uses.
///
/// A connection whose own end STATED where it was opened from carries that address instead
/// ([`super::data_plane`]): a client inside this emulator says so when it opens one, and that
/// is then the address the load balancer has for the request.
pub(super) const CONNECTION_ADDRESS: &str = "127.0.0.1";

/// What a load balancer does to the header before a target sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Processing {
    /// Add the address the request came from to the list the client supplied.
    Append,
    /// Pass the client's own header on unchanged.
    Preserve,
    /// Strip the header.
    Remove,
}

/// Which processing `record` performs: what [`XFF_HEADER_PROCESSING_MODE_ATTRIBUTE`] holds for
/// it — a caller's own write, else the value the attribute is carried at — read through the
/// ONE catalog of load balancer attributes, so what `DescribeLoadBalancerAttributes` reports
/// and what a target receives cannot drift.
///
/// A load balancer of a family that carries no such attribute passes the header on as it
/// arrived: the attribute is documented for Application Load Balancers alone, and the other
/// families' listeners read no request header out of a connection at all.
pub(super) fn processing(record: &LoadBalancerRecord) -> Processing {
    let Some(spec) = crate::state::load_balancer_attribute(XFF_HEADER_PROCESSING_MODE_ATTRIBUTE)
    else {
        return Processing::Preserve;
    };
    if !spec.applies_to(record) {
        return Processing::Preserve;
    }
    let held = record
        .attributes
        .get(XFF_HEADER_PROCESSING_MODE_ATTRIBUTE)
        .cloned()
        .or_else(|| spec.default_for(record))
        .unwrap_or_default();
    match held.trim() {
        PRESERVE_MODE => Processing::Preserve,
        REMOVE_MODE => Processing::Remove,
        // The attribute's domain admits `append`, `preserve` and `remove` and nothing else,
        // so the remaining value is the documented default this falls to.
        _ => Processing::Append,
    }
}

/// Carry out `processing` on the `X-Forwarded-For` of a request about to reach a target:
/// `client_address` is the address the request came from, which is what an append adds.
///
/// Every name is compared without regard to case, as a header name does. An append leaves the
/// list where the client's own header was — a header the client sent more than once is one
/// list, and it is passed on as one field — and writes the header for a request that carried
/// none at all, which is how a target behind a load balancer learns the address of a client
/// that claimed nothing.
pub(super) fn processed(
    headers: &mut Vec<(String, String)>,
    processing: Processing,
    client_address: &str,
) {
    if processing == Processing::Preserve {
        return;
    }
    let supplied: Vec<String> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(FORWARDED_FOR_HEADER))
        .map(|(_, value)| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    let at = headers
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case(FORWARDED_FOR_HEADER));
    headers.retain(|(name, _)| !name.eq_ignore_ascii_case(FORWARDED_FOR_HEADER));
    if processing == Processing::Remove {
        return;
    }
    let mut list = supplied.join(ENTRY_SEPARATOR);
    if !list.is_empty() {
        list.push_str(ENTRY_SEPARATOR);
    }
    list.push_str(client_address);
    let field = (FORWARDED_FOR_HEADER.to_string(), list);
    match at {
        Some(at) => headers.insert(at, field),
        None => headers.push(field),
    }
}

#[cfg(test)]
mod tests {
    //! The three processings alone. What a TARGET receives is pinned end to end in
    //! `tests/all/alb_desync_mitigation_and_forwarded_for.rs`.

    use super::*;

    /// The header list of a request carrying `headers`.
    fn request(headers: &[(&str, &str)]) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    /// The value of every `X-Forwarded-For` field of a request, in order.
    fn forwarded_for(headers: &[(String, String)]) -> Vec<&str> {
        headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(FORWARDED_FOR_HEADER))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// One row: the case, the processing, the header fields the client sent, and the value of
    /// every `X-Forwarded-For` the target must then receive.
    type Row<'a> = (
        &'a str,
        Processing,
        &'a [(&'a str, &'a str)],
        Vec<&'a str>,
    );

    #[test]
    fn each_processing_hands_the_target_the_header_its_own_documentation_gives_it() {
        // The rows span what the rule generalises over: each mode, against a request that
        // supplied no header, one address, a list of addresses, the header twice, and the
        // header under a name written in another case.
        let cases: &[Row<'_>] = &[
            (
                "append with nothing supplied",
                Processing::Append,
                &[("host", "alb")],
                vec![CONNECTION_ADDRESS],
            ),
            (
                "append onto one supplied address",
                Processing::Append,
                &[("x-forwarded-for", "203.0.113.9")],
                vec!["203.0.113.9, 127.0.0.1"],
            ),
            (
                "append onto a supplied list",
                Processing::Append,
                &[("x-forwarded-for", "203.0.113.9, 198.51.100.4")],
                vec!["203.0.113.9, 198.51.100.4, 127.0.0.1"],
            ),
            (
                "append onto the header sent twice, which is one list",
                Processing::Append,
                &[
                    ("x-forwarded-for", "203.0.113.9"),
                    ("X-Forwarded-For", "198.51.100.4"),
                ],
                vec!["203.0.113.9, 198.51.100.4, 127.0.0.1"],
            ),
            (
                "preserve one supplied address",
                Processing::Preserve,
                &[("x-forwarded-for", "203.0.113.9")],
                vec!["203.0.113.9"],
            ),
            (
                "preserve nothing supplied",
                Processing::Preserve,
                &[("host", "alb")],
                Vec::new(),
            ),
            (
                "preserve the header sent twice, exactly as it arrived",
                Processing::Preserve,
                &[
                    ("x-forwarded-for", "203.0.113.9"),
                    ("X-Forwarded-For", "198.51.100.4"),
                ],
                vec!["203.0.113.9", "198.51.100.4"],
            ),
            (
                "remove one supplied address",
                Processing::Remove,
                &[("X-Forwarded-For", "203.0.113.9")],
                Vec::new(),
            ),
            (
                "remove the header sent twice",
                Processing::Remove,
                &[
                    ("x-forwarded-for", "203.0.113.9"),
                    ("x-forwarded-for", "198.51.100.4"),
                ],
                Vec::new(),
            ),
            (
                "remove where nothing was supplied",
                Processing::Remove,
                &[("host", "alb")],
                Vec::new(),
            ),
        ];
        for (case, mode, sent, expected) in cases {
            let mut headers = request(sent);
            processed(&mut headers, *mode, CONNECTION_ADDRESS);
            let delivered = forwarded_for(&headers);
            assert_eq!(
                delivered, *expected,
                "case {case:?}: expected {expected:?}, got {delivered:?} in {headers:?}"
            );
            // Nothing but this one header is touched, whatever the mode.
            let others: Vec<&str> = headers
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case(FORWARDED_FOR_HEADER))
                .map(|(name, _)| name.as_str())
                .collect();
            let kept: Vec<&str> = sent
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case(FORWARDED_FOR_HEADER))
                .map(|(name, _)| *name)
                .collect();
            assert_eq!(
                others, kept,
                "case {case:?}: expected the other headers {kept:?} kept in order, got {others:?}"
            );
        }
    }
}
