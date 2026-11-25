// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of the absolute URI this service ANNOUNCES for a resource — the address
//! a caller is handed to address that resource with afterwards.
//!
//! Every create on this surface that binds an output member to the `Location` header reports
//! such a URI: "the fully qualified URI of the new distribution resource just created"
//! (`CreateDistribution`'s `Location`) and the same member on each of the other creates. It
//! is the address an SDK's redirect handling, a CLI or any client that hands the value on
//! DIALS next, so it has to lead back to the endpoint this emulator answered the create on.
//! AWS's own published control-plane host ([`SERVICE_ENDPOINT`]) resolves away from this
//! process on the machine it runs on: a client that follows a URI rooted there leaves the
//! emulator for public AWS, where the resource does not exist, or reaches nothing at all.
//!
//! So the SCHEME and the HOST are the ones this request reached, and only the PATH beneath
//! them is the resource's own ([`location_uri`]). A caller that resolves AWS's own hostnames
//! on an emulator served at no address of its own is the one case where there is no endpoint
//! of this process to name, and it is answered the service's published host — the address
//! such a caller is already resolving.

use super::SERVICE_ENDPOINT;

/// The `scheme://host[:port]` — no path — every URI this service announces is rooted on:
/// the endpoint serving THIS request.
///
/// [`::smithy_server_runtime::OperationContext::endpoint`] is "the base URL a later request
/// addresses… Callers send normally signed requests to it", so it is the authoritative
/// answer. Ahead of it comes the emulator's own endpoint the request REALLY reached, read
/// through the tree's one reading of that question
/// (`service_common::data_plane_addressing::reached_emulator_endpoint`): a client configured
/// with one local endpoint URL resolves nothing else, so it is handed back the very address
/// it is already talking to, port included. That reading answers `None` for a host that
/// names a service — which this service's own published host does — and such a request is
/// answered the address this process is served at, so a resource created through an
/// in-process call of another service is announced an address that caller's own client can
/// reach too.
///
/// An emulator served on no port at all ([`::smithy_server_runtime::UNSERVED_URL_BASE`]) has
/// no address of its own to name, and the caller that reached it on a service host resolves
/// AWS's own hostnames: that one is answered this service's published endpoint.
#[must_use]
pub fn announced_base(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let endpoint = ctx.endpoint();
    // Without the trailing slash, which would double the separator in front of the path.
    let base = endpoint.as_str().trim_end_matches('/');
    // The base's own authority, read through the same shared reading rather than a second
    // split of a URL in this crate; the scheme is whatever precedes it.
    let served = ::service_common::data_plane_addressing::url_authority(base);
    let scheme = match base.strip_suffix(served) {
        Some(scheme) if !scheme.is_empty() => scheme,
        _ => "https://",
    };
    match announced_endpoint_authority(ctx) {
        Some(authority) => format!("{scheme}{authority}"),
        None => format!("https://{SERVICE_ENDPOINT}"),
    }
}

/// The `host[:port]` of THIS EMULATOR's own endpoint, for the request `ctx` is serving — the
/// address every value this service announces for a caller to dial is rooted at — or `None`
/// for a request this emulator has no address of its own to name to.
///
/// The one reading behind [`announced_base`], so every URI this service announces for a caller
/// to follow is rooted at one endpoint: the endpoint the request REALLY reached first (a client
/// configured with one local endpoint URL resolves nothing else, so it is handed back the very
/// address it is already talking to, port included), and the address this process is served at
/// behind it, so a resource created through an in-process call of another service is announced
/// an address that caller's own client can reach too.
///
/// A distribution's `DomainName` is NOT rooted here: it is a DNS name rather than a URI, so it
/// names no address of this emulator at all, and what decides whether a listener is opened for
/// that distribution to be dialled at is read here only for that
/// ([`crate::state::opened_edge_endpoint`]).
///
/// `None` is the one case where there is no endpoint of this process to name: a caller that
/// resolves AWS's own hostnames, reaching an emulator served on no port at all
/// ([`::smithy_server_runtime::UNSERVED_URL_BASE`]).
#[must_use]
pub fn announced_endpoint_authority(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> Option<String> {
    let endpoint = ctx.endpoint();
    let base = endpoint.as_str().trim_end_matches('/');
    reached_emulator_endpoint(ctx.parts())
        .or_else(|| ::service_common::data_plane_addressing::served_endpoint_authority(base))
}

/// The fully qualified URI a `Location` header reports for `path`: the resource's own path
/// under the endpoint this request reached ([`announced_base`]), so following the header
/// reaches the resource that was just created rather than a host this emulator does not
/// serve.
#[must_use]
pub fn location_uri(ctx: &::smithy_server_runtime::OperationContext<'_>, path: &str) -> String {
    format!("{}{path}", announced_base(ctx))
}

/// The EMULATOR'S OWN endpoint this request reached — an address or a loopback name, which
/// is what a client configured with one local endpoint URL sends — and `None` for a request
/// whose host names a service, which is a client that resolves AWS's own hostnames.
///
/// The host travels in the `Host` header or in the authority of an absolute-form URI, and
/// both are read by the tree's one reading of the question
/// (`service_common::data_plane_addressing::reached_emulator_endpoint`), so this service
/// writes no second rule for which host is the emulator's own.
fn reached_emulator_endpoint(parts: &::http::request::Parts) -> Option<String> {
    ::service_common::data_plane_addressing::reached_emulator_endpoint(
        &parts.uri.to_string(),
        parts
            .headers
            .get(::http::header::HOST)
            .and_then(|value| value.to_str().ok()),
    )
}
