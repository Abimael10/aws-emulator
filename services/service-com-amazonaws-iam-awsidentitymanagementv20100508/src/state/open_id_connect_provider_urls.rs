// SPDX-License-Identifier: CC0-1.0

//! The URL an OIDC provider of this account is REGISTERED and REPORTED under.
//!
//! A provider's URL is a value its registration CARRIED: "the URL of the identity provider.
//! The URL must begin with https:// and should correspond to the iss claim in the provider's
//! OpenID Connect ID tokens. Per the OIDC standard, path components are allowed but query
//! parameters are not" (`CreateOpenIDConnectProvider$Url`). IAM stores it with the scheme
//! removed — the host-and-path form the provider's ARN is built from
//! ([`super::open_id_connect_provider_arn`]) and the form a relying party compares a token's
//! `iss` claim against — and a read of the provider reports exactly that stored text
//! (`GetOpenIDConnectProvider$Url`).
//!
//! So nothing of this emulator's own addressing goes into it. The `Url` is CALLER input
//! echoed back, not an address this emulator mints for the caller to fetch: rewriting it to
//! the host this process answers on would report a provider the account never registered,
//! and a client matching the issuer of a token against the provider it registered would not
//! find it. The addresses this emulator does publish — the account's own outbound web
//! identity issuer among them ([`super::outbound_web_identity_federation`]) — are the ones
//! this emulator ISSUES, and they are built from its endpoint for that reason.
//!
//! Split out of [`super`] so the one reading of a registered URL lives beside the record it
//! is stored in.

#[allow(unused_imports)]
use super::*;

/// The host-and-path form of the registered URL `url` — what the account STORES for a
/// provider and what a read of it reports — or `None` for a value outside the published form
/// of `CreateOpenIDConnectProvider$Url`.
///
/// Three readings, each the doc sentence's own: the value has to begin with the scheme the
/// URL "must begin with" ([`OPEN_ID_CONNECT_URL_SCHEME`]), it may not carry the query the
/// standard does not allow, and it has to name a host, since a URL "typically consists of
/// only a hostname" and one whose first path segment is empty identifies no provider.
///
/// The ONE place a registered URL is read into its stored form, so the text the ARN is
/// derived from, the text a second create collides with and the text a read reports cannot
/// diverge.
#[must_use]
pub fn registered_open_id_connect_url(url: &str) -> Option<&str> {
    let host_and_path = url.strip_prefix(OPEN_ID_CONNECT_URL_SCHEME)?;
    if host_and_path.contains('?') {
        return None;
    }
    let host = host_and_path.split('/').next().unwrap_or_default();
    (!host.is_empty()).then_some(host_and_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a registered URL inside the published form is stored as its host-and-path, with
    /// the scheme removed and every other character kept; one outside that form is read as
    /// naming no provider.
    #[test]
    fn a_registered_url_is_stored_as_its_host_and_path() {
        // The rows differ along what the rule generalises over: whether the scheme is the
        // one the URL must begin with, how many path segments the issuer carries, whether it
        // carries a path at all, whether it names a host, and whether it carries a query.
        let rows: [(&str, &str, Option<&str>); 9] = [
            (
                "a host alone",
                "https://server.example.org",
                Some("server.example.org"),
            ),
            (
                "a host and one path segment",
                "https://server.example.org/oidc",
                Some("server.example.org/oidc"),
            ),
            (
                "a host and several path segments",
                "https://oidc.eks.eu-west-1.amazonaws.com/id/7A1B2C3D/extra",
                Some("oidc.eks.eu-west-1.amazonaws.com/id/7A1B2C3D/extra"),
            ),
            (
                "an issuer of a service AWS itself hosts, which is stored as registered",
                "https://cognito-idp.us-west-2.amazonaws.com/us-west-2_A1b2C3d4E",
                Some("cognito-idp.us-west-2.amazonaws.com/us-west-2_A1b2C3d4E"),
            ),
            (
                "a host with a port, kept as written",
                "https://server.example.org:8443/oidc",
                Some("server.example.org:8443/oidc"),
            ),
            ("the other scheme", "http://server.example.org", None),
            ("no scheme at all", "server.example.org", None),
            (
                "a query, which the standard does not allow",
                "https://server.example.org/oidc?audience=a",
                None,
            ),
            ("no host at all", "https:///oidc", None),
        ];

        for (case, registered, expected) in rows {
            let stored = registered_open_id_connect_url(registered);
            assert_eq!(
                stored, expected,
                "case {case:?}: expected {expected:?}, got {stored:?}"
            );
        }
    }
}
