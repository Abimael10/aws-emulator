// SPDX-License-Identifier: CC0-1.0

//! The TRANSFER a device certificate is in: the record the source account holds while a
//! transfer is pending, what the account's own state refuses a transfer for, and the two
//! writes that start one and take it back.
//!
//! "Transfers the specified certificate to the specified Amazon Web Services account. ...
//! You can cancel the transfer until it is accepted by the recipient"
//! (`TransferCertificate`), so a transfer is a STATE the certificate is in and not an
//! event: the certificate stays in the source account's store, carrying the account it is
//! on its way to and the instant the transfer started, until the transfer ends. That is
//! why the record lives on [`super::RegisteredCertificate`] — one certificate, one
//! transfer state — rather than in a second map beside the certificate store.
//!
//! The preconditions are the two sentences the operation states: "The certificate being
//! transferred must not be in the ACTIVE state. You can use the UpdateCertificate action
//! to deactivate it. The certificate must not have any policies attached to it. You can
//! use the DetachPolicy action to detach them." Each is read from the account's own state
//! — the certificate's status, and the policy-attachment relation
//! ([`super::policy_attachments`]) — so a refusal names what the caller has to clear
//! rather than guessing.
//!
//! Taking a transfer back is the reverse write: "After a certificate transfer is
//! cancelled, the status of the certificate changes from PENDING_TRANSFER to INACTIVE"
//! (`CancelCertificateTransfer`), which is exactly what
//! [`super::ComAmazonawsIotAwsiotserviceState::cancel_certificate_transfer`] leaves, and a
//! certificate that is in no transfer has nothing to cancel.

use std::time::SystemTime;

use super::{
    missing_certificate_message, RegisteredCertificate, CERTIFICATE_STATUS_ACTIVE,
    CERTIFICATE_STATUS_INACTIVE, CERTIFICATE_STATUS_PENDING_TRANSFER,
};

/// The transfer a certificate is in, as the account that started it holds it.
///
/// It is the record behind the certificate's `transferData` — "Data used to transfer a
/// certificate to an Amazon Web Services account"
/// (`smithy_query(query="shape",shape="TransferData")`) — and behind every listing of the
/// certificates an account has transferred out, whose rows carry the account the transfer
/// was made to and the date it was initiated
/// (`smithy_query(query="shape",shape="OutgoingCertificate")`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateTransfer {
    /// "The Amazon Web Services account" the transfer is to
    /// (`TransferCertificate$targetAwsAccount`), modelled `length 12..12, pattern
    /// ^[0-9]+$`.
    pub target_aws_account: String,
    /// "The transfer message" the request carried (`TransferCertificate$transferMessage`),
    /// and `None` for a request that carried none: the member is optional, so a transfer
    /// started without one has no message to report rather than an empty one.
    pub transfer_message: Option<String>,
    /// "The date the transfer took place" (`TransferData$transferDate`) — the instant the
    /// transfer was started, which is also "the date the transfer was initiated" an
    /// outgoing-certificate row reports.
    pub transfer_date: SystemTime,
}

/// Why a certificate was NOT transferred.
///
/// Each variant names the modelled error class the refusal answers with, so the rules
/// `TransferCertificate` follows are stated once, beside the record the transfer writes,
/// rather than in the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateTransferRefusal {
    /// An id the account holds no certificate under: `ResourceNotFoundException`
    /// (HTTP 404).
    NotFound(String),
    /// A certificate whose STATUS the operation does not allow a transfer from — one that
    /// is [`CERTIFICATE_STATUS_ACTIVE`], or one already on its way to another account:
    /// `CertificateStateException` (HTTP 406), "The certificate operation is not allowed".
    State(String),
    /// A certificate that still holds attached policies: `TransferConflictException`
    /// (HTTP 409), "You can't transfer the certificate because authorization policies are
    /// still attached".
    AttachedPolicies(String),
}

impl CertificateTransferRefusal {
    /// The message the refusal states, whichever class it names.
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(message) | Self::State(message) | Self::AttachedPolicies(message) => {
                message
            }
        }
    }
}

/// Why a certificate transfer was NOT cancelled.
///
/// Both reasons are reported the same way, because what the call resolves is the PENDING
/// TRANSFER of the certificate the id names rather than the certificate: an id with no
/// pending transfer under it names nothing to cancel, whether or not the account holds a
/// certificate under it. They stay two variants so the record of what the call did says
/// which of the two it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateTransferCancellationRefusal {
    /// An id the account holds no certificate under: `ResourceNotFoundException`
    /// (HTTP 404).
    NotFound(String),
    /// A certificate that is in no pending transfer, so there is nothing to cancel:
    /// `ResourceNotFoundException` (HTTP 404) too, worded after the id the request
    /// carried, exactly as an id the account holds no certificate under is.
    NotInTransfer(String),
}

impl CertificateTransferCancellationRefusal {
    /// The message the refusal states, whichever class it names.
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(message) | Self::NotInTransfer(message) => message,
        }
    }
}

/// The message a transfer refuses a certificate whose status is
/// [`CERTIFICATE_STATUS_ACTIVE`] with.
///
/// The precondition is the operation's own — "The certificate being transferred must not
/// be in the ACTIVE state. You can use the UpdateCertificate action to deactivate it" — so
/// the refusal is worded after the state the certificate has to be in and names the
/// certificate by the id the request carried, the way AWS IoT words the same precondition
/// on the other operation that requires it ([`super::active_certificate_deletion_message`]),
/// because the caller's next step is `UpdateCertificate` on that id.
pub fn active_certificate_transfer_message(certificate_id: &str) -> String {
    format!("Certificate must be deactivated (not ACTIVE) before transfer. Id: {certificate_id}")
}

/// The message a transfer refuses a certificate that is ALREADY on its way to another
/// account with.
///
/// A certificate in [`CERTIFICATE_STATUS_PENDING_TRANSFER`] is one whose transfer the
/// recipient has neither accepted nor rejected, and the certificate can only be on its way
/// to one account, so the second transfer is refused and the refusal names the state the
/// certificate is in and the id the request carried — the id the caller cancels the
/// pending transfer by.
pub fn transfer_in_progress_message(certificate_id: &str) -> String {
    format!("Certificate transfer is already in progress. Id: {certificate_id}")
}

/// The message a transfer refuses a certificate that still holds attached policies with.
///
/// The precondition is the operation's own — "The certificate must not have any policies
/// attached to it. You can use the DetachPolicy action to detach them" — so the refusal is
/// worded after the relation that holds the certificate back and names the certificate by
/// its ARN, the way AWS IoT words the same conflict on a delete
/// ([`super::attached_policies_deletion_message`]), because `DetachPolicy` takes the
/// certificate as a target ARN rather than as an id.
pub fn attached_policies_transfer_message(certificate_arn: &str) -> String {
    format!("Certificate policies must be detached before transfer (arn: {certificate_arn})")
}

/// The message a cancellation refuses a certificate that is in NO pending transfer with.
///
/// `CancelCertificateTransfer` "Cancels a pending transfer for the specified certificate",
/// so what the call resolves is that pending transfer: an id with none under it names
/// nothing, and AWS IoT says so with the sentence it states for a `certificateId` that
/// resolves to nothing at all ([`super::missing_certificate_message`]) — one sentence for
/// one class of refusal, naming the id the request carried.
pub fn transfer_not_in_progress_message(certificate_id: &str) -> String {
    missing_certificate_message(certificate_id)
}

impl RegisteredCertificate {
    /// This record as the `OutgoingCertificate` a listing of the certificates this account
    /// has transferred out reports, and `None` for a certificate that is in no transfer.
    ///
    /// "A certificate that has been transferred but not yet accepted"
    /// (`smithy_query(query="shape",shape="OutgoingCertificate")`) is exactly a record
    /// carrying a pending [`CertificateTransfer`], so the transfer decides whether there is
    /// a row at all: the account the transfer "was made to" and the date it "was initiated"
    /// come from that record, the identity and the creation date from the certificate, and
    /// the transfer message only from a transfer that carried one.
    ///
    /// One function, for the same reason [`RegisteredCertificate::description`] is one: the
    /// listing and the description of one certificate cannot report the same transfer
    /// differently.
    pub fn outgoing_certificate(&self) -> Option<crate::ops::types::OutgoingCertificate> {
        let transfer = self.transfer.as_ref()?;
        Some(crate::ops::types::OutgoingCertificate {
            certificate_arn: Some(self.certificate_arn.clone()),
            certificate_id: Some(self.certificate_id.clone()),
            transferred_to: Some(transfer.target_aws_account.clone()),
            transfer_date: Some(::aws_smithy_types::DateTime::from(transfer.transfer_date)),
            transfer_message: transfer.transfer_message.clone(),
            creation_date: Some(::aws_smithy_types::DateTime::from(self.creation_date)),
        })
    }
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// Start the transfer of the certificate recorded under `certificate_id` to
    /// `target_aws_account`, answering the record as it stands afterwards, or the reason
    /// the account's state held the transfer back.
    ///
    /// The certificate stays in this account's store: a transfer is pending until the
    /// recipient accepts or rejects it, and until then the certificate carries
    /// [`CERTIFICATE_STATUS_PENDING_TRANSFER`] — the status "used internally by IoT"
    /// ([`super::INTERNAL_CERTIFICATE_STATUSES`]), which is why this write and not a
    /// caller's `UpdateCertificate` is what sets it — and the [`CertificateTransfer`] the
    /// request named. `now` is the instant the transfer started, which is what the
    /// certificate's transfer state reports as its transfer date and what stamps the
    /// record as last modified.
    ///
    /// Both documented preconditions are read from the account's own state before anything
    /// is written, so a refused transfer leaves the certificate exactly as it was: the
    /// status first (an ACTIVE certificate, or one already on its way to another account),
    /// then the policies attached to it, so a caller clearing the refusals reaches them in
    /// the order the operation's documentation states them.
    ///
    /// `transfer_message` is checked against the modelled `Message` constraints before a
    /// handler is reached, so there is nothing left to refuse about it here.
    pub fn transfer_certificate(
        &mut self,
        certificate_id: &str,
        target_aws_account: &str,
        transfer_message: Option<&str>,
        now: SystemTime,
    ) -> Result<RegisteredCertificate, CertificateTransferRefusal> {
        // The policy-attachment relation is read FIRST, into the one fact the refusal
        // needs, so the certificate itself is reached exactly once, mutably: the attached
        // policies are a relation beside the certificate ([`super::PolicyTarget::key`]) and
        // reading them cannot be done while the record is borrowed. The ORDER the refusals
        // are reported in is unchanged — the status first, then the policies, as the
        // operation's documentation states them.
        let target_key = super::PolicyTarget::Certificate(certificate_id.to_string()).key();
        let has_attached_policies = !self.policy_attachments.policies_of(&target_key).is_empty();

        let Some(certificate) = self.certificates.get_mut(certificate_id) else {
            return Err(CertificateTransferRefusal::NotFound(
                missing_certificate_message(certificate_id),
            ));
        };
        if certificate.status == CERTIFICATE_STATUS_ACTIVE {
            return Err(CertificateTransferRefusal::State(
                active_certificate_transfer_message(certificate_id),
            ));
        }
        if certificate.status == CERTIFICATE_STATUS_PENDING_TRANSFER {
            return Err(CertificateTransferRefusal::State(
                transfer_in_progress_message(certificate_id),
            ));
        }
        if has_attached_policies {
            return Err(CertificateTransferRefusal::AttachedPolicies(
                attached_policies_transfer_message(&certificate.certificate_arn),
            ));
        }

        certificate.status = CERTIFICATE_STATUS_PENDING_TRANSFER.to_string();
        certificate.transfer = Some(CertificateTransfer {
            target_aws_account: target_aws_account.to_string(),
            transfer_message: transfer_message.map(str::to_string),
            transfer_date: now,
        });
        certificate.last_modified_date = now;
        Ok(certificate.clone())
    }

    /// Cancel the pending transfer of the certificate recorded under `certificate_id`,
    /// answering the record as it stands afterwards, or the reason the cancellation was
    /// refused.
    ///
    /// "After a certificate transfer is cancelled, the status of the certificate changes
    /// from PENDING_TRANSFER to INACTIVE" (`CancelCertificateTransfer`), so the
    /// certificate the source account keeps is [`CERTIFICATE_STATUS_INACTIVE`] whatever
    /// status it carried before the transfer started, and the transfer state it reported
    /// while the transfer was pending is gone: there is no transfer left to describe.
    /// `now` stamps the record as last modified, because the cancellation is a change to
    /// the certificate.
    ///
    /// A certificate in no pending transfer is refused rather than quietly deactivated:
    /// "Cancels a pending transfer for the specified certificate" is the whole contract,
    /// so a call that names a certificate with no pending transfer changes nothing and is
    /// reported the way an id that resolves to nothing is
    /// ([`transfer_not_in_progress_message`]).
    pub fn cancel_certificate_transfer(
        &mut self,
        certificate_id: &str,
        now: SystemTime,
    ) -> Result<RegisteredCertificate, CertificateTransferCancellationRefusal> {
        let Some(certificate) = self.certificates.get_mut(certificate_id) else {
            return Err(CertificateTransferCancellationRefusal::NotFound(
                missing_certificate_message(certificate_id),
            ));
        };
        if certificate.status != CERTIFICATE_STATUS_PENDING_TRANSFER
            || certificate.transfer.is_none()
        {
            return Err(CertificateTransferCancellationRefusal::NotInTransfer(
                transfer_not_in_progress_message(certificate_id),
            ));
        }
        certificate.status = CERTIFICATE_STATUS_INACTIVE.to_string();
        certificate.transfer = None;
        certificate.last_modified_date = now;
        Ok(certificate.clone())
    }
}
