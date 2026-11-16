// SPDX-License-Identifier: CC0-1.0

//! Serves the ICON of a workload catalogue entry at the signed address `GetWorkload`
//! hands out for it.
//!
//! `WorkloadData$iconUrl` is an absolute URL a client fetches next, and the icon it names
//! is an artifact of the catalogue this emulator owns: so the address the operation signs
//! (`crate::state::workload_icons::icon_url`) names this emulator's own base URL, and this
//! route answers a `GET` on it with the image the catalogue entry's icon is
//! (`crate::state::workload_icons::workload_icon`). Nothing a client is handed leaves the
//! endpoint it is already talking to.
//!
//! The model binds no operation to those paths — an icon is not an API call, it is the
//! artifact an API member points at — so the route is claimed through
//! [`smithy_server_runtime::Service::override_operation`] rather than through the
//! generated routing table. It is claimed only for a PRE-SIGNED request
//! ([`is_presigned`]), whose signature and expiry window the framework verifies before the
//! route runs; WHICH icon an address may reach is then decided by the store that minted the
//! address, against the authenticator the address carries
//! (`crate::state::workload_icons::workload_icon`). So an icon has exactly one address: the
//! one a catalogue read handed out.

use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};

use crate::state::workload_icons;

/// The method an icon is fetched on, which is the method its address is signed for.
const FETCH_METHOD: &str = "GET";

/// The media type the one answer of this route that is not an image is served as.
const PLAIN_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// What this route answers an address naming no catalogue entry of this emulator's: the
/// address is fetched by a plain HTTP client rather than by an AWS SDK, so the answer is
/// the status itself with a one-line reason, and no API error envelope.
const NO_ICON: &str = "no workload icon at this location\n";

/// Serves the icon of one workload catalogue entry.
///
/// Owns the shared engine, as this service's operations do, because the icon it serves is
/// the one the catalogue entry in that store has: nothing is copied aside, so a catalogue
/// read and a fetch of the address it handed out cannot disagree about an entry's icon.
pub struct WorkloadIconRoute<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> WorkloadIconRoute<Eng> {
    /// Build the route from the shared engine, as the service's operations are built.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// The `workloadName` whose icon `req` asks for and the authenticator its address
    /// carries, or `None` when the request is not a fetch this route serves.
    ///
    /// Two things make it one: the path, read the way the signer wrote it — its segments
    /// percent-decoded — and the SigV4 query authentication of a PRE-SIGNED request
    /// ([`is_presigned`]). Whether the address is one this emulator MINTED is decided where
    /// the store is: the authenticator read off the query travels with the name for the
    /// store to check, so a request carrying none is answered exactly as one naming an entry
    /// that does not exist.
    pub fn claims(&self, req: &RtRequest) -> Option<(String, String)> {
        if req.method() != FETCH_METHOD || !is_presigned(req.uri()) {
            return None;
        }
        let workload_name =
            workload_icons::workload_in_icon_path(&decoded_segments(req.uri()))?.to_string();
        let token = query_field(req.uri(), workload_icons::ICON_TOKEN_FIELD).unwrap_or_default();
        Some((workload_name, token))
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for WorkloadIconRoute<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        _ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        // The context is not consulted, because a PRE-SIGNED fetch carries no caller
        // identity for it to hold: what the framework verified before this route ran is a
        // signature over this method, this path and this query, not who sent it. WHICH icon
        // an address may reach is therefore decided by the store that minted the address,
        // against the authenticator the address carries.
        let answer = match self.claims(req) {
            Some((workload_name, token)) => {
                match workload_icons::workload_icon(&*self.engine, &workload_name, &token) {
                    Some(image) => icon_response(200, workload_icons::ICON_MEDIA_TYPE, image),
                    // The address is not one this store minted for an entry it holds: it
                    // carries no authenticator of this store's, or one for another entry, or
                    // it names an entry the catalogue does not hold. Each is answered the
                    // same way: there is nothing at this location.
                    None => icon_response(404, PLAIN_MEDIA_TYPE, NO_ICON.as_bytes().to_vec()),
                }
            }
            // Unreachable through an `override_operation` that hands this route only the
            // requests it claims; answered as the framework answers a request that named
            // no operation rather than guessed at.
            None => svc.definition().routing_error_response(req),
        };
        Box::pin(async move { answer })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` keeps the framework's own decision for this service: the address this
        // route serves is SigV4 query-signed, and nothing here may weaken that.
        None
    }
}

/// One answer of this route: the bytes, their media type and their length.
fn icon_response(status: u16, media_type: &str, body: Vec<u8>) -> RtResponse {
    let length = body.len();
    let mut response = RtResponse::new(icon_status(status), SdkBody::from(body));
    response
        .headers_mut()
        .insert("content-type", media_type.to_string());
    response
        .headers_mut()
        .insert("content-length", length.to_string());
    response
}

/// A status this route answers with. Every call site passes a constant, so the conversion
/// cannot fail.
fn icon_status(code: u16) -> StatusCode {
    StatusCode::try_from(code).expect("a workload icon fetch answers constant statuses")
}

/// Whether `uri` carries the SigV4 query authentication of a pre-signed request: the
/// credential scope and the signature [`smithy_server_runtime::UrlSigner`] puts on a URL
/// it signs.
///
/// Presence is all this reads, and all it has to: the framework's own request
/// authentication is what VERIFIES such a request before any route runs — the signature
/// over the method, the path and the query, and the `X-Amz-Expires` window the signer
/// wrote, a URL past which is refused. What this test adds is that nothing which is not a
/// pre-signed request is served from these paths at all.
fn is_presigned(uri: &str) -> bool {
    const PRESIGNED_FIELDS: [&str; 2] = ["X-Amz-Credential", "X-Amz-Signature"];
    let query = uri.split_once('?').map(|(_, query)| query).unwrap_or("");
    PRESIGNED_FIELDS.iter().all(|field| {
        query.split('&').any(|pair| {
            pair.split_once('=')
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case(field))
        })
    })
}

/// The path segments of `uri`, percent-decoded — the same view of a path the signer built
/// the URL from.
fn decoded_segments(uri: &str) -> Vec<String> {
    uri.parse::<http::Uri>()
        .map(|parsed| {
            parsed
                .path()
                .strip_prefix('/')
                .unwrap_or_default()
                .split('/')
                .map(smithy_server_runtime::percent_decode)
                .collect()
        })
        .unwrap_or_default()
}

/// The value of one query field of `uri`, percent-decoded, or `None` when the request
/// carries no such field.
fn query_field(uri: &str, field: &str) -> Option<String> {
    let query = uri.split_once('?').map(|(_, query)| query)?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == field).then(|| smithy_server_runtime::percent_decode(value))
    })
}
