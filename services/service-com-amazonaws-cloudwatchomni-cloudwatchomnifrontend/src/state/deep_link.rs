// SPDX-License-Identifier: CC0-1.0

//! The one-time deep-link codes a domain hands a browser, and the rules a request for one
//! is admitted under.
//!
//! `CreateOneTimeDeepLinkCode`'s model doc reads "Generates a one-time code for deep-link
//! authentication. Direct the user's browser to the returned deepLinkUrl before it expires.
//! The code is exchanged for an authenticated, domain-scoped session and can be used only
//! once", and its `redirectUrl` member reads "The URL to redirect to after the deep-link
//! code is used. Must be an HTTPS URL in the domain with a path of /auth/callback, and
//! cannot include a query string or fragment. If omitted, no redirect is applied."
//!
//! Three things follow, and all three are facts about the DOMAIN rather than about one
//! request, which is why they live beside the store:
//!
//! * a code belongs to a domain, so the URL a browser is directed to is a RELATIVE
//!   reference ([`deep_link_url`]) carrying the code and the domain: it resolves against
//!   whichever endpoint of that domain the browser is already on, rather than pinning it to
//!   one of the endpoints the domain record reports;
//! * "in the domain" is a question about the domain's endpoints, so an admissible redirect
//!   is one of them followed by the callback path ([`redirect_url_rejection`]);
//! * the code is single-use and short-lived, so it is RECORDED against the domain with the
//!   instant it falls due ([`StoredDeepLinkCode`]) rather than merely returned.
//!
//! What is recorded is the code's DIGEST ([`deep_link_code_digest`]), never the code: the
//! code is redeemable for a session, so the store keeps only enough to recognise one that
//! is presented again.
//!
//! BOUNDED by the lifetime of what it holds: every mint drops the records whose expiry has
//! passed ([`ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::record_deep_link_code`]),
//! and the longest lifetime a request can ask for is the 600 seconds `ttlSeconds`'
//! `@range(min: 60, max: 600)` admits, so the map holds only the codes minted within one
//! such span.
//!
//! [`ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::record_deep_link_code`]: super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::record_deep_link_code

use std::collections::HashMap;

use crate::ops::types::Domain;

/// How long a code remains valid when the request names no `ttlSeconds`.
///
/// The member's own model doc: "How long the code remains valid, in seconds. Defaults to
/// 300."
pub const DEFAULT_DEEP_LINK_TTL_SECONDS: i32 = 300;

/// The only scheme a deep-link redirect may use, per `redirectUrl`'s "Must be an HTTPS
/// URL".
pub const HTTPS_SCHEME: &str = "https";

/// The one path a deep-link redirect may address, per `redirectUrl`'s "with a path of
/// /auth/callback".
pub const DEEP_LINK_REDIRECT_PATH: &str = "/auth/callback";

/// The path a one-time code is redeemed at, on the domain's own endpoint — the path
/// [`deep_link_url`] directs the browser to.
pub const DEEP_LINK_PATH: &str = "/auth/code";

/// The query parameter [`deep_link_url`] carries the one-time code in.
pub const DEEP_LINK_CODE_PARAMETER: &str = "code";

/// The query parameter [`deep_link_url`] carries the domain the code belongs to in.
pub const DEEP_LINK_DOMAIN_PARAMETER: &str = "domainId";

/// The query parameter [`deep_link_url`] carries the Region the domain lives in in.
pub const DEEP_LINK_REGION_PARAMETER: &str = "region";

/// The query parameter [`deep_link_url`] carries the redirect to apply after the code is
/// used in, when the request asked for one.
pub const DEEP_LINK_REDIRECT_PARAMETER: &str = "redirect";

/// How many characters of the minted secret a deep-link code keeps.
///
/// `DeepLinkCode` constrains neither the length nor the characters, so the code is as long
/// as an opaque single-use code AWS hands out in a URL: enough random characters that it
/// cannot be guessed, and short enough to sit in one.
const DEEP_LINK_CODE_CHARS: usize = 32;

/// What every refusal of a `redirectUrl` begins with, whichever condition the URL failed.
const INVALID_REDIRECT_URI: &str = "Invalid redirect URI";

/// The reason a redirect whose origin is not one of the domain's own earns.
///
/// Recorded behavior, and the only one of the reasons below a recording of this service
/// fixes: a redirect outside the domain is refused `Invalid redirect URI: unrecognized
/// domain`.
pub const UNRECOGNIZED_DOMAIN: &str = "unrecognized domain";

/// The reason text that does not parse as a URL at all earns.
pub const MALFORMED_REDIRECT_URI: &str = "malformed URL";

/// The reason a redirect that is not an HTTPS URL earns.
pub const REDIRECT_MUST_BE_HTTPS: &str = "scheme must be https";

/// The reason a redirect addressing any other path earns, naming the one path the member's
/// doc fixes.
pub const REDIRECT_PATH_REQUIRED: &str = "path must be /auth/callback";

/// The reason a redirect carrying a query string or a fragment earns, the third condition
/// the member's doc states.
pub const REDIRECT_CARRIES_QUERY_OR_FRAGMENT: &str = "query string and fragment are not allowed";

/// The sentence a request no code could be minted for is refused with — a failure on this
/// service's own side, decided by nothing in the request.
pub const DEEP_LINK_CODE_UNAVAILABLE: &str = "No deep-link code could be minted for this request.";

/// A one-time deep-link code as persisted.
///
/// Holds no code: [`Self::code_digest`] is what a code presented later is recognised by, so
/// the record cannot give the code back.
#[derive(Debug, Clone)]
pub struct StoredDeepLinkCode {
    /// The id of the domain the code was minted for, and the only domain it is a session
    /// of: the model calls the session it is exchanged for "domain-scoped".
    pub domain_id: String,
    /// [`deep_link_code_digest`] of the code handed to the caller — the key this record is
    /// held under as well, so a presented code resolves to its record without the store
    /// ever holding one.
    pub code_digest: String,
    /// The redirect the request asked for, applied when the code is used; `None` when it
    /// named none, which the member's doc states as "no redirect is applied".
    pub redirect_url: Option<String>,
    /// When the code was minted.
    pub created_at: ::aws_smithy_types::DateTime,
    /// When the code stops being usable — the `expiresAt` the response reports.
    pub expires_at: ::aws_smithy_types::DateTime,
}

/// The digest a deep-link code is recognised by: the SHA-256 of the code, in lowercase hex.
///
/// The code is a secret handed to its caller once and redeemable for a session, so the store
/// keeps only this one-way digest of it — enough to tell whether a code presented later is
/// one a live record was minted with, and not enough to reproduce it from the record.
pub fn deep_link_code_digest(code: &str) -> String {
    use ::sha2::Digest as _;
    let mut hasher = ::sha2::Sha256::new();
    hasher.update(code.as_bytes());
    ::hex::encode(hasher.finalize())
}

/// A fresh one-time deep-link code, or `None` when the credential store minted no secret to
/// build one from — which leaves the operation to refuse rather than report a code it made
/// up.
///
/// The code authenticates a browser to the DOMAIN's endpoint rather than to this emulator,
/// so it must never be accepted as a SigV4 credential — but it is still a secret handed to a
/// caller, and nothing in a service may construct a secret itself. It is therefore minted by
/// the one component that generates secrets, the emulator's credential store, and then
/// removed from that store again: the caller gets the secret, the emulator keeps no
/// credential it would accept, and nothing accumulates per call.
///
/// The code travels inside a URL ([`deep_link_url`]), so it keeps only the alphanumeric
/// characters of the minted secret: every one of them stands for itself in a URL, so the
/// code a browser sends back is the code that was minted rather than a percent-encoding of
/// it.
pub fn mint_deep_link_code(store: &::emulator_server::CredentialStore) -> Option<String> {
    let minted = store.create(&::emulator_server::aws_credentials::AWS_SESSION);
    let code: Option<String> = minted
        .secret(::emulator_server::aws_credentials::SESSION_TOKEN)
        .map(|secret| {
            secret
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(DEEP_LINK_CODE_CHARS)
                .collect()
        });
    // Reclaimed on EVERY path: the code is not a credential of this emulator (see above),
    // so a mint that yielded no secret must not leave one behind either.
    let _ = store.delete(&minted.id);
    code.filter(|code| code.chars().count() == DEEP_LINK_CODE_CHARS)
}

/// The URL a browser is directed to in order to redeem `code`, which is a RELATIVE
/// reference: the path a code is redeemed at, carrying the code and what redeeming it needs
/// — the domain the session is scoped to, the Region that domain lives in, and the redirect
/// to apply afterwards.
///
/// Recorded behavior: the URL has neither a scheme nor a host, and carries exactly those
/// four parameters. A relative reference resolves against whichever of the domain's own
/// endpoints the browser is already on — the one its id addresses and the ones derived from
/// its name are all the same domain — so the URL pins the browser to none of them. The
/// `redirect` parameter is always there and is EMPTY for a request that named no
/// `redirectUrl`, which is that member's own doc: "If omitted, no redirect is applied."
///
/// The query is assembled by the URL crate's form encoder, so a value that needs escaping —
/// the redirect, which is itself a URL — is percent-encoded rather than pasted in.
pub fn deep_link_url(
    domain_id: &str,
    region: &str,
    code: &str,
    redirect_url: Option<&str>,
) -> String {
    let mut query = ::url::form_urlencoded::Serializer::new(String::new());
    query.append_pair(DEEP_LINK_CODE_PARAMETER, code);
    query.append_pair(DEEP_LINK_DOMAIN_PARAMETER, domain_id);
    query.append_pair(DEEP_LINK_REGION_PARAMETER, region);
    query.append_pair(DEEP_LINK_REDIRECT_PARAMETER, redirect_url.unwrap_or_default());
    format!("{DEEP_LINK_PATH}?{}", query.finish())
}

/// When a code minted at `created_at` with a lifetime of `ttl_seconds` expires.
///
/// Whole seconds, like every other instant this service reports ([`super::now`]). The
/// lifetime is bounded by `ttlSeconds`' modelled `@range(min: 60, max: 600)`, so the sum
/// cannot leave the range of the type; a value that somehow would leaves the expiry at the
/// instant the code was minted rather than wrapping.
pub fn deep_link_expiry(
    created_at: ::aws_smithy_types::DateTime,
    ttl_seconds: i32,
) -> ::aws_smithy_types::DateTime {
    let seconds = created_at
        .secs()
        .checked_add(i64::from(ttl_seconds))
        .unwrap_or_else(|| created_at.secs());
    ::aws_smithy_types::DateTime::from_secs(seconds)
}

/// Why `redirect_url` is not a redirect `domain` may be sent to, or `None` when it is.
///
/// The three conditions `redirectUrl`'s model doc states, each reported as its own reason:
/// the URL has to be an HTTPS URL, it has to be one of the domain's own endpoints ("in the
/// domain"), the path it addresses BENEATH that endpoint has to be the one callback path,
/// and it may carry neither a query string nor a fragment. A port or a host the domain does
/// not serve is a different origin, so it is outside the domain just as another host is.
///
/// "A path of /auth/callback" is a path of the DOMAIN's endpoint: a domain reached at the
/// root of a host answers that path at the host's root, and one this emulator serves under a
/// path of its own endpoint ([`super::domain_endpoint_path`]) answers it under that path. So
/// the admissible redirect is an endpoint of the domain followed by the callback path,
/// whichever of the two forms the endpoint takes.
pub fn redirect_url_rejection(domain: &Domain, redirect_url: &str) -> Option<String> {
    let Ok(requested) = ::url::Url::parse(redirect_url) else {
        return Some(invalid_redirect_uri(MALFORMED_REDIRECT_URI));
    };
    if requested.scheme() != HTTPS_SCHEME {
        return Some(invalid_redirect_uri(REDIRECT_MUST_BE_HTTPS));
    }
    let at_the_domain: Vec<::url::Url> = domain_endpoints(domain)
        .filter_map(|endpoint| ::url::Url::parse(endpoint).ok())
        .filter(|endpoint| endpoint.origin() == requested.origin())
        .collect();
    if at_the_domain.is_empty() {
        return Some(invalid_redirect_uri(UNRECOGNIZED_DOMAIN));
    }
    if !at_the_domain
        .iter()
        .any(|endpoint| requested.path() == callback_path_of(endpoint))
    {
        return Some(invalid_redirect_uri(REDIRECT_PATH_REQUIRED));
    }
    if requested.query().is_some() || requested.fragment().is_some() {
        return Some(invalid_redirect_uri(REDIRECT_CARRIES_QUERY_OR_FRAGMENT));
    }
    None
}

/// The one path a redirect of the domain endpoint `endpoint` may address: that endpoint's
/// own path, then the callback path the member's doc fixes.
fn callback_path_of(endpoint: &::url::Url) -> String {
    format!(
        "{}{DEEP_LINK_REDIRECT_PATH}",
        endpoint.path().trim_end_matches('/')
    )
}

/// How a refused `redirectUrl` is worded: the fixed prefix, then the condition that failed.
/// Nothing of the URL the caller sent reaches the message.
fn invalid_redirect_uri(reason: &str) -> String {
    format!("{INVALID_REDIRECT_URI}: {reason}")
}

/// Every endpoint URL `domain` is reached at: the one its id addresses and the ones derived
/// from its name, which is the whole set `Domain` reports (`domainEndpointUrl` and
/// `customEndpointUrls`).
fn domain_endpoints(domain: &Domain) -> impl Iterator<Item = &str> {
    ::std::iter::once(domain.domain_endpoint_url.as_str()).chain(
        domain
            .custom_endpoint_urls
            .iter()
            .flatten()
            .map(String::as_str),
    )
}

impl super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState {
    /// Record `code` as a live one-time code of `domain_id`, and answer the instant it
    /// expires.
    ///
    /// Drops the records whose expiry `created_at` has passed before adding this one: a code
    /// that can no longer be redeemed is not state any caller can observe, and dropping it
    /// on every mint is what keeps the map to the codes of one lifetime's worth of calls.
    pub fn record_deep_link_code(
        &mut self,
        code: &str,
        domain_id: &str,
        redirect_url: Option<String>,
        created_at: ::aws_smithy_types::DateTime,
        ttl_seconds: i32,
    ) -> ::aws_smithy_types::DateTime {
        self.drop_expired_deep_link_codes(created_at);
        let expires_at = deep_link_expiry(created_at, ttl_seconds);
        let code_digest = deep_link_code_digest(code);
        self.deep_link_codes.insert(
            code_digest.clone(),
            StoredDeepLinkCode {
                domain_id: domain_id.to_string(),
                code_digest,
                redirect_url,
                created_at,
                expires_at,
            },
        );
        expires_at
    }

    /// Forget every code whose expiry `now` has reached — the one thing the CLOCK owes a
    /// code, which falls due whether or not anybody calls.
    pub fn drop_expired_deep_link_codes(&mut self, now: ::aws_smithy_types::DateTime) {
        self.deep_link_codes
            .retain(|_, stored| stored.expires_at.secs() > now.secs());
    }

    /// The record of `code`, if the store holds one that `now` has not expired.
    ///
    /// The code is resolved through its digest, so this never needs a stored code to compare
    /// against.
    pub fn live_deep_link_code(
        &self,
        code: &str,
        now: ::aws_smithy_types::DateTime,
    ) -> Option<&StoredDeepLinkCode> {
        self.deep_link_codes
            .get(&deep_link_code_digest(code))
            .filter(|stored| stored.expires_at.secs() > now.secs())
    }
}

/// The map a state struct holds its live deep-link codes in, keyed by
/// [`deep_link_code_digest`].
pub type DeepLinkCodes = HashMap<String, StoredDeepLinkCode>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The `host[:port]` the test domain's endpoints are announced at, standing for the
    /// address an emulator is reached on.
    const TEST_AUTHORITY: &str = "127.0.0.1:9297";

    /// A domain reached at one endpoint addressed by its id and one derived from its name,
    /// both rooted on the address this emulator serves the domain at — which is what
    /// [`super::super::announced_domain`] puts on every domain this service reports.
    fn test_domain() -> Domain {
        Domain {
            domain_id: "d-omni".to_string(),
            domain_arn: "arn:aws:cloudwatch:us-west-2:000000000000:domain/d-omni".to_string(),
            name: Some("omni-one".to_string()),
            identity_providers: vec![super::super::IDENTITY_PROVIDER_IDC.to_string()],
            identity_provider_configuration: None,
            domain_endpoint_url: format!(
                "https://{TEST_AUTHORITY}{}",
                super::super::domain_endpoint_path("d-omni")
            ),
            custom_endpoint_urls: Some(vec![format!(
                "https://{TEST_AUTHORITY}{}",
                super::super::domain_name_endpoint_path("omni-one")
            )]),
            identity_center_application_arn: None,
            region: "us-west-2".to_string(),
            created_at: ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
            updated_at: ::aws_smithy_types::DateTime::from_secs(1_700_000_000),
            status: super::super::DOMAIN_STATUS_ACTIVE.to_string(),
        }
    }

    /// A redirect at each endpoint the domain reports is admitted, and nothing else is.
    #[test]
    fn a_redirect_is_admitted_only_at_the_domains_own_endpoints() {
        let domain = test_domain();
        for endpoint in [
            domain.domain_endpoint_url.clone(),
            domain
                .custom_endpoint_urls
                .clone()
                .and_then(|urls| urls.first().cloned())
                .expect("a created domain reports a custom endpoint"),
        ] {
            let admitted = format!("{endpoint}{DEEP_LINK_REDIRECT_PATH}");
            assert_eq!(
                redirect_url_rejection(&domain, &admitted),
                None,
                "case {admitted:?}: expected an admitted redirect, got {:?}",
                redirect_url_rejection(&domain, &admitted)
            );
        }

        // Rows differ along the condition each one fails: the text is not a URL, the scheme,
        // the host, the port (a different origin of the same host), the path — another path
        // of the domain's endpoint, and the callback path at the origin's root rather than at
        // the endpoint — the query string and the fragment.
        let endpoint = domain.domain_endpoint_url.clone();
        let path = ::url::Url::parse(&endpoint)
            .expect("a domain endpoint is a URL")
            .path()
            .to_string();
        for (case, requested, reason) in [
            ("not a URL at all", "/auth/callback".to_string(), MALFORMED_REDIRECT_URI),
            (
                "an http URL at the domain",
                format!("http://{TEST_AUTHORITY}{path}{DEEP_LINK_REDIRECT_PATH}"),
                REDIRECT_MUST_BE_HTTPS,
            ),
            (
                "another host entirely",
                format!("https://attacker.example{path}{DEEP_LINK_REDIRECT_PATH}"),
                UNRECOGNIZED_DOMAIN,
            ),
            (
                "the host as a prefix of another",
                format!("https://127.0.0.1.attacker.example{path}{DEEP_LINK_REDIRECT_PATH}"),
                UNRECOGNIZED_DOMAIN,
            ),
            (
                "the domain's host on another port",
                format!("https://127.0.0.1:8443{path}{DEEP_LINK_REDIRECT_PATH}"),
                UNRECOGNIZED_DOMAIN,
            ),
            (
                "another path",
                format!("{endpoint}/auth/elsewhere"),
                REDIRECT_PATH_REQUIRED,
            ),
            (
                "the callback path outside the domain's own endpoint",
                format!("https://{TEST_AUTHORITY}{DEEP_LINK_REDIRECT_PATH}"),
                REDIRECT_PATH_REQUIRED,
            ),
            (
                "the path with a query string",
                format!("{endpoint}{DEEP_LINK_REDIRECT_PATH}?next=/home"),
                REDIRECT_CARRIES_QUERY_OR_FRAGMENT,
            ),
            (
                "the path with a fragment",
                format!("{endpoint}{DEEP_LINK_REDIRECT_PATH}#section"),
                REDIRECT_CARRIES_QUERY_OR_FRAGMENT,
            ),
        ] {
            let expected = Some(format!("{INVALID_REDIRECT_URI}: {reason}"));
            let actual = redirect_url_rejection(&domain, &requested);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The expiry is the lifetime past the mint, at whole-second resolution, at both
    /// published bounds of `ttlSeconds`' `@range(min: 60, max: 600)` and at the default.
    #[test]
    fn the_expiry_is_the_lifetime_past_the_mint() {
        let created_at = ::aws_smithy_types::DateTime::from_secs(1_700_000_000);
        for ttl_seconds in [60, DEFAULT_DEEP_LINK_TTL_SECONDS, 600] {
            let expires_at = deep_link_expiry(created_at, ttl_seconds);
            let expected = created_at.secs() + i64::from(ttl_seconds);
            assert_eq!(
                (expires_at.secs(), expires_at.subsec_nanos()),
                (expected, 0),
                "case {ttl_seconds}: expected {expected} whole seconds, got {expires_at:?}"
            );
        }
    }

    /// A recorded code is resolved by its digest until its expiry, the store holds no code,
    /// and a mint drops what has expired.
    #[test]
    fn a_recorded_code_lives_until_its_expiry_and_the_store_holds_only_its_digest() {
        let mut state = super::super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::default();
        let created_at = ::aws_smithy_types::DateTime::from_secs(1_700_000_000);
        // Rows differ along the lifetime asked for, so the two codes fall due at different
        // instants and the shorter one is dropped while the longer one is still live.
        let expires_at =
            state.record_deep_link_code("code-short", "d-omni", None, created_at, 60);
        let later = state.record_deep_link_code(
            "code-long",
            "d-omni",
            Some("https://d-omni.example/auth/callback".to_string()),
            created_at,
            600,
        );

        for (case, code, due) in [("code-short", "code-short", expires_at), ("code-long", "code-long", later)] {
            let stored = state
                .live_deep_link_code(code, created_at)
                .unwrap_or_else(|| panic!("case {case:?}: the code was recorded"));
            assert_eq!(
                (stored.domain_id.as_str(), stored.expires_at.secs()),
                ("d-omni", due.secs()),
                "case {case:?}: expected the domain and expiry it was recorded with, got {stored:?}"
            );
            assert_eq!(
                stored.code_digest,
                deep_link_code_digest(code),
                "case {case:?}: a record is keyed by the digest of its code, got {stored:?}"
            );
        }
        assert!(
            !format!("{:?}", state.deep_link_codes).contains("code-short"),
            "the store must hold no code, got {:?}",
            state.deep_link_codes
        );

        // One second past the shorter lifetime: the short code is gone and the long one is
        // untouched, whether it is read or a further mint drops what is due.
        let past_short = deep_link_expiry(created_at, 60);
        assert!(
            state.live_deep_link_code("code-short", past_short).is_none(),
            "case \"code-short\": an expired code resolves to nothing, got {:?}",
            state.live_deep_link_code("code-short", past_short)
        );
        state.drop_expired_deep_link_codes(past_short);
        assert_eq!(
            state.deep_link_codes.len(),
            1,
            "case \"a mint past one expiry\": expected only the live code to remain, got {:?}",
            state.deep_link_codes
        );
        assert!(
            state.live_deep_link_code("code-long", past_short).is_some(),
            "case \"code-long\": a code whose expiry has not passed stays live"
        );
    }

    /// The deep-link URL is a relative reference at the redemption path, carrying the code,
    /// the domain, the Region and the redirect — empty when the request asked for none.
    #[test]
    fn the_deep_link_url_is_a_relative_reference_carrying_what_redeeming_the_code_needs() {
        // Rows differ along the domain id, the Region and the redirect: none asked for, one
        // at the endpoint the domain's id addresses, and one at the endpoint derived from its
        // name — the last two values needing escaping to sit in a query.
        for (case, domain_id, region, redirect_url, expected) in [
            (
                "no redirect asked for",
                "d-0123456789abcdefghijklmno",
                "us-west-2",
                None,
                "/auth/code?code=abc&domainId=d-0123456789abcdefghijklmno&region=us-west-2\
                 &redirect=",
            ),
            (
                "a redirect at the endpoint the domain's id addresses",
                "d-0",
                "eu-central-1",
                Some("https://127.0.0.1:9297/_aws/cloudwatch-omni/domain/d-0/auth/callback"),
                "/auth/code?code=abc&domainId=d-0&region=eu-central-1\
                 &redirect=https%3A%2F%2F127.0.0.1%3A9297%2F_aws%2Fcloudwatch-omni%2Fdomain\
                 %2Fd-0%2Fauth%2Fcallback",
            ),
            (
                "a redirect at the endpoint derived from the domain's name",
                "d-0",
                "ap-southeast-2",
                Some("https://127.0.0.1:9297/_aws/cloudwatch-omni/domain-name/omni-one/auth/callback"),
                "/auth/code?code=abc&domainId=d-0&region=ap-southeast-2\
                 &redirect=https%3A%2F%2F127.0.0.1%3A9297%2F_aws%2Fcloudwatch-omni%2Fdomain-name\
                 %2Fomni-one%2Fauth%2Fcallback",
            ),
        ] {
            let actual = deep_link_url(domain_id, region, "abc", redirect_url);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            assert!(
                ::url::Url::parse(&actual).is_err(),
                "case {case:?}: the URL carries no scheme and no host, got {actual:?}"
            );
        }
    }
}
