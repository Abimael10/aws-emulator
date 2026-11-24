// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a read of one of a trust store's own Amazon S3 objects answers with.
//!
//! Two operations of this API report WHERE an object of a trust store is fetched from, and
//! each documents the same thing about the address it hands back: "This action returns a
//! pre-signed S3 URI which is active for ten minutes"
//! (`GetTrustStoreCaCertificatesBundle` and `GetTrustStoreRevocationContent`, through
//! `smithy_query(query="operation",operation="GetTrustStoreCaCertificatesBundle,\
//! GetTrustStoreRevocationContent")`). The object is the one the call that stored it read in
//! this emulator's own Amazon S3 ([`super::ca_certificates_bundle`] for the CA certificates
//! bundle, [`super::trust_store_revocations`] for a revocation file), so a client that
//! follows a reported address is routed back into this emulator and served the very bytes
//! the caller staged.
//!
//! Both reads sign that address HERE, so the two cannot disagree on how long it is valid
//! for, which store it names, or what a versioned object's address carries.

use ::std::time::Duration;

/// How long a pre-signed address this API reports stays valid: "This action returns a
/// pre-signed S3 URI which is active for ten minutes", documented by both operations that
/// report one.
pub const TRUST_STORE_OBJECT_URI_VALIDITY: Duration = Duration::from_secs(10 * 60);

/// The signing name of the store a trust store's objects live in, which is the store a
/// fetch of a reported address has to reach: the object is the one a caller staged in this
/// emulator's own Amazon S3 and this service read over the in-process callback, so the
/// signed address names that very service.
const OBJECT_STORE_SIGNING_NAME: &str = "s3";

/// The address the object `bucket`/`key` is fetched from: a `GET` of that object, pre-signed
/// for [`TRUST_STORE_OBJECT_URI_VALIDITY`] against the endpoint this emulator is reached on.
///
/// A versioned object keeps its version in the address, so the object reported is the one
/// the stored address was pointed at rather than whatever is current.
pub fn trust_store_object_uri(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    bucket: &str,
    key: &str,
    version: Option<&str>,
) -> String {
    let path = format!("/{bucket}/{key}");
    let mut request = ::smithy_server_runtime::SignedUrlRequest::new(
        ::smithy_server_runtime::http::Method::GET,
        OBJECT_STORE_SIGNING_NAME,
        &path,
    );
    if let Some(version) = version
        .map(str::trim)
        .filter(|version| !version.is_empty())
    {
        request = request.query("versionId", version);
    }
    match ctx
        .url_signer()
        .sign(request, TRUST_STORE_OBJECT_URI_VALIDITY)
    {
        Ok(signed) => signed.as_str().to_string(),
        // Nothing about a stored bucket name, a stored key or this emulator's own endpoint
        // makes a `GET` unsignable, so this is unreachable in practice. The object IS at that
        // address either way, so the address is what is reported rather than no location at
        // all. The endpoint is `scheme://host[:port]` with no path of its own, and a trailing
        // separator is dropped so the object's path is the only one the address carries.
        Err(error) => {
            ::tracing::warn!(
                %error,
                path = %path,
                "a trust store object's address could not be signed"
            );
            format!("{}{path}", ctx.endpoint().as_str().trim_end_matches('/'))
        }
    }
}
