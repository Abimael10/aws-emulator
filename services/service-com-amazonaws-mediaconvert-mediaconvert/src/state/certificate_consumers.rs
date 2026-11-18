// SPDX-License-Identifier: CC0-1.0

//! Which Certificate Manager certificates this account's MediaConvert service is using, as
//! the service that HOLDS those certificates has to see it.
//!
//! `AssociateCertificate` "associates an AWS Certificate Manager (ACM) Amazon Resource Name
//! (ARN) with AWS Elemental MediaConvert", so after the call the certificate really is
//! referenced by something outside Certificate Manager — and that service reports the
//! resources using a certificate (`CertificateDetail$InUseBy`) and protects one while any of
//! them does. It cannot see this store, so the fact has to be written where it can read it:
//! the ONE shared directory
//! ([`::service_common::acm_certificate_associations::CertificateAssociationDirectory`]),
//! which the engine keys by TYPE, so a copy of its own in this service's state would be
//! invisible to the reader.
//!
//! THE CONSUMER IS THE ACCOUNT'S SERVICE, not a resource of it: nothing this service holds
//! names one association — `AssociateCertificate` takes the certificate's ARN and no
//! identifier at all, and what it changes is the account's own set of trusted certificates
//! ([`State::certificates`]) — so the resource an ACM report names here is this service
//! itself in the Region the association was made for ([`service_resource_arn`]).
//!
//! One call rewrites that whole set ([`record_certificate_consumers`]), so an association
//! added or removed leaves exactly the references the store still holds.
//!
//! LOCK ORDER: the caller holds this service's own store and this module takes the directory
//! under it, never the other way round, so two services cannot invert the order between
//! them.

use std::time::{Duration, SystemTime};

use super::{ComAmazonawsMediaconvertMediaconvertState as State, ARN_NAMESPACE};

/// The resource id the ARN of this service itself carries: the account's MediaConvert
/// service in a Region is addressed as `arn:<partition>:mediaconvert:<region>:<account>:
/// service`, with no resource name after it, because the account has one — this service is
/// what holds the certificate trust rather than any named queue, preset or job template of
/// it.
const SERVICE_RESOURCE: &str = "service";

/// The ARN that names the consumer of every certificate this account has associated: this
/// service in `region`, in the account and partition every ARN this emulator mints carries
/// ([`::engine::emulator_arn`]).
pub fn service_resource_arn(region: &str) -> String {
    ::engine::emulator_arn(ARN_NAMESPACE, region, SERVICE_RESOURCE)
}

/// How long a reference this service gives up stays accounted for: NO time at all.
///
/// A certificate is given up by `DisassociateCertificate`, and what that call changes is the
/// account's own set of trusted certificate ARNs — the whole of the association, which
/// [`State::disassociate_certificate`] removes inside the call. There is no endpoint left
/// presenting the certificate and no work outstanding after it, so the record follows the set
/// this store holds rather than holding a reference the service itself no longer has.
const RELEASE_IS_IMMEDIATE: Duration = Duration::ZERO;

/// The SHARED certificate-association directory in the shared engine: call it with a
/// handler's `&*self.engine`.
///
/// Answer `None` by leaving the directory unwritten: the record is an aid to the service that
/// holds the certificate, never part of this operation's own answer, so an engine carrying no
/// directory still serves the call.
pub fn certificate_association_directory<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<
    ::engine::ServiceState<
        ::service_common::acm_certificate_associations::CertificateAssociationDirectory,
    >,
> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(
            ::engine::InMemoryEngine::state::<
                ::service_common::acm_certificate_associations::CertificateAssociationDirectory,
            >,
        )
}

/// Record in the shared directory exactly which certificates this account's MediaConvert
/// service is using, as `store` now holds them.
///
/// Called by the two operations that change that set, with this service's own store already
/// locked and holding the result of the change. The directory entry is REPLACED, so a
/// certificate the store no longer holds cannot survive the call.
///
/// The entry is held to the directory's own per-resource ceiling
/// (`MAX_CERTIFICATES_PER_RESOURCE`), which is below the number of certificates one account may
/// associate ([`super::MAX_CERTIFICATES`]): past that ceiling the directory accounts for the
/// first of them, so a certificate an account with more associations than that holds may be
/// reported as used by nothing. The ceiling is the shared directory's, and bounding it is what
/// keeps one caller from growing it without end.
pub fn record_certificate_consumers<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &State,
    region: &str,
    now: SystemTime,
) {
    let Some(directory) = certificate_association_directory(engine) else {
        return;
    };
    directory.lock().record_resource(
        &service_resource_arn(region),
        store.certificates.iter(),
        now,
        RELEASE_IS_IMMEDIATE,
    );
}
