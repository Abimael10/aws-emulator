// SPDX-License-Identifier: CC0-1.0

//! The AMAZON COGNITO IDENTITY an invocation was made BY, as the runtime reports it to the
//! handler in `context.identity`.
//!
//! An identity pool vends a caller a role session in exchange for the identity it holds for
//! that caller: `GetCredentialsForIdentity` assumes the pool's role through
//! `sts:AssumeRoleWithWebIdentity`, and the session that assume mints carries the claims of
//! the exchanged token — its audience, which is the identity POOL, and its subject, which is
//! the IDENTITY in that pool. So the identity behind a credential is a fact the token
//! service's session registry already holds, resolved here off the access key id the request
//! was signed with rather than taken from anything the caller said about itself.
//!
//! That is what makes the identity REPORTABLE: a credential whose session carries those two
//! claims was vended to one identity of one pool, and an invocation signed with it is an
//! invocation of that identity. Every other credential — an IAM user's long-term access key,
//! a role session assumed by any other means, the account's own key — carries no such claim,
//! so an invocation signed with one is reported as made by NO Cognito identity at all, which
//! is what lets a handler authorizing on the identity tell an authenticated end user from an
//! AWS principal.
//!
//! Nothing is invented and nothing is defaulted: an identity is reported only where the
//! session carries both claims, and a session the registry does not hold or has expired
//! reports none.

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

/// The claim of a web identity token carrying the AUDIENCE it was issued for, which for a
/// token an identity pool exchanges is the pool itself: "the intended audience (also known as
/// client ID) of the web identity token", published to a policy decision as
/// `<provider>:aud`.
const AUDIENCE_CLAIM: &str = "aud";

/// The claim carrying the SUBJECT the token names, which for a pool's token is the identity
/// the pool holds for the caller; published as `<provider>:sub`.
const SUBJECT_CLAIM: &str = "sub";

/// The Cognito identity one invocation was made by: the identity, and the pool it belongs
/// to.
///
/// Both members together or neither: the pair is read off the two claims of one exchanged
/// token, so there is no invocation of an identity of no pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationIdentity {
    /// The identity the credentials were vended to (`IdentityId`), as
    /// `context.identity.cognito_identity_id`.
    pub identity_id: String,
    /// The identity pool that vended them (`IdentityPoolId`), as
    /// `context.identity.cognito_identity_pool_id`.
    pub identity_pool_id: String,
}

impl InvocationIdentity {
    /// The identity the condition keys `claims` publish name, or `None` when they name none:
    /// the keys the Cognito Identity provider publishes about the token the credential's
    /// session was minted by exchanging.
    ///
    /// A session minted by any other mint publishes no claim under that provider, so it
    /// resolves no identity — which is the absent `context.identity` of an IAM or
    /// assumed-role caller.
    #[must_use]
    pub fn from_federated_claims(claims: &[(String, String)]) -> Option<Self> {
        let provider = sts_store::COGNITO_IDENTITY_PROVIDER_NAME;
        let claimed = |claim: &str| {
            let key = format!("{provider}:{claim}");
            claims
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty())
        };
        Some(Self {
            identity_id: claimed(SUBJECT_CLAIM)?,
            identity_pool_id: claimed(AUDIENCE_CLAIM)?,
        })
    }

    /// This identity as the RUNTIME is handed it, under the member names the Lambda runtime
    /// interface carries a caller's Cognito identity in — the document a managed runtime
    /// builds `context.identity` out of.
    #[must_use]
    pub fn runtime_document(&self) -> ::serde_json::Value {
        ::serde_json::json!({
            "cognitoIdentityId": self.identity_id,
            "cognitoIdentityPoolId": self.identity_pool_id,
        })
    }

    /// [`Self::runtime_document`] for the invocation `identity` belongs to, and the JSON
    /// `null` for an invocation made by no Cognito identity — which is what makes the
    /// runtime report `context.identity` as carrying none.
    #[must_use]
    pub fn runtime_member(identity: Option<&Self>) -> ::serde_json::Value {
        identity.map_or(::serde_json::Value::Null, Self::runtime_document)
    }
}

/// The Cognito identity the request `ctx` carries was signed by, or `None` when its
/// credential was vended to no identity of any pool.
///
/// The credential is resolved in the token service's own session registry in the shared
/// engine — the registry a vend records its session in — so the identity reported for an
/// invocation is the one the exchange that minted its credential was made for. A session the
/// registry does not hold (a long-term access key, the account's own key) and one that has
/// expired by `now` carry no identity.
#[must_use]
pub fn caller_cognito_identity<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    now: ::std::time::SystemTime,
) -> Option<InvocationIdentity> {
    let access_key_id = ::emulator_server::extract_akid(&ctx.parts().headers);
    let sessions = sts_store::service_state(engine)?;
    let sessions = sessions.lock();
    let session = sessions.session(&access_key_id)?;
    if session.is_expired(now) {
        return None;
    }
    InvocationIdentity::from_federated_claims(&session.federated_claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The provider every Cognito-vended session's claims are published under.
    fn key(claim: &str) -> String {
        format!(
            "{}:{claim}",
            sts_store::COGNITO_IDENTITY_PROVIDER_NAME
        )
    }

    /// One row of the table below: what it is, the claims the session carries, and the
    /// identity they name.
    struct Row {
        case: &'static str,
        claims: Vec<(String, String)>,
        want: Option<InvocationIdentity>,
    }

    /// RULE: the identity an invocation reports is the pair of claims the exchanged token
    /// published — its subject is the identity, its audience the pool — and a claim set that
    /// does not carry both names no identity at all.
    #[test]
    fn only_both_claims_of_the_exchanged_token_name_an_identity() {
        let pool = "us-east-1:11111111-2222-3333-4444-555555555555";
        let identity = "us-east-1:66666666-7777-8888-9999-aaaaaaaaaaaa";
        let rows = vec![
            Row {
                case: "both claims",
                claims: vec![
                    (key(SUBJECT_CLAIM), identity.to_string()),
                    (key(AUDIENCE_CLAIM), pool.to_string()),
                ],
                want: Some(InvocationIdentity {
                    identity_id: identity.to_string(),
                    identity_pool_id: pool.to_string(),
                }),
            },
            Row {
                case: "the subject alone",
                claims: vec![(key(SUBJECT_CLAIM), identity.to_string())],
                want: None,
            },
            Row {
                case: "the audience alone",
                claims: vec![(key(AUDIENCE_CLAIM), pool.to_string())],
                want: None,
            },
            Row {
                case: "no claim at all",
                claims: Vec::new(),
                want: None,
            },
            Row {
                case: "the same claims of ANOTHER provider",
                claims: vec![
                    ("accounts.google.com:sub".to_string(), identity.to_string()),
                    ("accounts.google.com:aud".to_string(), pool.to_string()),
                ],
                want: None,
            },
            Row {
                case: "an empty value for one of them",
                claims: vec![
                    (key(SUBJECT_CLAIM), identity.to_string()),
                    (key(AUDIENCE_CLAIM), String::new()),
                ],
                want: None,
            },
        ];
        for Row { case, claims, want } in rows {
            let got = InvocationIdentity::from_federated_claims(&claims);
            assert_eq!(
                got, want,
                "case {case:?}: expected {want:?}, got {got:?}"
            );
        }
    }

    /// RULE: the document the runtime is handed carries the identity under the member names
    /// the runtime interface uses, and is the JSON null for an invocation made by none.
    #[test]
    fn the_runtime_member_names_the_identity_or_is_null() {
        let identity = InvocationIdentity {
            identity_id: "us-east-1:an-identity".to_string(),
            identity_pool_id: "us-east-1:a-pool".to_string(),
        };
        let document = InvocationIdentity::runtime_member(Some(&identity));
        assert_eq!(
            document["cognitoIdentityId"], "us-east-1:an-identity",
            "the identity is reported under its own member, got {document}"
        );
        assert_eq!(
            document["cognitoIdentityPoolId"], "us-east-1:a-pool",
            "the pool is reported under its own member, got {document}"
        );
        let absent = InvocationIdentity::runtime_member(None);
        assert!(
            absent.is_null(),
            "an invocation made by no identity reports none, got {absent}"
        );
    }
}
