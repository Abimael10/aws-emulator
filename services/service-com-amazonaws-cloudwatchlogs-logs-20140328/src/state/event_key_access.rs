// SPDX-License-Identifier: CC0-1.0

//! Whether the customer managed key a STORED log event was ingested under can still serve
//! it — the question every read of a log group's events is answered with.
//!
//! A log group associated with a customer managed key has its stored log data encrypted with
//! that key: "CloudWatch Logs supports encrypting your log data using a KMS key", and
//! `AssociateKmsKey` is what binds the key to the data the group takes in from then on, while
//! leaving what is already stored as it is ("it does not affect any of the data that was
//! already encrypted"). So the key an event is held under is the one its group was associated
//! with when the event ARRIVED, which is why the binding is recorded per event
//! ([`super::StoredEvent::ingest_kms_key_id`]) rather than per group.
//!
//! Serving such an event means decrypting it with that key, which CloudWatch Logs can do only
//! while KMS lets it: a key KMS reports in any state but enabled cannot be used
//! ([`super::kms_key_reference`]), and a key whose own key policy no longer grants CloudWatch
//! Logs the encryption operations for that log group cannot either
//! ([`super::kms_key_grant`]) — the same two questions an association is judged by, asked
//! again at read time because either answer can change after the association. An event the
//! key cannot serve is WITHHELD from the read rather than refusing it: the call is a read of
//! the log stream, which still exists, so it answers with the events it could serve and
//! without the ones it could not. Nothing of the store changes either way, so the next read
//! answers from the same events and reports them again as soon as the key is usable.
//!
//! An event ingested while its group carried no customer managed key is held under the
//! service's own key, so no customer key decides it and it is always served.
//!
//! KMS is asked with NO guard of this service's store held (the two in-process stores are
//! never locked at once), and asked once per distinct key rather than once per event. An
//! answer that is not KMS reporting on the key establishes nothing and therefore withholds
//! nothing, exactly as where an association is judged.

use std::collections::BTreeSet;

/// The customer managed keys the events of one log group were ingested under — the keys a
/// read of that group has to be able to use. Read under the store's guard; the keys
/// themselves are then taken to KMS with no guard held.
#[must_use]
pub fn ingest_keys(group: &super::LogGroupRecord) -> BTreeSet<String> {
    group
        .streams
        .values()
        .flat_map(|stream| stream.events.iter())
        .filter_map(|event| event.ingest_kms_key_id.clone())
        .collect()
}

/// The customer managed keys of one log group's stored events whose data CloudWatch Logs can
/// no longer serve, out of `keys` — the keys KMS does not report as usable for
/// `log_group_arn` (see the module docs).
///
/// `log_group_arn` is the group's own `logGroupArn`, the value CloudWatch Logs passes KMS as
/// the encryption context of that group's log data, so a key policy scoped to the group is
/// judged against the group it was scoped to.
pub async fn unusable_keys(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    log_group_arn: &str,
    keys: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut unusable = BTreeSet::new();
    for key in keys {
        if super::kms_key_reference::refusal_for(ctx, region, key)
            .await
            .is_some()
            || super::kms_key_grant::refusal_for(ctx, region, key, log_group_arn)
                .await
                .is_some()
        {
            unusable.insert(key.clone());
        }
    }
    unusable
}

impl super::StoredEvent {
    /// Whether this event can be served while `unusable` are the keys CloudWatch Logs cannot
    /// use ([`unusable_keys`]): an event held under one of them is withheld, and one held
    /// under no customer managed key at all is served (see the module docs).
    #[must_use]
    pub fn is_readable(&self, unusable: &BTreeSet<String>) -> bool {
        self.ingest_kms_key_id
            .as_ref()
            .is_none_or(|key| !unusable.contains(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One stored event ingested under `key`.
    fn stored(key: Option<&str>) -> super::super::StoredEvent {
        super::super::StoredEvent {
            timestamp: 1_767_225_600_000,
            message: "one".to_string(),
            masked_message: None,
            transformed_message: None,
            ingestion_time: 1_767_225_600_000,
            ingestion_seq: 1,
            ingest_kms_key_id: key.map(str::to_string),
        }
    }

    /// RULE: an event is served unless the key it was INGESTED under is one CloudWatch Logs
    /// cannot use; an event ingested under no customer managed key is always served.
    #[test]
    fn an_event_is_withheld_only_with_its_own_ingest_key() {
        let key_a = "arn:aws:kms:eu-west-2:000000000000:key/aaaa";
        let key_b = "arn:aws:kms:eu-west-2:000000000000:key/bbbb";
        let unusable: BTreeSet<String> = [key_a.to_string()].into_iter().collect();
        for (case, event, expected) in [
            ("an event under the unusable key", stored(Some(key_a)), false),
            ("an event under another key", stored(Some(key_b)), true),
            ("an event under no customer key", stored(None), true),
        ] {
            let actual = event.is_readable(&unusable);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected readable == {expected}, got {actual}"
            );
        }
        // With no key withheld, every event reads.
        let nothing_withheld = BTreeSet::new();
        for (case, event) in [
            ("an event under a key", stored(Some(key_a))),
            ("an event under no customer key", stored(None)),
        ] {
            assert!(
                event.is_readable(&nothing_withheld),
                "case {case:?}: expected the event to be served while every key is usable"
            );
        }
    }
}
