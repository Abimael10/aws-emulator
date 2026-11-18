// SPDX-License-Identifier: CC0-1.0

//! The EVENT TRACKERS a dataset group ingests through: the record `CreateEventTracker`
//! persists, the ARN it is addressed by, and the output shapes `DescribeEventTracker` and
//! `ListEventTrackers` rebuild from it.
//!
//! # Why these live together
//!
//! An event tracker is the control-plane half of incremental ingestion: it "[c]reates an
//! event tracker that you use when adding event data to a specified dataset group using
//! the PutEvents API", and "the response includes a tracking ID, which you pass as a
//! parameter when you use the PutEvents operation"
//! (`smithy_query(query="doc", shape="CreateEventTracker")`). The other half — the stream
//! that tracking id ingests into — belongs to Amazon Personalize Events, so the create
//! records the id it minted in the registry that service reads
//! ([`crate::state::ingestion_registry`]) exactly as `CreateDataset` records a dataset
//! ARN, and the delete withdraws it.
//!
//! The rules every operation over these records follows:
//!
//! * a tracker is addressed by an ARN minted from its own name, so a duplicate-name
//!   create collides on an ARN that is already present;
//! * "[o]nly one event tracker can be associated with a dataset group", so a second
//!   create naming a group that already has one collides as well;
//! * the status a read reports is a function of the stored due instant and the clock
//!   ([`crate::state::creation_status`]) and no read writes anything; the create does the
//!   tracker's own work before it answers, so that instant is the create's own
//!   ([`crate::state::CREATE_SPAN`]).

use std::time::SystemTime;

use crate::ops::types::{EventTracker, EventTrackerSummary, Tag};
use crate::state::{creation_status, timestamp_of};

/// How many event trackers THIS store holds before a NEW `CreateEventTracker` is refused
/// with the modelled `LimitExceededException`. The EMULATOR's own bound, set the same way
/// as [`crate::state::MAX_SCHEMAS`].
///
/// Named for the store it bounds, because a create is bounded by TWO ceilings and the
/// lower one is the other: the ingestion registry this create also writes keeps its own
/// maximum (`MAX_EVENT_TRACKERS` in the Amazon Personalize Events crate), so whenever that
/// service is mounted its bound is the one a caller reaches first.
pub const MAX_STORED_EVENT_TRACKERS: usize = 10_000;

/// An event tracker's ARN:
/// `arn:aws:personalize:<region>:<account>:event-tracker/<name>`.
///
/// Derived from the name, so the name's uniqueness IS the ARN's and the value is
/// reproducible rather than per-run.
pub fn event_tracker_arn(region: &str, name: &str) -> String {
    ::emulator_server::emulator_arn("personalize", region, &format!("event-tracker/{name}"))
}

/// What `CreateEventTracker` persists, and what `DescribeEventTracker` /
/// `ListEventTrackers` reconstruct an [`EventTracker`] / [`EventTrackerSummary`] from.
///
/// `status` is NOT stored, for the reason [`crate::state::DatasetGroupRecord`] gives: the
/// model documents the progression, so the status is a function of the clock and is
/// derived from `active_at` on every read.
#[derive(Debug, Clone)]
pub struct EventTrackerRecord {
    pub name: String,
    pub event_tracker_arn: String,
    /// The tracking id this tracker's ingestion is addressed by, minted once by the
    /// create and kept, so every later read reports the same id and the registry entry
    /// the create seeded stays resolvable.
    pub tracking_id: String,
    /// The dataset group "that receives the event data"
    /// (`smithy_query(query="doc", shape="EventTracker", member="datasetGroupArn")`).
    pub dataset_group_arn: String,
    pub creation_date_time: SystemTime,
    pub last_updated_date_time: SystemTime,
    /// The instant the clock owes this tracker its `ACTIVE` status.
    pub active_at: SystemTime,
    pub tags: Vec<Tag>,
}

/// The `EventTracker` `DescribeEventTracker` returns for a stored record.
pub fn event_tracker_of(record: &EventTrackerRecord) -> EventTracker {
    EventTracker {
        name: Some(record.name.clone()),
        event_tracker_arn: Some(record.event_tracker_arn.clone()),
        // "The Amazon Web Services account that owns the event tracker": the one account
        // this emulator serves, which is also the ARN's account segment.
        account_id: Some(::emulator_server::emulator_account_id().to_string()),
        tracking_id: Some(record.tracking_id.clone()),
        dataset_group_arn: Some(record.dataset_group_arn.clone()),
        status: Some(creation_status(record.active_at).to_string()),
        creation_date_time: Some(timestamp_of(record.creation_date_time)),
        last_updated_date_time: Some(timestamp_of(record.last_updated_date_time)),
    }
}

/// The `EventTrackerSummary` `ListEventTrackers` returns for a stored record.
///
/// A summary is the describe answer without the members that address the tracker's
/// ingestion: "Provides a summary of the properties of an event tracker. For a complete
/// listing, call the DescribeEventTracker API"
/// (`smithy_query(query="doc", shape="EventTrackerSummary")`), and the summary shape
/// carries no `trackingId`, `accountId` or `datasetGroupArn` at all.
pub fn event_tracker_summary_of(record: &EventTrackerRecord) -> EventTrackerSummary {
    EventTrackerSummary {
        name: Some(record.name.clone()),
        event_tracker_arn: Some(record.event_tracker_arn.clone()),
        status: Some(creation_status(record.active_at).to_string()),
        creation_date_time: Some(timestamp_of(record.creation_date_time)),
        last_updated_date_time: Some(timestamp_of(record.last_updated_date_time)),
    }
}
