// SPDX-License-Identifier: CC0-1.0

//! WHEN a record a mapping's poller just read was WRITTEN to its shard, read out of the
//! store of the service that OWNS the record.
//!
//! The age of a batch is the span between the write of its newest record and the read that
//! took the batch off the shard ("Age is the difference between the time Lambda received the
//! batch and the time the last record in the batch was written to the stream", the
//! `IteratorAge` series of [`super::metrics`]), and the age the mapping's
//! `MaximumRecordAgeInSeconds` discards a record past is the same span of that record
//! ([`super::event_source_poller::record_age`]) — so a span measured from a stamp EARLIER
//! than the write is longer than the record ever waited.
//!
//! One source publishes such a stamp. A DynamoDB stream record reports its creation instant
//! through `StreamRecord.ApproximateCreationDateTime`, which the model documents as "in UNIX
//! epoch time format and rounded down to the closest second": the member names a moment up to
//! a second before the write, so it is an approximation to report and not an instant to
//! measure from. An Amazon Kinesis record's `ApproximateArrivalTimestamp` is the write instant
//! itself, so that source needs nothing of this module.
//!
//! The write instant is read through the owning service's OWN store, reached through the
//! shared engine, exactly as [`super::event_source_resource_policy`] reads a source's own
//! policy — so what the age is measured from is the very instant that service stamped the
//! record with, rather than a second copy of it. An engine carrying no such store, and a
//! record that service no longer retains, are each "nothing to read": the caller then falls
//! back on the stamp the read itself carried.
//!
//! LOCK ORDER: the owning store is taken LAST and alone. [`SourceArrivals::table_stream_writes`]
//! locks it once for the records of one batch and takes no other lock, and its caller holds
//! none while it does, so this store is never held while another is taken.

use std::time::SystemTime;

use ::service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::state as streams_store;

/// The stores a record's write instant may live in, resolved once by a poller that meters
/// many batches over the same engine.
///
/// Clone-cheap: the field is a store handle, so a poller holds its own copy for as long as it
/// runs and still reads the one store that service's handlers write.
#[derive(Clone)]
pub struct SourceArrivals {
    /// The Amazon DynamoDB Streams store, where a table stream's records live.
    table_streams: Option<
        ::engine::ServiceState<
            streams_store::ComAmazonawsDynamodbstreamsDynamodbstreams20120810State,
        >,
    >,
}

impl ::std::fmt::Debug for SourceArrivals {
    /// Whether this engine carries the owning store, which is all of it that is not that
    /// store (a store handle has no rendering of its own).
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("SourceArrivals")
            .field("table_streams", &self.table_streams.is_some())
            .finish()
    }
}

impl SourceArrivals {
    /// The stores of `engine`: the one lookup, so a poller that meters many batches resolves
    /// the handle once.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Self {
        Self {
            table_streams: streams_store::service_state(engine),
        }
    }

    /// The instant each of `sequence_numbers` was WRITTEN to the DynamoDB stream `stream_arn`,
    /// in the same order, with `None` for each one there is nothing to read for (see the module
    /// docs).
    ///
    /// A whole batch at once, under ONE lock of the owning store: a pass asks about the records
    /// it just read together, so the store is locked once for them rather than once per record.
    #[must_use]
    pub fn table_stream_writes(
        &self,
        stream_arn: &str,
        sequence_numbers: &[String],
    ) -> Vec<Option<SystemTime>> {
        let Some(state) = self.table_streams.as_ref() else {
            return vec![None; sequence_numbers.len()];
        };
        let store = state.lock();
        sequence_numbers
            .iter()
            .map(|sequence_number| store.record_written_at(stream_arn, sequence_number))
            .collect()
    }
}
