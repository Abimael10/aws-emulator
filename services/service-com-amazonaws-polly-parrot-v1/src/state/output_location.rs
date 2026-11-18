// SPDX-License-Identifier: CC0-1.0

//! WHERE a synthesis task's output is ANNOUNCED: the address this emulator serves that
//! object at.
//!
//! # The rule this module implements
//!
//! `SynthesisTask$OutputUri` is the "Pathway for the output speech file" — an address the
//! CALLER follows to read the object a completed task left in the bucket the start call
//! named. So it has to lead back to the object storage of THIS emulator, which is reached
//! at the endpoint the request itself reached: a URI naming a host under AWS's own zones
//! sends the client out of the emulated environment, where the bucket does not exist, or
//! — with no egress — nowhere at all.
//!
//! Beneath that address the object is addressed PATH-STYLE, `/<bucket>/<key>`, which is
//! the form the one endpoint this emulator answers on serves: a bucket's name is a label
//! of a published AWS hostname rather than of this emulator's own host.
//!
//! The reading of WHICH address that is comes from
//! `service_common::data_plane_addressing`, in the order every service announcing an
//! address at the front door takes it, so this service cannot drift from the others:
//!
//! * the EMULATOR'S OWN endpoint the request addressed
//!   ([`reached_emulator_endpoint_of_parts`]) — a client configured with one local
//!   endpoint URL resolves nothing else, so it is handed back the very address it is
//!   already talking to, port included;
//! * else, for a caller that resolves AWS's own hostnames, the address this process is
//!   SERVED at ([`served_endpoint_authority`]), the one address a client on this machine
//!   can reach it at;
//! * else the base [`::smithy_server_runtime::OperationContext::endpoint`] reports for an
//!   emulator served on no port at all — a host nothing resolves, which is the truth
//!   about such an emulator rather than a name of AWS's.
//!
//! Nothing here resolves a name or opens a connection: it reads request parts and returns
//! a string.

use ::service_common::data_plane_addressing::{
    reached_emulator_endpoint_of_parts, served_endpoint_authority, url_authority,
};
use ::smithy_server_runtime::OperationContext;

/// The address the output object `key` of `bucket` is announced at, for the request `ctx`
/// is answering: the object addressed path-style under [`announced_base`].
///
/// The key is taken as the writer built it, so the location a task reports and the object
/// its completion writes cannot drift apart.
#[must_use]
pub fn announced_output_uri(ctx: &OperationContext<'_>, bucket: &str, key: &str) -> String {
    format!("{}/{bucket}/{key}", announced_base(ctx))
}

/// The base URL — `scheme://host[:port]`, no path — an announced location is rooted at:
/// the endpoint serving THIS request, in the order this module's header states.
fn announced_base(ctx: &OperationContext<'_>) -> String {
    let endpoint = ctx.endpoint();
    // Without the trailing slash, which would double the separator in front of the path.
    let base = endpoint.as_str().trim_end_matches('/');
    // The base's own authority, read through the shared reading rather than a second split
    // of a URL here; the scheme is whatever precedes it. `EndpointUrl` admits only an
    // absolute `https` URL, so the default stands in for nothing the core reports.
    let served = url_authority(base);
    let scheme = match base.strip_suffix(served) {
        Some(scheme) if !scheme.is_empty() => scheme,
        _ => "https://",
    };
    let authority = reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| served_endpoint_authority(base))
        .unwrap_or_else(|| served.to_string());
    format!("{scheme}{authority}")
}
