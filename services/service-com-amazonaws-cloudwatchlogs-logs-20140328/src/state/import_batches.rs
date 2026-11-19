// SPDX-License-Identifier: CC0-1.0

//! The BATCHES of an import task — "the individual batches within an import task" —  as they
//! are stored, advanced by the clock and reported.
//!
//! `DescribeImportTaskBatches` states what one is: "For CloudTrail Event Data Store sources, a
//! batch refers to a subset of stored events grouped by their eventTime." A batch is therefore
//! NOT a resource a request creates: it is part of the import `CreateImportTask` starts, so the
//! batches of a task are derived from that task's own event-time window
//! ([`import_batch_records`]) when the task is created, and kept on the record beside it.
//!
//! Two things about a batch are reported, and both come from stored state:
//!
//! * `ImportBatch$batchId`, "the unique identifier of the import batch", minted from the task's
//!   own id and the batch's position in the event-time order the import walks them in
//!   ([`mint_import_batch_id`]), so it is unique across the store and derived from state;
//! * `ImportBatch$status`, which is the model's `ImportStatus` — the same enum the TASK's status
//!   is drawn from, because a batch is a part of the import and finishes as that import moves
//!   through it. The status is owed by the CLOCK, exactly as the task's terminal status is: each
//!   batch has an instant it finishes at, spread across the span the import takes
//!   ([`super::IMPORT_COMPLETION_DELAY_MS`]), with the last batch finishing when the task does.
//!   A batch the import never got to, because the task left `IN_PROGRESS` for a status other
//!   than `COMPLETED`, takes the status the task stopped in — and only a `FAILED` batch reports
//!   `errorMessage`, which is "only present when status is FAILED".
//!
//! Nothing here is moved on because something was read: a report materialises the transitions
//! the clock had already made due ([`ImportTaskRecord::materialize_due_batch_transitions`]) and
//! nothing else.

use super::{
    ImportTaskRecord, IMPORT_COMPLETION_DELAY_MS, IMPORT_STATUS_COMPLETED, IMPORT_STATUS_FAILED,
    IMPORT_STATUS_IN_PROGRESS,
};
use crate::ops::types::ImportBatch;

/// The span of EVENT TIME one import batch groups: one day.
///
/// A batch is "a subset of stored events grouped by their eventTime" and CloudWatch Logs
/// publishes no granularity for that grouping, so the span is THE EMULATOR'S OWN CHOICE — the
/// one an event-time grouping is naturally stated in, so an import of a window of several days
/// is reported as several batches rather than as one opaque unit.
pub const IMPORT_BATCH_EVENT_TIME_SPAN_MS: i64 = 24 * 60 * 60 * 1_000;

/// THE EMULATOR'S OWN CEILING on the batches one import task is grouped into, not a quota cited
/// from AWS: the event-time window a create names is caller-chosen and unbounded in length, and
/// every batch of every stored task stays resident, so a window longer than this many days is
/// grouped into this many equal subsets instead of one per day.
pub const MAX_IMPORT_BATCHES_PER_TASK: usize = 50;

/// The number of batches `DescribeImportTaskBatches` returns when the request omits `limit`:
/// "The maximum number of import batches to return in the response. Default: 10".
pub const DEFAULT_IMPORT_BATCH_LIMIT: i32 = 10;

/// One batch of an import task — "a collection of events being imported to CloudWatch" — as it
/// is stored. Every member of the model's `ImportBatch` shape is reported from this record, so
/// `DescribeImportTaskBatches` answers from state alone.
#[derive(Debug, Clone)]
pub struct ImportBatchRecord {
    /// `ImportBatch$batchId` — "The unique identifier of the import batch."
    pub batch_id: String,
    /// `ImportBatch$status` — "The current status of the import batch", one of the
    /// [`IMPORT_STATUS_IN_PROGRESS`] family the model's `ImportStatus` enum holds.
    pub status: String,
    /// `ImportBatch$errorMessage` — "The error message if the batch failed to import. Only
    /// present when status is FAILED."
    pub error_message: Option<String>,
    /// The instant this batch is due to finish importing, stored rather than waited for: a
    /// reporting operation materialises the transition once the clock is past it
    /// ([`ImportTaskRecord::materialize_due_batch_transitions`]). `None` once the batch is in a
    /// terminal status, which owes no further transition.
    pub completion_due_ms: Option<i64>,
}

impl ImportBatchRecord {
    /// This batch as the model's `ImportBatch` structure, which is what
    /// `DescribeImportTaskBatches` reports.
    pub fn reported(&self) -> ImportBatch {
        ImportBatch {
            batch_id: self.batch_id.clone(),
            status: self.status.clone(),
            error_message: self.error_message.clone(),
        }
    }
}

/// The id of one batch of the import task `import_id`, at the 1-based position `ordinal` in the
/// event-time order the import walks its batches in.
///
/// "The unique identifier of the import batch" (`BatchId`, `@length(1..256)`, no pattern): it is
/// derived from the task's own id, which is unique for the life of the store, and from the
/// batch's position within that task, so no two batches of this account can ever share one.
pub fn mint_import_batch_id(import_id: &str, ordinal: usize) -> String {
    format!("{import_id}-{ordinal}")
}

/// How many batches an import of the event time window `[start_event_time_ms,
/// end_event_time_ms]` groups its events into.
///
/// The grouping is by event time ([`IMPORT_BATCH_EVENT_TIME_SPAN_MS`]), so it needs the extent of
/// the window: only a create that bounded the event time at BOTH ends states one, and an import
/// whose window is open at either end moves its events as a single group. Every import moves at
/// least one subset, and no import is grouped into more than
/// [`MAX_IMPORT_BATCHES_PER_TASK`] of them.
pub fn import_batch_count(
    start_event_time_ms: Option<i64>,
    end_event_time_ms: Option<i64>,
) -> usize {
    let (Some(start), Some(end)) = (start_event_time_ms, end_event_time_ms) else {
        return 1;
    };
    let span = u64::try_from(end.saturating_sub(start)).unwrap_or(0);
    let days = usize::try_from(span.div_ceil(
        u64::try_from(IMPORT_BATCH_EVENT_TIME_SPAN_MS).unwrap_or(1),
    ))
    .unwrap_or(MAX_IMPORT_BATCHES_PER_TASK);
    days.clamp(1, MAX_IMPORT_BATCHES_PER_TASK)
}

/// The batches a brand-new import task starts with: every one of them still running, and each
/// due to finish at its own share of the span the import takes, so the LAST batch finishes at
/// the instant the task itself completes.
///
/// This is what [`ImportTaskRecord::new`] stores for the import `CreateImportTask` starts.
pub fn import_batch_records(
    import_id: &str,
    start_event_time_ms: Option<i64>,
    end_event_time_ms: Option<i64>,
    creation_time_ms: i64,
) -> Vec<ImportBatchRecord> {
    let count = import_batch_count(start_event_time_ms, end_event_time_ms);
    let total = i64::try_from(count).unwrap_or(1).max(1);
    (1..=count)
        .map(|ordinal| {
            let share = i64::try_from(ordinal)
                .unwrap_or(total)
                .saturating_mul(IMPORT_COMPLETION_DELAY_MS)
                / total;
            ImportBatchRecord {
                batch_id: mint_import_batch_id(import_id, ordinal),
                status: IMPORT_STATUS_IN_PROGRESS.to_string(),
                error_message: None,
                completion_due_ms: Some(creation_time_ms.saturating_add(share)),
            }
        })
        .collect()
}

impl ImportTaskRecord {
    /// Materialise the batch status changes the clock owes this task, as of `now_ms`.
    ///
    /// A batch whose own instant has passed WHILE the import was still moving data is
    /// `COMPLETED`, updated at that instant and not at the time something happened to look. The
    /// import stops moving data when the task leaves `IN_PROGRESS` — at `lastUpdatedTime`, the
    /// instant the task reached the status it is in — so a batch the import never got to takes
    /// the status the task stopped in, carrying the task's own error message where the model says
    /// one is present.
    pub fn materialize_due_batch_transitions(&mut self, now_ms: i64) {
        let stopped = self.is_terminal();
        let moved_through = if stopped {
            self.last_updated_time_ms
        } else {
            now_ms
        };
        let stopped_in = self.import_status.clone();
        let failure = self.error_message.clone();
        for batch in &mut self.batches {
            if batch.status != IMPORT_STATUS_IN_PROGRESS {
                continue;
            }
            match batch.completion_due_ms {
                Some(due) if due <= moved_through => {
                    batch.status = IMPORT_STATUS_COMPLETED.to_string();
                    batch.completion_due_ms = None;
                }
                _ if stopped => {
                    batch.status = stopped_in.clone();
                    if stopped_in == IMPORT_STATUS_FAILED {
                        batch.error_message = failure.clone();
                    }
                    batch.completion_due_ms = None;
                }
                _ => {}
            }
        }
    }
}
