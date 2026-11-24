// SPDX-License-Identifier: CC0-1.0

//! What a request addressed to a CUSTOM DOMAIN NAME of this account reaches: the record its
//! `Host` names, the api mapping of that record which answers the path, and the path WITHIN
//! the api that mapping selects.
//!
//! A custom domain name is a host of the CALLER's own choosing — `CreateDomainName` takes the
//! name and `CreateApiMapping` says what answers under it, "the API mapping key" and the
//! `{ApiId, Stage}` it binds — so a request that arrived on such a host names its api and its
//! stage nowhere in the request itself: both are read out of this service's own records, here.
//!
//! A client reaches a custom domain name at TWO addresses of this emulator, and the same
//! records resolve both:
//!
//! * the SOCKET the record publishes as its `ApiGatewayDomainName`
//!   ([`crate::service::domain_name_socket`]), which is what a client that was handed that
//!   address dials, and which re-addresses the request it accepted at the api's own invoke
//!   path;
//! * this emulator's own ENDPOINT, which is the one address a client configured with a single
//!   endpoint URL can reach at all: such a client sends the custom domain name as the `Host`
//!   of a request made there, exactly as a client of AWS sends it to API Gateway's own front
//!   door, and this module is what resolves it. The host the client STATED is what decides it
//!   (`service_common::data_plane_addressing::addressed_authority`), so nothing of the
//!   addressing is invented.
//!
//! Both front doors read ONE statement of which mapping answers a path
//! ([`crate::service::domain_name_socket::matching_mapping`]), ONE statement of which host
//! names a record ([`crate::service::domain_name_socket::host_names_domain_name`]) and ONE
//! statement of the path within the api a mapping selects ([`invoke_path`], which the socket
//! re-addresses with), so the address a request arrives at cannot change which api answers it.
//!
//! Nothing here writes: every reading is of records the control plane persisted.

use super::Addressed;
use crate::state::ComAmazonawsApigatewayv2Apigatewayv2State as ServiceStore;

/// What a request addressed to a custom domain name reaches: the record it arrived on, the api
/// the mapping that answers its path names, and the path within that api's endpoint.
///
/// The path is OWNED, because it is COMPOSED — the stage the mapping names, then what the
/// client carried under the base path — rather than a slice of the request.
#[derive(Debug)]
pub(super) struct Arrival<'s> {
    /// The custom domain name the request was addressed to, which is what says it did NOT
    /// arrive on the api's own default `execute-api` endpoint.
    pub(super) domain_name: &'s str,
    /// The api the mapping that answers this path names.
    pub(super) api_id: &'s str,
    /// `/{stage}/{path-under-the-base-path}`, the path within the api's endpoint — what the
    /// stage and the route are read from, exactly as for a request that reached the api's own
    /// endpoint host.
    pub(super) invoke_path: String,
}

/// What `addressed` reaches through the account's custom domain names, or `None` when it
/// reaches none: a host that names no record of the account, and a path no api mapping of that
/// record answers.
pub(super) fn arrival<'s>(
    store: &'s ServiceStore,
    addressed: &Addressed<'_>,
) -> Option<Arrival<'s>> {
    let record = addressed_record(store, addressed)?;
    let (mapping, under) =
        crate::service::domain_name_socket::matching_mapping(record, addressed.path)?;
    Some(Arrival {
        domain_name: &record.domain_name,
        api_id: &mapping.api_id,
        invoke_path: invoke_path(&mapping.stage, under),
    })
}

/// Whether `addressed` is a request to a custom domain name of this account AT ALL — whatever
/// its path resolves to.
///
/// What it is for is the CLAIM: the front door of a custom domain name is the account's, so a
/// request addressed to one of its names is this plane's to answer even when no api mapping of
/// that record answers the path. Such a request reaches no api of the account, which is the
/// refusal API Gateway's own front door makes (`super::unresolved_endpoint_answer`), and
/// leaving it unclaimed would answer it with the emulator declining to place an address the
/// account itself created.
pub(super) fn addresses_a_domain_name(store: &ServiceStore, addressed: &Addressed<'_>) -> bool {
    addressed_record(store, addressed).is_some()
}

/// The custom domain name record `addressed` arrived on, or `None` for a request that arrived
/// on none: a host that names no record of the account, a host that is an ADDRESS of this
/// emulator or sits in a zone AWS publishes its own endpoints in, and a path that is a PLANE of
/// this emulator rather than one a mapping answers.
///
/// An account that has created no custom domain name at all is answered without reading a host:
/// every request of every stage asks this, and nothing can be addressed to a record that is not
/// there.
///
/// The two kinds of host no record stands for, whatever a record is NAMED: an address of this
/// emulator itself (`::emulator_server::is_loopback_host`, the core's own reading), which is
/// what a client addresses the front door by and is no DNS name a customer could point
/// anywhere, and a host in a zone AWS publishes its OWN endpoints in
/// (`service_common::data_plane_addressing::addresses_an_aws_endpoint`, the tree's one reading
/// of that question) — a name no customer can be issued there, so a record whose name is a
/// wildcard over such a zone can take neither another service's traffic nor this service's own
/// control plane.
fn addressed_record<'s>(
    store: &'s ServiceStore,
    addressed: &Addressed<'_>,
) -> Option<&'s crate::state::DomainName> {
    if store.domain_names.is_empty() || addresses_another_plane(addressed.path) {
        return None;
    }
    let addressed_host = addressed.host?;
    let host = ::service_common::data_plane_addressing::host_only(addressed_host);
    if host.is_empty() || ::emulator_server::is_loopback_host(host) {
        return None;
    }
    if ::service_common::data_plane_addressing::addresses_an_aws_endpoint(addressed_host) {
        return None;
    }
    store.domain_names.values().find(|record| {
        crate::service::domain_name_socket::host_names_domain_name(&record.domain_name, host)
    })
}

/// Whether `path` names a PLANE of this emulator rather than a path an api mapping answers:
/// the reserved namespace a client of this emulator names an api's own endpoint in
/// ([`crate::state::LOCAL_ENDPOINT_PREFIX`]), and the `@connections` path space, which belongs
/// to ApiGatewayManagementApi on every host of this service.
///
/// No client of AWS carries such a path to a custom domain name, and a request that carries one
/// names the plane it spells — so it is served there, under whatever host it arrived on, and no
/// mapping is read for it. (This service's own modelled control-plane path space is refused
/// ahead of this reading, by [`super::resolve`], for the same reason.)
fn addresses_another_plane(path: &str) -> bool {
    if super::addresses_connections(path) {
        return true;
    }
    super::split_first_segment(path).is_some_and(|(first, _rest)| {
        first == crate::state::LOCAL_ENDPOINT_PREFIX || crate::ROUTE_PATH_SEGMENTS.contains(&first)
    })
}

/// The path WITHIN the api that a mapping naming `stage` selects for the path `under` left
/// beneath its base path.
///
/// A named stage is reached with its name as the first path segment, and the stage that serves
/// an api's root ([`crate::state::DEFAULT_STAGE_NAME`]) with no stage segment at all — its name
/// is a reserved word of this service, never a path segment of a deployment. What the client
/// carried under the base path follows, with its separator, and NOTHING follows a path that was
/// the base path itself: a `/` written there would be one more segment than the client sent
/// (`super::Target` counts the empty segment a trailing `/` makes).
///
/// THE statement of that composition, for both front doors of a custom domain name: this plane,
/// and the socket the record publishes, which re-addresses the request it accepted under the
/// same path ([`crate::service::domain_name_socket`]) — so one request resolves to one stage
/// path at either address. `pub(in crate::service)` for that second caller.
pub(in crate::service) fn invoke_path(stage: &str, under: &str) -> String {
    let stage = if stage == crate::state::DEFAULT_STAGE_NAME {
        String::new()
    } else {
        format!("/{stage}")
    };
    if under.is_empty() {
        return stage;
    }
    format!("{stage}/{under}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the path a mapping selects is the stage it names followed by what the client
    /// carried under the base path, and the stage that serves an api's root adds no segment.
    #[test]
    fn a_mappings_invoke_path_is_its_stage_and_the_path_under_its_base_path() {
        // The rows span what the composition generalises over: a named stage and the root
        // stage, each with a path under the base path, with nothing under it, and with a
        // nested path.
        let cases: &[(&str, &str, &str, &str)] = &[
            ("a named stage and a path", "prod", "items", "/prod/items"),
            ("a named stage and nothing under the key", "prod", "", "/prod"),
            (
                "a named stage and a nested path",
                "prod",
                "items/42",
                "/prod/items/42",
            ),
            (
                "the root stage and a path",
                crate::state::DEFAULT_STAGE_NAME,
                "items",
                "/items",
            ),
            (
                "the root stage and nothing under the key",
                crate::state::DEFAULT_STAGE_NAME,
                "",
                "",
            ),
        ];
        for (case, stage, under, expected) in cases {
            let composed = invoke_path(stage, under);
            assert_eq!(
                composed, *expected,
                "case {case:?}: expected {expected:?}, got {composed:?}"
            );
        }
    }
}
