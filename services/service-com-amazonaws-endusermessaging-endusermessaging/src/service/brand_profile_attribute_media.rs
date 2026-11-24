// SPDX-License-Identifier: CC0-1.0

//! Serves the media of a brand-profile attribute at the presigned URL an attribute read
//! hands out for it.
//!
//! `MediaDownloadUrl` is "a presigned Amazon S3 URL that you can use to download the
//! attribute media", and the location this emulator keeps that media at is this emulator
//! itself: `CreateBrandProfileAttributes` and `GetBrandProfileAttribute` sign a URL
//! addressing the path `crate::state::attribute_media_path` gives the attribute, so the URL
//! leads back here and verifies, and this route answers a `GET` on that address with the
//! very bytes the `attachmentBody` of that attribute carried.
//!
//! The model binds no operation to those paths — attribute media is not an API call, it is
//! the artifact an attribute points at — so the route is claimed through
//! [`smithy_server_runtime::Service::override_operation`] rather than through the generated
//! routing table. It is claimed only for a PRESIGNED request ([`is_presigned`]), whose
//! signature and one-hour window the framework verifies before the route runs; WHICH
//! attribute an address reaches is then read back off the path by the one reader of that
//! form (`crate::state::parse_attribute_media_path`), so an attribute's media has exactly
//! the address its own URL named.
//!
//! An attribute nothing has supplied media for serves nothing: no bytes are invented for
//! it, so such an address names no object, exactly as an address naming an attribute or a
//! profile the store does not hold does.
//!
//! What a download serves is also NAMED: the bytes are the artifact an attribute points at,
//! so the answer carries the attribute's own name and the file extension of the type those
//! bytes announce ([`content_disposition`]), which is what lets a caller render or save it
//! under a name. The name follows from the attribute in the store, so a download served
//! after an update is named exactly as the media it replaced would have been.

use std::sync::Arc;

use smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;

/// The method attribute media is fetched on, as the URL is signed for.
const DOWNLOAD_METHOD: &str = "GET";

/// The media type the one answer of this route that is not attribute media is served as.
const PLAIN_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// What this route answers an address that names no media this emulator holds: a presigned
/// download URL is fetched by a plain HTTP client rather than by an AWS SDK, so the answer
/// is the status itself with a one-line reason and no API error envelope.
const NO_MEDIA: &str = "no brand profile attribute media at this location\n";

/// Serves a download of one attribute's media.
///
/// Owns the shared engine, as an operation does, because the bytes it serves are the ones
/// the attribute holds: nothing is kept beside the attribute, so a download and a read of
/// the same attribute cannot disagree about its media.
pub struct AttributeMediaRoute<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> AttributeMediaRoute<Eng> {
    /// Build the route from the shared engine, as this service's operations are built.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// The profile and attribute `req` asks for, or `None` when it is not a download this
    /// route serves.
    ///
    /// Two things make it one: the path, read the way the signer wrote it — its segments
    /// percent-decoded, so an attribute name carrying the spaces
    /// `BrandProfileAttributeName` admits names the same attribute here as it does in the
    /// store — and the SigV4 query authentication of a presigned request
    /// ([`is_presigned`]).
    pub fn claims(&self, req: &RtRequest) -> Option<(String, String)> {
        if req.method() != DOWNLOAD_METHOD || !is_presigned(req.uri()) {
            return None;
        }
        crate::state::parse_attribute_media_path(&decoded_segments(req.uri()))
    }

    /// The media the address names, as `(media type, bytes)`, or `None` when this store
    /// holds none at it: a profile it does not hold, an attribute that profile does not
    /// hold, or an attribute nothing has supplied media for.
    fn stored_media(&self, brand_profile_id: &str, attribute_name: &str) -> Option<(String, Vec<u8>)> {
        let state = crate::state::service_state(&*self.engine)?;
        let store = state.lock();
        let media = store
            .brand_profile_attribute(brand_profile_id, attribute_name)?
            .media
            .as_ref()?;
        Some((media.content_type.clone(), media.bytes.clone()))
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for AttributeMediaRoute<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        _ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        // The context is not consulted, because a presigned fetch carries no caller
        // identity for it to hold: what the framework verified before this route ran is a
        // signature over this method, this path and this query, inside the window the URL
        // was signed for. Nothing here waits on anything, so the media is handed back the
        // moment it is asked for.
        let answer = match self.claims(req) {
            Some((brand_profile_id, attribute_name)) => {
                match self.stored_media(&brand_profile_id, &attribute_name) {
                    Some((media_type, bytes)) => media_response(
                        200,
                        &media_type,
                        bytes,
                        Some(content_disposition(&attribute_name, &media_type)),
                    ),
                    None => media_response(
                        404,
                        PLAIN_MEDIA_TYPE,
                        NO_MEDIA.as_bytes().to_vec(),
                        None,
                    ),
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
        // `None` keeps the framework's own decision for this service: the URL this route
        // serves is SigV4 query-signed, and nothing here may weaken that.
        None
    }
}

/// One download response: the bytes, their media type, their length and — for the media of
/// an attribute — how the download is to be presented and named.
fn media_response(
    status: u16,
    media_type: &str,
    body: Vec<u8>,
    disposition: Option<String>,
) -> RtResponse {
    let length = body.len();
    let mut response = RtResponse::new(media_status(status), SdkBody::from(body));
    // Inserted through the FALLIBLE `try_insert`, never the panicking `insert`: the media
    // type was read off caller-supplied bytes, and a value that is not valid HTTP must
    // leave this route serving the media under the generic "these are bytes" type rather
    // than panicking inside a request.
    if response
        .headers_mut()
        .try_insert("content-type", media_type.to_string())
        .is_err()
    {
        let _ = response.headers_mut().try_insert(
            "content-type",
            ::service_common::web_media_type::GENERIC_MEDIA_TYPE,
        );
    }
    if let Some(disposition) = disposition {
        // Fallible for the same reason: the attribute name this is built from reaches the
        // route off the request's own path, so a name that no HTTP header value can carry
        // leaves the media served unnamed rather than panicking inside a request.
        let _ = response
            .headers_mut()
            .try_insert("content-disposition", disposition);
    }
    let _ = response
        .headers_mut()
        .try_insert("content-length", length.to_string());
    response
}

/// How a download of one attribute's media is presented and named: served INLINE — this is
/// the artifact the attribute points at, which a caller renders — under the attribute's own
/// name and the file extension of the type its content announces
/// (`crate::state::attribute_media_file_name`).
///
/// Built for every download this route serves, whichever call last supplied the bytes: the
/// name follows from the attribute in the store and the type of the media it now holds, so a
/// replacement is named exactly as the media it replaced would have been.
fn content_disposition(attribute_name: &str, media_type: &str) -> String {
    format!(
        "inline; filename=\"{}\"",
        crate::state::attribute_media_file_name(attribute_name, media_type)
    )
}

/// A status this route answers with. Every call site passes a constant, so the conversion
/// cannot fail.
fn media_status(code: u16) -> StatusCode {
    StatusCode::try_from(code).expect("an attribute media download answers constant statuses")
}

/// Whether `uri` carries the SigV4 query authentication of a presigned request: the
/// credential scope and the signature [`smithy_server_runtime::UrlSigner`] puts on a URL it
/// signs.
///
/// Presence is all this reads, and all it has to: the framework's own request
/// authentication is what VERIFIES such a request before any route runs — the signature
/// over the method, the path and the query, and the `X-Amz-Expires` window the signer
/// wrote, a URL past which is refused. What this test adds is that nothing which is not a
/// presigned request is served from these paths at all, so an attribute's media has no
/// address but the URL this service signed for it.
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
