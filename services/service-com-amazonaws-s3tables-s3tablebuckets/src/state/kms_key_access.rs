// SPDX-License-Identifier: CC0-1.0

//! Whether KMS answers the calling principal about the customer managed key an
//! `encryptionConfiguration` names — the decision an operation that SETS such a
//! configuration takes before it writes anything.
//!
//! A configuration naming `aws:kms` is not a label this service only stores and
//! reports back: the key it names is the key S3 Tables encrypts the bucket's
//! tables under, and the model says so — "if you choose SSE-KMS encryption you
//! must grant the S3 Tables maintenance principal access to your KMS key"
//! (`smithy_query(query="doc",shape="PutTableBucketEncryption")`). So the key is
//! RESOLVED first, the way the caller itself would resolve it, and a key no
//! answer comes back for is refused rather than stored: a stored configuration
//! pointing at a key nothing stands behind would read as valid while every write
//! under it failed.
//!
//! The question asked is the one `kms:DescribeKey` answers — does this
//! identifier name a key of this account in this Region — and nothing more. A
//! key's STATE is not read here: a key a customer has disabled is still a key
//! the caller can describe, and what KMS says about using it belongs to the
//! operation that uses it.
//!
//! The key is resolved in the KMS service's OWN store on the shared engine
//! ([`kms_store::ComAmazonawsKmsTrentserviceState::resolve`], the resolution its
//! own handlers take), so this reads exactly what that service's `CreateKey`,
//! `ReplicateKey` and `ScheduleKeyDeletion` wrote and the two services cannot
//! disagree about a key. An engine carrying no KMS store beside this service
//! answers nothing about any key, so nothing is withheld on evidence that is not
//! there.
//!
//! The REFUSAL is KMS's own sentence
//! ([`kms_store::unauthorized_resource_message`]) — the one that service words
//! an authorization failure it can report nothing about the resource with — so
//! the caller reads the same words whether it asked KMS itself or asked this
//! service to. Its only variable part is the principal the request speaks for,
//! which the caller is handed in.
//!
//! LOCKING: this takes the KMS store's guard, and asks for it with NO guard of
//! this service's own store held. The work under it is one map lookup, and no
//! KMS operation reads this service's store.

use ::service_com_amazonaws_kms_trentservice::state as kms_store;

/// The KMS action a service performs to learn whether it may use a key at all:
/// `DescribeKey` "provides detailed information about a KMS key"
/// (`smithy_query(query="doc",shape="DescribeKey")` on the KMS model) and is the
/// read that resolves a key identifier and nothing else.
const KEY_RESOLUTION_ACTION: &str = "DescribeKey";

/// `Some(message)` when KMS resolves no key for `encryption`'s `kmsKeyArn` at
/// `region` — the sentence the refusal carries — and `None` when it does, or
/// when the configuration names no key at all.
///
/// `region` is the Region the request was signed for: a KMS key is a regional
/// resource, so the key an ARN names is the one that Region's endpoint serves.
/// `principal` answers the identity the request speaks for, which is what the
/// refusal names; it is asked only once the configuration is found to name a
/// key, so resolving that identity costs nothing on a configuration this gate
/// has no question about.
///
/// Asked before the operation writes anything, so a refused request leaves the
/// configuration the bucket already carried.
#[must_use]
pub fn refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    principal: impl FnOnce() -> String,
    encryption: &super::BucketEncryption,
) -> Option<String> {
    // Only the algorithm that takes a key carries one, which is the invariant
    // the stored form already holds to: a configuration without one names no
    // key for KMS to answer about.
    let identifier = encryption.kms_key_arn.as_deref()?;
    let kms = kms_store::service_state(engine)?;
    let resolved = matches!(kms.lock().resolve(region, identifier), kms_store::KeyRef::Found(_));
    if resolved {
        return None;
    }
    Some(kms_store::unauthorized_resource_message(
        &principal(),
        KEY_RESOLUTION_ACTION,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a configuration that names no key names nothing for KMS to answer
    /// about, so the gate asks nothing of it — not even who the caller is —
    /// whatever algorithm it carries.
    #[test]
    fn a_configuration_carrying_no_key_is_never_refused() {
        let engine = ::engine::InMemoryEngine::new();
        for algorithm in super::super::SseAlgorithm::ALL {
            let encryption = super::super::BucketEncryption {
                sse_algorithm: *algorithm,
                kms_key_arn: None,
            };
            let refused = refusal(
                &engine,
                "us-east-1",
                || panic!("the principal is resolved only for a configuration naming a key"),
                &encryption,
            );
            assert!(
                refused.is_none(),
                "case {wire:?}: expected no refusal for a configuration with no key, got \
                 {refused:?}",
                wire = algorithm.as_wire()
            );
        }
    }

    /// RULE: the sentence a refusal carries is KMS's own, about the action that
    /// resolves a key, and its only variable part is the principal.
    #[test]
    fn the_refusal_is_kmss_own_sentence_about_resolving_a_key() {
        let engine = ::engine::InMemoryEngine::new();
        // Two principals of the forms a request can speak for: the account's own
        // published user, and an assumed-role session.
        for principal in [
            "arn:aws:iam::000000000000:user/emulator",
            "arn:aws:sts::000000000000:assumed-role/probe-role/probe",
        ] {
            let encryption = super::super::BucketEncryption {
                sse_algorithm: super::super::SseAlgorithm::AwsKms,
                kms_key_arn: Some(
                    "arn:aws:kms:us-east-1:000000000000:key/00000000-0000-0000-0000-000000000000"
                        .to_string(),
                ),
            };
            let refused = refusal(&engine, "us-east-1", || principal.to_string(), &encryption);
            assert_eq!(
                refused.as_deref(),
                Some(
                    kms_store::unauthorized_resource_message(principal, KEY_RESOLUTION_ACTION)
                        .as_str()
                ),
                "case {principal:?}: expected KMS's own sentence, got {refused:?}"
            );
        }
    }
}
