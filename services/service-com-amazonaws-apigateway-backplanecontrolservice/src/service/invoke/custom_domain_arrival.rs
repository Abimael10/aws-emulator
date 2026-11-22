// SPDX-License-Identifier: CC0-1.0

//! What a request ADDRESSED TO A CUSTOM DOMAIN NAME of this account reaches: the record the
//! host names, the base path mapping of that record which answers the path, and the path
//! WITHIN the api that mapping selects.
//!
//! A custom domain name is a host of the CALLER's own choosing — "the custom domain name as
//! an API host name" — and what it serves is decided by the `BasePathMapping`s created
//! against it: each one "relates a path to an API". So a request that arrived on such a host
//! names its api and its stage nowhere in the request itself; they are read out of this
//! service's own records, here.
//!
//! ONE reading, asked by both halves of that front door:
//!
//! * the SOCKET a regional custom domain name publishes as its `regionalDomainName`
//!   ([`crate::service::custom_domain_socket`]), which refuses a path no mapping of the
//!   record answers before it relays anything;
//! * the DATA PLANE the relayed request is then served by, which asks it ahead of resolving
//!   the request ([`super::Addressed::arriving_on`]) and reads the api, the stage and the
//!   resource path out of the answer. Every reader of that resolution asks it — the serving,
//!   the claim, the web ACL in front of the stage, the stage's access log and its published
//!   metrics — so one request resolves to one api wherever it is read. The ONE seam that does
//!   not is the cross-origin admission of a browser request ([`super::browser_invoke`]),
//!   which is handed no store of its own to read an arrival under: a request a browser sends
//!   from another site to a custom domain name's address is refused by the front door, as
//!   every other cross-site request this emulator receives is.
//!
//! The socket relays the client's request AS THE CLIENT SENT IT — its verb, its path, its
//! query, its headers and its body — because a SigV4 signature covers exactly those: a leg
//! that re-addressed the request to the api's own endpoint would present the stage a request
//! no client ever signed, and an `AWS_IAM` method would refuse every correctly signed caller.
//! What the stage needs instead of a re-addressed path is this resolution, which is why it
//! lives beside the plane rather than inside the socket.
//!
//! Only a REGIONAL record is resolved here: its `regionalDomainName` is an address this
//! emulator itself answers on, while an edge-optimized record is served by the CloudFront
//! distribution API Gateway puts in front of it and a private one is reached through the VPC
//! endpoints its endpoint configuration names — neither of which is this process's address
//! (see [`crate::state::custom_domain_endpoint`]).
//!
//! Nothing here writes: every reading is of records the control plane persisted.

use crate::state::{
    BasePathMappingEntry, ComAmazonawsApigatewayBackplanecontrolserviceState as ApiGatewayState,
    DomainNameEntry,
};

/// What a request addressed to a custom domain name reaches: the record it arrived on, the
/// api its mapping names, and the path within that api's endpoint.
///
/// OWNED, and read under ONE acquisition of the store: a request is served with nothing of
/// this service's store held, so what the serving carries across is the answer rather than a
/// borrow of the records it was read from.
#[derive(Debug, Clone)]
pub(crate) struct Arrival {
    /// The custom domain name the request was addressed to — the host a client points at
    /// this front door, which is what says the request did NOT arrive on the api's own
    /// default `execute-api` endpoint.
    pub(crate) domain_name: String,
    /// The api the base path mapping that answers this path names.
    pub(crate) api_id: String,
    /// `/{stage}/{resource-path}`, the path within the api's endpoint — what the stage and
    /// the resource are read from, exactly as for a request that reached the api's own
    /// endpoint host.
    pub(crate) invoke_path: String,
}

/// What a request addressed to `host` with path `path` reaches, read under one acquisition of
/// `state` — the pre-pass every caller of [`super::resolve`] makes, since the record a host
/// names and the mapping that answers the path are this service's own records.
pub(crate) fn arrival_of(
    state: &::engine::ServiceState<ApiGatewayState>,
    host: Option<&str>,
    path: &str,
) -> Option<Arrival> {
    arrival(&state.lock(), host, path)
}

/// [`arrival_of`] under a store the caller already holds.
///
/// `None` when the request reaches no api of this account: a host that names no custom domain
/// name of it, and a path no base path mapping of that domain name answers.
pub(crate) fn arrival(
    store: &ApiGatewayState,
    host: Option<&str>,
    path: &str,
) -> Option<Arrival> {
    let record = addressed_record(store, host?)?;
    let (mapping, remainder) = matching_mapping(record, path)?;
    Some(Arrival {
        domain_name: record.domain_name.clone(),
        api_id: mapping.rest_api_id.clone(),
        invoke_path: invoke_path(mapping, remainder),
    })
}

/// The custom domain name record a request addressed to `host` arrived on, or `None` for a
/// host that names none.
///
/// TWO spellings address one record, because a client of this emulator reaches it at a
/// different address than a client of AWS reaches the custom domain at: the domain NAME
/// itself — what a client states in `Host` after pointing DNS at the published address, read
/// case-insensitively as a host is — and the `host:port` the record PUBLISHED as its
/// `regionalDomainName`, which is what a client that resolves no name at all dials and
/// states.
pub(crate) fn addressed_record<'s>(
    store: &'s ApiGatewayState,
    host: &str,
) -> Option<&'s DomainNameEntry> {
    // An account that has created no custom domain name at all is answered without reading
    // a host: every request of every stage asks this, and nothing can be addressed to a
    // record that is not there.
    if store.domain_names.is_empty() {
        return None;
    }
    let authority = host.rsplit('@').next().unwrap_or(host);
    let name = ::service_common::data_plane_addressing::host_only(authority);
    if name.is_empty() {
        return None;
    }
    store
        .domain_names
        .values()
        .filter(|record| crate::state::is_regional_domain_name(record))
        .find(|record| {
            record.domain_name.eq_ignore_ascii_case(name)
                || record
                    .regional_domain_name
                    .as_deref()
                    .is_some_and(|published| published.eq_ignore_ascii_case(authority))
        })
}

/// Whether `path` reaches any base path mapping of `record` — the question the socket in
/// front of a custom domain name asks before it relays a request, since a path no mapping
/// answers reaches no api of the account at all.
pub(crate) fn reaches_a_mapping(record: &DomainNameEntry, path: &str) -> bool {
    matching_mapping(record, path).is_some()
}

/// The base path mapping of `record` that answers `path`, and the path left under it.
///
/// "Specify '(none)' if you do not want callers to specify any base path name after the
/// domain name" (`BasePathMapping$basePath`), so the mapping stored under that spelling
/// answers every path no other mapping claims, and the LONGEST base path that is a prefix of
/// the path — on a segment boundary, since a base path is a path segment and not a string
/// prefix — is the one that serves it.
///
/// A path carrying a DOT SEGMENT (`.` or `..`) answers to no mapping: such a segment is a
/// relative reference, and what a mapping selects is the api and stage the path under its
/// base path is served by — a remainder that could climb back out of that prefix selects
/// something the mapping does not name, so it reaches no api at all.
fn matching_mapping<'r, 'p>(
    record: &'r DomainNameEntry,
    path: &'p str,
) -> Option<(&'r BasePathMappingEntry, &'p str)> {
    if carries_a_dot_segment(path) || addresses_another_plane(path) {
        return None;
    }
    let requested = path.trim_start_matches('/');
    let mut best: Option<(&BasePathMappingEntry, &str)> = None;
    for (base_path, mapping) in &record.base_path_mappings {
        if base_path == crate::state::NONE_BASE_PATH {
            continue;
        }
        let base = base_path.trim_matches('/');
        let Some(under) = requested.strip_prefix(base) else {
            continue;
        };
        let remainder = match under.strip_prefix('/') {
            Some(remainder) => remainder,
            // The path IS the base path, with nothing under it.
            None if under.is_empty() => under,
            // A longer segment that merely starts with the base path is a different path.
            None => continue,
        };
        if best.is_none_or(|(held, _)| held.base_path.trim_matches('/').len() < base.len()) {
            best = Some((mapping, remainder));
        }
    }
    best.or_else(|| {
        record
            .base_path_mappings
            .get(crate::state::NONE_BASE_PATH)
            .map(|mapping| (mapping, requested))
    })
}

/// Whether `path` names a plane of this EMULATOR rather than a path a base path mapping
/// answers: the reserved namespace a client of this emulator names an api's own endpoint in
/// ([`super::EMULATOR_PATH_NAMESPACE`]), and this service's own modelled control-plane path
/// space ([`crate::ROUTE_PATH_SEGMENTS`]).
///
/// Neither is a path a client of AWS carries to a custom domain name, and both are read
/// BEFORE this addressing is — the prefixed path-style by [`super::resolve`] and the control
/// plane by the front door — so a request carrying one is served by the plane it names
/// whatever a mapping would have selected. Answering it from a mapping as well is what this
/// keeps from happening: the front door in front of a custom domain name relays no such
/// request at all ([`reaches_a_mapping`]), so one address answers one plane.
fn addresses_another_plane(path: &str) -> bool {
    ::service_common::data_plane_addressing::first_segment(path).is_some_and(|(first, _)| {
        first == super::EMULATOR_PATH_NAMESPACE || crate::ROUTE_PATH_SEGMENTS.contains(&first)
    })
}

/// Whether `path` carries a segment that is a relative reference — `.` or `..`, in the
/// percent-encoded spellings a URI may write them in as well, since nothing between a client
/// and this front door decodes them.
fn carries_a_dot_segment(path: &str) -> bool {
    path.split('/').any(|segment| {
        let decoded = segment.replace("%2e", ".").replace("%2E", ".");
        decoded == "." || decoded == ".."
    })
}

/// The path WITHIN the api that `mapping` selects for the path `remainder` left under its
/// base path.
///
/// A mapping that names a `stage` serves that one stage, so the stage is prefixed: the
/// resolved path is `/{stage}/{remainder}`, which is what a request on the api's own endpoint
/// carries — and `/{stage}` alone for a path that is the base path itself, which is the path
/// such a client sends for the stage's root. A mapping that names NONE leaves the stage to
/// the caller — the first segment of the path, exactly as on that endpoint — so the remainder
/// is the whole invoke path.
fn invoke_path(mapping: &BasePathMappingEntry, remainder: &str) -> String {
    match (mapping.stage.as_deref(), remainder.is_empty()) {
        (Some(stage), true) => format!("/{stage}"),
        (Some(stage), false) => format!("/{stage}/{remainder}"),
        (None, _) => format!("/{remainder}"),
    }
}

#[cfg(test)]
mod tests;
