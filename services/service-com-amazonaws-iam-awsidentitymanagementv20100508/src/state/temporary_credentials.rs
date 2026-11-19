// SPDX-License-Identifier: CC0-1.0

//! The TEMPORARY credentials this account has minted, and what their CLASS decides.
//!
//! Which principal an access key id speaks for is a question about the account's
//! identities, and this store answers it: a long-term key through
//! [`super::ComAmazonawsIamAwsidentitymanagementv20100508State::access_keys`], a role's
//! session through
//! [`super::ComAmazonawsIamAwsidentitymanagementv20100508State::role_sessions`]. What
//! neither answers is which KIND of credential was presented, and two of the rules AWS
//! states about a temporary credential are about exactly that:
//!
//! * `aws:PrincipalType` names the class of the calling principal — `AssumedRole`,
//!   `FederatedUser` or `User` — which is what a statement conditioned on that key selects
//!   on; and
//! * a credential of some classes may not call IAM at all: "you cannot call any IAM
//!   operations using the Amazon Web Services CLI or the Amazon Web Services API"
//!   (`GetFederationToken`, the STS model), and with a `GetSessionToken` credential "you
//!   cannot call any IAM API operations unless MFA authentication information is included
//!   in the request" (`GetSessionToken`).
//!
//! The class is not a document, so it cannot travel with the session policies a mint
//! passed, and it is not a fact about the identity the credential speaks for either — the
//! same IAM user is behind a long-term key, a `GetSessionToken` session and a
//! `GetFederationToken` session, and the three are not interchangeable. So the operation
//! that MINTS a temporary credential records its class here, beside the role session it
//! records for a role's, and this service's own gate reads it
//! ([`super::authorization`]). The classification itself is the workspace's one
//! ([`::service_common::temporary_credential_classes`]), so the minting service, this gate
//! and every other service that decides such a request name one class alike.

use ::service_common::temporary_credential_classes::TemporaryCredentialClass;

use super::{
    ComAmazonawsIamAwsidentitymanagementv20100508State as IamState, MAX_ROLE_SESSIONS,
};

/// One temporary credential this account has minted, keyed in
/// [`IamState::temporary_credentials`] by the access key id it was minted under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporaryCredentialRecord {
    /// Which class of credential it is, which is what `aws:PrincipalType` answers for a
    /// request it signs and what the per-class bans are stated against.
    pub class: TemporaryCredentialClass,
    /// The ARN of the principal the credential speaks for — the session's own ARN — which
    /// is what a refusal names as the caller.
    pub principal_arn: String,
    /// Whether the MINT of this credential satisfied a multi-factor-authentication
    /// challenge, which is the other half of the `GetSessionToken` ban.
    pub mfa_authenticated: bool,
    /// The instant the credential stops being usable, as the minting operation promised it.
    /// A record past it speaks for nobody: no call can be made with the credential, and the
    /// record is kept only until this store next prunes it.
    pub expires: ::aws_smithy_types::DateTime,
}

impl TemporaryCredentialRecord {
    /// Whether a request signed with this credential may call IAM at all
    /// ([`TemporaryCredentialClass::may_call_iam`], the workspace's one statement of the
    /// per-class ban).
    #[must_use]
    pub fn may_call_iam(&self) -> bool {
        self.class.may_call_iam(self.mfa_authenticated)
    }

    /// What `aws:PrincipalType` answers for a request signed with this credential.
    #[must_use]
    pub fn principal_type(&self) -> &'static str {
        self.class.principal_type()
    }
}

impl IamState {
    /// Record the class of the temporary credential `access_key_id` names, so a request
    /// signed with it is decided as the kind of credential it is.
    ///
    /// Called by the operation that MINTS such a credential, which is the only thing that
    /// knows which class it vended. Recording it again under the same key replaces the
    /// record, because the credential store never hands out an id it is already holding one
    /// for.
    ///
    /// Bounded exactly as [`IamState::record_role_session`] is, and for the same reason: the
    /// records the clock has already expired are pruned first — that credential is withdrawn
    /// at its expiry whether or not anybody called — and at [`MAX_ROLE_SESSIONS`] live
    /// records the one CLOSEST TO EXPIRING gives way, while the credential being recorded is
    /// always recorded, since a credential this store holds no record of is one whose class
    /// no decision can read.
    pub fn record_temporary_credential(
        &mut self,
        access_key_id: &str,
        credential: TemporaryCredentialRecord,
        now: ::aws_smithy_types::DateTime,
    ) {
        self.temporary_credentials
            .retain(|_, held| held.expires.secs() > now.secs());
        while !self.temporary_credentials.contains_key(access_key_id)
            && self.temporary_credentials.len() >= MAX_ROLE_SESSIONS
        {
            let earliest = self
                .temporary_credentials
                .iter()
                .min_by_key(|(key, held)| (held.expires.secs(), (*key).clone()))
                .map(|(key, _)| key.clone());
            match earliest {
                Some(key) => {
                    self.temporary_credentials.remove(&key);
                }
                None => break,
            }
        }
        self.temporary_credentials
            .insert(access_key_id.to_string(), credential);
    }

    /// The temporary credential `access_key_id` names at the instant `now`, or `None` when
    /// this account holds no live record for that key — a long-term access key, the
    /// credential this emulator publishes at start, or a credential whose expiry has passed,
    /// which can make no call at all.
    ///
    /// A READ — the record is left where it is, and the next
    /// [`Self::record_temporary_credential`] prunes it.
    #[must_use]
    pub fn temporary_credential(
        &self,
        access_key_id: &str,
        now: ::aws_smithy_types::DateTime,
    ) -> Option<&TemporaryCredentialRecord> {
        self.temporary_credentials
            .get(access_key_id)
            .filter(|credential| credential.expires.secs() > now.secs())
    }

    /// Give up every temporary credential this account minted for the role at `role_arn`,
    /// and answer the access key ids it gave up.
    ///
    /// "Deleting a role … will immediately invalidate any existing temporary credentials
    /// issued for that role" (IAM User Guide, *Deleting roles or instance profiles*): the
    /// role is the principal those credentials speak for, so once it is gone they speak for
    /// nobody and a call made with one is not to be served by any service. The records this
    /// store holds for them are dropped here, and the caller — the operation that deleted the
    /// role — withdraws the credentials themselves from the emulator's credential store under
    /// the ids answered, which is what stops them being accepted whichever service they are
    /// presented to.
    ///
    /// Recreating a role under the same name does not revive them: the ids are gone from both
    /// this store and the credential store, so there is nothing left to resolve.
    pub fn withdraw_role_credentials(&mut self, role_arn: &str) -> Vec<String> {
        let withdrawn: Vec<String> = self
            .role_sessions
            .iter()
            .filter(|(_, session)| session.role_arn == role_arn)
            .map(|(access_key_id, _)| access_key_id.clone())
            .collect();
        for access_key_id in &withdrawn {
            self.role_sessions.remove(access_key_id);
            self.temporary_credentials.remove(access_key_id);
        }
        withdrawn
    }
}
