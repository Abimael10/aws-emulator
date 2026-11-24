// SPDX-License-Identifier: CC0-1.0

//! The URL an OIDC provider of this account is PUBLISHED under, as a read of the provider
//! reports it.
//!
//! A provider's URL is not only a value the account stored: it is an ADDRESS a relying party
//! FETCHES. "The URL ... should correspond to the iss claim in the provider's OpenID Connect
//! ID tokens" (`CreateOpenIDConnectProvider$Url`), and an OIDC client bootstraps a federation
//! by resolving that issuer and reading the two documents it publishes about itself
//! (`<issuer>/.well-known/openid-configuration` and the key set it names,
//! [`::service_common::openid_discovery`]). So the address a read of the provider publishes
//! has to lead back to this emulator, which is the rule every URL this emulator announces
//! follows — the account's own outbound web identity issuer is built the same way
//! ([`super::outbound_web_identity_federation`]).
//!
//! An issuer hosted by AWS ITSELF is exactly where the two part company: a user pool's issuer
//! is `cognito-idp.<region>.amazonaws.com/<pool-id>`, a host no client of this emulator
//! resolves and that this emulator does not answer on — while the SERVICE behind it is
//! emulated here, at this emulator's own endpoint, where that pool publishes its discovery
//! documents under the same path. Such a URL is therefore published under the address this
//! emulator is reached at, with the issuer's own path kept, so the fetch a client makes next
//! lands on the service that serves it. An issuer under any other host is an address outside
//! this emulator either way and is published exactly as it was registered.
//!
//! What the account STORES is untouched: the provider's URL is its identity — its ARN is
//! derived from it and "you cannot register the same provider multiple times in a single
//! Amazon Web Services account" (`$Url`) — so the registered text is what the store keys,
//! what a token's `iss` claim is compared against, and what a second create collides with.
//! Only the published REPORT is addressed at this emulator.

#[allow(unused_imports)]
use super::*;

/// The address this emulator is reached at, as a registered provider records it: the endpoint
/// the registering request itself reached when that endpoint is the emulator's own
/// ([`::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts`]), which is
/// a client that resolves nothing else, and otherwise the one address this process answers on
/// ([`::service_common::data_plane_addressing::served_endpoint_authority`]), so a provider
/// registered by an in-process call of another service publishes an address that service's
/// caller can reach too.
///
/// Both readings are the workspace's shared ones, so this service writes no second rule for
/// which host is the emulator's own. `None` only for an emulator served on no port at all,
/// where there is no address to publish.
#[must_use]
pub fn published_endpoint_authority(
    parts: &::http::request::Parts,
    served_base: &str,
) -> Option<String> {
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(parts)
        .or_else(|| ::service_common::data_plane_addressing::served_endpoint_authority(served_base))
}

/// The `Url` a read of an OIDC provider reports: the registered host-and-path for a provider
/// whose issuer is hosted outside this emulator, and the same path under `served_endpoint`
/// for one whose issuer names a host AWS itself publishes an endpoint at
/// ([`::service_common::data_plane_addressing::addresses_an_aws_endpoint`]) — the services
/// this emulator answers for.
///
/// `served_endpoint` is the `host[:port]` this emulator is reached at, recorded when the
/// provider was registered ([`OpenIdConnectProviderRecord::served_endpoint`]) so one provider
/// publishes ONE address however its later callers address this emulator. `None` is an
/// emulator with no address to publish at all, where the registered text is all there is to
/// report.
///
/// The URL carries no scheme in either form, because a provider is "stored and reported
/// WITHOUT the scheme" (`crate::ops::create_open_id_connect_provider`): the host-and-path
/// form the ARN is built from and a token's `iss` claim is compared against.
#[must_use]
pub fn published_open_id_connect_url(url: &str, served_endpoint: Option<&str>) -> String {
    let Some(endpoint) = served_endpoint else {
        return url.to_string();
    };
    let (host, path) = match url.split_once('/') {
        Some((host, path)) => (host, Some(path)),
        None => (url, None),
    };
    if !::service_common::data_plane_addressing::addresses_an_aws_endpoint(host) {
        return url.to_string();
    }
    match path {
        Some(path) => format!("{endpoint}/{path}"),
        None => endpoint.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a provider whose issuer names a host AWS publishes an endpoint at is published
    /// under the address this emulator is reached at, keeping the issuer's path; every other
    /// issuer is published as it was registered.
    #[test]
    fn an_aws_hosted_issuer_is_published_at_this_emulators_address() {
        let endpoint = "127.0.0.1:9297";
        // The rows differ along what the rule generalises over: whether the host is one AWS
        // publishes an endpoint at, which partition's DNS suffix it is under, how many path
        // segments the issuer carries, whether it carries a path at all, and the case the
        // host is written in.
        let rows: [(&str, &str, &str); 8] = [
            (
                "a user pool issuer, host and pool path",
                "cognito-idp.us-west-2.amazonaws.com/us-west-2_A1b2C3d4E",
                "127.0.0.1:9297/us-west-2_A1b2C3d4E",
            ),
            (
                "an issuer of another AWS service, with a longer path",
                "oidc.eks.eu-west-1.amazonaws.com/id/7A1B2C3D/extra",
                "127.0.0.1:9297/id/7A1B2C3D/extra",
            ),
            (
                "an AWS host under another partition's suffix",
                "cognito-idp.cn-north-1.amazonaws.com.cn/cn-north-1_pool",
                "127.0.0.1:9297/cn-north-1_pool",
            ),
            (
                "an AWS host written in another case",
                "COGNITO-IDP.US-WEST-2.AMAZONAWS.COM/us-west-2_pool",
                "127.0.0.1:9297/us-west-2_pool",
            ),
            (
                "an AWS host carrying no path at all",
                "oidc.eks.eu-west-1.amazonaws.com",
                "127.0.0.1:9297",
            ),
            (
                "a third-party issuer",
                "server.example.com/oidc",
                "server.example.com/oidc",
            ),
            (
                "a third-party issuer whose host merely contains the suffix",
                "amazonaws.com.example.net/oidc",
                "amazonaws.com.example.net/oidc",
            ),
            (
                "an issuer already addressed at this emulator",
                "127.0.0.1:9297/us-west-2_pool",
                "127.0.0.1:9297/us-west-2_pool",
            ),
        ];

        for (case, stored, expected) in rows {
            let published = published_open_id_connect_url(stored, Some(endpoint));
            assert_eq!(
                published, expected,
                "case {case:?}: expected {expected:?}, got {published:?}"
            );
            // An emulator with no address to publish reports the registered text itself.
            let unserved = published_open_id_connect_url(stored, None);
            assert_eq!(
                unserved, stored,
                "case {case:?}: expected the registered URL {stored:?} with no address to \
                 publish, got {unserved:?}"
            );
        }
    }
}
