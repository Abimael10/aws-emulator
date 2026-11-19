// SPDX-License-Identifier: CC0-1.0

//! WHERE an uploaded list's `Location` is ANNOUNCED on a read-back: the address THIS
//! emulator serves that list's object at.
//!
//! # The rule this module implements
//!
//! Every uploaded-list family carries a `Location` — "the URI of the file that contains
//! the IPSet" and its threat-list and entity-set siblings
//! (`smithy_query(operation="GetIPSet")`) — an address a caller FOLLOWS to read the file
//! the list was uploaded as. So the address a read reports has to lead back to the object
//! storage of THIS emulator, which is reached at the endpoint the request itself reached:
//! an address under a host AWS publishes sends the client out of the emulated
//! environment, where neither the bucket nor the object exists, or — with no egress —
//! nowhere at all.
//!
//! What the caller's own value fixes is WHICH object: the bucket and the key it names
//! ([`super::s3_object_of_location`], the one reading a create and an update are held to
//! as well). That value is what the store keeps, verbatim, and what an update compares
//! the value it was given against; only the address it is REPORTED at is this emulator's.
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
//!
//! The one projection the four uploaded-list reads share, so none of them can drift from
//! the others.

use ::service_common::data_plane_addressing::{
    reached_emulator_endpoint_of_parts, served_endpoint_authority, url_authority,
};
use ::smithy_server_runtime::OperationContext;

/// The `Location` a read-back of an uploaded list reports for `stored` — the object the
/// create or the update named, addressed path-style at the endpoint serving this request.
///
/// A `stored` value that addresses no object at all is reported as it is held: every
/// stored location passed [`super::is_s3_location`], so this stands in for nothing a
/// create or an update can persist, and reporting the record is still the truth about
/// what the service holds.
#[must_use]
pub fn announced_list_location(ctx: &OperationContext<'_>, stored: &str) -> String {
    match super::s3_object_of_location(stored) {
        Some((bucket, key)) => format!("{}/{bucket}/{key}", announced_base(ctx)),
        None => stored.to_string(),
    }
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
