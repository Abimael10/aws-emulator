// SPDX-License-Identifier: CC0-1.0

//! The TRACE a load balancer stamps on every request it forwards: `X-Amzn-Trace-Id`.
//!
//! An Application Load Balancer traces the requests it serves — "the load balancer adds a
//! header with a trace identifier to each request it receives" — so the request a TARGET
//! receives names the chain the load balancer served it on, and nothing a client sends has to
//! carry one. It is what lets a target, and the records it writes, be joined to the request
//! the load balancer answered: a target handed no trace identifier cannot tell that a request
//! reached it through a load balancer at all.
//!
//! Three cases, as Elastic Load Balancing's own request tracing states them:
//!
//! * a request carrying no header of that name is forwarded carrying one the load balancer
//!   generates, with a [`ROOT_FIELD`] naming a fresh chain
//!   (`Root=1-<8 hex>-<24 hex>`, the form X-Ray gives a trace id, minted through the one
//!   reading of it this workspace has — [`::service_common::xray_trace::mint_trace_id`]);
//! * a request whose header already names a chain keeps it, and the load balancer inserts a
//!   [`SELF_FIELD`] of its own AHEAD of it, naming its own leg of that chain — whatever else
//!   the header carries beside the root travels on, because what a tracer put there is the
//!   tracer's;
//! * a header that names no chain at all is forwarded with a root the load balancer generates
//!   ahead of whatever it does carry, so the request is traced either way.
//!
//! A load balancer never UPDATES a `Self` field it was handed: a request that already names
//! one has been through a load balancer, and the leg that one named is not this one's to
//! rewrite.
//!
//! Only an APPLICATION Load Balancer traces a request this way ([`traces`]): tracing is a
//! header on an HTTP request, and that family's listeners are the ones that READ the client's
//! request and forward a request of their own. A Network Load Balancer's listener hands on the
//! connection it accepted and a Gateway Load Balancer's carries `GENEVE`, so neither has a
//! request of its own to stamp.
//!
//! Stamped where the request LEAVES the load balancer for a target, as the
//! `X-Forwarded-For` processing and the client-certificate hand-off are
//! ([`super::forwarded_for`], [`super::client_certificates`]), so the rules its listener
//! evaluated were matched against the request as the client sent it. Nothing here writes to
//! the store: a trace identifier is minted for the request being served and kept nowhere.

use ::std::time::SystemTime;

use ::service_common::xray_trace::{continued_root, mint_trace_id, TRACE_HEADER};

use crate::state::{LoadBalancerFamily, LoadBalancerRecord};

/// The field naming the CHAIN a request belongs to, which every hop of it carries.
const ROOT_FIELD: &str = "Root";

/// The field one load balancer names its OWN leg of that chain in.
const SELF_FIELD: &str = "Self";

/// How the fields of one tracing header are separated.
const FIELD_SEPARATOR: char = ';';

/// Whether the load balancer `record` traces the requests it serves — the Application
/// family's, as this module's own documentation states.
///
/// Read off the same store read the rest of what a load balancer does to a request is
/// ([`super::data_plane`]).
pub(super) fn traces(record: &LoadBalancerRecord) -> bool {
    record.family() == LoadBalancerFamily::Application
}

/// Stamp the trace of the request `headers` belongs to, as this module's own documentation
/// states it, for a request being served `at`.
pub(super) fn stamped(headers: &mut Vec<(String, String)>, at: SystemTime) {
    let carried = headers
        .iter_mut()
        .find(|(name, _)| name == TRACE_HEADER)
        .map(|(_, value)| value);
    let Some(carried) = carried else {
        headers.push((
            TRACE_HEADER.to_string(),
            format!("{ROOT_FIELD}={}", mint_trace_id(at)),
        ));
        return;
    };
    *carried = traced(carried, at);
}

/// The value the forwarded request carries, for a request that arrived carrying `supplied`.
fn traced(supplied: &str, at: SystemTime) -> String {
    if continued_root(supplied).is_none() {
        // A header naming no chain names no leg of one either: the load balancer generates
        // the root, ahead of whatever the header does carry.
        return format!("{ROOT_FIELD}={};{supplied}", mint_trace_id(at));
    }
    if names_its_own_leg(supplied) {
        // A request that already names a leg has been through a load balancer, and that leg
        // is not this one's to rewrite.
        return supplied.to_string();
    }
    format!("{SELF_FIELD}={};{supplied}", mint_trace_id(at))
}

/// Whether `supplied` already names a leg of the chain it carries.
fn names_its_own_leg(supplied: &str) -> bool {
    supplied
        .split(FIELD_SEPARATOR)
        .filter_map(|field| field.split_once('='))
        .any(|(name, _)| name.trim() == SELF_FIELD)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant the rows are served at, whose epoch seconds a generated id carries.
    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + ::std::time::Duration::from_secs(1_793_000_000)
    }

    /// A trace id of the form X-Ray gives one, for a row that names a chain of its own, and
    /// the header that names it.
    const CHAIN: &str = "1-67891233-abcdef012345678912345678";
    const NAMES_THE_CHAIN: &str = "Root=1-67891233-abcdef012345678912345678";

    /// What a row expects of the value the target receives.
    #[derive(Debug, PartialEq, Eq)]
    enum Expected {
        /// A chain the load balancer generated, and no leg named.
        GeneratedChain,
        /// The client's own chain, with the load balancer's leg inserted ahead of it, and
        /// everything else the header carried kept.
        OwnLegAheadOf(&'static str),
        /// Exactly what the client sent: a header naming a leg already.
        Unchanged,
    }

    /// RULE: the request a target receives names the chain the load balancer served it on —
    /// a chain the load balancer generated where the client named none, the client's own
    /// where it named one, with the load balancer's own leg inserted ahead of it and a leg it
    /// was handed left alone.
    #[test]
    fn every_forwarded_request_names_the_chain_it_belongs_to() {
        // The rows span what the rule generalises over: whether the client sent the header at
        // all, whether what it sent names a chain, what else it carries beside the chain, and
        // whether it already names a leg.
        let rows: &[(&str, Option<&str>, Expected)] = &[
            ("a client that sent no header", None, Expected::GeneratedChain),
            (
                "a client that named a chain",
                Some(NAMES_THE_CHAIN),
                Expected::OwnLegAheadOf("named a chain"),
            ),
            (
                "a client that named a chain and a field of its own",
                Some("Root=1-67891233-abcdef012345678912345678;CalledFrom=app"),
                Expected::OwnLegAheadOf("named a chain and a field"),
            ),
            (
                "a request that already names a leg",
                Some("Self=1-67891233-12456789abcdef012345678;Root=1-67891233-abcdef012345678912345678"),
                Expected::Unchanged,
            ),
            (
                "a header naming nothing this reading recognises",
                Some("nothing-of-a-trace"),
                Expected::GeneratedChain,
            ),
            (
                "a header whose root is no trace id",
                Some("Root=not-a-trace-id"),
                Expected::GeneratedChain,
            ),
        ];
        for (case, supplied, expected) in rows {
            let mut headers: Vec<(String, String)> =
                vec![("accept".to_string(), "*/*".to_string())];
            if let Some(supplied) = supplied {
                headers.push((TRACE_HEADER.to_string(), (*supplied).to_string()));
            }
            stamped(&mut headers, at());
            let carried = headers
                .iter()
                .filter(|(name, _)| name == TRACE_HEADER)
                .map(|(_, value)| value.clone())
                .collect::<Vec<String>>();
            assert_eq!(
                carried.len(),
                1,
                "case {case:?}: a target is handed one trace identifier, got {carried:?}"
            );
            let carried = carried.first().expect("one value").clone();
            let root = continued_root(&carried);
            assert!(
                root.is_some(),
                "case {case:?}: expected the value to name a chain, got {carried:?}"
            );
            match expected {
                Expected::GeneratedChain => {
                    assert!(
                        !names_its_own_leg(&carried),
                        "case {case:?}: a generated chain names no leg, got {carried:?}"
                    );
                    assert!(
                        carried.starts_with(&format!("{ROOT_FIELD}=")),
                        "case {case:?}: expected a generated root first, got {carried:?}"
                    );
                    // The client's own fields travel on beside it.
                    if let Some(supplied) = supplied {
                        assert!(
                            carried.contains(supplied),
                            "case {case:?}: expected {supplied:?} to travel on, got {carried:?}"
                        );
                    }
                }
                Expected::OwnLegAheadOf(_) => {
                    assert_eq!(
                        root,
                        Some(CHAIN),
                        "case {case:?}: expected the client's own chain, got {carried:?}"
                    );
                    assert!(
                        carried.starts_with(&format!("{SELF_FIELD}=")),
                        "case {case:?}: expected the load balancer's own leg first, got \
                         {carried:?}"
                    );
                    let supplied = supplied.expect("the row supplied a header");
                    assert!(
                        carried.ends_with(supplied),
                        "case {case:?}: expected {supplied:?} to travel on, got {carried:?}"
                    );
                }
                Expected::Unchanged => assert_eq!(
                    Some(carried.as_str()),
                    *supplied,
                    "case {case:?}: a leg the load balancer was handed is left alone"
                ),
            }
        }
    }
}
