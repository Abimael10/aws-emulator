// SPDX-License-Identifier: CC0-1.0

//! What a service-specific credential is minted with: the identifier the account reports
//! it under, and the secret half a create generates and a reset replaces.
//!
//! The two halves come from two different places, for two different reasons. The
//! IDENTIFIER is an IAM resource identifier like a user's `UserId` or an SSH public key's
//! `SSHPublicKeyId`, so it is minted by the account itself, from the account's own counter
//! and the resource type's published prefix, exactly as every other identifier this
//! service generates is. The SECRET is credential material, which nothing in a service may
//! construct, so it comes from the one component that generates it.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::{unique_id, ComAmazonawsIamAwsidentitymanagementv20100508State as IamState};

/// The prefix AWS gives a service-specific credential's identifier — the `ACCA` prefix the
/// "IAM identifiers" page of the IAM User Guide lists for a service-specific credential
/// under "unique ID prefixes", beside the `AIDA`, `AROA`, `AGPA`, `AIPA`, `ANPA`, `ASCA`
/// and `APKA` prefixes this account already gives the identifiers of its other resource
/// types. `ServiceSpecificCredential$ServiceSpecificCredentialId` is modelled as
/// `serviceSpecificCredentialId` (len 20..128, pattern `^[\w]+$`,
/// `smithy_query(query="shape", shape="serviceSpecificCredentialId")`), which this prefix
/// plus the account's unique-id suffix satisfies: 21 uppercase alphanumerics.
const SERVICE_SPECIFIC_CREDENTIAL_ID_PREFIX: &str = "ACCA";

impl IamState {
    /// Mint the next service-specific credential's `ServiceSpecificCredentialId`.
    ///
    /// Unique and stable for the same reason every other minted id of this account is: the
    /// counter is bijectively mixed into the suffix, so no two credentials the account ever
    /// mints share an id, and the id is minted once and then stored with the credential, so
    /// "the unique identifier for the service-specific credential"
    /// (`ServiceSpecificCredential$ServiceSpecificCredentialId`) is the handle every later
    /// read, reset, status update and delete of that credential takes.
    pub fn next_service_specific_credential_id(&mut self) -> String {
        self.service_specific_credential_id_seq += 1;
        unique_id(
            SERVICE_SPECIFIC_CREDENTIAL_ID_PREFIX,
            self.service_specific_credential_id_seq,
        )
    }
}

/// A fresh secret for a service-specific credential: the `ServicePassword` of a
/// user-name-and-password credential, or the `ServiceCredentialSecret` of a long-term API
/// key.
///
/// A service-specific credential authenticates its IAM user to ONE other Amazon Web
/// Services service — CodeCommit over HTTPS Git, Amazon Keyspaces over CQL — and never to
/// this emulator over SigV4, so the pair must not stay in the emulator's credential store.
/// Nothing in a service may construct a secret itself, though, so it is minted by the one
/// component that generates them, the credential store, and the record is then removed from
/// it again — the shape [`super::mint_virtual_mfa_device_seed`] uses for the same reason.
/// The identifier that comes back with it is NOT the credential's id: an id is an IAM
/// resource identifier, minted by
/// [`IamState::next_service_specific_credential_id`] so it carries the resource type's
/// published prefix.
///
/// `None` when the store minted no secret, which leaves the caller to refuse rather than
/// report a credential it made up.
pub fn mint_service_specific_credential_secret(
    store: &::emulator_server::CredentialStore,
) -> Option<String> {
    let minted = store.create(&::emulator_server::aws_credentials::AWS_ACCESS_KEY);
    let secret = minted
        .secret(::emulator_server::aws_credentials::SECRET_ACCESS_KEY)
        .map(str::to_string);
    // The pair is not a credential of this emulator; see above.
    let _ = store.delete(&minted.id);
    secret
}
