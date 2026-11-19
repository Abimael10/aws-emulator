// SPDX-License-Identifier: CC0-1.0

//! WHICH address this account's dedicated custom endpoint is ANNOUNCED at — the one value
//! `RegisterCustomEndpoint` mints and `GetCustomEndpoint` reports.
//!
//! `EndpointAddress` is "The IoT managed integrations dedicated, custom endpoint for the
//! device to route traffic through": not a label a client reads, but the connection target it
//! then DIALS. So the only thing the member may carry is an address THIS emulator answers on:
//! the `host[:port]` of the one endpoint it serves its whole surface at
//! ([`::service_common::data_plane_addressing::emulator_endpoint_authority`]).
//!
//! A name in a zone Amazon Web Services publishes its own endpoints in resolves nowhere near
//! this process, or worse at genuine infrastructure, so a device handed one leaves the
//! emulated environment carrying the credentials it holds and reaches no endpoint at all. A
//! name of this emulator's OWN invention is no better: nothing answers for a sub-name of the
//! host this process is reached at, so a client that resolves one reaches nothing either.
//!
//! ONE address therefore answers for every account and every region here, where AWS mints a
//! dedicated host per account: this process serves its whole surface at a single endpoint and
//! resolves each request to the service its SigV4 credential scope names, so a client pointed
//! at that address reaches this service, in the region it signs for. The modelled
//! `EndpointAddress` pattern (`^[A-Za-z0-9._@-]+$`, which admits no `:`) describes the public
//! hostname AWS mints, which no address of this process is; the member carries the address the
//! caller can actually dial, because for a served emulator every value that pattern admits is
//! one it cannot.
//!
//! Nothing of a request moves the answer: the serving address is a property of the PROCESS,
//! not of the host a request happened to carry, so the address the register mints and the
//! address the read reports cannot drift from one another.

/// The address a registered custom endpoint carries, for the emulator serving `ctx`: the
/// `host[:port]` of the one endpoint this process answers on, with no scheme and no path,
/// which is the form a hostname member carries.
///
/// This is the tree's one reading of where this emulator answers
/// ([`::service_common::data_plane_addressing::emulator_endpoint_authority`], "the
/// `host[:port]` of the ONE endpoint this emulator serves its whole surface at … what a
/// DISCOVERY operation publishes for a plane the client then DIALS"), so this service states
/// no second rule for it.
#[must_use]
pub fn announced_custom_endpoint_address(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
}
