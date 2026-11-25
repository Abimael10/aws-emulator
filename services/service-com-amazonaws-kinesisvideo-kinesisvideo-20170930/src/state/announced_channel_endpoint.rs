// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a signaling channel's `HTTPS` and `WEBRTC` endpoint is ANNOUNCED at.
//!
//! `GetSignalingChannelEndpoint` "provides an endpoint for the specified signaling channel to
//! send and receive messages", and "if you specify HTTPS as the protocol, this API generates
//! an HTTPS endpoint"
//! (`smithy_query(query="doc", shape="GetSignalingChannelEndpoint")`). So the value is not a
//! label a client reads: it is the base URL of the client's NEXT request — the signaling
//! service's `GetIceServerConfig` and `SendAlexaOfferToMaster` routes — which the client then
//! DIALS.
//!
//! RULE: an address this emulator announces for a client to dial names a host THIS process
//! ANSWERS on. Two readings answer that, both taken from the tree's one reading of a data
//! plane's addressing ([`::service_common::data_plane_addressing`]):
//!
//! * the EMULATOR'S OWN endpoint the request reached, when this process answers on the host
//!   that endpoint names — the interface the core binds its listeners on
//!   ([`::service_common::data_plane_addressing::host_names_the_bound_interface`]) — so a
//!   client configured with one local endpoint URL is handed the very address it is already
//!   talking to, port and all;
//! * otherwise the address this process is SERVED at
//!   ([`::service_common::data_plane_addressing::emulator_endpoint_authority`], the base
//!   [`::smithy_server_runtime::OperationContext::endpoint`] reports).
//!
//! What a request CARRIED is therefore adopted only when it is an address of this process. A
//! carried authority is whatever the client was pointed at — a forwarding front end, an
//! endpoint of an earlier run, or one of AWS's own regional hosts — and announcing it sends
//! the next request to an address this process does not answer on: the client either leaves the
//! emulated environment for infrastructure the test has no relationship with, or resolves
//! nothing at all, while the answer claims a channel is reachable there. The same holds for
//! this service's own published regional host ([`super::service_host`]), which names where AWS
//! answers, not where this process does.
//!
//! The `WSS` protocol is announced at the socket the signaling service serves a channel's peers
//! on, which is that crate's reading of the same question (its `state::ensure_wss_endpoint`,
//! over [`::service_common::data_plane_addressing::listener_authority`]), and the relay URIs
//! `GetIceServerConfig` hands a viewer are read there too — so every address this family
//! announces is one of this emulator's own, and none of them is written twice.

/// The `host[:port]` every `HTTPS` and `WEBRTC` channel endpoint this service announces is
/// addressed at, for the request being answered: the address this process answers on, by the
/// reading this module's header states.
///
/// A URL announcing it carries the scheme the `ChannelProtocol` names and this authority, and
/// nothing else: "the client needs to add the API name to the returned endpoint" for the
/// family's data planes, so an announced value carries no path of its own.
#[must_use]
pub fn announced_channel_endpoint_authority(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    use ::service_common::data_plane_addressing as addressing;
    addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .filter(|authority| addressing::host_names_the_bound_interface(authority))
        .unwrap_or_else(|| addressing::emulator_endpoint_authority(ctx))
}
