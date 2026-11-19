// SPDX-License-Identifier: CC0-1.0

//! The DATA KEY a queue on a customer managed KMS key seals its messages under: where it
//! comes from, how long it is reused, and what the account's audit record says about each
//! call this service makes to KMS for one.
//!
//! # The rule
//!
//! A queue encrypted with a KMS key is server-side encrypted with ENVELOPE encryption:
//! Amazon SQS does not hold the key, it asks KMS for a data key and uses that data key for
//! the messages it seals. How often it asks is the queue's own
//! `KmsDataKeyReusePeriodSeconds` — "the length of time, in seconds, for which Amazon SQS
//! can reuse a data key to encrypt or decrypt messages before calling KMS again. An integer
//! representing seconds, between 60 seconds (1 minute) and 86,400 seconds (24 hours).
//! Default: 300 (5 minutes). A shorter time period provides better security but results in
//! more calls to KMS" (the `KmsDataKeyReusePeriodSeconds` entry of the `CreateQueue`
//! `Attributes` documentation).
//!
//! So the number of KMS calls a burst of sends makes is COUNT-BASED and one-sided: every
//! send inside one queue's reuse window shares the data key the first of them obtained, and
//! the first send after that window asks KMS again. That is what this module holds — one
//! data key per queue ([`ComAmazonawsSqsAmazonsqsState::data_keys`]), with the instant it
//! was obtained, so the window is measured from the call that obtained it rather than from
//! anything a request carries.
//!
//! # Whose call it is, and what the record says
//!
//! The call is made on the CALLER's behalf: the client called `SendMessage`, and the
//! `GenerateDataKey` that send needed was made by Amazon SQS for it. CloudTrail records it
//! as a call of the service that was CALLED — `eventSource` `kms.amazonaws.com`, `eventName`
//! `GenerateDataKey` — with the caller's own `userIdentity` and `invokedBy` naming this
//! service, which is how an auditor tells the two apart. The one builder of such a record is
//! shared ([`::service_common::on_behalf_of_calls`]); what this module states is the call:
//! the key the queue is on, the data key spec, and the encryption context
//! ([`QUEUE_ENCRYPTION_CONTEXT_NAME`]) naming the queue the data key is for.
//!
//! No record carries key material: the data key's sealed copy and the plaintext copy are
//! both left out of the record (the `responseElements` of such a call is null), exactly as
//! CloudTrail leaves the material of a cryptographic operation out of event history.
//!
//! # The two legs
//!
//! * a SEND obtains the data key ([`data_key_for_send`]) — the live one of the queue, or a
//!   new one from the queue's KMS key — and stamps the message with it
//!   ([`MessageDataKey`]), so the store records which data key each message belongs to;
//! * a RECEIVE opens the data keys of the messages it is about to hand over
//!   ([`open_message_data_keys`]): the data key still inside its window needs no call, and
//!   one past it is opened by KMS, which is the `kms:Decrypt` on the queue's key that a
//!   consumer of an encrypted queue performs.
//!
//! Whether the caller may have the key used at all is decided before either leg runs
//! ([`super::encryption::authorize_queue_key`]), which is also where the key's existence and
//! its key state are judged — so nothing here refuses a call: it performs the KMS leg of one
//! that was already allowed.
//!
//! The plaintext copy of a data key is never stored. It is drawn, used for the call it was
//! drawn for, and dropped with it; what the queue keeps is the SEALED copy KMS answered
//! with, which is the copy a later `kms:Decrypt` opens.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

use ::service_com_amazonaws_kms_trentservice::state as kms_store;
use ::service_common::on_behalf_of_calls::{record_on_behalf_of_answered, OnBehalfOfCall};
use ::service_common::query_error_envelope::new_request_id;

use super::encryption::{
    relayed, KeyRefusal, QueueKey, DEFAULT_DATA_KEY_REUSE_PERIOD_SECONDS,
    KMS_DATA_KEY_REUSE_PERIOD_SECONDS,
};

/// The name of the encryption-context entry Amazon SQS makes every call on a queue's key
/// under: the queue's own ARN, under the key `aws:sqs:arn`. A consumer joins a KMS record to
/// the queue it was made for by this entry, and a key policy may be conditioned on it.
pub const QUEUE_ENCRYPTION_CONTEXT_NAME: &str = "aws:sqs:arn";

/// The data key spec Amazon SQS asks for: a 256-bit symmetric key. `KeySpec`'s own value
/// set is `AES_128 | AES_256` (KMS `GenerateDataKeyRequest$KeySpec`).
pub const DATA_KEY_SPEC: &str = "AES_256";

/// How many bytes that spec is: 256 bits.
const DATA_KEY_BYTES: usize = 32;

/// The `eventSource` of the service these calls are made TO, in CloudTrail's
/// `<endpoint-prefix>.amazonaws.com` form.
pub const KMS_EVENT_SOURCE: &str = "kms.amazonaws.com";

/// The KMS operation a send performs on the queue's key, as KMS's model names it.
pub const GENERATE_DATA_KEY_OPERATION: &str = "GenerateDataKey";

/// The KMS operation a receive performs on it, as KMS's model names it.
pub const DECRYPT_OPERATION: &str = "Decrypt";

/// A cryptographic operation on a KMS key READS the key: it changes nothing the account
/// holds, which is what CloudTrail records as the event's `readOnly`.
const KMS_CRYPTOGRAPHIC_CALL_IS_READ_ONLY: bool = true;

/// The published bounds of the reuse period: "an integer representing seconds, between 60
/// seconds (1 minute) and 86,400 seconds (24 hours)". A queue carries a value inside them —
/// the attribute's own range is enforced when it is set — and a value from anywhere else is
/// held to them here, so no window can be shorter or longer than the attribute publishes.
const MIN_REUSE_PERIOD_SECONDS: u64 = 60;
/// The upper bound of that range.
const MAX_REUSE_PERIOD_SECONDS: u64 = 86_400;

/// The data key ONE message was sealed under, as the message carries it.
///
/// It names the data key rather than holding it: the sealed copy is the copy KMS answered
/// with, which only the queue's KMS key can open, so a message records which data key it
/// belongs to without holding any usable material.
#[derive(Clone, PartialEq, Eq)]
pub struct MessageDataKey {
    /// The ARN of the KMS key the data key is sealed under.
    pub key_arn: String,
    /// The queue the data key was obtained for: the value of the
    /// [`QUEUE_ENCRYPTION_CONTEXT_NAME`] entry the sealed copy authenticates, which a later
    /// open has to present again. It travels with the message, so a message moved to another
    /// queue is still opened under the context it was sealed under.
    pub queue_arn: String,
    /// The sealed copy of the data key, as KMS answered it.
    pub sealed: Vec<u8>,
}

/// Written by hand rather than derived: what is diagnosable about a message's data key is
/// which key and queue it belongs to, never the bytes of the sealed copy — a message that is
/// printed must not print one.
impl std::fmt::Debug for MessageDataKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageDataKey")
            .field("key_arn", &self.key_arn)
            .field("queue_arn", &self.queue_arn)
            .finish_non_exhaustive()
    }
}

/// The data key a queue is sealing its messages under right now, and when it was obtained.
///
/// One per queue. The window the data key may be reused in is measured from
/// [`obtained_at`](Self::obtained_at), and the key the queue is ON is part of the entry: a
/// queue moved to another KMS key, or deleted and created again under the same name, is not
/// reusing the data key the earlier one obtained.
#[derive(Clone)]
pub struct QueueDataKey {
    /// The data key itself, as a message is stamped with it.
    pub key: MessageDataKey,
    /// Which incarnation of the queue obtained it (`Queue::incarnation`).
    pub incarnation: String,
    /// When KMS answered with it: the instant the reuse window runs from.
    pub obtained_at: SystemTime,
}

/// Written by hand rather than derived: what is diagnosable about a data key is which key
/// and queue it belongs to and when it was obtained, never its bytes — a store that is
/// printed must not print material.
impl std::fmt::Debug for QueueDataKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QueueDataKey")
            .field("key_arn", &self.key.key_arn)
            .field("queue_arn", &self.key.queue_arn)
            .field("incarnation", &self.incarnation)
            .field("obtained_at", &self.obtained_at)
            .finish_non_exhaustive()
    }
}

/// The window a queue carrying `attributes` may reuse one data key in.
///
/// The queue's own `KmsDataKeyReusePeriodSeconds`, else the documented default of 300
/// seconds, held to the published range.
#[must_use]
pub fn reuse_period(attributes: &BTreeMap<String, String>) -> Duration {
    let seconds = attributes
        .get(KMS_DATA_KEY_REUSE_PERIOD_SECONDS)
        .map(|value| value.trim())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            DEFAULT_DATA_KEY_REUSE_PERIOD_SECONDS
                .parse::<u64>()
                .unwrap_or(MIN_REUSE_PERIOD_SECONDS)
        });
    Duration::from_secs(seconds.clamp(MIN_REUSE_PERIOD_SECONDS, MAX_REUSE_PERIOD_SECONDS))
}

/// The encryption context every call on `queue_arn`'s key is made under.
#[must_use]
pub fn encryption_context(queue_arn: &str) -> HashMap<String, String> {
    HashMap::from([(
        QUEUE_ENCRYPTION_CONTEXT_NAME.to_string(),
        queue_arn.to_string(),
    )])
}

/// The data key the queue at `queue_url` seals the message this send is accepting under.
///
/// The queue's live data key when its reuse window has not run out — no call is made for
/// one, which is what the window is for — and otherwise a new one from the queue's KMS key,
/// recorded as that caller's own `GenerateDataKey` call of `kms.amazonaws.com`.
///
/// `key` is what the authorization already resolved ([`QueueKey`]), so the queue's
/// `KmsMasterKeyId` is resolved once per call however it is spelled.
///
/// `Ok(None)` when this engine carries no KMS service to ask: a queue whose key nothing can
/// be said about is sent to exactly as it was before, which is also what the authorization
/// does with it.
pub fn data_key_for_send<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    queue_url: &str,
    key: &QueueKey,
    now: SystemTime,
) -> Result<Option<MessageDataKey>, KeyRefusal> {
    let Some(store) = crate::state::service_state(engine) else {
        return Ok(None);
    };
    let region = crate::state::request_region(ctx.parts());
    // What the window is measured against and the key that is live inside it, read under one
    // guard and then released: KMS is never asked anything while this service's own guard is
    // held.
    let held = {
        let state = store.lock();
        state.queue_by_url(&region, queue_url).map(|queue| {
            let period = reuse_period(&queue.attributes);
            let live = state
                .live_data_key(
                    &region,
                    &queue.name,
                    &queue.incarnation,
                    &key.key_arn,
                    period,
                    now,
                )
                .map(|held| held.key.clone());
            (queue.name.clone(), queue.incarnation.clone(), live)
        })
    };
    let Some((name, incarnation, live)) = held else {
        return Ok(None);
    };
    if let Some(live) = live {
        return Ok(Some(live));
    }
    let Some(generated) = generate_data_key(engine, ctx, &region, key, now)? else {
        return Ok(None);
    };
    {
        let mut state = store.lock();
        state.data_keys.insert(
            crate::state::queue_key(&region, &name),
            QueueDataKey {
                key: generated.clone(),
                incarnation,
                obtained_at: now,
            },
        );
    }
    Ok(Some(generated))
}

/// Ask KMS for a data key under the queue's key, as this request's caller.
///
/// The data key is drawn and sealed by KMS's OWN state — the same material a
/// `kms:GenerateDataKey` call would draw it from and seal it under — so the sealed copy is
/// one KMS's `Decrypt` opens, and the key is recorded as used for a cryptographic operation
/// exactly as that call records it.
fn generate_data_key<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    key: &QueueKey,
    now: SystemTime,
) -> Result<Option<MessageDataKey>, KeyRefusal> {
    let Some(kms) = kms_store::service_state(engine) else {
        return Ok(None);
    };
    let context = encryption_context(&key.queue_arn);
    let (key_id, sealed) = {
        let store = kms.lock();
        let kms_store::KeyRef::Found(resolved) = store.resolve(region, &key.key_arn) else {
            // The key was resolved a moment ago by the authorization and is gone now, so
            // there is no key to generate a data key under: KMS's own sentence for an
            // identifier naming no key, relayed as this service relays every KMS refusal.
            return Err(KeyRefusal::NotFound(relayed(
                &kms_store::key_not_found_message(region, &key.key_arn),
                "NotFoundException",
            )));
        };
        let key_id = resolved.key_id.clone();
        let Some(material) = kms_store::random_bytes(DATA_KEY_BYTES)
            .and_then(|plaintext| kms_store::seal(resolved, &plaintext, Some(&context)))
        else {
            return Err(KeyRefusal::InvalidKeyUsage(relayed(
                &format!(
                    "{} cannot be used to generate a data key; its KeyUsage must be {} and \
                     its key spec a symmetric encryption spec.",
                    key.key_arn,
                    kms_store::KEY_USAGE_ENCRYPT_DECRYPT
                ),
                "InvalidKeyUsageException",
            )));
        };
        (key_id, material)
    };
    // The call this service just made to KMS, recorded for the caller it was made for, and
    // the use of the key it made — named by the ONE id that call was answered under, so the
    // record in event history and the key's own usage record name one request.
    let answered_as = new_request_id();
    let recorded_as = record_kms_call(
        engine,
        ctx,
        region,
        GENERATE_DATA_KEY_OPERATION,
        ::serde_json::json!({
            "keyId": key.key_arn,
            "keySpec": DATA_KEY_SPEC,
            "encryptionContext": {QUEUE_ENCRYPTION_CONTEXT_NAME: key.queue_arn},
        }),
        now,
        answered_as.clone(),
    );
    // The key was used in a successful cryptographic operation, which is what
    // `GetKeyLastUsage` reports for it.
    kms.lock().record_last_usage_of_service_call(
        region,
        &key_id,
        GENERATE_DATA_KEY_OPERATION,
        answered_as,
        recorded_as,
    );
    Ok(Some(MessageDataKey {
        key_arn: key.key_arn.clone(),
        queue_arn: key.queue_arn.clone(),
        sealed,
    }))
}

/// Open the data keys the messages a receive is about to hand over were sealed under.
///
/// A data key still inside its queue's reuse window is reused as it is — that window is
/// what spares the call — and one past it is opened by KMS, recorded as the caller's own
/// `Decrypt` call of `kms.amazonaws.com`. Each distinct data key is opened once however many
/// of the messages carry it.
///
/// Nothing is refused here: whether the caller may have the queue's key used for a decrypt
/// at all, and whether the key can serve one, were decided before this ran
/// ([`super::encryption::authorize_queue_key`]). A sealed copy KMS cannot open names no
/// state a caller can reach, so it is reported to the operator rather than turned into a
/// refusal of a call that was allowed.
pub fn open_message_data_keys<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    sealed: &[MessageDataKey],
    now: SystemTime,
) {
    if sealed.is_empty() {
        return;
    }
    let Some(kms) = kms_store::service_state(engine) else {
        return;
    };
    let live = live_data_keys(engine, region, now);
    let mut opened: Vec<&[u8]> = Vec::new();
    for key in sealed {
        if live
            .get(&key.queue_arn)
            .is_some_and(|held| held == &key.sealed)
        {
            continue;
        }
        if opened.contains(&key.sealed.as_slice()) {
            continue;
        }
        opened.push(&key.sealed);
        let context = encryption_context(&key.queue_arn);
        let opened_key = {
            let store = kms.lock();
            let kms_store::KeyRef::Found(resolved) = store.resolve(region, &key.key_arn) else {
                continue;
            };
            let key_id = resolved.key_id.clone();
            let plaintext = kms_store::parse_envelope(&key.sealed)
                .and_then(|envelope| kms_store::unseal(resolved, &envelope, Some(&context)));
            plaintext.map(|_| key_id)
        };
        let Some(key_id) = opened_key else {
            // Not silent: a data key its own key cannot open is a store nothing produced,
            // and the receive goes on serving the messages it was allowed to serve.
            ::tracing::warn!(
                key_arn = key.key_arn,
                queue_arn = key.queue_arn,
                "the data key of a message could not be opened by the queue's KMS key"
            );
            continue;
        };
        // One id for the call this service made to KMS: the record of it in event history
        // and the key's own usage record name the same request (see [`generate_data_key`]).
        let answered_as = new_request_id();
        let recorded_as = record_kms_call(
            engine,
            ctx,
            region,
            DECRYPT_OPERATION,
            ::serde_json::json!({
                "keyId": key.key_arn,
                "encryptionAlgorithm": kms_store::ENCRYPTION_ALGORITHM_SYMMETRIC_DEFAULT,
                "encryptionContext": {QUEUE_ENCRYPTION_CONTEXT_NAME: key.queue_arn},
            }),
            now,
            answered_as.clone(),
        );
        kms.lock().record_last_usage_of_service_call(
            region,
            &key_id,
            DECRYPT_OPERATION,
            answered_as,
            recorded_as,
        );
    }
}

/// The store's own reader of the data key a queue is sealing under.
impl crate::state::ComAmazonawsSqsAmazonsqsState {
    /// The queue's data key when it may still be reused at `now`, and `None` otherwise.
    ///
    /// A data key may be reused only for the queue incarnation that obtained it and only
    /// while it is the key the queue is ON: a queue moved to another `KmsMasterKeyId`, and a
    /// name created again after a delete, start afresh. Nothing is written here — a reader
    /// of the cache must not change what a later send sees — so an entry past its window is
    /// simply not answered and the next send replaces it.
    #[must_use]
    pub fn live_data_key(
        &self,
        region: &str,
        name: &str,
        incarnation: &str,
        key_arn: &str,
        period: Duration,
        now: SystemTime,
    ) -> Option<&QueueDataKey> {
        self.data_keys
            .get(&crate::state::queue_key(region, name))
            .filter(|held| held.incarnation == incarnation)
            .filter(|held| held.key.key_arn == key_arn)
            .filter(|held| now < held.obtained_at + period)
    }
}

/// The sealed copy of the data key each queue of `region` is sealing under at `now`, keyed
/// by that queue's own ARN — the data key a call on THAT queue needs no KMS leg for.
///
/// Keyed by the queue rather than pooled, because reuse is a property of one queue's window:
/// a data key is live for the queue that obtained it and for no other, whatever another
/// queue's window is doing.
fn live_data_keys<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    now: SystemTime,
) -> BTreeMap<String, Vec<u8>> {
    let Some(store) = crate::state::service_state(engine) else {
        return BTreeMap::new();
    };
    let state = store.lock();
    state
        .data_keys
        .iter()
        .filter_map(|((held_region, name), held)| {
            if held_region != region {
                return None;
            }
            let queue = state.queue(region, name)?;
            let live = state.live_data_key(
                region,
                name,
                &queue.incarnation,
                &held.key.key_arn,
                reuse_period(&queue.attributes),
                now,
            )?;
            Some((queue.arn.clone(), live.key.sealed.clone()))
        })
        .collect()
}

/// Record one call this service made to KMS for the caller of `ctx`, answered under
/// `answered_as`; answer the id of the event it was recorded in.
///
/// `answered_as` is the id KMS gave that in-process call, which this service also hands to
/// the key's usage record, so event history and KMS's own usage tracking name ONE request
/// for it.
///
/// Best effort, like every recorder in this tree: an engine carrying no event history
/// records nothing and the call it was made for is served exactly as it would be otherwise.
fn record_kms_call<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request_parameters: ::serde_json::Value,
    now: SystemTime,
    answered_as: String,
) -> Option<String> {
    record_on_behalf_of_answered(
        engine,
        ctx.parts(),
        OnBehalfOfCall {
            event_name: operation.to_string(),
            event_source: KMS_EVENT_SOURCE.to_string(),
            invoked_by: crate::service::management_events::EVENT_SOURCE.to_string(),
            aws_region: region.to_string(),
            read_only: KMS_CRYPTOGRAPHIC_CALL_IS_READ_ONLY,
            request_parameters,
            // A cryptographic operation answers with key material, which no record carries.
            response_elements: None,
        },
        now,
        answered_as,
    )
}
