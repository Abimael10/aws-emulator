// SPDX-License-Identifier: CC0-1.0

//! Whether Amazon S3 can have a notification ENCRYPTED for a destination that seals its
//! messages under a customer managed KMS key.
//!
//! The rule: a destination carrying a `KmsMasterKeyId` has every message written to it
//! encrypted under that key, and the destination's own service performs that encryption by
//! asking KMS for a data key as the WRITER. A notification is written by Amazon S3 as its own
//! service principal, on behalf of the publishing bucket, so the KEY's own policy is a second
//! authority over the publication, beside the destination's own resource policy
//! ([`super::notifications`]): a key whose policy admits that principal nothing seals no
//! message, however freely the destination's own policy grants the publish. A notification
//! configuration naming such a destination therefore names one S3 cannot publish to, which is
//! what `PutBucketNotificationConfiguration` refuses the whole container for — and the
//! refusal lifts the moment the key policy grants the principal, with nothing else changed.
//!
//! The decision is KMS's OWN ([`kms_authorization::authorize_use`]) over the key record KMS's
//! own handlers wrote, so `PutKeyPolicy` moves it with nothing else changed and the two
//! services cannot disagree about what a key policy says. It is taken for the use as it really
//! reaches KMS: THROUGH the service that owns the destination (the value of KMS's
//! `kms:ViaService` condition key, which is how a key policy written for an encrypted queue or
//! topic is scoped), for the Amazon S3 service principal acting for the bucket (which is what a
//! grant narrowed by `aws:SourceArn` is decided against), under the operations sealing one
//! message performs ([`SEALING_OPERATIONS`]).
//!
//! Nothing is attributed to a key where nothing of the kind can be shown, so a configuration
//! is never refused on a guess: a destination on no customer managed key, an engine carrying no
//! KMS store to ask, an identifier that resolves to no key record this endpoint holds, and a
//! key Amazon Web Services MANAGES, whose policy AWS writes for the service that owns it.
//!
//! Nothing here is written: the key is resolved as the destination already records it and no
//! alias is materialised, so a validation decided here leaves every later caller seeing exactly
//! what it would have seen.

use ::service_com_amazonaws_kms_trentservice::state as kms_store;
use ::service_com_amazonaws_kms_trentservice::state::authorization as kms_authorization;

/// The KMS operations sealing ONE message under a customer managed key performs: the data key
/// the message is encrypted under is generated, and a data key already in use is decrypted to
/// seal another message under it — which is why the permissions AWS documents for a key that
/// encrypts a destination's messages are `kms:GenerateDataKey*` and `kms:Decrypt`.
///
/// Spelled without KMS's own `kms:` namespace, which is the form
/// [`kms_authorization::RequestedUse`] takes the operation in.
const SEALING_OPERATIONS: &[&str] = &["GenerateDataKey", "Decrypt"];

/// Why the key `master_key_id` names — the one the destination seals its messages under —
/// cannot have a notification of `source_arn` sealed under it for `principal`, or `None` when
/// it can.
///
/// `master_key_id` is the destination's `KmsMasterKeyId` in any form KMS accepts: a key id, a
/// key ARN, an alias or an alias ARN. `namespace` is the ARN namespace of the service that owns
/// the destination (`sqs`, `sns`), whose regional endpoint is the value of KMS's
/// `kms:ViaService` condition key for a use that reaches KMS through it. `region` is where the
/// destination and its key live, and `principal` the service principal Amazon S3 publishes
/// under, acting for the bucket `source_arn` names.
///
/// The answer carries KMS's own sentence for the refusal, which is what an operator reading the
/// log sees beside the destination the configuration named.
///
/// Call it with no store of this service locked: the KMS guard is taken here and released
/// before it answers.
pub fn unsealable_notification<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    master_key_id: &str,
    namespace: &str,
    principal: &str,
    source_arn: &str,
) -> Option<String> {
    let master_key_id = master_key_id.trim();
    if master_key_id.is_empty() {
        return None;
    }
    let Some(kms) = kms_store::service_state(engine) else {
        // Visible to an operator: a lookup nothing answered lets the destination through, so
        // without this line it would be indistinguishable from a key whose policy KMS said
        // admits the publication.
        ::tracing::warn!(
            master_key_id,
            "no KMS service is mounted beside Amazon S3; an encrypted notification \
             destination's key is accepted"
        );
        return None;
    };
    let caller = kms_authorization::Caller::service_principal(principal, source_arn);
    let via_service = format!("{namespace}.{region}.amazonaws.com");
    let store = kms.lock();
    // An identifier that resolves to no key record attributes no condition to a key, so it
    // refuses nothing here: WHICH key a destination may name at all is its own service's
    // subject, and that service's own refusal of the publication is what reports it.
    let kms_store::KeyRef::Found(key) = store.resolve(region, master_key_id) else {
        return None;
    };
    if key.key_manager != kms_store::KEY_MANAGER_CUSTOMER {
        return None;
    }
    // A key no cryptographic operation can use right now seals nothing for anybody, so the
    // notification cannot be encrypted whatever the key's policy says — KMS's own sentence for
    // that state is the reason.
    if !key.is_usable() {
        return Some(kms_store::invalid_state_message(key));
    }
    SEALING_OPERATIONS.iter().find_map(|operation| {
        kms_authorization::authorize_use(
            &caller,
            &store,
            region,
            key,
            kms_authorization::RequestedUse::through_service(operation, &via_service),
            // A sealed message carries no encryption context of the publication's own, so a
            // grant constrained to one does not permit this use: the authority it carries is
            // not claimed rather than assumed.
            None,
            None,
        )
        .err()
    })
}
