// SPDX-License-Identifier: CC0-1.0

//! What a mapping's `FilterCriteria` admit of ONE batch read off a shard, and what each
//! record of it is matched AS.
//!
//! The decision about one record is the shared one every poller takes
//! ([`crate::state::event_source_filter`]); what belongs to a SHARD source is which records
//! of a batch that decision leaves to be handed over, how the ones it rejects are accounted
//! for — the poller itself consumed them, so the shard's position moves past them exactly as
//! it moves past a record an invocation processed — and the record each pattern is matched
//! against, which is the record its own SOURCE reported ([`filter_event`]).

use super::*;
use crate::state::event_source_filter;

/// What a mapping's `FilterCriteria` admitted of ONE batch read off a shard.
pub(super) enum Admitted<'batch> {
    /// Every record of the batch: what a mapping carrying no criteria polls, and what a
    /// batch each record of which matches a pattern comes to.
    Whole(&'batch [ShardRecord]),
    /// Some of the batch: the records the criteria admitted, in the order the shard holds
    /// them, and the sequence numbers of the ones they rejected.
    Part {
        /// The records the function is invoked with.
        invoking: Vec<ShardRecord>,
        /// The records no pattern matched, by sequence number: they are the PASS's own to
        /// account for, since no invocation will report about them.
        rejected: Vec<String>,
    },
}

impl Admitted<'_> {
    /// The records the function is to be invoked with, and `None` when the criteria admitted
    /// no record of the batch at all — which is a batch there is no invocation to make for.
    pub(super) fn invoking(&self) -> Option<&[ShardRecord]> {
        let invoking = match self {
            Self::Whole(records) => *records,
            Self::Part { invoking, .. } => invoking.as_slice(),
        };
        (!invoking.is_empty()).then_some(invoking)
    }

    /// The records of the batch this pass HANDLED, by sequence number: the ones the
    /// invocation reported as processed, plus the ones the criteria rejected.
    ///
    /// A rejected record is discarded by the poller itself — "FilterCriteria … determine
    /// whether Lambda should process an event", and one that is not to be processed is never
    /// handed to the function — so it is handled as soon as it has been read, which is what
    /// lets the shard's position move past it ([`checkpointed_through`]).
    pub(super) fn handled(&self, processed: &[String]) -> Vec<String> {
        match self {
            Self::Whole(_) => processed.to_vec(),
            Self::Part { rejected, .. } => {
                let mut handled = processed.to_vec();
                handled.extend(rejected.iter().cloned());
                handled
            }
        }
    }
}

/// What `criteria` admit of `records`, read as the mapping's source hands each record over.
///
/// The decision about one record is the shared one
/// ([`crate::state::event_source_filter::Filters`]); what is a shard source's own is the
/// record each one is matched AS ([`filter_event`]).
pub(super) fn admitted_of<'batch>(
    criteria: &event_source_filter::Filters,
    run: &ShardSourcedMapping,
    records: &'batch [ShardRecord],
) -> Admitted<'batch> {
    if criteria.admits_everything() {
        return Admitted::Whole(records);
    }
    let mut budget = event_source_filter::budget();
    let mut invoking: Vec<ShardRecord> = Vec::new();
    let mut rejected: Vec<String> = Vec::new();
    for record in records {
        if criteria.admits(&filter_event(run.source, &record.envelope), &mut budget) {
            invoking.push(record.clone());
        } else {
            rejected.push(record.sequence_number.clone());
        }
    }
    if rejected.is_empty() {
        return Admitted::Whole(records);
    }
    Admitted::Part {
        invoking,
        rejected,
    }
}

/// One record as the FILTER sees it: the record its SOURCE reported, with a payload that
/// source carries as opaque text read as the document that text spells where it spells one —
/// the same rule a queue source's body follows ([`crate::state::sqs_poller`]).
pub(super) fn filter_event(source: ShardSource, envelope: &Value) -> Cow<'_, Value> {
    match source {
        // A DynamoDB stream record carries the change itself as members of its own — which
        // mutation it was, the item's keys and the images the stream captures, each a typed
        // attribute map — so the record that source reports IS the envelope the function
        // receives, and the document a pattern names fields in.
        ShardSource::DynamoDbStream => Cow::Borrowed(envelope),
        // A Kinesis record is the fields the stream reported for it, which the envelope
        // carries under one member of its own ([`kinesis_stream::filter_event`]).
        ShardSource::Kinesis => kinesis_stream::filter_event(envelope),
    }
}
