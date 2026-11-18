// SPDX-License-Identifier: CC0-1.0

//! WHERE an uploaded list is ANNOUNCED: the address a read-back hands a caller for
//! the list object the create named.
//!
//! # The rule this module implements
//!
//! Every uploaded-list family reports a `Location` — "the URI of the file that
//! contains the IPSet" and its threat-list and entity-set siblings
//! (`smithy_query(operation="GetIPSet")`) — and that URI is an address the CALLER
//! follows to fetch the list: the console does, automation that round-trips a list
//! does, and so does any ingestion path. So it has to lead back to the object storage
//! of THIS emulator, reached at the endpoint the request itself arrived at. A URI
//! naming a host under AWS's own zones sends the client out of the emulated
//! environment, where the object does not exist, or — with no egress — nowhere at
//! all.
//!
//! What a read-back announces is therefore the SAME object the create named — the
//! bucket and key are read off the stored value
//! ([`super::s3_object_location`]) — addressed PATH-STYLE beneath this emulator's own
//! endpoint, `/<bucket>/<key>`, which is the form the one endpoint this emulator
//! answers on serves: a bucket's name is a label of a published AWS hostname rather
//! than of this emulator's own host.
//!
//! The stored value itself is NOT rewritten: a create or an update is held to the S3
//! URL syntax ([`super::is_s3_location`]) and keeps what the caller submitted, so the
//! validation side of that member is untouched and an update still compares against
//! the value it was given. Only the read-back's projection is announced.
//!
//! The reading of WHICH address this emulator is reached at comes from
//! `service_common::data_plane_addressing`, in the order every service announcing an
//! address at the front door takes it, so this service cannot drift from the others:
//!
//! * the EMULATOR'S OWN endpoint the request addressed
//!   ([`reached_emulator_endpoint_of_parts`]) — a client configured with one local
//!   endpoint URL resolves nothing else, so it is handed back the very address it is
//!   already talking to, port included;
//! * else, for a caller that resolves AWS's own hostnames, the address this process
//!   is SERVED at ([`served_endpoint_authority`]), the one address a client on this
//!   machine can reach it at;
//! * else the base [`::smithy_server_runtime::OperationContext::endpoint`] reports for
//!   an emulator served on no port at all — a host nothing resolves, which is the
//!   truth about such an emulator rather than a name of AWS's.
//!
//! Nothing here resolves a name or opens a connection: it reads request parts and
//! returns a string.

use ::service_common::data_plane_addressing::{
    reached_emulator_endpoint_of_parts, served_endpoint_authority, url_authority,
};
use ::smithy_server_runtime::OperationContext;

/// The `Location` a read-back of an uploaded list announces for `stored` — the
/// location the create or update persisted.
///
/// The same object, announced at this emulator's own endpoint. Two stored values are
/// announced unchanged: one that addresses no S3 object, since there is no object in it
/// to re-address (the create that accepted it is what holds the syntax,
/// [`super::is_s3_location`]), and one whose announced address would not fit the
/// modelled `Location` of `len 1..300` ([`LOCATION_MAX_LENGTH`]), since a URL cut to
/// length addresses nothing at all.
#[must_use]
pub fn announced_list_location(ctx: &OperationContext<'_>, stored: &str) -> String {
    let Some((bucket, key)) = super::s3_object_location(stored) else {
        return stored.to_string();
    };
    let announced = format!("{}/{bucket}/{key}", announced_base(ctx));
    match announced.chars().count() <= LOCATION_MAX_LENGTH {
        true => announced,
        false => stored.to_string(),
    }
}

/// The longest a `Location` may be: every uploaded-list family targets the same
/// `len 1..300` member (`smithy_query(operation="GetIPSet")`).
pub const LOCATION_MAX_LENGTH: usize = 300;

/// The base URL — `scheme://host[:port]`, no path — an announced location is rooted
/// at: the endpoint serving THIS request, in the order this module's header states.
fn announced_base(ctx: &OperationContext<'_>) -> String {
    let endpoint = ctx.endpoint();
    // Without the trailing slash, which would double the separator in front of the
    // path.
    let base = endpoint.as_str().trim_end_matches('/');
    // The base's own authority, read through the shared reading rather than a second
    // split of a URL here; the scheme is whatever precedes it. `EndpointUrl` admits
    // only an absolute `https` URL, so the default stands in for nothing the core
    // reports.
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
