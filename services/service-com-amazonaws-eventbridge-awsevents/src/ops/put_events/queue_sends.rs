// SPDX-License-Identifier: CC0-1.0

//! The events one request hands to its QUEUE targets, sent to each queue in
//! BATCHES rather than one call per event.
//!
//! What AWS does is unchanged by this: a rule whose target is an Amazon SQS queue
//! delivers every event its pattern matched to that queue, each as its own
//! message carrying the whole envelope, so what a consumer of the queue reads is
//! one message per (event, target) however the service grouped the sends. Amazon
//! SQS takes a send either one message at a time (`SendMessage`) or up to
//! [`MAX_BATCH_MESSAGES`] at a time (`SendMessageBatch`, "delivers up to ten
//! messages to the specified queue"), and the two store the same messages — so a
//! fan-out that hands ONE queue many of a request's events makes one call per
//! batch of them instead of one call per event.
//!
//! Why that matters here: every delivery this emulator makes is a whole request
//! through the in-process callback into the service that owns the target, which is
//! the dominant cost of a fan-out. A request matching a thousand events against
//! rules with queue targets otherwise makes a call per (event, rule, target) —
//! the PRODUCT — and a bulk publication does not converge. Batching makes the
//! calls proportional to the batches a queue receives, with the per-event work
//! (which document decides the delivery, what the target is handed, what became
//! of it) unchanged and still per event.
//!
//! Nothing is reported before it happened: a message held here is NOT recorded as
//! delivered, and the outcome of every held message — the entry in the invocation
//! log, the dead letter of one the queue refused, and the rule's invocation
//! metrics — is written from what the batched send ANSWERED, in [`flush`], which
//! the request runs before it answers its publisher.

use super::*;

/// How many messages ONE batched send carries: `SendMessageBatch` "delivers up to
/// ten messages to the specified queue", so a batch past ten is a call Amazon SQS
/// refuses as `TooManyEntriesInBatchRequest` rather than a bigger send.
const MAX_BATCH_MESSAGES: usize = 10;

/// How many bytes of PAYLOAD one batched send carries: "the maximum allowed
/// individual message size and the maximum total payload size (the sum of the
/// individual lengths of all of the batched messages) are both 256 KiB (262,144
/// bytes)" (`SendMessageBatch`), so the payload of one batch is summed against it
/// and a batch that would pass it is split rather than refused.
const MAX_BATCH_PAYLOAD_BYTES: usize = 256 * 1024;

/// The attribute name a trace header travels under, charged against
/// [`MAX_BATCH_PAYLOAD_BYTES`] with its value: what Amazon SQS measures a message
/// at is everything it carries, the message system attributes included ("the
/// 256 KiB … is the sum of the individual lengths of all of the batched
/// messages"), so a batch split that counted bodies alone could build one the
/// queue then refuses whole. Read from the SDK's own name for that attribute, the
/// one the send below sets it under, so the charge and the send cannot disagree.
fn trace_attribute_name() -> &'static str {
    ::aws_sdk_sqs::types::MessageSystemAttributeNameForSends::AwsTraceHeader.as_str()
}

/// One message a queue target is to be handed, in the owned form it waits in.
///
/// It carries the whole handover rather than a reference to it, for the reason
/// [`deferred::Deferred`] carries one: the send happens after the routing pass
/// that decided it has moved on.
struct Held {
    /// The Region the queue's own ARN names, which is the Region it is reached
    /// in.
    region: String,
    queue_url: String,
    /// The whole event envelope, or the text the target asked for instead.
    body: String,
    event_id: String,
    bus: String,
    rule: String,
    rule_arn: String,
    target: CompiledTarget,
    trace_header: Option<String>,
    /// When the event was published, which is what a target's event-age bound is
    /// read against.
    published: ::std::time::SystemTime,
}

impl Held {
    /// What this message weighs against the payload one batched send carries:
    /// everything it hands the queue — the body, and the trace header with the
    /// name it travels under when the publication carried one.
    fn charged_bytes(&self) -> usize {
        let trace = self
            .trace_header
            .as_ref()
            .map_or(0, |header| trace_attribute_name().len() + header.len());
        self.body.len().saturating_add(trace)
    }
}

/// The messages one request's queue targets are waiting for, in the order the
/// routing pass decided them.
///
/// How many it holds is bounded by the request's own delivery budget
/// ([`MAX_DELIVERIES_PER_REQUEST`] deliveries and
/// [`MAX_DELIVERED_BYTES_PER_REQUEST`] bytes), which [`deliver`] charges BEFORE a
/// message is held: a request holds no more messages than it may deliver, so this
/// grows with the budget rather than with what a publisher asks for.
#[derive(Default)]
pub(crate) struct QueueSends {
    held: Vec<Held>,
}

impl QueueSends {
    /// Hold one message for the queue at `queue_url`, to be sent in that queue's
    /// next batch.
    ///
    /// Called only once every document in force over the delivery has admitted it
    /// ([`queue_delivery_refusal`]) and the request's delivery budget has been
    /// charged for it, so a held message is one that is going to be sent.
    fn hold(&mut self, region: &str, queue_url: &str, handover: &Handover<'_>, body: &str) {
        self.held.push(Held {
            region: region.to_string(),
            queue_url: queue_url.to_string(),
            body: body.to_string(),
            event_id: handover.event_id.to_string(),
            bus: handover.bus.to_string(),
            rule: handover.rule.to_string(),
            rule_arn: handover.rule_arn.to_string(),
            target: handover.target.clone(),
            trace_header: handover.trace_header.map(str::to_string),
            published: handover.published,
        });
    }
}

/// A held message is a delivery that was admitted and charged for, so one its
/// request never sent is an event lost with nothing recorded for it: dropping a
/// buffer that still holds messages says so, rather than leaving a caller that
/// forgot [`flush`] silently short of deliveries.
impl Drop for QueueSends {
    fn drop(&mut self) {
        if !self.held.is_empty() {
            ::tracing::error!(
                held = self.held.len(),
                "EventBridge deliveries were admitted to a queue target and never sent: the \
                 pass that held them did not flush them"
            );
        }
    }
}

/// What came of handing one event to a queue target: the message is waiting for
/// its batch, or the delivery was decided before anything was enqueued.
pub(super) enum Handled {
    /// The message is held for its queue's next batch, and [`flush`] records what
    /// came of it.
    Held,
    /// Nothing is held: this is the delivery's outcome, recorded by the caller as
    /// any other is.
    Outcome(crate::state::InvocationOutcome),
}

/// Hand one event to the queue target the handover names: decide the delivery
/// against the documents in force over that queue, and hold the message for that
/// queue's next batch.
pub(super) async fn hand_over(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    stores: &Stores<'_>,
    region: &str,
    handover: &Handover<'_>,
    body: &str,
    sends: &mut QueueSends,
) -> Handled {
    let arn = handover.target.arn.as_str();
    let Some(queue) = sqs_queue(arn) else {
        return Handled::Outcome(crate::state::InvocationOutcome::NotAttempted(
            "the target ARN names no queue".to_string(),
        ));
    };
    // One client for every call this request makes on a queue of this Region: the
    // handle is a copy over the shared in-process transport, so it addresses the
    // same endpoint the cached one does.
    let client = clients.sqs(callback, region).clone();
    // The QUEUE's own documents decide this delivery before anything is enqueued:
    // its `Policy`, and — for an encrypted queue — the policy of the key its
    // messages are encrypted under ([`queue_delivery_refusal`]). They are read
    // ONCE per request per queue ([`Clients::queue_attributes`]), so a fan-out
    // handing one queue many of a request's events reads that queue's documents
    // once rather than once per event.
    let attributes = clients.queue_attributes(&client, &queue).await;
    if let Some(refusal) = queue_delivery_refusal(
        attributes.as_ref(),
        stores.kms,
        region,
        &queue,
        TargetKind::Queue.action(),
        arn,
        handover,
    ) {
        return Handled::Outcome(refusal);
    }
    sends.hold(region, &queue, handover, body);
    Handled::Held
}

/// Send every held message, and record what came of each one.
///
/// The messages of ONE queue go out together, in batches of at most
/// [`MAX_BATCH_MESSAGES`] and [`MAX_BATCH_PAYLOAD_BYTES`] and in the order the
/// routing pass held them, so the messages one rule's target receives are the
/// events that matched it and nothing else. One request's fan-out reaches a queue
/// once per BATCH of the events it is handed, which is what a bulk publication
/// needs: a queue is grouped over the whole pass rather than over the run of
/// messages that happened to be adjacent, because a fan-out decides its targets
/// event by event and one queue's messages are spread across them.
///
/// Each message's own outcome is read off the answer — Amazon SQS reports a
/// batched send entry by entry ("the result of sending each message is reported
/// individually in the response") — so a message its queue refused is a FAILED
/// delivery carrying that refusal while the rest of its batch are deliveries that
/// happened.
///
/// What a sent message is then owed is what any delivery is owed: the dead letter
/// of one that failed, the rule's invocation metrics, and the entry in the
/// invocation log, which is appended to `invocations` for the caller to record
/// with the rest.
pub(crate) async fn flush(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    stores: &Stores<'_>,
    mut sends: QueueSends,
    invocations: &mut Vec<crate::state::TargetInvocation>,
) {
    // Taken out of the buffer, which leaves it empty: what is held here is on its
    // way, so the guard that reports an unflushed buffer ([`QueueSends::drop`])
    // has nothing to report for this one.
    let held = ::std::mem::take(&mut sends.held);
    // The messages of each queue, keyed by the queue they are for and in the order
    // the routing pass decided them: a queue's messages are grouped over the WHOLE
    // pass, so one request reaches a queue once per batch of its messages rather
    // than once per run of adjacent ones.
    let mut grouped: ::std::collections::BTreeMap<(&str, &str), Vec<usize>> =
        ::std::collections::BTreeMap::new();
    for (index, message) in held.iter().enumerate() {
        grouped
            .entry((message.region.as_str(), message.queue_url.as_str()))
            .or_default()
            .push(index);
    }
    for ((region, queue_url), indices) in grouped {
        let mut start = 0usize;
        while start < indices.len() {
            // One batch: the next messages of this queue, as many as one batched
            // send carries.
            let mut end = start;
            let mut bytes = 0usize;
            while end < indices.len()
                && end - start < MAX_BATCH_MESSAGES
                && (end == start
                    || bytes + held[indices[end]].charged_bytes() <= MAX_BATCH_PAYLOAD_BYTES)
            {
                bytes += held[indices[end]].charged_bytes();
                end += 1;
            }
            let batch: Vec<&Held> = indices[start..end]
                .iter()
                .map(|index| &held[*index])
                .collect();
            let outcomes = send_batch(callback, clients, region, queue_url, &batch).await;
            for (message, outcome) in batch.iter().zip(outcomes) {
                let handover = Handover {
                    event_id: &message.event_id,
                    bus: &message.bus,
                    rule: &message.rule,
                    rule_arn: &message.rule_arn,
                    target: &message.target,
                    // What this delivery handed over, which is what a dead letter
                    // of it parks: the envelope, or the text the target asked for.
                    rendered: &message.body,
                    trace_header: message.trace_header.as_deref(),
                    published: message.published,
                };
                // A send nothing retries: the retries a target's `RetryPolicy`
                // bounds are made by an api-destination invocation, which is not
                // a queue delivery ([`crate::state::retry_bounds`]).
                let dead_lettered = dead_letter_refusal(
                    callback,
                    clients,
                    region,
                    stores.kms,
                    &handover,
                    &message.body,
                    &outcome,
                    None,
                )
                .await;
                // The send is ONE attempt and nothing retries it: the retries a
                // target's `RetryPolicy` bounds are made by an api-destination
                // invocation, which is not a queue delivery
                // ([`crate::state::retry_bounds`]).
                count_invocation(stores.cloudwatch, &handover, &outcome, 1, dead_lettered);
                invocations.push(crate::state::TargetInvocation {
                    event_id: message.event_id.clone(),
                    rule: message.rule.clone(),
                    target_id: message.target.id.clone(),
                    target_arn: message.target.arn.clone(),
                    outcome,
                });
            }
            start = end;
        }
    }
}

/// Send ONE batch of messages to the queue at `queue_url`, and answer what came
/// of each of them, in the order they were given.
///
/// A call the queue's service refused as a whole — its key, its url, the batch
/// itself — is every message of that batch failing under the condition that
/// answer belongs to ([`outcome`]), because none of them was enqueued; an answer
/// naming an entry among its failures is that ONE message failing, which is how
/// Amazon SQS reports a message it would not take beside ones it took.
async fn send_batch(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    clients: &mut Clients,
    region: &str,
    queue_url: &str,
    batch: &[&Held],
) -> Vec<crate::state::InvocationOutcome> {
    let client = clients.sqs(callback, region).clone();
    let mut request = client.send_message_batch().queue_url(queue_url);
    // The positions this emulator could not build an entry for, so what is
    // reported for them names THAT rather than blaming the queue for a message
    // nothing sent it.
    let mut unbuilt: ::std::collections::BTreeSet<usize> = ::std::collections::BTreeSet::new();
    for (index, message) in batch.iter().enumerate() {
        // The entry id is the message's POSITION in this batch: it identifies the
        // entry in the answer and nowhere else ("an identifier for a message in
        // this batch used to communicate the result"), and the ids of one batch
        // must differ, which positions do.
        let mut entry = ::aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
            .id(index.to_string())
            .message_body(&message.body);
        // "Contains the message group ID to use when the target is an Amazon SQS
        // fair or FIFO queue" (`Target$SqsParameters`): such a queue takes no
        // message without one, so the group the target names travels with the
        // delivery. No deduplication id goes with it — "if you specify a fair or
        // FIFO queue as a target, the queue must have content-based deduplication
        // enabled", so the queue derives one from the body.
        if let Some(message_group_id) = &message.target.message_group_id {
            entry = entry.message_group_id(message_group_id);
        }
        // "An X-Ray trace header … that contains the trace-id associated with the
        // event" travels WITH the event: a queue carries it as the one message
        // system attribute Amazon SQS takes on a send, under the name that
        // attribute publishes, so a consumer of the queue reads the trace the
        // publisher named.
        if let Some(trace_header) = &message.trace_header {
            match ::aws_sdk_sqs::types::MessageSystemAttributeValue::builder()
                .data_type(STRING_ATTRIBUTE_TYPE)
                .string_value(trace_header)
                .build()
            {
                Ok(attribute) => {
                    entry = entry.message_system_attributes(
                        ::aws_sdk_sqs::types::MessageSystemAttributeNameForSends::AwsTraceHeader,
                        attribute,
                    );
                }
                Err(error) => ::tracing::warn!(
                    error = %error,
                    "an event's trace header could not be carried to its queue target"
                ),
            }
        }
        match entry.build() {
            Ok(entry) => request = request.entries(entry),
            Err(error) => {
                ::tracing::warn!(
                    queue = %queue_url,
                    error = %error,
                    "an EventBridge delivery to a queue target could not be built"
                );
                unbuilt.insert(index);
            }
        }
    }
    let answered = match request.send().await {
        Ok(answered) => answered,
        // The whole call failed, so no message of this batch was enqueued: each
        // of them failed the way a single send that answered this would have.
        Err(error) => {
            let failure = outcome::<(), _>(Err(error));
            return ::std::vec![failure; batch.len()];
        }
    };
    let refused: ::std::collections::BTreeMap<&str, &::aws_sdk_sqs::types::BatchResultErrorEntry> =
        answered
            .failed()
            .iter()
            .map(|entry| (entry.id(), entry))
            .collect();
    let taken: ::std::collections::BTreeSet<&str> = answered
        .successful()
        .iter()
        .map(::aws_sdk_sqs::types::SendMessageBatchResultEntry::id)
        .collect();
    (0..batch.len())
        .map(|index| {
            // A message this emulator could not build an entry for was never
            // sent, so nothing is claimed for it and the gap is named as its
            // own: it is this emulator's, not an answer from the queue.
            if unbuilt.contains(&index) {
                return crate::state::InvocationOutcome::NotAttempted(
                    "this emulator could not build the batch entry for this delivery".to_string(),
                );
            }
            let id = index.to_string();
            if let Some(entry) = refused.get(id.as_str()) {
                return crate::state::InvocationOutcome::Failed {
                    code: delivery_condition(entry.code()).to_string(),
                    message: entry.message().unwrap_or(entry.code()).to_string(),
                };
            }
            if taken.contains(id.as_str()) {
                return crate::state::InvocationOutcome::Invoked;
            }
            // An answer that reports this entry neither way says nothing about
            // the message, so nothing is claimed for it: it is recorded as the
            // handover whose result is unknown rather than as one the queue took.
            crate::state::InvocationOutcome::NotAttempted(format!(
                "the queue {queue_url} answered the batched send without reporting this message"
            ))
        })
        .collect()
}
