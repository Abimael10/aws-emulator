// SPDX-License-Identifier: CC0-1.0

//! The POINT OF PRESENCE one viewer request is served from, and the name every plane that
//! reports it carries.
//!
//! CloudFront serves a viewer from one of its edge locations, and it NAMES the location that
//! served a request wherever it reports anything about that request: the `Server-Timing`
//! header's `cdn-pop` metric carries that name to the viewer, and the log field that reports
//! the edge location carries it to whoever reads the request's log record. So a POP name is
//! not a value of the configuration a caller submitted — it is the identity of the location
//! the edge served from, and it is the SAME value on every plane that reports one request.
//!
//! The spelling is the published one: an edge location "is identified by a three-letter code
//! and an arbitrarily assigned number (for example, `DFW3`)", where "the three-letter code
//! typically corresponds with the International Air Transport Association (IATA) airport code
//! for an airport near the edge location" (the CloudFront developer guide's standard log file
//! field `x-edge-location`, the same field the real-time log catalog names).
//!
//! ## Which location serves a request here
//!
//! This emulator has ONE edge location, for the reason it has one viewer address, one viewer
//! port and one viewer country ([`super::generated_headers`]): every viewer of it dials a
//! loopback address, so there is one place a request can be served from and nothing to choose
//! between — and that one location is [`EDGE_POP`]. A reader comparing two requests of this
//! emulator therefore sees one POP, which is what it reads; and because every plane reads the
//! name here, a response header and a log record of one request can never name two.

/// The one point of presence this emulator serves viewer requests from.
///
/// Named in the published spelling ([`is_pop_name`] is that spelling, checked): the IATA code
/// of the airport nearest the Region CloudFront's own endpoint is served from
/// (`service_common::management_events::PARTITION_LEADER_REGION`, `us-east-1`, served out of
/// Northern Virginia — Washington Dulles, `IAD`), and a location number, which the published
/// spelling states is arbitrary. That is the same location the country the edge reports for
/// its viewers names ([`super::generated_headers::EDGE_COUNTRY`]), so the POP a reader sees
/// and the country an origin reads describe one place.
///
/// `pub` because it is what the edge REPORTS: a suite reading the `cdn-pop` metric or the log
/// field for the edge location reads the name here rather than restating it.
pub const EDGE_POP: &str = "IAD12";

/// Whether `name` is spelled the way a point of presence is named: three ASCII letters and
/// then a number, as `x-edge-location` publishes the spelling.
///
/// The one reading of that spelling here, so the name this module states cannot drift out of
/// the form a client parsing it expects.
pub fn is_pop_name(name: &str) -> bool {
    let (code, number) = name.split_at_checked(3).unwrap_or(("", ""));
    code.len() == 3
        && code.chars().all(|letter| letter.is_ascii_uppercase())
        && !number.is_empty()
        && number.chars().all(|digit| digit.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the name this emulator's edge location carries is spelled the way CloudFront
    /// publishes a POP name — a three-letter code and a number.
    #[test]
    fn the_edge_location_is_named_in_the_published_spelling() {
        assert!(
            is_pop_name(EDGE_POP),
            "expected the edge location to be named <three letters><number>, got {EDGE_POP:?}"
        );
    }

    /// RULE: that spelling is three ASCII letters followed by a number, and nothing else is one.
    #[test]
    fn a_pop_name_is_three_letters_and_a_number() {
        // The rows differ along what the spelling generalises over: the length of each half,
        // the character classes in them and their order.
        for (case, name, named) in [
            ("the published example", "DFW3", true),
            ("a multi-digit number", "IAD89", true),
            ("another code", "LHR6", true),
            ("no number", "IAD", false),
            ("no code", "12", false),
            ("a lowercase code", "iad12", false),
            ("a four-letter code", "IADX12", false),
            ("a two-letter code", "IA12", false),
            ("a number before the code", "12IAD", false),
            ("a sub-location suffix", "IAD12-C1", false),
            ("nothing at all", "", false),
        ] {
            assert_eq!(
                is_pop_name(name),
                named,
                "case {case:?}: expected {name:?} to be read as a POP name: {named}"
            );
        }
    }
}
