// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of WHICH SERVICE the credential a presigned URL carries in its own QUERY
//! names.
//!
//! A presigned URL is a request whose authorization travels in the query string rather than
//! in an `Authorization` header — "you can use query parameters to … authenticate the
//! request" (AWS General Reference, *Authenticating requests: using query parameters*) — so
//! such a URL STATES the service it was signed for, whatever its host and path addressing:
//! nothing distinguishes the virtual-hosted form (`<bucket>.<prefix>.<region>.amazonaws.com`)
//! from the path-style one (the endpoint's own host with the bucket as the first path
//! segment), and a URL minted against an endpoint of this emulator carries no service in its
//! host at all. Two forms carry that credential:
//!
//! * the SigV4 query form, whose `X-Amz-Credential` scope names the service — read by the
//!   front door's own reader of it (`emulator_server::service_from_sigv4_query`), the same
//!   reading that routes such a request when the router is handed one, so the scope grammar
//!   stays in the one place that owns it; and
//! * the LEGACY query form, whose parameters are [`LEGACY_CREDENTIAL_PARAMETERS`]. Its
//!   credential names no service, and it is Amazon S3's own form, so the FORM names the
//!   service ([`LEGACY_CREDENTIAL_SERVICE`]).
//!
//! Read by whatever has to ATTRIBUTE such a URL to a service: Amazon S3 claims the legacy
//! form in its own `can_handle`, and the seam that carries a running function's own outbound
//! HTTP into this emulator attributes a presigned URL on the emulator's own endpoint to the
//! service it was signed for.

/// The query parameters a LEGACY presigned URL carries its credential in: the access key id,
/// the signature and the expiry, all three required — "`AWSAccessKeyId`, `Signature` and
/// `Expires`" (AWS General Reference, *Authenticating requests: using query parameters*).
pub const LEGACY_CREDENTIAL_PARAMETERS: [&str; 3] = ["AWSAccessKeyId", "Signature", "Expires"];

/// The service whose own form the legacy query credential is, as the router keys a signing
/// name: Amazon S3 serves it and no other service does, so the form itself names the service
/// its scope does not.
pub const LEGACY_CREDENTIAL_SERVICE: &str = "s3";

/// The signing name of the service the presigned credential in `uri`'s query names, or `None`
/// for a URI carrying no presigned credential at all — which is a URL that states no service
/// of its own.
///
/// The SigV4 form is read first: it names its service outright, so a URL carrying both forms'
/// parameters is attributed to the one it STATES rather than to the one its other form
/// implies.
#[must_use]
pub fn signed_for(uri: &::http::Uri) -> Option<String> {
    if let Some(signing_name) = ::emulator_server::service_from_sigv4_query(uri) {
        return Some(signing_name);
    }
    carries_a_legacy_credential(uri.query()).then(|| LEGACY_CREDENTIAL_SERVICE.to_string())
}

/// Whether `query` carries all of [`LEGACY_CREDENTIAL_PARAMETERS`] — the shape that makes a
/// request a legacy presigned URL rather than any other request naming no service.
///
/// `None` is a request with no query string, which carries no credential. Each parameter is
/// read through this crate's one reader of a raw query string ([`crate::query_parameter`]),
/// which compares a SigV4 parameter name as written, the way every reader of one here does.
#[must_use]
pub fn carries_a_legacy_credential(query: Option<&str>) -> bool {
    let query = query.unwrap_or_default();
    LEGACY_CREDENTIAL_PARAMETERS
        .iter()
        .all(|parameter| crate::query_parameter(query, parameter).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row: the query a URL carries, and the service it says the URL was signed for.
    struct Row {
        case: &'static str,
        /// The query string, or `None` for a URL carrying none at all.
        query: Option<&'static str>,
        /// The signing name the query names, or `None` for one naming no service.
        expected: Option<&'static str>,
    }

    /// The URI a row's query is read off: the request target such a URL addresses.
    fn addressed(query: Option<&str>) -> ::http::Uri {
        match query {
            Some(query) => format!("/bucket/key?{query}"),
            None => "/bucket/key".to_string(),
        }
        .parse()
        .expect("the row states a request target")
    }

    /// RULE: a presigned URL's own query names the service it was signed for — the SigV4
    /// form in its credential scope, the legacy form by being that form — and a query
    /// carrying no presigned credential names none.
    #[test]
    fn a_presigned_query_names_the_service_it_was_signed_for() {
        // The rows span what the rule generalises over: which form carries the credential,
        // which service the scope names, how the scope is encoded, where the parameter sits
        // among the others, and the queries that carry no credential at all.
        let rows: &[Row] = &[
            Row {
                case: "the legacy form, as a client orders its parameters",
                query: Some("AWSAccessKeyId=AKIDEXAMPLE&Signature=abc%2Bdef&Expires=1762000000"),
                expected: Some("s3"),
            },
            Row {
                case: "the legacy form with a response override beside it",
                query: Some(
                    "response-content-type=text%2Fplain&AWSAccessKeyId=AKIDEXAMPLE&\
                     Expires=1762000000&Signature=abc",
                ),
                expected: Some("s3"),
            },
            Row {
                case: "the legacy form missing its signature",
                query: Some("AWSAccessKeyId=AKIDEXAMPLE&Expires=1762000000"),
                expected: None,
            },
            Row {
                case: "the SigV4 form, with its scope separators encoded",
                query: Some(
                    "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLE%2F20240101\
                     %2Fus-west-2%2Fs3%2Faws4_request&X-Amz-Date=20240101T000000Z&\
                     X-Amz-Expires=600&X-Amz-SignedHeaders=host&X-Amz-Signature=deadbeef",
                ),
                expected: Some("s3"),
            },
            Row {
                case: "the SigV4 form of another service",
                query: Some(
                    "X-Amz-Credential=AKIDEXAMPLE%2F20240101%2Feu-central-1%2Fexecute-api\
                     %2Faws4_request&X-Amz-Signature=deadbeef",
                ),
                expected: Some("execute-api"),
            },
            Row {
                case: "a SigV4 scope naming the service in mixed case",
                query: Some("X-Amz-Credential=AKID%2F20240101%2Fus-east-1%2FS3%2Faws4_request"),
                expected: Some("s3"),
            },
            Row {
                case: "a credential that is not a scope at all",
                query: Some("X-Amz-Credential=AKIDEXAMPLE"),
                expected: None,
            },
            Row {
                case: "a scope whose last field is not the terminator",
                query: Some(
                    "X-Amz-Credential=AKID%2F20240101%2Fus-east-1%2Fs3%2Fnot_the_terminator",
                ),
                expected: None,
            },
            Row {
                case: "a scope naming no service",
                query: Some("X-Amz-Credential=AKID%2F20240101%2Fus-east-1%2F%2Faws4_request"),
                expected: None,
            },
            Row {
                case: "both forms at once, where the scope states the service",
                query: Some(
                    "AWSAccessKeyId=AKID&Signature=abc&Expires=1&X-Amz-Credential=AKID%2F\
                     20240101%2Fus-east-1%2Fs3express%2Faws4_request",
                ),
                expected: Some("s3express"),
            },
            Row {
                case: "a query carrying no credential",
                query: Some("versionId=3&partNumber=1"),
                expected: None,
            },
            Row {
                case: "no query at all",
                query: None,
                expected: None,
            },
            Row {
                case: "an empty query",
                query: Some(""),
                expected: None,
            },
        ];
        for row in rows {
            let case = row.case;
            let signed = signed_for(&addressed(row.query));
            let actual = signed.as_deref();
            assert_eq!(
                actual, row.expected,
                "case {case:?}: expected {:?}, got {actual:?}",
                row.expected
            );
        }
    }
}
