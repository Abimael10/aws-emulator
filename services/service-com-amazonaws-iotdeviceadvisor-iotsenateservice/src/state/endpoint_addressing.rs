// SPDX-License-Identifier: CC0-1.0

//! WHICH address an operation that REPORTS a Device Advisor endpoint reports: the one value
//! `GetEndpoint` vends and a suite run reports and persists for its primary device.
//!
//! `endpoint` is not a label a client reads — it is the connection target it then DIALS: the
//! device under test, or the SDK driving it, is handed the value and opens its connection to
//! it. So the only thing the member may carry is an address THIS emulator answers on: the
//! `host[:port]` of the one endpoint it serves its whole surface at
//! ([`::service_common::data_plane_addressing::emulator_endpoint_authority`], read here off
//! the base that function reads).
//!
//! A name of AWS's own — the `<endpoint-id>.deviceadvisor.iot.<region>.amazonaws.com` host
//! AWS vends — resolves nowhere near this process, or worse at genuine AWS infrastructure,
//! so a device handed one leaves the emulated environment carrying the credentials it holds
//! and reaches no Device Advisor plane at all. A name of this emulator's own invention is no
//! better: nothing answers for a sub-name of the host this process is reached at, so a
//! client that resolves one reaches nothing either.
//!
//! ONE address therefore answers for every account, every region and every device here,
//! where AWS vends a hostname per account and per device: this process serves its whole
//! surface at a single endpoint and resolves each request to the service its SigV4
//! credential scope names, so a client pointed at that address reaches this service in the
//! region it signs for. The modelled `Endpoint` length bounds (`len 45..75`) describe the
//! public hostname AWS mints, which no address of this process is; the member carries the
//! address the caller can actually reach, because every value inside those bounds is one it
//! cannot.
//!
//! Nothing of a request moves the answer: the serving address is a property of the PROCESS,
//! not of the host a request happened to carry, so the address a suite run persists at its
//! start and the address `GetEndpoint` vends for that run's device cannot drift.

use ::service_common::data_plane_addressing as addressing;

/// The address a reported Device Advisor endpoint carries, for an emulator whose endpoint is
/// served at `served_base` ([`::smithy_server_runtime::OperationContext::endpoint`]): that
/// endpoint's `host[:port]`, with no scheme and no path, which is the form a hostname member
/// carries.
///
/// This is the tree's one reading of where this emulator answers
/// ([`addressing::url_authority`] of the base, which is what
/// [`addressing::emulator_endpoint_authority`] answers a handler), so this service states no
/// second rule for it.
pub fn announced_endpoint(served_base: &str) -> String {
    addressing::url_authority(served_base)
        .trim_end_matches('/')
        .to_string()
}
