// SPDX-License-Identifier: CC0-1.0

//! How a request to the Amazon SimpleDB 2009-04-15 API is AUTHENTICATED on this endpoint.
//!
//! That API publishes Signature Version 2: a request carries its caller's credential
//! among its OWN parameters — `AWSAccessKeyId`, `SignatureVersion=2`, `SignatureMethod`,
//! `Timestamp` and `Signature` — and sends no `Authorization` header at all. A conforming
//! client (every AWS SDK that still speaks this API signs this way) therefore presents
//! nothing the router's SigV4 authenticator recognises, and a router with request
//! authentication on refuses such a request before any handler runs: it can find no
//! authenticator for the scheme the request presents.
//!
//! So this surface is the one that authenticates those requests, exactly as Amazon S3's
//! browser-form upload authenticates its own: the request is served under NO scheme the
//! router verifies ([`ErasedOperation::auth_schemes`] answers an empty list), and NOTHING
//! is waved through — [`refusal_for`] below verifies the signature the request carries
//! before the action is served, against the credentials this emulator accepts, and refuses
//! a request whose signature does not match.
//!
//! A request that presents SigV4 instead is NOT this surface's: it is served by
//! [`super::SimpleDbQueryApi`], whose schemes name SigV4, so the router verifies it the way
//! it verifies every modelled operation of this service
//! ([`crate::service::EmulatedService::override_operation`] decides between the two from
//! what the request presents).
//!
//! # The signature
//!
//! Signature Version 2 signs a canonical string built from the request alone: the HTTP
//! method, the host it addressed, the path, and every parameter it carries EXCEPT
//! `Signature`, sorted by parameter name and percent-encoded with only the unreserved
//! characters left as they are. The signature is that string's HMAC — under the secret of
//! the access key the request names, with the hash its `SignatureMethod` states —
//! base64-encoded.
//!
//! What is judged here is the CREDENTIAL: the version and hash the request states, the
//! access key it names, and the signature it presents. A request's FRESHNESS is not —
//! the `Timestamp` and `Expires` of this scheme are inside the string the signature
//! covers, so neither can be altered without invalidating it, and this emulator serves a
//! developer's own machine rather than a replay-exposed endpoint. Judging the window is
//! its own gap, to be implemented against the recorded behaviour of the codes AWS
//! publishes for it.

use std::collections::BTreeMap;
use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

use super::envelope::{self, QueryApiError};
use super::parameters::QueryParameters;
use super::SimpleDbQueryApi;

/// The parameter naming the access key the request is signed with.
const ACCESS_KEY_ID_PARAMETER: &str = "AWSAccessKeyId";

/// The parameter naming the signature scheme the request is signed under.
const SIGNATURE_VERSION_PARAMETER: &str = "SignatureVersion";

/// The parameter naming the hash the signature was computed with.
const SIGNATURE_METHOD_PARAMETER: &str = "SignatureMethod";

/// The parameter carrying the signature itself.
const SIGNATURE_PARAMETER: &str = "Signature";

/// The one signature version this API publishes.
const SIGNATURE_VERSION: &str = "2";

/// The hashes a `SignatureMethod` may name, as Amazon SimpleDB publishes them for that
/// parameter.
const HMAC_SHA1: &str = "HmacSHA1";
/// See [`HMAC_SHA1`]. It is the hash a request naming none is read as having asked for,
/// which is the one every current client signs with.
const HMAC_SHA256: &str = "HmacSHA256";
/// The two together, as the refusal for any other value states them.
const SIGNATURE_METHODS: &[&str] = &[HMAC_SHA1, HMAC_SHA256];

/// The characters a signed parameter keeps as they are: the unreserved set of RFC 3986
/// (`A-Z a-z 0-9 - . _ ~`), which is what Signature Version 2 leaves unescaped — every
/// other byte is percent-encoded, upper-case hex, in the canonical string.
const UNRESERVED: &::percent_encoding::AsciiSet = &::percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Whether `req` presents a credential of ITS OWN — among its parameters, where the
/// signature versions of this wire put one — rather than in an `Authorization` header:
/// it states a `SignatureVersion`, or it carries the access key and the signature such a
/// version travels as.
///
/// A version this API does not publish states one of these credentials too, so a request
/// stating it is this seam's to answer, with that parameter's own published refusal
/// ([`refusal_for`]) rather than the router's refusal of a scheme it holds no
/// authenticator for.
///
/// It is a question about what the request PRESENTS, never about what it is for: the
/// surface it addresses is decided by [`super::addresses_query_api`], and this only says
/// which of the two scheme seams serves it.
pub fn presents_its_own_credential(req: &RtRequest) -> bool {
    let parameters = QueryParameters::of(req);
    if named(&parameters, SIGNATURE_VERSION_PARAMETER).is_some() {
        return true;
    }
    named(&parameters, ACCESS_KEY_ID_PARAMETER).is_some()
        && named(&parameters, SIGNATURE_PARAMETER).is_some()
}

/// The Amazon SimpleDB 2009-04-15 API as served to a client signing it the way that API
/// publishes.
///
/// Holds a [`SimpleDbQueryApi`] of its own over the SAME shared engine, so a request
/// authenticated here is answered by the one surface every request of this API is answered
/// by, from the one store the export operations read.
pub struct SignatureVersion2Surface<Eng> {
    api: SimpleDbQueryApi<Eng>,
}

impl<Eng> SignatureVersion2Surface<Eng> {
    /// Build this surface from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            api: SimpleDbQueryApi::new(engine),
        }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for SignatureVersion2Surface<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            match refusal_for(req, ctx) {
                None => self.api.serve(req, ctx),
                Some(refusal) => {
                    let request_id = ::service_common::query_error_envelope::new_request_id();
                    super::audit(
                        ctx,
                        QueryParameters::of(req)
                            .value(super::ACTION_PARAMETER)
                            .unwrap_or_default()
                            .trim(),
                        refusal.code,
                    );
                    envelope::error_response(&refusal, &request_id)
                }
            }
        })
    }

    /// Served with NO scheme the router verifies, because a Signature Version 2 request
    /// carries no `Authorization` header for it to verify: its credential is the
    /// `AWSAccessKeyId` and `Signature` parameters. Nothing is waved through — the serving
    /// path above verifies that signature itself and refuses a request whose signature does
    /// not match the credentials this emulator accepts.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        Some(&[])
    }
}

/// Why this request may not be served, or `None` when its Signature Version 2 credential
/// is one this emulator accepts.
///
/// `None` ALSO when this emulator accepts no credential at all — a router with request
/// authentication off, which hands out none and verifies no request: there is nothing to
/// check a signature against, and refusing here would make this one surface demand a
/// credential the rest of the emulator does not.
pub fn refusal_for(req: &RtRequest, ctx: &OperationContext<'_>) -> Option<QueryApiError> {
    let accepted = ctx.callback().credentials();
    if accepted.is_empty() {
        ::tracing::debug!(
            "this emulator verifies no request, so the signature this SimpleDB request \
             carries is not checked here"
        );
        return None;
    }
    let parameters = QueryParameters::of(req);
    if let Some(version) = parameters.value(SIGNATURE_VERSION_PARAMETER).map(str::trim) {
        if version != SIGNATURE_VERSION {
            return Some(envelope::invalid_parameter_value(
                SIGNATURE_VERSION_PARAMETER,
                version,
                &format!("{SIGNATURE_VERSION_PARAMETER} must be {SIGNATURE_VERSION}."),
            ));
        }
    }
    let method = parameters
        .value(SIGNATURE_METHOD_PARAMETER)
        .map(str::trim)
        .unwrap_or(HMAC_SHA256);
    if !SIGNATURE_METHODS.contains(&method) {
        return Some(envelope::invalid_parameter_value(
            SIGNATURE_METHOD_PARAMETER,
            method,
            &format!(
                "{SIGNATURE_METHOD_PARAMETER} must be one of [{}].",
                SIGNATURE_METHODS.join(", ")
            ),
        ));
    }
    let Some(key_id) = named(&parameters, ACCESS_KEY_ID_PARAMETER) else {
        return Some(envelope::missing_authentication_token());
    };
    let Some(presented) = named(&parameters, SIGNATURE_PARAMETER) else {
        return Some(envelope::incomplete_signature());
    };
    let Some(secret) = secret_of(&accepted, key_id) else {
        return Some(envelope::invalid_client_token_id());
    };
    let signed = string_to_sign(req, &parameters);
    match signature_matches(method, &secret, &signed, presented) {
        true => None,
        false => Some(envelope::signature_does_not_match()),
    }
}

/// The value of `name`, trimmed, or `None` when the request carries no such parameter or
/// carries it empty — an empty credential field presents nothing.
fn named<'p>(parameters: &'p QueryParameters, name: &str) -> Option<&'p str> {
    parameters
        .value(name)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// The secret of the access key `key_id`, among the credentials this emulator accepts.
fn secret_of(
    accepted: &[::emulator_server::request_authentication::VendedCredentials],
    key_id: &str,
) -> Option<String> {
    use ::emulator_server::request_authentication::VendedCredentials;

    accepted.iter().find_map(|credential| match credential {
        VendedCredentials::AwsAccessKey {
            access_key_id,
            secret_access_key,
        } if access_key_id == key_id => Some(secret_access_key.clone()),
        _ => None,
    })
}

/// The canonical string a Signature Version 2 request's signature is of: the HTTP method,
/// the host the request addressed, its path, and its own parameters, each on its own line.
///
/// The path is `"/"` when the request names none, which is where this API is served. The
/// parameters are every one the request carries EXCEPT the signature itself, sorted by
/// parameter name, each name and value percent-encoded down to the [`UNRESERVED`] set and
/// joined as a query string — so the string signed is a function of the request alone.
fn string_to_sign(req: &RtRequest, parameters: &QueryParameters) -> String {
    let host = ::service_common::data_plane_addressing::addressed_host(
        req.uri(),
        req.headers().get("host"),
    )
    .unwrap_or_default();
    let path = match ::service_common::uri_path(req.uri()) {
        "" => "/",
        path => path,
    };
    format!(
        "{method}\n{host}\n{path}\n{query}",
        method = req.method(),
        query = canonical_parameters(parameters),
    )
}

/// The parameters of the canonical string: every field but the signature, sorted by name,
/// percent-encoded.
///
/// A name the request carries twice is ONE parameter whose value is the first it sent,
/// which is how every other reader of this API reads a repeated parameter
/// ([`QueryParameters::value`]).
///
/// The fields are collected into an ordered map, which is what bounds this: a request
/// chooses how many parameters it sends, so both the de-duplication and the ordering have
/// to cost no more than one insertion per field — a scan of what has been collected so far,
/// per field, would make the work the PRODUCT of two values the caller picks.
fn canonical_parameters(parameters: &QueryParameters) -> String {
    let mut signed: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, value) in parameters.fields() {
        if name == SIGNATURE_PARAMETER {
            continue;
        }
        signed.entry(name.as_str()).or_insert(value.as_str());
    }
    signed
        .iter()
        .map(|(name, value)| format!("{}={}", encoded(name), encoded(value)))
        .collect::<Vec<String>>()
        .join("&")
}

/// `text` with every byte outside the [`UNRESERVED`] set percent-encoded.
fn encoded(text: &str) -> String {
    ::percent_encoding::utf8_percent_encode(text, UNRESERVED).to_string()
}

/// Whether `presented` is the base64 HMAC of `signed` under `secret`, hashed the way
/// `method` names.
///
/// The comparison is the MAC's own verification rather than a string compare, so it takes
/// the same time whatever the presented bytes are; a `presented` that is not base64 at all
/// matches nothing, and so does a `method` this API does not publish — there is no hash to
/// compute the signature with.
fn signature_matches(method: &str, secret: &str, signed: &str, presented: &str) -> bool {
    use ::base64::Engine as _;
    use ::hmac::Mac as _;

    let Ok(presented) = ::base64::engine::general_purpose::STANDARD.decode(presented) else {
        return false;
    };
    let key = secret.as_bytes();
    let message = signed.as_bytes();
    match method {
        HMAC_SHA1 => <::hmac::Hmac<::sha1::Sha1> as ::hmac::Mac>::new_from_slice(key)
            .is_ok_and(|mut mac| {
                mac.update(message);
                mac.verify_slice(&presented).is_ok()
            }),
        HMAC_SHA256 => <::hmac::Hmac<::sha2::Sha256> as ::hmac::Mac>::new_from_slice(key)
            .is_ok_and(|mut mac| {
                mac.update(message);
                mac.verify_slice(&presented).is_ok()
            }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_types::body::SdkBody;

    /// One posted form, as a client of this API sends it: `POST "/"` to `host` with the
    /// fields in the body.
    fn posted(host: &str, body: &str) -> RtRequest {
        let request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", host)
            .body(SdkBody::from(body.to_string()))
            .expect("a well-formed request");
        RtRequest::try_from(request).expect("a request the runtime API accepts")
    }

    #[test]
    fn a_request_presents_its_own_credential_when_it_states_a_version_or_carries_one() {
        let rows: &[(&str, &str, bool)] = &[
            (
                "a conforming client, which states the version",
                "Action=CreateDomain&SignatureVersion=2&AWSAccessKeyId=AKID&Signature=abc",
                true,
            ),
            (
                "the credential among the parameters, with no version stated",
                "Action=CreateDomain&AWSAccessKeyId=AKID&Signature=abc",
                true,
            ),
            (
                "a request stating a signature version this API does not publish",
                "Action=CreateDomain&SignatureVersion=1&AWSAccessKeyId=AKID&Signature=abc",
                true,
            ),
            (
                "a request presenting no credential of its own",
                "Action=CreateDomain&Version=2009-04-15",
                false,
            ),
        ];
        for (case, body, expected) in rows {
            let actual = presents_its_own_credential(&posted("sdb.amazonaws.com", body));
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn the_canonical_string_is_the_request_sorted_and_signature_free() {
        let request = posted(
            "sdb.us-west-2.amazonaws.com",
            "Action=CreateDomain&DomainName=my+domain&Signature=abc&AWSAccessKeyId=AKID",
        );
        let signed = string_to_sign(&request, &QueryParameters::of(&request));
        assert_eq!(
            signed,
            "POST\nsdb.us-west-2.amazonaws.com\n/\n\
             AWSAccessKeyId=AKID&Action=CreateDomain&DomainName=my%20domain",
            "case a posted form: the canonical string carries the method, host and path, \
             then the parameters sorted by name with the signature left out, got {signed:?}"
        );
    }

    #[test]
    fn a_signature_matches_only_the_string_it_is_of() {
        use ::base64::Engine as _;
        use ::hmac::Mac as _;

        let secret = "a-secret";
        let signed = "POST\nsdb.amazonaws.com\n/\nAction=CreateDomain";
        let mut mac = <::hmac::Hmac<::sha2::Sha256> as ::hmac::Mac>::new_from_slice(
            secret.as_bytes(),
        )
        .expect("HMAC takes a key of any length");
        mac.update(signed.as_bytes());
        let correct = ::base64::engine::general_purpose::STANDARD
            .encode(mac.finalize().into_bytes());
        let rows: &[(&str, &str, &str, &str, bool)] = &[
            (
                "the signature of this very string",
                "HmacSHA256",
                secret,
                correct.as_str(),
                true,
            ),
            (
                "the signature of another string",
                "HmacSHA256",
                secret,
                "cnLctX6qcM0Y1FDEDeH1aTaZkLnUoRkcmbVQ9CnGvVo=",
                false,
            ),
            (
                "the right signature under another secret",
                "HmacSHA256",
                "another-secret",
                correct.as_str(),
                false,
            ),
            (
                "the right signature read as another hash",
                "HmacSHA1",
                secret,
                correct.as_str(),
                false,
            ),
            (
                "a signature that is not base64 at all",
                "HmacSHA256",
                secret,
                "not base64",
                false,
            ),
        ];
        for (case, method, secret, presented, expected) in rows {
            let actual = signature_matches(method, secret, signed, presented);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }
}
