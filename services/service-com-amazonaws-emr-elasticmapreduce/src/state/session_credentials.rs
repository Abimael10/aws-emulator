// SPDX-License-Identifier: CC0-1.0

//! The SESSION CREDENTIALS a cluster with fine-grained access control issues.
//!
//! `GetClusterSessionCredentials` "provides temporary, HTTP basic credentials that are
//! associated with a given runtime IAM role and used by a cluster with fine-grained access
//! control activated" (its own Smithy doc). A cluster gets that activation from its SECURITY
//! CONFIGURATION — the document `CreateSecurityConfiguration` stored and the launch named —
//! so whether a cluster can issue credentials at all is COMPUTED from that document
//! ([`ComAmazonawsEmrElasticmapreduceState::activates_fine_grained_access_control`]) rather
//! than assumed either way:
//!
//! * `AuthorizationConfiguration.IAMConfiguration.EnableApplicationScopedIAMRole` activates
//!   runtime roles for the cluster's steps and interactive submissions (Amazon EMR Management
//!   Guide, "Runtime roles for Amazon EMR steps");
//! * `AuthenticationConfiguration.IdentityCenterConfiguration.EnableIdentityCenter` activates
//!   the cluster's IAM Identity Center integration (Amazon EMR Management Guide, "Use IAM
//!   Identity Center with Amazon EMR").
//!
//! A cluster whose configuration carries neither — and a cluster that names no security
//! configuration at all — has no runtime role for credentials to be issued against, which is
//! the one refusal this operation words for that class
//! ([`super::runtime_roles_not_enabled_message`]).
//!
//! # What is issued
//!
//! The pair a cluster endpoint authenticates with: a USERNAME that identifies the session and
//! a PASSWORD that authenticates it.
//!
//! The username is DERIVED from the cluster and the identity the session belongs to
//! ([`session_credentials_username`]) — the runtime role the request names, or the cluster's
//! own Identity Center integration where it names none. Nothing of the clock or of any
//! randomness enters it, so asking twice for the same role on the same cluster identifies the
//! same session and two roles never share one. AWS documents no format for the member, so the
//! form here is this emulator's: what IS the contract is that it is non-empty, stable per
//! (cluster, identity) pair and distinct between pairs.
//!
//! The password is a SECRET, so it is minted by the emulator's credential store — the one
//! component that generates secrets — and removed from it again, exactly as the browser token
//! of a presigned application-UI URL is ([`super::mint_presigned_url_token`]): it
//! authenticates an HTTP basic connection to a cluster endpoint, not a SigV4 request to this
//! emulator, so the emulator must not go on accepting it as one. Nothing of the request, of
//! the cluster or of the clock enters it, and no call's password is predictable from
//! another's.

use super::*;

/// The section of a security-configuration document that carries the cluster's
/// authorization settings.
pub(super) const AUTHORIZATION_CONFIGURATION: &str = "AuthorizationConfiguration";

/// The section inside it that carries the IAM settings.
pub(super) const IAM_CONFIGURATION: &str = "IAMConfiguration";

/// The flag that activates runtime roles — application-scoped IAM roles — for the cluster.
pub(super) const ENABLE_APPLICATION_SCOPED_IAM_ROLE: &str = "EnableApplicationScopedIAMRole";

/// The section of a security-configuration document that carries the cluster's
/// authentication settings.
const AUTHENTICATION_CONFIGURATION: &str = "AuthenticationConfiguration";

/// The section inside it that carries the IAM Identity Center settings.
const IDENTITY_CENTER_CONFIGURATION: &str = "IdentityCenterConfiguration";

/// The flag that activates the cluster's IAM Identity Center integration.
const ENABLE_IDENTITY_CENTER: &str = "EnableIdentityCenter";

/// How long the credentials this operation issues are good for.
///
/// CHOSEN, not an AWS duration: AWS documents only that `ExpiresAt` is "the time when the
/// credentials that are returned by the GetClusterSessionCredentials API expire"
/// (`GetClusterSessionCredentialsOutput$ExpiresAt` doc) and names no span, so this is a
/// bounded session lifetime of this emulator's own — long enough for an interactive
/// submission, short enough to be temporary.
pub const SESSION_CREDENTIALS_LIFETIME: Duration = Duration::from_secs(60 * 60);

/// How many characters of the derived username are kept.
///
/// CHOSEN: 32 hexadecimal characters of the digest below, which the member admits comfortably
/// (`UsernamePassword$Username` targets `XmlStringMaxLen256`) and which no two pairs of this
/// account collide in.
const SESSION_USERNAME_LEN: usize = 32;

/// How long the password a call issues is.
///
/// CHOSEN: 32 characters out of the store's own secret, which carries far more entropy than
/// one HTTP basic connection needs and fits the member (`UsernamePassword$Password` targets
/// `XmlStringMaxLen256`).
const SESSION_PASSWORD_LEN: usize = 32;

/// What the session the credentials belong to is SCOPED to, when the request names no runtime
/// role: the cluster's own IAM Identity Center integration, which is the identity such a
/// session carries.
pub const IDENTITY_CENTER_SESSION_SCOPE: &str = "identity-center";

impl ComAmazonawsEmrElasticmapreduceState {
    /// The security-configuration document the cluster `cluster_id` was created with, parsed
    /// — `None` for a cluster this account holds none of, a cluster naming no security
    /// configuration, one naming a configuration this account does not hold, and one whose
    /// stored document is not JSON.
    ///
    /// The document is held VERBATIM ([`SecurityConfigurationRecord`]), so this is a read of
    /// exactly what `CreateSecurityConfiguration` stored.
    #[must_use]
    pub fn cluster_security_configuration(&self, cluster_id: &str) -> Option<::serde_json::Value> {
        let name = self.clusters.get(cluster_id)?.security_configuration.clone()?;
        let record = self.security_configurations.get(&name)?;
        ::serde_json::from_str::<::serde_json::Value>(&record.security_configuration).ok()
    }

    /// Whether the cluster `cluster_id`'s security configuration activates its IAM Identity
    /// Center integration.
    #[must_use]
    pub fn is_identity_center_enabled_cluster(&self, cluster_id: &str) -> bool {
        self.cluster_security_configuration(cluster_id)
            .and_then(|document| {
                flag(
                    &document,
                    &[
                        AUTHENTICATION_CONFIGURATION,
                        IDENTITY_CENTER_CONFIGURATION,
                        ENABLE_IDENTITY_CENTER,
                    ],
                )
            })
            .unwrap_or(false)
    }

    /// Whether the cluster `cluster_id` has FINE-GRAINED ACCESS CONTROL activated: its
    /// security configuration enables runtime roles, its IAM Identity Center integration, or
    /// both. That is the class of cluster session credentials are issued for.
    #[must_use]
    pub fn activates_fine_grained_access_control(&self, cluster_id: &str) -> bool {
        self.is_runtime_role_enabled_cluster(cluster_id)
            || self.is_identity_center_enabled_cluster(cluster_id)
    }
}

/// The boolean `path` carries in `document`, when it carries one.
pub(super) fn flag(document: &::serde_json::Value, path: &[&str]) -> Option<bool> {
    let mut section = document;
    for name in path {
        section = section.get(name)?;
    }
    section.as_bool()
}

/// The USERNAME the session credentials of `cluster_id` for `identity` carry.
///
/// A digest of the pair, so the value is stable for one pair and distinct between pairs
/// without anything about it being stored: the member identifies the session, and the session
/// is the cluster plus the identity it was asked for. The two parts are joined by a separator
/// neither a cluster id nor an ARN carries, so no two different pairs render as one string.
#[must_use]
pub fn session_credentials_username(cluster_id: &str, identity: &str) -> String {
    use ::sha2::Digest;

    let mut digest = ::sha2::Sha256::new();
    digest.update(cluster_id.as_bytes());
    digest.update([0u8]);
    digest.update(identity.as_bytes());
    let mut rendered = ::hex::encode(digest.finalize());
    rendered.truncate(SESSION_USERNAME_LEN);
    rendered
}

/// A fresh PASSWORD for one call's session credentials, or `None` when the store minted no
/// secret or the secret could not be removed from it again.
///
/// See the module docs: the secret is the credential store's, and it is removed again because
/// it authenticates a cluster endpoint rather than a request to this emulator. Only the
/// characters that travel through an HTTP basic header as themselves are kept
/// ([`is_presigned_url_token_char`] is the same filter the browser token of a presigned URL is
/// drawn through), which is a filter over a uniformly drawn secret rather than a derivation
/// from anything.
#[must_use]
pub fn mint_session_password(store: &::emulator_server::CredentialStore) -> Option<String> {
    mint_store_secret(store, SESSION_PASSWORD_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the username is stable for one (cluster, identity) pair and distinct between
    /// pairs — in the cluster, in the identity, and in how the two are spelled.
    #[test]
    fn the_username_identifies_the_cluster_and_identity_pair() {
        let rows: [(&str, &str, &str); 5] = [
            ("one cluster, one role", "j-1", "arn:aws:iam::123456789012:role/A"),
            ("the same cluster, another role", "j-1", "arn:aws:iam::123456789012:role/B"),
            ("another cluster, the first role", "j-2", "arn:aws:iam::123456789012:role/A"),
            ("a session with no runtime role", "j-1", IDENTITY_CENTER_SESSION_SCOPE),
            // The separator is what keeps a pair from rendering as another pair's string.
            ("a role whose name carries the cluster id", "j", "1arn"),
        ];
        let mut seen: Vec<(&str, String)> = Vec::new();
        for (case, cluster_id, identity) in rows {
            let username = session_credentials_username(cluster_id, identity);
            assert_eq!(
                username.len(),
                SESSION_USERNAME_LEN,
                "case {case:?}: expected {SESSION_USERNAME_LEN} characters, got {username:?}"
            );
            assert_eq!(
                username,
                session_credentials_username(cluster_id, identity),
                "case {case:?}: asking again identifies the same session"
            );
            if let Some((other, _)) = seen
                .iter()
                .find(|(_, already)| already.as_str() == username.as_str())
            {
                panic!("case {case:?}: shares its username with case {other:?}");
            }
            seen.push((case, username));
        }
    }
}
