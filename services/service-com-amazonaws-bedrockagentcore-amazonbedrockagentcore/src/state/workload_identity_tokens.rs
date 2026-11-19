// SPDX-License-Identifier: CC0-1.0

//! The reading this plane makes of a WORKLOAD IDENTITY token — the token a first-party
//! AgentCore Identity call presents back to the service that minted it.
//!
//! `GetResourceApiKey` and its siblings are handed a `workloadIdentityToken`, "the identity
//! token of the workload" the call acts for. That value is a CREDENTIAL CARRIED IN THE
//! REQUEST BODY, not the caller's own transport credential, so a call presenting one this
//! service cannot accept is an authorization verdict on the request:
//! `AccessDeniedException` (HTTP 403), worded with the one fixed sentence AWS refuses this
//! gate with ([`INVALID_WORKLOAD_ACCESS_TOKEN_MESSAGE`]). The `UnauthorizedException`
//! (HTTP 401) these operations also model is the refusal thrown "when the JWT bearer token
//! is invalid or not found for OAuth bearer token based access" — the inbound OAuth bearer
//! credential of the caller ([`super::user_of_presented_token`]), which is a different value
//! than this one.
//!
//! What decides the gate is the ISSUANCE this service recorded
//! ([`super::IssuedWorkloadAccessTokens`]), not the bytes the caller sent: a workload access
//! token is minted by the call that asks for one, so the only tokens this plane accepts are
//! the ones it handed out and has not seen expire. The registry answers one question — does a
//! live issuance stand behind the presented value at this instant — and a value it does not
//! resolve is refused with that one sentence, whether this service never issued it, whether
//! the clock has run out on the issuance, or whether the value is no token at all. The
//! refusal quotes nothing of the presented value back: it is a credential.

use std::time::SystemTime;

use ::service_common::jwt;

use super::{IssuedWorkloadAccessToken, IssuedWorkloadAccessTokens};

/// The fixed text AWS refuses a workload identity token it does not accept with. It carries
/// no request- or state-derived part: the token is sensitive and is never echoed, and the
/// sentence names no workload, provider or account.
pub const INVALID_WORKLOAD_ACCESS_TOKEN_MESSAGE: &str = "Invalid workload access token";

/// The issuance `presented` stands for, when it is a workload access token this service
/// issued and the issuance is still valid at `now`.
///
/// `Some` is the record the mint wrote — the workload identity the token was issued from and
/// the user it acts for — which is what makes a token minted a moment ago immediately usable
/// against a first-party call. `None` is the gate refusing the call, which the handler
/// answers in its modelled `AccessDeniedException` with
/// [`INVALID_WORKLOAD_ACCESS_TOKEN_MESSAGE`].
#[must_use]
pub fn workload_of_identity_token<'registry>(
    issued: &'registry IssuedWorkloadAccessTokens,
    presented: &str,
    now: SystemTime,
) -> Option<&'registry IssuedWorkloadAccessToken> {
    // A presentation prefix is not part of the token, and the tree strips it in one place.
    issued.issued(jwt::without_bearer_scheme(presented), now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The instant every case is decided at, and the same instant in epoch seconds.
    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    /// A token of this service's own minting, for `workload`, valid from `issued_at` for
    /// `ttl`, recorded in `registry` exactly as a mint records one.
    fn mint(
        registry: &mut IssuedWorkloadAccessTokens,
        workload: &str,
        user: Option<&str>,
        issued_at: SystemTime,
        ttl: Duration,
        seq: u64,
    ) -> String {
        let expires_at = issued_at + ttl;
        let token = super::super::mint_workload_access_token(
            "us-east-1",
            workload,
            user,
            issued_at,
            expires_at,
            seq,
        );
        registry.record(
            token.clone(),
            IssuedWorkloadAccessToken {
                workload_name: workload.to_string(),
                user_id: user.map(str::to_string),
                issued_at,
                expires_at,
            },
            issued_at,
        );
        token
    }

    /// RULE: the issuance this service recorded is what decides a presented token — a live
    /// one resolves to the workload and user it was minted for, and nothing else resolves at
    /// all.
    #[test]
    fn only_a_live_issuance_of_this_service_resolves() {
        let now = now();
        let mut registry = IssuedWorkloadAccessTokens::default();
        let live = mint(
            &mut registry,
            "demo.agent",
            Some("user-1"),
            now - Duration::from_secs(60),
            Duration::from_secs(900),
            1,
        );
        let for_the_workload = mint(
            &mut registry,
            "other-agent",
            None,
            now,
            Duration::from_secs(900),
            2,
        );
        let expiring_now = mint(
            &mut registry,
            "demo.agent",
            Some("user-1"),
            now - Duration::from_secs(900),
            Duration::from_secs(900),
            3,
        );

        let read = workload_of_identity_token(&registry, &live, now);
        assert!(
            read.is_some_and(|held| held.workload_name == "demo.agent"
                && held.user_id.as_deref() == Some("user-1")),
            "case \"a live issuance for a user\": expected the workload and user it was minted \
             for, got {read:?}"
        );
        let read = workload_of_identity_token(&registry, &for_the_workload, now);
        assert!(
            read.is_some_and(|held| held.workload_name == "other-agent"
                && held.user_id.is_none()),
            "case \"a live issuance for the workload itself\": expected that workload and no \
             user, got {read:?}"
        );
        // Presented under the bearer scheme, the same token is the same issuance.
        let read = workload_of_identity_token(&registry, &format!("Bearer {live}"), now);
        assert!(
            read.is_some(),
            "case \"presented under the bearer scheme\": expected the issuance, got {read:?}"
        );

        for (case, presented) in [
            ("its window closed at this very instant", expiring_now),
            (
                "a token this service never issued",
                super::super::mint_workload_access_token(
                    "us-east-1",
                    "demo.agent",
                    Some("user-1"),
                    now,
                    now + Duration::from_secs(900),
                    99,
                ),
            ),
            (
                "three segments that are not base64url JSON",
                "abc.def.ghi".to_string(),
            ),
            ("not a token at all", "not-a-token".to_string()),
            ("an empty value", String::new()),
        ] {
            let read = workload_of_identity_token(&registry, &presented, now);
            assert!(
                read.is_none(),
                "case {case:?}: expected no issuance to resolve, got {read:?}"
            );
        }

        // The clock alone ends an issuance: the live token stops resolving once `now` passes
        // the instant it was minted until, with no call made in between.
        let read = workload_of_identity_token(&registry, &live, now + Duration::from_secs(900));
        assert!(
            read.is_none(),
            "case \"past its own expiry\": expected no issuance to resolve, got {read:?}"
        );
    }
}
