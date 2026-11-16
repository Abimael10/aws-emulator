// SPDX-License-Identifier: CC0-1.0

//! How an Application Load Balancer FRAMES the request it has just read, and the attribute
//! that decides which framings it refuses itself rather than passing on.
//!
//! `LoadBalancerAttribute$Key` carries the switch:
//! "`routing.http.desync_mitigation_mode` - Determines how the load balancer handles requests
//! that might pose a security risk to your application. The possible values are `monitor`,
//! `defensive`, and `strictest`. The default is `defensive`." A request "that might pose a
//! security risk" is one whose FRAMING — the headers that say where its body ends — does not
//! say it the one way HTTP/1.1 states: two recipients of such a message can read two
//! different requests out of the same bytes, which is what lets one request be smuggled
//! inside another. So the attribute is about the message itself rather than about any
//! listener, rule or target, and it is read per LOAD BALANCER, exactly as the sibling
//! attribute that drops invalid header fields is ([`super::invalid_header_fields`]).
//!
//! # What the three modes do
//!
//! The three documented modes are an ascending scale over [`Framing`]: `monitor` forwards
//! every request it reads, `defensive` refuses the framings a recipient can read two ways,
//! and `strictest` refuses every framing that is not written the way HTTP/1.1 states it —
//! including the ones that are not ambiguous. A refusal is the LOAD BALANCER's own `400`,
//! produced before any rule is evaluated and before any target is reached, so nothing behind
//! the load balancer sees a request it refused.
//!
//! # Which framings those are
//!
//! Read off the headers that frame a request body, as RFC 7230 §3.3.2 and §3.3.3 state them:
//!
//! * a request carrying BOTH `Transfer-Encoding` and `Content-Length` — the transfer coding
//!   overrides the length for one recipient and does not for another, which is the framing a
//!   smuggled request travels in;
//! * a `Transfer-Encoding` whose FINAL coding is not `chunked`: the body's length "cannot be
//!   determined reliably", so where one recipient ends the message another reads the bytes
//!   that follow as a request of their own;
//! * several `Content-Length` values that DISAGREE, or one that is not a decimal number at
//!   all: the framing is an unrecoverable error rather than a length;
//! * several `Content-Length` values that AGREE: a length every recipient reads the same way,
//!   written in a form the grammar does not admit — refused by `strictest` alone, because
//!   nothing can be smuggled inside it.
//!
//! A request whose framing names neither header, or names one well-formed `Content-Length`,
//! is [`Framing::Compliant`] and is passed on under every mode.

use crate::state::LoadBalancerRecord;

/// The attribute that decides which framings are refused:
/// `LoadBalancerAttribute$Key`'s `routing.http.desync_mitigation_mode`.
const DESYNC_MITIGATION_MODE_ATTRIBUTE: &str = "routing.http.desync_mitigation_mode";

/// The three values that attribute takes, as its own documentation spells them.
const MONITOR_MODE: &str = "monitor";
const STRICTEST_MODE: &str = "strictest";

/// The two headers that frame a request body.
const CONTENT_LENGTH_HEADER: &str = "content-length";
const TRANSFER_ENCODING_HEADER: &str = "transfer-encoding";

/// The transfer coding that must END a request's `Transfer-Encoding` for its body's length to
/// be readable at all.
const CHUNKED_CODING: &str = "chunked";

/// The status a load balancer answers a request whose framing its mode refuses: the request
/// is answered by the LOAD BALANCER, with no rule evaluated and no target reached.
pub(super) const REFUSED_FRAMING_STATUS: u16 = 400;

/// How much of a non-compliant framing a load balancer refuses: the value its
/// `routing.http.desync_mitigation_mode` is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mitigation {
    /// Every request is passed on, whatever its framing.
    Monitor,
    /// A framing a recipient can read two ways is refused; one that is merely not written as
    /// the grammar has it is passed on.
    Defensive,
    /// Every framing that is not written as the grammar has it is refused.
    Strictest,
}

/// How a request's framing reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Framing {
    /// Written as HTTP/1.1 states it: every recipient reads the same message.
    Compliant,
    /// Not written as HTTP/1.1 states it, but still one message for every recipient: nothing
    /// can be hidden inside it.
    NotCompliant,
    /// Two recipients can read two different messages out of the same bytes.
    Ambiguous,
}

/// Which mitigation `record` is at: what [`DESYNC_MITIGATION_MODE_ATTRIBUTE`] holds for it — a
/// caller's own write, else the value the attribute is carried at — read through the ONE
/// catalog of load balancer attributes, so what `DescribeLoadBalancerAttributes` reports and
/// what the data plane refuses cannot drift.
///
/// A load balancer of a family that carries no such attribute refuses nothing: the attribute
/// is documented for Application Load Balancers alone, and the other families' listeners read
/// no request out of a connection to frame.
pub(super) fn mitigation(record: &LoadBalancerRecord) -> Mitigation {
    let Some(spec) = crate::state::load_balancer_attribute(DESYNC_MITIGATION_MODE_ATTRIBUTE) else {
        return Mitigation::Monitor;
    };
    if !spec.applies_to(record) {
        return Mitigation::Monitor;
    }
    let held = record
        .attributes
        .get(DESYNC_MITIGATION_MODE_ATTRIBUTE)
        .cloned()
        .or_else(|| spec.default_for(record))
        .unwrap_or_default();
    match held.trim() {
        MONITOR_MODE => Mitigation::Monitor,
        STRICTEST_MODE => Mitigation::Strictest,
        // The attribute's domain admits `monitor`, `defensive` and `strictest` and nothing
        // else, so the remaining value is the documented default this falls to.
        _ => Mitigation::Defensive,
    }
}

/// Whether a load balancer at `mitigation` refuses a request framed `framing`.
pub(super) fn refuses(mitigation: Mitigation, framing: Framing) -> bool {
    match mitigation {
        Mitigation::Monitor => false,
        Mitigation::Defensive => framing == Framing::Ambiguous,
        Mitigation::Strictest => framing != Framing::Compliant,
    }
}

/// How the framing of a request carrying `headers` reads — one `(name, value)` per header
/// field as it arrived, in order, with the name in any case: a request may carry either
/// framing header more than once, and each one may itself carry a list.
pub(super) fn framing<'a, I>(headers: I) -> Framing
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut lengths: Vec<&str> = Vec::new();
    let mut codings: Vec<String> = Vec::new();
    let mut transfer_encoded = false;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case(CONTENT_LENGTH_HEADER) {
            lengths.extend(value.split(',').map(str::trim));
        } else if name.eq_ignore_ascii_case(TRANSFER_ENCODING_HEADER) {
            transfer_encoded = true;
            codings.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|coding| !coding.is_empty())
                    .map(|coding| coding.to_ascii_lowercase()),
            );
        }
    }
    if transfer_encoded {
        // A transfer coding frames the body, and a length beside it is the framing a
        // smuggled request travels in; the coding list has to END in the chunked one for the
        // body to have a readable length at all, and no coding may be the chunked one twice.
        if !lengths.is_empty() {
            return Framing::Ambiguous;
        }
        let chunked = codings
            .iter()
            .filter(|coding| coding.as_str() == CHUNKED_CODING)
            .count();
        if chunked != 1 || codings.last().map(String::as_str) != Some(CHUNKED_CODING) {
            return Framing::Ambiguous;
        }
        return Framing::Compliant;
    }
    match lengths.len() {
        0 => Framing::Compliant,
        // One length: a decimal number is a framing, anything else is no length at all.
        1 if declares_a_length(lengths[0]) => Framing::Compliant,
        1 => Framing::Ambiguous,
        // Several lengths that agree are one length written twice; several that disagree —
        // or any one of them that is no number — leave the body's end unknown.
        _ if lengths.iter().all(|length| declares_a_length(length))
            && lengths.windows(2).all(|pair| pair[0] == pair[1]) =>
        {
            Framing::NotCompliant
        }
        _ => Framing::Ambiguous,
    }
}

/// Whether `value` is a body length: a decimal number, which is the only form a
/// `Content-Length` takes.
fn declares_a_length(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    //! The classification and the mode scale alone. What a CLIENT and a target see is pinned
    //! end to end in `tests/all/alb_desync_mitigation_and_forwarded_for.rs`.

    use super::*;

    /// The framing of one request, read off its header list.
    fn framing_of(headers: &[(&str, &str)]) -> Framing {
        framing(headers.iter().map(|(name, value)| (*name, *value)))
    }

    /// One row: the case, the header fields the request carries, and the framing they state.
    type Row<'a> = (&'a str, &'a [(&'a str, &'a str)], Framing);

    #[test]
    fn a_framing_reads_as_the_headers_that_frame_the_body_state_it() {
        // The rows span the forms the two framing headers take: neither header, one
        // well-formed length, a length written twice with the same and with a different
        // value, a length that is no number, a list of lengths in one field, the chunked
        // coding alone, a coding list that does not end in it, the chunked coding twice, an
        // empty coding list, and either header beside the other.
        let cases: &[Row<'_>] = &[
            (
                "no framing header at all",
                &[("host", "alb")],
                Framing::Compliant,
            ),
            (
                "one decimal length",
                &[("content-length", "8")],
                Framing::Compliant,
            ),
            ("a zero length", &[("content-length", "0")], Framing::Compliant),
            (
                "the chunked coding alone",
                &[("transfer-encoding", "chunked")],
                Framing::Compliant,
            ),
            (
                "a coding list ending in the chunked one",
                &[("transfer-encoding", "gzip, chunked")],
                Framing::Compliant,
            ),
            (
                "the same length twice, in two fields",
                &[("content-length", "8"), ("Content-Length", "8")],
                Framing::NotCompliant,
            ),
            (
                "the same length twice, in one field",
                &[("content-length", "8, 8")],
                Framing::NotCompliant,
            ),
            (
                "two lengths that disagree",
                &[("content-length", "8"), ("content-length", "9")],
                Framing::Ambiguous,
            ),
            (
                "a length that is no number",
                &[("content-length", "eight")],
                Framing::Ambiguous,
            ),
            (
                "an empty length",
                &[("content-length", "")],
                Framing::Ambiguous,
            ),
            (
                "a length beside a transfer coding",
                &[("content-length", "8"), ("transfer-encoding", "chunked")],
                Framing::Ambiguous,
            ),
            (
                "a transfer coding beside a length, in the other order",
                &[("Transfer-Encoding", "chunked"), ("Content-Length", "8")],
                Framing::Ambiguous,
            ),
            (
                "a coding list that does not end in the chunked one",
                &[("transfer-encoding", "chunked, gzip")],
                Framing::Ambiguous,
            ),
            (
                "a coding that is not a transfer coding at all",
                &[("transfer-encoding", "cheese")],
                Framing::Ambiguous,
            ),
            (
                "the chunked coding twice",
                &[
                    ("transfer-encoding", "chunked"),
                    ("transfer-encoding", "chunked"),
                ],
                Framing::Ambiguous,
            ),
            (
                "the identity coding, which frames no body",
                &[("transfer-encoding", "identity")],
                Framing::Ambiguous,
            ),
            (
                "a transfer coding with no coding in it at all",
                &[("transfer-encoding", "")],
                Framing::Ambiguous,
            ),
        ];
        for (case, headers, expected) in cases {
            let read = framing_of(headers);
            assert_eq!(
                read, *expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    #[test]
    fn each_mode_refuses_the_framings_its_own_documentation_gives_it() {
        // Every pair of the scale: the mode, the framing, and whether the load balancer
        // answers the request itself.
        let cases: &[(Mitigation, Framing, bool)] = &[
            (Mitigation::Monitor, Framing::Compliant, false),
            (Mitigation::Monitor, Framing::NotCompliant, false),
            (Mitigation::Monitor, Framing::Ambiguous, false),
            (Mitigation::Defensive, Framing::Compliant, false),
            (Mitigation::Defensive, Framing::NotCompliant, false),
            (Mitigation::Defensive, Framing::Ambiguous, true),
            (Mitigation::Strictest, Framing::Compliant, false),
            (Mitigation::Strictest, Framing::NotCompliant, true),
            (Mitigation::Strictest, Framing::Ambiguous, true),
        ];
        for (mode, framing, expected) in cases {
            let refused = refuses(*mode, *framing);
            assert_eq!(
                refused, *expected,
                "case {mode:?} with {framing:?}: expected refused {expected}, got {refused}"
            );
        }
    }
}
