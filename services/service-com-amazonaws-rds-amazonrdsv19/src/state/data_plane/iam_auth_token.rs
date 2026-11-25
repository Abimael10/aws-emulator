// SPDX-License-Identifier: CC0-1.0

//! The AUTHENTICATION TOKEN an IAM-authenticated login presents to a DB instance's engine
//! endpoint in place of a password.
//!
//! IAM database authentication has two halves, and this is the endpoint's half. A client
//! generates a token for one endpoint, one port and one database user (the AWS SDKs'
//! `generate_db_auth_token`), which is a SigV4-presigned request to `connect` as that user:
//!
//! ```text
//! <host>:<port>/?Action=connect&DBUser=<user>&X-Amz-Algorithm=AWS4-HMAC-SHA256
//!   &X-Amz-Credential=<key>%2F<date>%2F<region>%2Frds-db%2Faws4_request
//!   &X-Amz-Date=<date>T<time>Z&X-Amz-Expires=<seconds>&X-Amz-SignedHeaders=host
//!   &X-Amz-Signature=<signature>
//! ```
//!
//! The engine's half is to admit a login that presents such a token AS the user it was
//! generated for — and only on the endpoint it was generated for, in the window it was
//! generated for. What this reads is therefore the token's own claims, each against the
//! endpoint the session arrived on:
//!
//! * `Action=connect`, which is the only action a token is generated for;
//! * `DBUser`, which must be the user the session is logging in as;
//! * the host and port, which must be this endpoint's published address — a token for
//!   another instance is not a credential for this one;
//! * the credential scope, which must name the `rds-db` signing service in this instance's
//!   own Region;
//! * `X-Amz-Date` and `X-Amz-Expires`, which bound the window: "the token has a lifetime of
//!   15 minutes" (Amazon RDS User Guide, IAM database authentication), so a token whose
//!   window has passed — or whose stated lifetime is longer than AWS issues — authenticates
//!   nothing;
//! * `X-Amz-Signature`, which must be a SigV4 signature.
//!
//! What it does NOT do is recompute the signature: the signing key belongs to the
//! credential the token was signed with, and no engine-endpoint listener in this emulator
//! can resolve one. The other half of the decision is the DATABASE's: the user must be a
//! role of the instance that is enabled for IAM authentication, which on PostgreSQL means a
//! member of `rds_iam` ([`super::pg_grants`]) — so a token alone admits nobody.

use std::time::{Duration, SystemTime};

/// The signing service name an RDS authentication token is signed for, which is the service
/// the token's credential scope names.
const TOKEN_SIGNING_SERVICE: &str = "rds-db";

/// The only action a token is generated for.
const TOKEN_ACTION: &str = "connect";

/// The algorithm a presigned SigV4 request names.
const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The longest lifetime a token may state: "the token has a lifetime of 15 minutes".
const MAX_TOKEN_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// How far ahead of this clock a token's signing instant may be: the skew AWS allows a
/// signed request, so a client whose clock runs slightly fast still authenticates.
const MAX_CLOCK_SKEW: Duration = Duration::from_secs(15 * 60);

/// How many characters a SigV4 signature is: the hex of a SHA-256 HMAC.
const SIGNATURE_CHARS: usize = 64;

/// The endpoint, Region and user a token has to have been generated for.
#[derive(Debug, Clone)]
pub struct TokenTarget<'a> {
    /// The host the instance's endpoint publishes.
    pub host: &'a str,
    /// The port it publishes.
    pub port: u16,
    /// The Region the instance lives in, which the token's credential scope must name.
    pub region: &'a str,
    /// The database user the session is logging in as.
    pub user: &'a str,
}

/// Why a token authenticates nobody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRefusal {
    /// It is not a presigned `connect` request at all.
    Malformed,
    /// It names another action than `connect`.
    NotAConnectToken,
    /// It was generated for another endpoint.
    AnotherEndpoint,
    /// It was generated for another database user.
    AnotherUser,
    /// Its credential scope names another signing service or another Region.
    AnotherScope,
    /// Its window has passed, has not opened, or is longer than AWS issues.
    OutsideItsWindow,
    /// It carries no SigV4 signature.
    Unsigned,
}

/// The ACCESS KEY ID the credential scope of `token` names: the key the presigned request was
/// signed with, which is the credential the token speaks for.
///
/// `<key>/<date>/<region>/<service>/aws4_request` — the key is the first segment, as SigV4
/// writes the scope. `None` for a value that is not a presigned request, or one carrying no
/// credential at all.
///
/// A READING OF THE TEXT, and nothing else: WHO that key speaks for is a question for the
/// services that hold credentials ([`super::proxy_client_auth`]), which is where a login
/// authenticated by a proxy's IAM setting takes it.
#[must_use]
pub fn signing_access_key_id(token: &str) -> Option<String> {
    let fields = query_fields(token)?;
    let credential = field_of(&fields, "X-Amz-Credential")?;
    let key = credential.split('/').next().unwrap_or_default();
    (!key.is_empty()).then(|| key.to_string())
}

/// The query fields of a presigned request, each as `(name, value)`.
fn query_fields(token: &str) -> Option<Vec<(String, String)>> {
    let (_authority, rest) = token.split_once('/')?;
    let query = rest.strip_prefix('?')?;
    Some(
        ::form_urlencoded::parse(query.as_bytes())
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
    )
}

/// The value of the field `name`, read however the request spelled its name.
fn field_of<'f>(fields: &'f [(String, String)], name: &str) -> Option<&'f str> {
    fields
        .iter()
        .find(|(held, _)| held.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Whether `token` is an authentication token generated for `target`, as of `now`.
pub fn admits(token: &str, target: &TokenTarget<'_>, now: SystemTime) -> Result<(), TokenRefusal> {
    admits_at_one_of(
        token,
        &[(target.host, target.port)],
        target.region,
        target.user,
        now,
    )
}

/// [`admits`] for an endpoint that answers at SEVERAL addresses: whether `token` was generated
/// for `user` in `region` at ANY of `addresses`, as of `now`.
///
/// One endpoint, several pairs a client may legitimately have generated the token for. A DB
/// instance publishes its address and its port as two members, so a client composes exactly
/// one pair from them; `DBProxy$Endpoint` publishes ONE member carrying no port, so the client
/// completes it with the port of the databases the proxy's engine family names — and in this
/// emulator the published member carries the port of the listener as well, so the forms a
/// client can arrive with are several addresses OF ONE endpoint rather than several endpoints.
/// Which pairs those are is the caller's reading ([`super::wire_session`]); every other claim
/// the token carries is read here exactly once, for every address.
pub fn admits_at_one_of(
    token: &str,
    addresses: &[(&str, u16)],
    region: &str,
    user: &str,
    now: SystemTime,
) -> Result<(), TokenRefusal> {
    let (authority, _rest) = token.split_once('/').ok_or(TokenRefusal::Malformed)?;
    let (host, port) = authority.rsplit_once(':').ok_or(TokenRefusal::Malformed)?;
    let port: u16 = port.parse().map_err(|_| TokenRefusal::Malformed)?;
    let fields = query_fields(token).ok_or(TokenRefusal::Malformed)?;
    let field = |name: &str| field_of(&fields, name);

    if field("Action").ok_or(TokenRefusal::Malformed)? != TOKEN_ACTION {
        return Err(TokenRefusal::NotAConnectToken);
    }
    let names_the_endpoint = addresses
        .iter()
        .any(|(named, at)| host.eq_ignore_ascii_case(named) && port == *at);
    if !names_the_endpoint {
        return Err(TokenRefusal::AnotherEndpoint);
    }
    if field("DBUser").ok_or(TokenRefusal::Malformed)? != user {
        return Err(TokenRefusal::AnotherUser);
    }
    if field("X-Amz-Algorithm").ok_or(TokenRefusal::Malformed)? != SIGV4_ALGORITHM {
        return Err(TokenRefusal::AnotherScope);
    }
    // `<key>/<date>/<region>/<service>/aws4_request`: the Region and the service are what
    // say which endpoint the token was signed to reach.
    let credential = field("X-Amz-Credential").ok_or(TokenRefusal::Malformed)?;
    let scope: Vec<&str> = credential.split('/').collect();
    let scoped = scope.len() == 5
        && scope.get(2) == Some(&region)
        && scope.get(3) == Some(&TOKEN_SIGNING_SERVICE)
        && scope.get(4) == Some(&"aws4_request");
    if !scoped {
        return Err(TokenRefusal::AnotherScope);
    }
    let signature = field("X-Amz-Signature").ok_or(TokenRefusal::Unsigned)?;
    if signature.len() != SIGNATURE_CHARS
        || !signature.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(TokenRefusal::Unsigned);
    }
    within_window(
        field("X-Amz-Date").ok_or(TokenRefusal::Malformed)?,
        field("X-Amz-Expires").ok_or(TokenRefusal::Malformed)?,
        now,
    )
}

/// Whether `now` lies in the window a token signed at `signed_at` and valid for `expires`
/// seconds is usable in.
fn within_window(signed_at: &str, expires: &str, now: SystemTime) -> Result<(), TokenRefusal> {
    let signed_at = signing_instant(signed_at).ok_or(TokenRefusal::Malformed)?;
    let lifetime = Duration::from_secs(
        expires
            .parse::<u64>()
            .map_err(|_| TokenRefusal::Malformed)?,
    );
    if lifetime.is_zero() || lifetime > MAX_TOKEN_LIFETIME {
        return Err(TokenRefusal::OutsideItsWindow);
    }
    if now > signed_at + lifetime || signed_at > now + MAX_CLOCK_SKEW {
        return Err(TokenRefusal::OutsideItsWindow);
    }
    Ok(())
}

/// The instant `X-Amz-Date` names, in the basic ISO 8601 form SigV4 writes it in
/// (`20260101T010203Z`), read through the calendar of the `chrono` crate rather than by
/// hand.
fn signing_instant(stamp: &str) -> Option<SystemTime> {
    let parsed = ::chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ").ok()?;
    let seconds = u64::try_from(parsed.and_utc().timestamp()).ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The endpoint every token below is read against.
    fn target<'a>(user: &'a str) -> TokenTarget<'a> {
        TokenTarget {
            host: "127.0.0.1",
            port: 5432,
            region: "us-west-2",
            user,
        }
    }

    /// The claims one token carries, so a row states only the claim it varies.
    #[derive(Debug, Clone, Copy)]
    struct Claims<'a> {
        host: &'a str,
        port: u16,
        user: &'a str,
        action: &'a str,
        region: &'a str,
        service: &'a str,
        stamp: &'a str,
        expires: &'a str,
        signature: &'a str,
    }

    impl Claims<'_> {
        /// The token itself, built the way the SDKs build one.
        fn token(&self) -> String {
            let Self {
                host,
                port,
                user,
                action,
                region,
                service,
                stamp,
                expires,
                signature,
            } = self;
            format!(
                "{host}:{port}/?Action={action}&DBUser={user}\
                 &X-Amz-Algorithm={SIGV4_ALGORITHM}\
                 &X-Amz-Credential=AKIAEXAMPLE%2F20250101%2F{region}%2F{service}%2Faws4_request\
                 &X-Amz-Date={stamp}&X-Amz-Expires={expires}&X-Amz-SignedHeaders=host\
                 &X-Amz-Signature={signature}"
            )
        }
    }

    /// The token a client generated for this endpoint and this user, which every row below
    /// varies one claim of.
    fn generated() -> Claims<'static> {
        Claims {
            host: "127.0.0.1",
            port: 5432,
            user: "iamuser",
            action: "connect",
            region: "us-west-2",
            service: "rds-db",
            stamp: STAMP,
            expires: "900",
            signature: SIGNATURE,
        }
    }

    /// The signature shape a presigned request carries.
    const SIGNATURE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// The instant the rows are signed at, in the form SigV4 writes it.
    const STAMP: &str = "20250115T020000Z";

    /// A token admits exactly the login it was generated for: the rows differ along every
    /// claim the token carries — the endpoint, the user, the action, the scope, the window
    /// and the signature.
    #[test]
    fn a_token_admits_the_one_login_it_was_generated_for() {
        let signed = signing_instant(STAMP).expect("the stamp is a signing instant");
        let now = signed + Duration::from_secs(1);
        let rows: Vec<(&str, String, Result<(), TokenRefusal>)> = vec![
            ("the token the client generated", generated().token(), Ok(())),
            (
                "a token for another host",
                Claims {
                    host: "other.host",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::AnotherEndpoint),
            ),
            (
                "a token for another port",
                Claims {
                    port: 15_432,
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::AnotherEndpoint),
            ),
            (
                "a token for another user",
                Claims {
                    user: "someone",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::AnotherUser),
            ),
            (
                "a token for another action",
                Claims {
                    action: "describe",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::NotAConnectToken),
            ),
            (
                "a token signed for another Region",
                Claims {
                    region: "eu-west-1",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::AnotherScope),
            ),
            (
                "a token signed for another service",
                Claims {
                    service: "rds",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::AnotherScope),
            ),
            (
                "a token whose window has passed",
                Claims {
                    stamp: "20250115T010000Z",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::OutsideItsWindow),
            ),
            (
                "a token claiming a longer lifetime than AWS issues",
                Claims {
                    expires: "901",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::OutsideItsWindow),
            ),
            (
                "a token signed far in the future",
                Claims {
                    stamp: "20250115T050000Z",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::OutsideItsWindow),
            ),
            (
                "a token carrying no signature",
                Claims {
                    signature: "not-a-signature",
                    ..generated()
                }
                .token(),
                Err(TokenRefusal::Unsigned),
            ),
            (
                "a password that is not a token at all",
                "a-password".to_string(),
                Err(TokenRefusal::Malformed),
            ),
        ];
        for (case, token, expected) in rows {
            let found = admits(&token, &target("iamuser"), now);
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?}, got {found:?} for {token}"
            );
        }
        // And the same token is still read against the clock: once its lifetime has
        // elapsed it admits nobody.
        let late = signed + Duration::from_secs(901);
        assert_eq!(
            admits(&generated().token(), &target("iamuser"), late),
            Err(TokenRefusal::OutsideItsWindow),
            "case the token after its lifetime: expected it to be outside its window"
        );
    }

    /// RULE: the credential a token was signed with is the FIRST segment of its credential
    /// scope, which is the key the principal behind the token is resolved by; a value carrying
    /// no such scope names no credential at all.
    ///
    /// The rows differ along what the token carries: a scope naming a long-term key, one
    /// naming a session key, a presigned request carrying no credential field, and a password
    /// that is no token.
    #[test]
    fn the_key_a_token_was_signed_with_is_the_first_segment_of_its_scope() {
        let rows: Vec<(&str, String, Option<String>)> = vec![
            (
                "the token the client generated",
                generated().token(),
                Some("AKIAEXAMPLE".to_string()),
            ),
            (
                "a scope naming a session key",
                generated().token().replace("AKIAEXAMPLE", "ASIASESSIONKEY"),
                Some("ASIASESSIONKEY".to_string()),
            ),
            (
                "a presigned request carrying no credential",
                "127.0.0.1:5432/?Action=connect&DBUser=iamuser".to_string(),
                None,
            ),
            (
                "a password that is not a token at all",
                "a-password".to_string(),
                None,
            ),
        ];
        for (case, token, expected) in rows {
            let found = signing_access_key_id(&token);
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
        }
    }
}
