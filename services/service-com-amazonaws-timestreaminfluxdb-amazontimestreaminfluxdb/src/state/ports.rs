// SPDX-License-Identifier: CC0-1.0

//! The ports InfluxDB does not accept connections on, and the refusal a request naming
//! one of them gets.
//!
//! `port` carries a constraint this model DOCUMENTS without declaring it — "Constraints:
//! The value can't be 2375-2376, 7788-7799, 8090, or 51678-51680"
//! (`smithy_query(query="doc", shape="CreateDbClusterInput", member="port")`, the same
//! sentence on `CreateDbInstanceInput$port` and `UpdateDbInstanceInput$port`) — so the
//! framework's boundary walk, which enforces only the `@range` 1024..65535 the shape
//! declares, admits values the service itself does not. Every operation that takes a port
//! asks this module about it, so one unavailable value cannot be taken by one operation
//! and refused by another, and the request is refused BEFORE anything is minted, written
//! or moved.

use crate::ops::types::ValidationException;

/// The `ValidationExceptionReason` this service reports when a well-formed request
/// carries a value one of its FIELDS does not accept: the member of the modelled enum
/// (`crate::ops::types::VALIDATION_EXCEPTION_REASON_VALUES`) that names exactly that,
/// beside the catch-all [`super::VALIDATION_EXCEPTION_REASON_OTHER`] a refusal about the
/// account rather than about the request carries.
pub const VALIDATION_EXCEPTION_REASON_FIELD_VALIDATION_FAILED: &str = "FIELD_VALIDATION_FAILED";

/// The port values the service does not accept, as INCLUSIVE spans in the order the
/// documented constraint lists them; a single unavailable value is a span of one.
///
/// The one home of that list: both the check below and the sentence the refusal states
/// are derived from it, so the words a caller is shown can never name a span the check
/// does not hold to.
pub const UNAVAILABLE_PORT_SPANS: &[(i32, i32)] = &[
    (2375, 2376),
    (7788, 7799),
    (8090, 8090),
    (51678, 51680),
];

/// Whether `port` is one of the values above — a port the service documents as
/// unavailable, whatever the declared `@range` admits.
#[must_use]
pub fn port_is_unavailable(port: i32) -> bool {
    UNAVAILABLE_PORT_SPANS
        .iter()
        .any(|(first, last)| (*first..=*last).contains(&port))
}

/// How the documented constraint lists the unavailable spans: each span as `first-last`
/// (a span of one as the value itself), separated by commas, with `or` before the last.
#[must_use]
pub fn unavailable_ports_text() -> String {
    let spans: Vec<String> = UNAVAILABLE_PORT_SPANS
        .iter()
        .map(|(first, last)| {
            if first == last {
                first.to_string()
            } else {
                format!("{first}-{last}")
            }
        })
        .collect();
    match spans.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, earlier)) => format!("{}, or {last}", earlier.join(", ")),
    }
}

/// The refusal a request asking InfluxDB to accept connections on an unavailable `port`
/// gets: the modelled `ValidationException` (HTTP 400), naming the port the request asked
/// for and restating the constraint the documentation words it with.
///
/// A field of the request is what failed, so the `reason` the model marks required is
/// [`VALIDATION_EXCEPTION_REASON_FIELD_VALIDATION_FAILED`] rather than the catch-all.
#[must_use]
pub fn unavailable_port(port: i32) -> ValidationException {
    ValidationException {
        message: format!(
            "The port [{port}] is not available. The value can't be {}",
            unavailable_ports_text()
        ),
        reason: VALIDATION_EXCEPTION_REASON_FIELD_VALIDATION_FAILED.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a port inside a documented span is unavailable and every other port the
    /// declared range admits is available — asserted at both ends of every span, one
    /// value either side of it, and at the bounds of the modelled `Port` range
    /// (1024..65535).
    #[test]
    fn exactly_the_documented_spans_are_unavailable() {
        for (first, last) in UNAVAILABLE_PORT_SPANS.iter().copied() {
            for port in [first, last] {
                assert!(
                    port_is_unavailable(port),
                    "case \"the bound {port} of span {first}-{last}\": expected an \
                     unavailable port"
                );
            }
            for port in [first - 1, last + 1] {
                assert_eq!(
                    port_is_unavailable(port),
                    UNAVAILABLE_PORT_SPANS
                        .iter()
                        .any(|(low, high)| (*low..=*high).contains(&port)),
                    "case \"the port {port} beside span {first}-{last}\": the answer must \
                     follow the span list alone"
                );
            }
        }
        // The engine defaults (8086 for v2, 8181 for v3) and the bounds of the modelled
        // `Port` range are all available.
        for port in [1024, 8086, 8181, 65535] {
            assert!(
                !port_is_unavailable(port),
                "case \"the port {port}\": expected an available port"
            );
        }
    }

    /// RULE: the refusal's sentence lists the spans the check holds to, in the words the
    /// documented constraint uses.
    #[test]
    fn the_refusal_restates_the_documented_constraint() {
        assert_eq!(
            unavailable_ports_text(),
            "2375-2376, 7788-7799, 8090, or 51678-51680",
            "case \"the documented span list\": unexpected rendering {:?}",
            unavailable_ports_text()
        );
        let refusal = unavailable_port(8090);
        assert_eq!(
            refusal.message,
            "The port [8090] is not available. The value can't be 2375-2376, 7788-7799, \
             8090, or 51678-51680",
            "case \"a refused port\": unexpected message {:?}",
            refusal.message
        );
        assert_eq!(
            refusal.reason, VALIDATION_EXCEPTION_REASON_FIELD_VALIDATION_FAILED,
            "case \"a refused port\": expected the field-validation reason, got {:?}",
            refusal.reason
        );
    }
}
