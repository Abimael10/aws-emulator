// SPDX-License-Identifier: CC0-1.0

//! The `/responder-gateway/{gatewayId}/certificate` surface's state: which ACM
//! certificates a stored responder gateway carries, and where each one is in its
//! association lifecycle.
//!
//! `AssociateCertificate`, `GetCertificateAssociation` and `DisassociateCertificate`
//! are three operations on one surface, so the record they share — and the one
//! statement of the precondition the surface enforces — lives here rather than three
//! times: the write, the read and the removal then answer about exactly the same set
//! of associations.

use std::time::SystemTime;

/// `CertificateAssociationStatus` member: the association is accepted, the
/// certificate is not serving yet.
pub const CERTIFICATE_STATUS_PENDING_ASSOCIATION: &str = "PENDING_ASSOCIATION";
/// `CertificateAssociationStatus` member: the certificate is serving on the gateway.
pub const CERTIFICATE_STATUS_ASSOCIATED: &str = "ASSOCIATED";
/// `CertificateAssociationStatus` member: the removal is accepted, the certificate is
/// still serving.
pub const CERTIFICATE_STATUS_PENDING_DISASSOCIATION: &str = "PENDING_DISASSOCIATION";
/// `CertificateAssociationStatus` member: terminal, the certificate no longer serves
/// on the gateway.
pub const CERTIFICATE_STATUS_DISASSOCIATED: &str = "DISASSOCIATED";

/// What a gateway whose type does not admit a certificate is refused with.
///
/// Fixed text: it states the precondition and takes nothing from the request or from
/// state, which is how the service words this refusal.
pub const CERTIFICATE_EXTERNAL_ONLY_MESSAGE: &str =
    "Certificate association is only supported for EXTERNAL gatewayType";

/// How many certificate associations one responder gateway holds before
/// `AssociateCertificate` refuses another with the modelled
/// `ServiceQuotaExceededException` (HTTP 402).
///
/// AWS publishes no per-gateway certificate limit, so this is the emulator's OWN
/// bound on the store rather than a claim about any account's quota: it sits far above
/// any workflow an emulator serves, and it is what stops a caller growing one
/// gateway's association set without end.
pub const CERTIFICATE_ASSOCIATIONS_PER_GATEWAY_QUOTA: usize = 25;

/// One ACM certificate's association with one responder gateway, as
/// `AssociateCertificate` persisted it.
///
/// Keyed in the store by (`gatewayId`, `acmCertificateArn`): the pair IS the
/// association's identity — the operations mint no id of their own — so a retry of the
/// same association addresses this record rather than making a second one.
#[derive(Debug, Clone)]
pub struct CertificateAssociation {
    pub gateway_id: String,
    pub acm_certificate_arn: String,
    pub status: String,
    /// The instant the clock makes the pending transition due; `None` once the record
    /// has settled.
    pub settles_at: Option<SystemTime>,
    /// When the association that is now in force was asked for.
    pub associated_at: SystemTime,
    pub updated_at: SystemTime,
}

impl CertificateAssociation {
    /// Materialise the transition the clock has already made due — an association
    /// becomes `ASSOCIATED`, a removal becomes `DISASSOCIATED` — dated at the instant
    /// it was owed rather than now, so the record does not depend on when it was next
    /// read.
    pub fn settle(&mut self, now: SystemTime) {
        let Some(due) = self.settles_at.filter(|due| *due <= now) else {
            return;
        };
        let settled = match self.status.as_str() {
            CERTIFICATE_STATUS_PENDING_ASSOCIATION => CERTIFICATE_STATUS_ASSOCIATED,
            CERTIFICATE_STATUS_PENDING_DISASSOCIATION => CERTIFICATE_STATUS_DISASSOCIATED,
            // A status with nothing pending on it keeps whatever it holds.
            settled => settled,
        };
        self.status = settled.to_string();
        self.settles_at = None;
        self.updated_at = due;
    }

    /// Whether the association this record holds is the one a caller asking to
    /// associate the certificate asks for: it is accepted and settling in, or it is
    /// in place. A repeat of the association answers such a record as it stands, and
    /// a removal of it is the removal of an association that is there to remove.
    ///
    /// A record whose removal has been accepted is NOT one of these: the certificate
    /// is still serving, but the association the store holds is on its way out, which
    /// is what [`disassociation_pending`](Self::disassociation_pending) answers for.
    pub fn association_in_force(&self) -> bool {
        self.status == CERTIFICATE_STATUS_PENDING_ASSOCIATION
            || self.status == CERTIFICATE_STATUS_ASSOCIATED
    }

    /// Whether this association's removal is already outstanding: the service has
    /// accepted it and the clock has not completed it yet, so the record still holds
    /// the (gateway, certificate) pair's identity and no call may write a new
    /// association over it.
    pub fn disassociation_pending(&self) -> bool {
        self.status == CERTIFICATE_STATUS_PENDING_DISASSOCIATION
    }

    /// The words a refusal carries when this association's current status does not
    /// admit the transition a call asks for: the pair the call addressed and the
    /// status that refuses it, both from the record. Worded exactly as the gateway
    /// and link records word the same refusal.
    pub fn status_conflict_message(&self, transition: &str) -> String {
        format!(
            "certificate {} on responder gateway {} is {} and cannot be {}",
            self.acm_certificate_arn, self.gateway_id, self.status, transition
        )
    }
}
