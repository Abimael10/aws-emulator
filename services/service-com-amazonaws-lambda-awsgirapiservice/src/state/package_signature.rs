// SPDX-License-Identifier: CC0-1.0

//! WHO signed a deployment package: the signature the package itself carries, verified
//! against the service that produced it.
//!
//! A code signature travels with the artifact, not with the call that deploys it: the signing
//! service writes it into the .zip package it signs ([`::service_common::code_signing_signature`]),
//! and `ZipFile` or the Amazon S3 object a deployment names carries those very bytes. So what
//! a deployment to a function attests to is read out of the package in front of this
//! service, and nothing in the request has to name a publisher.
//!
//! Reading it is not the same as believing it. The envelope STATES a signing profile version
//! and carries a public key, and anyone can write both; what makes the attribution good is
//! that the signature verifies under the key pair the SIGNING SERVICE holds for that profile
//! version, which only that service can answer for. It is asked through the signing service's
//! own store in the SAME shared engine — the seam [`super::image_source`] resolves a
//! container image through — so a deployment is attributed to a publisher only when the
//! service that signs for that publisher confirms the signature is one of its own.
//!
//! Three things therefore leave a package unattributed, and a deployment control decides what
//! to do about each of them alike ([`super::code_signing`]): a package bearing no signature at
//! all, one whose signature does not cover what it travels with (contents changed after they
//! were signed, an attribution rewritten after signing, an envelope lifted off another
//! artifact), and one whose signature does not verify under the profile version it names
//! (another publisher's key, a profile version this emulator's signing service does not hold).

/// The signing service's store, where the key pair of each signing profile version lives:
/// AWS Signer's OWN state struct, in the shared engine.
pub type SignerState =
    ::service_com_amazonaws_signer_wallabyservice::ComAmazonawsSignerWallabyserviceState;

/// The signing service's store in the SAME shared engine. `None` means the engine carries no
/// such store — no service to confirm a signature with, so no package can be attributed to a
/// publisher.
fn signer_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<SignerState>> {
    ::service_com_amazonaws_signer_wallabyservice::state::service_state(engine)
}

/// WHO a deployment package's own signature attributes it to, once that signature has been
/// verified.
///
/// The two members a signed function reports: `SigningProfileVersionArn`, "The ARN of the
/// signing profile version", and `SigningJobArn`, "The ARN of the signing job".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentSignature {
    /// The signing profile version the signature was made under — the publisher a
    /// code-signing configuration's `AllowedPublishers` is checked against.
    pub profile_version_arn: String,
    /// The ARN of the signing job that made the signature, as the signing service's own
    /// record of that job names it.
    ///
    /// `None` when that service holds no such job any more: the signature still verifies, so
    /// the publisher is known, while a job ARN is reported only for a job the service that
    /// owns it still answers for.
    pub signing_job_arn: Option<String>,
}

/// How long the signature work of ONE deployment may take before this emulator stops waiting
/// for it.
///
/// A chosen bound, in the shape the signing service bounds the same kind of work in: reading
/// and digesting up to the ceiling [`::service_common::code_signing_signature`] holds caller
/// content to is bounded work, but it is work a REQUEST sized, so it is given a wall-clock
/// ceiling as well — and it runs on a blocking worker ([`verified_package_signature`]), so
/// this ceiling is something the runtime can actually land on.
const SIGNATURE_WORK_TIMEOUT: ::std::time::Duration = ::std::time::Duration::from_secs(10);

/// The publisher the package `package` carries a VERIFIED signature from, or `None` when it
/// carries none.
///
/// The one reading of a deployment package's signature, shared by every operation that
/// deploys one: the signature is read out of the package, checked to cover the package's own
/// contents, and then verified under the key pair the signing service holds for the profile
/// version it names. `None` is a package attributed to nobody — see the module header for the
/// three ways that happens, plus the three that show nothing at all: a deployment whose bytes
/// this emulator does not hold (a package left in Amazon S3, a container image), an engine
/// carrying no signing service to ask, and work that did not finish inside
/// [`SIGNATURE_WORK_TIMEOUT`].
///
/// Reading and digesting the archive is CPU-bound work with no I/O to await in it, so it is
/// handed to a blocking worker rather than run on the thread serving the request: the
/// request's own await point stays cancellable, and a shutdown, a timeout and the emulator's
/// health endpoint keep landing while the work runs. The call is therefore made BEFORE this
/// service's store is reached, like every other call this crate makes out of a handler: no
/// store guard may be held across one.
pub async fn verified_package_signature<Eng: Send + Sync + 'static>(
    engine: &Eng,
    package: Option<&[u8]>,
) -> Option<DeploymentSignature> {
    let signature = package_signature_off_thread(package?.to_vec()).await?;
    let (region, _, _) = ::service_com_amazonaws_signer_wallabyservice::state::profile_version_in_arn(
        &signature.profile_version_arn,
    )?;
    let state = signer_state(engine)?;
    // ONE constant-size check under the signing service's guard: the signature is verified
    // over the 48-byte digest material read above, and no key material leaves that store.
    let store = state.lock();
    if !store.signature_made_under_profile_version(
        &signature.profile_version_arn,
        &signature.signed_material,
        &signature.signature,
    ) {
        return None;
    }
    Some(DeploymentSignature {
        profile_version_arn: signature.profile_version_arn.clone(),
        // Read off the signing service's own record of the job, so the ARN reported names a
        // job that service still answers for rather than one built from the envelope's id.
        signing_job_arn: store
            .job(region, &signature.job_id)
            .map(::service_com_amazonaws_signer_wallabyservice::state::SigningJobRecord::arn),
    })
}

/// The signature `package` carries, read on a blocking worker and under
/// [`SIGNATURE_WORK_TIMEOUT`].
///
/// `None` for a package bearing no readable signature at all, and for work the worker did not
/// finish — a package nothing could be shown about either way.
async fn package_signature_off_thread(
    package: Vec<u8>,
) -> Option<::service_common::code_signing_signature::PackageSignature> {
    let work = ::tokio::task::spawn_blocking(move || {
        ::service_common::code_signing_signature::package_signature(&package)
    });
    match ::tokio::time::timeout(SIGNATURE_WORK_TIMEOUT, work).await {
        Ok(Ok(signature)) => signature,
        // The worker did not finish — it panicked or was cancelled — or the work outran the
        // ceiling: either way this emulator has established nothing about the package.
        Ok(Err(_)) | Err(_) => None,
    }
}
