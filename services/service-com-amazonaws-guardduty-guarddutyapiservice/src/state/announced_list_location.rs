// SPDX-License-Identifier: CC0-1.0

//! WHAT an uploaded list's `Location` reads back as.
//!
//! # The rule this module implements
//!
//! Every uploaded-list family carries a `Location` — "the URI of the file that contains
//! the IPSet" and its threat-list and entity-set siblings
//! (`smithy_query(operation="GetIPSet")`) — and that URI is the ADDRESS a client
//! dereferences to fetch the list. A read therefore REPORTS the stored URI as the writer
//! spelled it, with one exception: a URI that names a HOST this emulator does not answer
//! on is announced at the address the reading request arrived at, because an address this
//! process cannot stand behind is an address no client of it can fetch the list from.
//!
//! So:
//!
//! * an `s3://bucket/key` URI names NO host — its authority is the bucket — so it can
//!   address nothing outside this emulator and is reported character for character,
//!   scheme, bucket and key included;
//! * an object URL already on the address the request arrived at is likewise reported as
//!   stored;
//! * any other object URL — one on Amazon's own S3 endpoints, or on any third-party host —
//!   is re-announced as the path-style object URL `https://<this emulator>/<bucket>/<key>`,
//!   naming the SAME bucket and key ([`super::s3_object_of_location`]), so a client that
//!   follows it reaches this emulator instead of leaving it.
//!
//! The re-announcement never touches the STORED value: a create and an update hold their
//! own `Location` to the S3 object URL syntax ([`super::is_s3_location`]) and compare
//! against the value the account submitted, so what validation and an update-time
//! comparison see is always the caller's own string, whatever a read announces.
//!
//! The one projection the four uploaded-list reads share, so none of them can drift from
//! the others.

use ::service_common::data_plane_addressing as addressing;

/// The modelled maximum of the `Location` member — `@length 1..300`
/// (`smithy_query(shape="Location")`). An announced address that would not fit it is not
/// served: the stored URI, which was accepted under the same bound, is reported instead.
const LOCATION_MAX_LENGTH: usize = 300;

/// The scheme an address of THIS emulator is announced under: its listeners serve TLS.
const ANNOUNCED_SCHEME: &str = "https";

/// The `Location` a read-back of an uploaded list reports for `stored`, for the request
/// `ctx` is serving.
#[must_use]
pub fn announced_list_location(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    stored: &str,
) -> String {
    announced_at(own_authority(ctx).as_deref(), stored)
}

/// The address this emulator is reached at for the request being served: the endpoint the
/// request ADDRESSED when its caller resolves nothing else
/// ([`addressing::reached_emulator_endpoint_of_parts`]), else the one this process is
/// SERVED at ([`addressing::served_endpoint_authority`]).
///
/// `None` when neither resolves — a caller that reached this service by its own published
/// AWS host while nothing is bound to a socket — where there is no address of this process
/// to announce at all.
fn own_authority(ctx: &smithy_server_runtime::OperationContext<'_>) -> Option<String> {
    addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| addressing::served_endpoint_authority(ctx.endpoint().as_str()))
}

/// [`announced_list_location`] for an already-resolved own address: the rule itself, read
/// off the stored URI and that address alone.
fn announced_at(own: Option<&str>, stored: &str) -> String {
    // A URI naming no host — the `s3://` form — addresses nothing this emulator could
    // fail to serve, and a value this service cannot read as an S3 object is reported
    // exactly as stored rather than guessed at.
    if names_no_host(stored) {
        return stored.to_string();
    }
    let Some(own) = own else {
        return stored.to_string();
    };
    if location_authority(stored).eq_ignore_ascii_case(own) {
        return stored.to_string();
    }
    let Some((bucket, key)) = super::s3_object_of_location(stored) else {
        return stored.to_string();
    };
    let announced = format!("{ANNOUNCED_SCHEME}://{own}/{bucket}/{key}");
    match announced.chars().count() <= LOCATION_MAX_LENGTH {
        true => announced,
        false => stored.to_string(),
    }
}

/// Does `location` name no host at all? True for the `s3://bucket/key` form, whose
/// authority is the BUCKET rather than a host, and for a value carrying no `scheme://` —
/// neither of which can point at a host this emulator does not answer on.
fn names_no_host(location: &str) -> bool {
    match location.split_once("://") {
        Some((scheme, _)) => scheme.eq_ignore_ascii_case("s3"),
        None => true,
    }
}

/// The authority `location` names: what follows its `scheme://` up to the first `/`.
fn location_authority(location: &str) -> &str {
    let after_scheme = addressing::url_authority(location);
    after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
}
