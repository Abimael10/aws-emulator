// SPDX-License-Identifier: CC0-1.0

//! The PATH a request of this endpoint addresses, read from the request as dispatch carries
//! it.
//!
//! The one reader the seams that answer this endpoint's non-modelled routes share
//! ([`super::cors_preflight`], [`super::oauth2`]), so the request a seam CLAIMS and the
//! request it ANSWERS cannot drift apart from each other: dispatch carries the URI as the
//! client sent it, which is a path for an HTTP/1 request and an absolute URI for the
//! in-process front door, and a query states nothing about which path was addressed.

/// The path `uri` addresses, in either form a request carries one in.
pub fn of(uri: &str) -> &str {
    let without_query = uri.split('?').next().unwrap_or(uri);
    match without_query.split_once("://") {
        // An absolute URI whose authority is followed by no path addresses the root.
        Some((_, authority_and_path)) => match authority_and_path.find('/') {
            Some(at) => authority_and_path.split_at(at).1,
            None => "/",
        },
        None => without_query,
    }
}

/// Whether `uri` addresses the ROOT, which is the path every `awsJson` operation of this
/// service is posted to.
pub fn addresses_root(uri: &str) -> bool {
    of(uri).trim_matches('/').is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row: the URI as dispatch carries it, the path it addresses, and whether that path
    /// is the root. The rows differ along the form of the URI (a path, an absolute URI, with
    /// and without a query) and the depth of the path.
    type PathRow = (&'static str, &'static str, &'static str, bool);

    #[test]
    fn a_uri_addresses_the_path_after_its_authority_and_before_its_query() {
        let rows: [PathRow; 8] = [
            ("the root", "/", "/", true),
            ("the root with a query", "/?x=1", "/", true),
            ("an empty path", "", "", true),
            (
                "an absolute URI of the root",
                "https://cognito-idp.us-east-1.amazonaws.com/",
                "/",
                true,
            ),
            (
                "an absolute URI with no path at all",
                "https://cognito-idp.us-east-1.amazonaws.com",
                "/",
                true,
            ),
            (
                "an absolute URI of a deeper path",
                "https://127.0.0.1:9297/auth-example/oauth2/token",
                "/auth-example/oauth2/token",
                false,
            ),
            (
                "a path with a query",
                "/auth-example/oauth2/token?x=1",
                "/auth-example/oauth2/token",
                false,
            ),
            (
                "a document path",
                "/us-east-1_abc123def/.well-known/jwks.json",
                "/us-east-1_abc123def/.well-known/jwks.json",
                false,
            ),
        ];
        for (case, uri, expected, is_root) in rows {
            let read = of(uri);
            assert_eq!(
                read, expected,
                "case {case:?}: expected the path {expected:?}, got {read:?}"
            );
            let root = addresses_root(uri);
            assert_eq!(
                root, is_root,
                "case {case:?}: expected addresses_root to be {is_root}, got {root}"
            );
        }
    }
}
