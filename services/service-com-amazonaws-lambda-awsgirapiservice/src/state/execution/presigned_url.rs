// SPDX-License-Identifier: CC0-1.0

//! The URL an SDK inside a RUNNING handler presigns, signed so a later caller can fetch it.
//!
//! Presigning is not a call: it is a signature over a request that has not been made, which
//! the handler hands to somebody else to send. So what the handler is given has to be an
//! address that later request can REACH, and a signature the service that serves it accepts:
//!
//! * the ADDRESS is this emulator's own base URL ([`Signing::endpoint`]) with the resource's
//!   path under it, because this emulator is reached on its own endpoint alone and no
//!   production AWS hostname resolves to it — a URL naming one is a URL nothing can be
//!   fetched from;
//! * the SIGNATURE is made with the credential the invocation's own calls are made with (the
//!   session of the function's execution role, [`super::execution_role_session`]), through
//!   the framework's own query-presigner — the one the emulator's request verifier reads
//!   back. A presigned URL therefore speaks for the SAME identity a call the handler makes
//!   speaks for, which is what lets the service serving the later request authorize it
//!   against the policies in force on that role, exactly as AWS does. Where the invocation
//!   has no session of its own, the URL is signed with the credential the framework hands
//!   this emulator's handlers, which is what every other in-process call presents.
//!
//! That presigner is the one the framework's own URL signer signs an announced location with,
//! reached here directly for the one thing the signer does not take: WHICH identity signs. A
//! URL an operation of this service announces speaks for the emulator itself and goes through
//! the signer ([`crate::state::download_location`]); this one speaks for the function's role,
//! so the credential is passed in. Nothing here computes a signature of its own.
//!
//! The credential's SECRET never reaches the guest: what comes back is the URL alone — the key
//! id, the session token and a signature over the one request it names, which is what every
//! AWS SDK's presigned URL carries — good for that request and for as long as it says.

use ::std::time::{Duration, SystemTime};

use ::emulator_server::request_authentication::{presign_url, PresignTarget};
use ::smithy_server_runtime::{EmbeddedEmulator, EndpointUrl};

use super::aws_api::CallerIdentity;

/// The query field a query-signed request carries a TEMPORARY credential's session token in,
/// which the emulator's verifier reads back there for a presigned request, as every AWS SDK's
/// own presigner writes it.
const SESSION_TOKEN_FIELD: &str = "X-Amz-Security-Token";

/// The longest a presigned URL is good for: SigV4's own ceiling for a query-signed request is
/// seven days ("X-Amz-Expires … the maximum value is 604800"), so a guest asking for longer
/// is given a URL good for that rather than one that is refused.
pub const MAX_LIFETIME: Duration = Duration::from_secs(604_800);

/// The shortest one is good for, which is the smallest window the signer accepts.
const MIN_LIFETIME: Duration = Duration::from_secs(1);

/// WHO signs a URL and WHERE it leads: everything about the signature that is not part of the
/// request itself.
pub struct Signing<'a> {
    /// The base URL this emulator is reached on, which the signed URL addresses.
    pub endpoint: &'a EndpointUrl,
    /// The credential the URL is signed with: the session of the function's execution role,
    /// or `None` for an invocation that was given none — in which case it is signed with the
    /// credential the framework hands this emulator's handlers.
    pub identity: Option<&'a CallerIdentity>,
    /// The callback that credential is read off ([`EmbeddedEmulator::credentials`]).
    pub callback: &'a EmbeddedEmulator,
    /// The Region the signature's credential scope names, which is the Region the call the URL
    /// names would have been made in.
    pub region: &'a str,
}

/// The ONE request a presigned URL names, laid out the way the serving service binds it.
pub struct Request<'a> {
    /// The method that later request uses.
    pub method: ::http::Method,
    /// The signing name of the service that serves it, which is also what routes it here.
    pub signing_name: &'a str,
    /// Its path, DECODED, as the service's own model lays it out; the signer encodes it the
    /// way that service's verifier reads it back.
    pub path: &'a str,
    /// The query fields it carries, decoded, beside the signature's own.
    pub query: &'a [(&'a str, &'a str)],
    /// The headers the caller MUST send with it, bound into the signature.
    pub headers: &'a [(&'a str, &'a str)],
}

/// The signed URL for `request`, good for `expires_in` (clamped to the window a query
/// signature may carry), or `None` where the request cannot be signed at all.
#[must_use]
pub fn signed(
    signing: &Signing<'_>,
    request: &Request<'_>,
    expires_in: Duration,
) -> Option<String> {
    let vended = signing.callback.credentials();
    let (key_id, secret, session_token) = match signing.identity {
        Some(identity) => (
            identity.access_key_id.as_str(),
            identity.secret_access_key.as_str(),
            Some(identity.session_token.as_str()),
        ),
        None => {
            let (key_id, secret) = ::emulator_server::embedded::signing_key_pair(&vended);
            (key_id, secret, None)
        }
    };
    let mut query: Vec<(&str, &str)> = request.query.to_vec();
    if let Some(token) = session_token {
        query.push((SESSION_TOKEN_FIELD, token));
    }
    let target = PresignTarget {
        endpoint: signing.endpoint.as_str(),
        method: request.method.as_str(),
        signing_name: request.signing_name,
        region: signing.region,
        path: request.path,
        query: &query,
        headers: request.headers,
    };
    presign_url(
        &target,
        key_id,
        secret,
        SystemTime::now(),
        expires_in.clamp(MIN_LIFETIME, MAX_LIFETIME).as_secs(),
    )
}
