// SPDX-License-Identifier: CC0-1.0

//! The customer managed key an ARCHIVE is configured with, and whether that key's own
//! policy lets this service use it — the check every writer of an archive's
//! `KmsKeyIdentifier` makes before the archive is stored as encrypted under it.
//!
//! "If you do not specify a customer managed key identifier, EventBridge uses an Amazon
//! Web Services owned key to encrypt the archive"
//! (`smithy_query(query="doc",shape="CreateArchiveRequest",member="KmsKeyIdentifier")`),
//! so an archive that DOES name one has everything it keeps encrypted under that key — by
//! EventBridge itself, as its own service principal ([`super::SERVICE_PRINCIPAL`]) acting
//! for that archive. Such a principal is no identity of the account and holds no
//! identity-based policies, so the KEY's own policy is the whole authority over the use: a
//! key whose policy does not admit this service admits it nothing, whatever the
//! configuring caller may do.
//!
//! That is why the configuration is refused rather than stored, by the same reasoning the
//! writers of an event bus's key follow ([`super::bus_encryption`]): an archive stored on a
//! key EventBridge cannot use would be reported as `ENABLED` and encrypted under a key that
//! can encrypt nothing, and the failure would surface on each captured event — or never —
//! instead of at the call that asked for it.
//!
//! The decision is KMS's OWN ([`kms_authorization::authorize_use`]) over the key record
//! that service's handlers wrote, so `PutKeyPolicy` moves it with nothing else changed and
//! the two services cannot disagree about what a key policy says.
//!
//! The emulator refuses only what it can SHOW, so each of these is evidence of nothing and
//! lets the configuration through: an engine carrying no KMS store beside this service, an
//! identifier the KMS store holds no key for — which may name a key of another account or
//! Region — and a key the account does not manage itself, whose policy AWS manages for the
//! service that owns it.

use ::std::collections::HashMap;

use ::service_com_amazonaws_kms_trentservice::state as kms_store;
use ::service_com_amazonaws_kms_trentservice::state::authorization as kms_authorization;

use super::*;

/// The KMS operation EventBridge performs under an archive's customer managed key, and the
/// one a refusal names: sealing what the archive keeps means having a data key generated
/// under that key ("GenerateDataKey returns a unique symmetric data key for use outside of
/// KMS", which is how a service encrypts a payload under a customer managed key).
const ARCHIVE_KEY_OPERATION: &str = "GenerateDataKey";

/// The sentence EventBridge refuses an archive whose key withholds the use with: which KMS
/// call came back denied, for which key, and what the key's policy has to admit for the
/// configuration to be accepted — the service principal this call is made as.
fn denied_message(key_arn: &str) -> String {
    format!(
        "KMS returned an access denied exception when calling the {ARCHIVE_KEY_OPERATION} API \
         for the key with ARN {key_arn}. Please ensure that {SERVICE_PRINCIPAL} has permissions \
         to access {ARCHIVE_KEY_OPERATION}."
    )
}

/// Why the key `identifier` names does not let this service encrypt the archive `archive_arn`
/// under it, as EventBridge words that refusal — or `None` when it does, and for every case
/// the module docs record as evidence of nothing.
///
/// The use is one this service makes for its own resource rather than one the configuring
/// caller asked KMS for, so it is judged FOR this service's own principal acting for the
/// archive ([`kms_authorization::Caller::service_principal`], which is what gives a
/// statement scoped by `aws:SourceArn` or `aws:SourceAccount` its values) and as arriving
/// THROUGH this service ([`kms_authorization::RequestedUse::through_service`], which is what
/// a statement scoped by `kms:ViaService` is written against). What the archive keeps is the
/// events of ONE event bus, so the cryptographic operation carries that bus's own
/// encryption-context entry ([`super::bus_encryption::EVENT_BUS_CONTEXT_ENTRY`]) naming
/// `event_source_arn`: a key policy scoped to the source bus covers the archive of that bus
/// too.
///
/// Call it with NO store of this service locked: the KMS guard is taken here and released
/// before it answers.
pub fn archive_key_denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    identifier: &str,
    archive_arn: &str,
    event_source_arn: &str,
) -> Option<String> {
    let Some(kms) = super::target_encryption::kms_state(engine) else {
        // Visible to an operator: a lookup nothing answered lets the configuration
        // through, so without this line it would be indistinguishable from a key whose
        // policy KMS said admits this service.
        ::tracing::warn!(
            "no KMS service is mounted beside EventBridge; an archive's customer managed key is \
             accepted"
        );
        return None;
    };
    let caller = kms_authorization::Caller::service_principal(SERVICE_PRINCIPAL, archive_arn);
    let via_service = super::target_encryption::via_service(ARN_NAMESPACE, region);
    let context = HashMap::from([(
        super::bus_encryption::EVENT_BUS_CONTEXT_ENTRY.to_string(),
        event_source_arn.to_string(),
    )]);
    let store = kms.lock();
    let kms_store::KeyRef::Found(key) = store.resolve(region, identifier) else {
        return None;
    };
    if key.key_manager != kms_store::KEY_MANAGER_CUSTOMER {
        return None;
    }
    // The decision is what KMS answers; the SENTENCE is this service's own
    // ([`denied_message`]), because the caller asked EventBridge for an archive rather
    // than asking KMS for a data key.
    kms_authorization::authorize_use(
        &caller,
        &store,
        region,
        key,
        kms_authorization::RequestedUse::through_service(ARCHIVE_KEY_OPERATION, &via_service),
        Some(&context),
        None,
    )
    .err()
    .map(|_| denied_message(&key.arn))
}
