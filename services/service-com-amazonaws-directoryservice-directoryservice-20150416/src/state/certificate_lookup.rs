// SPDX-License-Identifier: CC0-1.0

//! RESOLVING THE CERTIFICATE a call names, and which absence that resolution reports.
//!
//! ## The rule
//!
//! An operation whose subject is a registered certificate (`DescribeCertificate`,
//! `DeregisterCertificate`) is given two identifiers, and both can name something the
//! account does not have. The absence it reports is the CERTIFICATE's: a `CertificateId` no
//! registration of this account answers to is refused as the certificate that is not in the
//! system (`CertificateDoesNotExistException`) whatever the directory the request named
//! turns out to be, because the certificate is the subject of the call and a caller
//! branching on the class is told which of the two it has to re-register. The
//! directory-level absence (`DirectoryDoesNotExistException`) is reported only for a request
//! whose certificate the account DOES hold, where the id it was looked for under is the
//! thing that is wrong.
//!
//! So a certificate is resolved in the account's whole inventory before an absent directory
//! is reported, and WITHIN the directory the request names when that directory is there: a
//! registration of another directory is as absent as one nothing registered, since a
//! certificate belongs to the directory it was registered with.
//!
//! One decision for both operations, so an idempotent "remove it if it is there" client and
//! a read cannot be told two different things about the same pair of identifiers.

use super::*;

/// What a certificate the request named could not be resolved to, as the two modelled
/// absences this service reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateAbsence {
    /// No registration of this account answers to the `CertificateId`: the certificate is
    /// not in the system, which is the absence the call reports whatever the directory the
    /// request named is.
    NoSuchCertificate,
    /// The account holds that certificate, and the `DirectoryId` the request looked for it
    /// under names no directory of this account.
    NoSuchDirectory,
}

impl ComAmazonawsDirectoryserviceDirectoryservice20150416State {
    /// The registration `certificate_id` names in the directory `directory_id`, or which
    /// absence the call reports when there is none.
    ///
    /// Reporting only: it reads the store and changes nothing. The scan that decides whether
    /// the account holds the certificate at all is over the directories the account stores,
    /// which is bounded by the Region's own directory quota
    /// ([`DirectoryLimitCategory::quota`]) rather than by anything the request carries.
    pub fn resolve_certificate(
        &self,
        directory_id: &str,
        certificate_id: &str,
    ) -> Result<&CertificateRecord, CertificateAbsence> {
        if let Some(directory) = self.directories.get(directory_id) {
            return directory
                .certificates
                .get(certificate_id)
                // A certificate id the named directory does not hold is not in the system
                // for this call, whether another directory of the account holds one under
                // it or nothing does.
                .ok_or(CertificateAbsence::NoSuchCertificate);
        }
        if self.holds_certificate(certificate_id) {
            return Err(CertificateAbsence::NoSuchDirectory);
        }
        Err(CertificateAbsence::NoSuchCertificate)
    }

    /// Whether ANY directory of this account holds a registration under `certificate_id` —
    /// the account-wide half of the resolution above.
    fn holds_certificate(&self, certificate_id: &str) -> bool {
        self.directories
            .values()
            .any(|directory| directory.certificates.contains_key(certificate_id))
    }
}
