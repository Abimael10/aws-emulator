// SPDX-License-Identifier: CC0-1.0

//! The reading this plane makes of a WORKLOAD IDENTITY token — the token a first-party
//! AgentCore Identity call presents back to the service that minted it.
//!
//! `GetResourceApiKey` and its siblings are handed a `workloadIdentityToken`, "the identity
//! token of the workload" the call acts for, and each models `UnauthorizedException`
//! (HTTP 401), the refusal thrown "when the JWT bearer token is invalid or not found". So the
//! presented value is a value to be CHECKED before the call resolves anything it named: a
//! token this service did not issue, or one the clock has run out on, earns that refusal and
//! the call resolves nothing.
//!
//! What decides it is the ISSUANCE this service recorded
//! ([`super::IssuedWorkloadAccessTokens`]), not the bytes the caller sent: a workload access
//! token is minted by the call that asks for one, so the only tokens this plane accepts are
//! the ones it handed out and has not seen expire. A token whose issuance the registry no
//! longer resolves is still READ — through the reader of a token of this service's own
//! minting ([`::service_common::jwt::read_unsecured`]) — but only to word the refusal: a
//! claim read out of a value a caller supplied is evidence for refusing it, never for
//! granting anything.

use std::time::SystemTime;

use ::service_common::jwt;

use super::{IssuedWorkloadAccessToken, IssuedWorkloadAccessTokens};

/// Why a presented workload identity token is not one this plane accepts, with the class AWS
/// words each refusal as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadIdentityTokenRefusal {
    /// The token's own validity window has run out, so the issuance it stands for is over.
    Expired,
    /// The value is not a token this service issued: not a readable compact JWS, or one no
    /// issuance of this service's registry stands behind.
    Invalid,
}

impl WorkloadIdentityTokenRefusal {
    /// The sentence this refusal is worded with.
    ///
    /// The two sentences this plane already refuses a presented bearer token with
    /// ([`super::EXPIRED_USER_TOKEN_MESSAGE`], [`super::INVALID_USER_TOKEN_MESSAGE`]): one
    /// `UnauthorizedException` class, so the plane cannot word the same condition two ways
    /// depending on which token the call was handed. Neither quotes the presented value
    /// back — it is a bearer credential.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Expired => super::EXPIRED_USER_TOKEN_MESSAGE,
            Self::Invalid => super::INVALID_USER_TOKEN_MESSAGE,
        }
    }
}

/// The issuance `presented` stands for, when it is a workload access token this service
/// issued and the issuance is still valid at `now`.
///
/// `Ok` is the record the mint wrote — the workload identity the token was issued from and
/// the user it acts for — which is what makes a token minted a moment ago immediately usable
/// against a first-party call. `Err` is the class the call is refused as, which the handler
/// answers in its modelled `UnauthorizedException`.
pub fn workload_of_identity_token<'registry>(
    issued: &'registry IssuedWorkloadAccessTokens,
    presented: &str,
    now: SystemTime,
) -> Result<&'registry IssuedWorkloadAccessToken, WorkloadIdentityTokenRefusal> {
    // A presentation prefix is not part of the token, and the tree strips it in one place.
    let token = jwt::without_bearer_scheme(presented);
    if let Some(record) = issued.issued(token, now) {
        return Ok(record);
    }
    // No issuance stands behind it. The token itself says which of the two classes to word:
    // one whose own `exp` has passed is the expiry class, and everything else — a value that
    // is no readable token, or a readable one this service never issued — is the other.
    Err(match jwt::read_unsecured(token) {
        Ok(read) if matches!(read.outside_window(now), Some(jwt::WindowDefect::Expired)) => {
            WorkloadIdentityTokenRefusal::Expired
        }
        _ => WorkloadIdentityTokenRefusal::Invalid,
    })
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
            read.is_ok_and(|held| held.workload_name == "demo.agent"
                && held.user_id.as_deref() == Some("user-1")),
            "case \"a live issuance for a user\": expected the workload and user it was minted \
             for, got {read:?}"
        );
        let read = workload_of_identity_token(&registry, &for_the_workload, now);
        assert!(
            read.is_ok_and(|held| held.workload_name == "other-agent" && held.user_id.is_none()),
            "case \"a live issuance for the workload itself\": expected that workload and no \
             user, got {read:?}"
        );
        // Presented under the bearer scheme, the same token is the same issuance.
        let read = workload_of_identity_token(&registry, &format!("Bearer {live}"), now);
        assert!(
            read.is_ok(),
            "case \"presented under the bearer scheme\": expected the issuance, got {read:?}"
        );

        for (case, presented, expected) in [
            (
                "its window closed at this very instant",
                expiring_now.clone(),
                WorkloadIdentityTokenRefusal::Expired,
            ),
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
                WorkloadIdentityTokenRefusal::Invalid,
            ),
            (
                "three segments that are not base64url JSON",
                "abc.def.ghi".to_string(),
                WorkloadIdentityTokenRefusal::Invalid,
            ),
            (
                "not a token at all",
                "not-a-token".to_string(),
                WorkloadIdentityTokenRefusal::Invalid,
            ),
            (
                "an empty value",
                String::new(),
                WorkloadIdentityTokenRefusal::Invalid,
            ),
        ] {
            let read = workload_of_identity_token(&registry, &presented, now);
            assert_eq!(
                read.err(),
                Some(expected),
                "case {case:?}: expected {expected:?}, got {:?}",
                workload_of_identity_token(&registry, &presented, now)
            );
        }

        // The clock alone ends an issuance: the live token stops resolving once `now` passes
        // the instant it was minted until, with no call made in between.
        let read = workload_of_identity_token(&registry, &live, now + Duration::from_secs(900));
        assert_eq!(
            read.err(),
            Some(WorkloadIdentityTokenRefusal::Expired),
            "case \"past its own expiry\": expected the expiry class, got {read:?}"
        );
    }

    /// RULE: each refusal class carries its own fixed sentence, and neither quotes the
    /// presented token back.
    #[test]
    fn each_refusal_class_carries_its_own_fixed_sentence() {
        assert_eq!(
            WorkloadIdentityTokenRefusal::Expired.message(),
            "Token has expired",
            "case \"expired\": expected the fixed text of the expiry class"
        );
        assert_eq!(
            WorkloadIdentityTokenRefusal::Invalid.message(),
            "Invalid JWT token",
            "case \"invalid\": expected the fixed text of the invalid-token class"
        );
    }
}
