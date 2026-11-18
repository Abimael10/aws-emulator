// SPDX-License-Identifier: CC0-1.0

//! Serves the NOTEBOOK at the signed address `CreatePresignedNotebookUrl` hands out for a
//! session.
//!
//! `CreatePresignedNotebookUrlResponse$NotebookUrl` is an absolute URL a client opens next —
//! it "points directly to the opened notebook" — and the notebook it names is a file this
//! emulator owns: so the address that operation signs
//! (`crate::state::notebook_url`) names this emulator's own base URL, and this route answers
//! a `GET` on it with the ipynb content of the notebook the session was started in
//! (`crate::state::notebook_at_address`). Nothing a client is handed leaves the endpoint it
//! is already talking to.
//!
//! The model binds no operation to those paths — a notebook file is the artifact an API
//! member points at rather than an API call — so the route is claimed through
//! [`smithy_server_runtime::Service::override_operation`] rather than through the generated
//! routing table. It is claimed only for a PRE-SIGNED request ([`is_presigned`]), whose
//! signature and expiry window the framework verifies before the route runs; WHICH notebook
//! an address may reach is then decided by the store that minted the address, against the
//! authentication token the address carries. So a notebook has exactly one address per
//! session: the one a grant handed out.

use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};

use crate::state::{
    notebook_at_address, session_in_notebook_path, AUTH_TOKEN_QUERY_FIELD, NOTEBOOK_MEDIA_TYPE,
};

/// The method a notebook is opened on, which is the method its address is signed for.
const FETCH_METHOD: &str = "GET";

/// The media type the one answer of this route that is not a notebook is served as.
const PLAIN_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// What this route answers an address that leads to no notebook of this emulator's: the
/// address is opened by a plain HTTP client rather than by an AWS SDK, so the answer is the
/// status itself with a one-line reason, and no API error envelope.
const NO_NOTEBOOK: &str = "no notebook at this location\n";

/// Serves the notebook one session's address names.
///
/// Owns the shared engine, as this service's operations do, because the notebook it serves is
/// the one that store holds: nothing is copied aside, so a read of the notebook and an open
/// of the address a grant handed out cannot disagree about its content.
pub struct NotebookEditorRoute<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> NotebookEditorRoute<Eng> {
    /// Build the route from the shared engine, as the service's operations are built.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// The session whose notebook `req` opens, the file name its address carries and the
    /// authentication token it presents, or `None` when the request is not an open this route
    /// serves.
    ///
    /// Two things make it one: the path, read the way the signer wrote it — its segments
    /// percent-decoded — and the SigV4 query authentication of a PRE-SIGNED request
    /// ([`is_presigned`]). Whether the token is one this emulator MINTED is decided where the
    /// store is: it travels with the session for the store to check, so a request carrying
    /// none is answered exactly as one naming a session that does not exist.
    pub fn claims(&self, req: &RtRequest) -> Option<(String, Option<String>, String)> {
        if req.method() != FETCH_METHOD || !is_presigned(req.uri()) {
            return None;
        }
        let (session_id, notebook_name) = session_in_notebook_path(&decoded_segments(req.uri()))?;
        let token = query_field(req.uri(), AUTH_TOKEN_QUERY_FIELD).unwrap_or_default();
        Some((session_id, notebook_name, token))
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for NotebookEditorRoute<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        _ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        // The context is not consulted, because a PRE-SIGNED open carries no caller identity
        // for it to hold: what the framework verified before this route ran is a signature
        // over this method, this path and this query, not who sent it. WHICH notebook an
        // address may reach is therefore decided by the store that minted the address,
        // against the authentication token the address carries.
        let answer = match self.claims(req) {
            Some((session_id, notebook_name, token)) => {
                let opened = notebook_at_address(
                    &*self.engine,
                    &session_id,
                    notebook_name.as_deref(),
                    &token,
                    ::std::time::SystemTime::now(),
                );
                match opened {
                    Some(content) => {
                        notebook_response(200, NOTEBOOK_MEDIA_TYPE, content.into_bytes())
                    }
                    // The address leads to no notebook of this emulator's: its token is not
                    // one this store minted, or was minted for another session, or the
                    // session is gone or no longer running, or it serves no notebook of that
                    // name. Each is answered the same way: there is nothing at this location.
                    None => {
                        notebook_response(404, PLAIN_MEDIA_TYPE, NO_NOTEBOOK.as_bytes().to_vec())
                    }
                }
            }
            // Unreachable through an `override_operation` that hands this route only the
            // requests it claims; answered as the framework answers a request that named no
            // operation rather than guessed at.
            None => svc.definition().routing_error_response(req),
        };
        Box::pin(async move { answer })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` keeps the framework's own decision for this service: the address this route
        // serves is SigV4 query-signed, and nothing here may weaken that.
        None
    }
}

/// One answer of this route: the bytes, their media type and their length.
fn notebook_response(status: u16, media_type: &str, body: Vec<u8>) -> RtResponse {
    let length = body.len();
    let mut response = RtResponse::new(notebook_status(status), SdkBody::from(body));
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
fn notebook_status(code: u16) -> StatusCode {
    StatusCode::try_from(code).expect("a notebook open answers constant statuses")
}

/// Whether `uri` carries the SigV4 query authentication of a pre-signed request: the
/// credential scope and the signature [`smithy_server_runtime::UrlSigner`] puts on a URL it
/// signs.
///
/// Presence is all this reads, and all it has to: the framework's own request authentication
/// is what VERIFIES such a request before any route runs — the signature over the method, the
/// path and the query, and the `X-Amz-Expires` window the signer wrote, a URL past which is
/// refused. What this test adds is that nothing which is not a pre-signed request is served
/// from these paths at all.
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

/// The path segments of `uri`, percent-decoded — the same view of a path the signer built the
/// URL from.
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

/// The value of one query field of `uri`, percent-decoded, or `None` when the request carries
/// no such field.
fn query_field(uri: &str, field: &str) -> Option<String> {
    let query = uri.split_once('?').map(|(_, query)| query)?;
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == field).then(|| smithy_server_runtime::percent_decode(value))
    })
}
