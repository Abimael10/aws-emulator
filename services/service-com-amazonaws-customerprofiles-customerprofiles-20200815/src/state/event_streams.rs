// SPDX-License-Identifier: CC0-1.0

//! The event streams a domain holds: the destination `CreateEventStream` persisted, the
//! ARN it minted, and the sentences every operation addressing one words its refusals
//! with.
//!
//! An event stream is a resource OF its domain, identified by the
//! (`DomainName`, `EventStreamName`) pair — the name is the `@httpLabel` of every
//! event-stream operation — so the records live on [`super::Domain`] and are dropped with
//! it.
//!
//! `State` and `DestinationDetails$Status` are NOT stored. `EventStreamState` is
//! `RUNNING | STOPPED` and `EventStreamDestinationStatus` is `HEALTHY | UNHEALTHY`
//! (`smithy_query(query="shape", shape="EventStreamState,EventStreamDestinationStatus")`),
//! and the stopped/unhealthy halves describe a destination the service found it could no
//! longer deliver to — `StoppedSince` is "The timestamp when the State changed to STOPPED"
//! and `UnhealthySince` "The timestamp when the status last changed to UNHEALHY". No
//! operation of this surface stops a stream or marks its destination unhealthy, so a
//! stored stream reports the running half, from the one projection below, and reports no
//! instant for a change that never happened.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{DestinationSummary, EventStreamDestinationDetails, EventStreamSummary};

/// Emulator resource guard, NOT an AWS quota: the most event streams this emulator stores
/// for one domain. Set well above any realistic emulator workload, and checked before the
/// insert so repeated `CreateEventStream` calls cannot grow the store without bound.
pub const MAX_EVENT_STREAMS_PER_DOMAIN: usize = 1_000;

/// The `EventStreamState` of a stream the service is delivering events to — "The
/// operational state of destination stream for export".
pub const EVENT_STREAM_STATE_RUNNING: &str = "RUNNING";

/// The `EventStreamDestinationStatus` of a destination the service has found no delivery
/// problem with — "The status of enabling the Kinesis stream as a destination for export".
pub const EVENT_STREAM_DESTINATION_HEALTHY: &str = "HEALTHY";

/// One stored event stream: the destination `CreateEventStream` persisted, the ARN minted
/// for it, the tags the request carried, and the instant it was created.
#[derive(Debug, Clone)]
pub struct StoredEventStream {
    /// `EventStreamName`, as stored (also this record's key in
    /// [`super::Domain::event_streams`]).
    pub name: String,
    /// `EventStreamArn`: minted once, when the stream was created, so a later read reports
    /// the ARN the stream was created with.
    pub arn: String,
    /// `Uri`, as stored: "The StreamARN of the destination to deliver profile events to".
    /// It names a resource of ANOTHER service, so it is persisted as the request stated it
    /// and reported back unchanged — the same way a domain holds its `DeadLetterQueueUrl`.
    pub uri: String,
    /// `Tags`, as stored.
    pub tags: Option<HashMap<String, String>>,
    /// `CreatedAt`: generated once, when the stream was created, and never recomputed.
    pub created_at: SystemTime,
}

impl StoredEventStream {
    /// `State` as the wire reports it.
    pub fn state(&self) -> String {
        EVENT_STREAM_STATE_RUNNING.to_string()
    }

    /// `Tags` as an event-stream projection reports it: the tag map this stream carries,
    /// PRESENT and empty for a stream that carries none, exactly as
    /// [`super::Domain::reported_tags`] reports a domain's and
    /// [`super::StoredSegmentDefinition::reported_tags`] a segment definition's.
    ///
    /// Unlike `StoppedSince`, absent from a read because no such instant exists, the tag
    /// map is always reported, so a caller that indexes `Tags` can tell "this stream
    /// carries no tags" from "tags were not reported".
    pub fn reported_tags(&self) -> Option<HashMap<String, String>> {
        Some(self.tags.clone().unwrap_or_default())
    }

    /// `DestinationDetails` as `GetEventStream` reports it: the destination this stream
    /// was created with and its status. `Message` is left unset — the member is "The
    /// human-readable string that corresponds to the error or success while enabling the
    /// streaming destination", and nothing of this surface produces one.
    pub fn destination_details(&self) -> EventStreamDestinationDetails {
        EventStreamDestinationDetails {
            uri: self.uri.clone(),
            status: EVENT_STREAM_DESTINATION_HEALTHY.to_string(),
            unhealthy_since: None,
            message: None,
        }
    }

    /// `DestinationSummary` as `ListEventStreams` reports it: the same destination and
    /// status [`Self::destination_details`] reports, less the prose member a summary does
    /// not carry, so a listing and a read cannot disagree about one stream's destination.
    pub fn destination_summary(&self) -> DestinationSummary {
        let details = self.destination_details();
        DestinationSummary {
            uri: details.uri,
            status: details.status,
            unhealthy_since: details.unhealthy_since,
        }
    }

    /// This stream as one item of a listing, under the domain that holds it.
    pub fn summary(&self, domain_name: &str) -> EventStreamSummary {
        EventStreamSummary {
            domain_name: domain_name.to_string(),
            event_stream_name: self.name.clone(),
            event_stream_arn: self.arn.clone(),
            state: self.state(),
            stopped_since: None,
            destination_summary: Some(self.destination_summary()),
            tags: self.tags.clone(),
        }
    }
}

/// One event stream's `EventStreamArn`: this service's ARN namespace, the region the
/// stream was created in, and the domain-scoped resource path the stream is addressed by —
/// the same path its operations are bound to.
pub fn event_stream_arn(region: &str, domain_name: &str, event_stream_name: &str) -> String {
    ::engine::emulator_arn(
        super::ARN_SERVICE,
        region,
        &format!("domains/{domain_name}/event-streams/{event_stream_name}"),
    )
}

/// The `ResourceNotFoundException` message for an event stream a domain does not hold, as
/// the service words that refusal: the resource kind as the API spells it, the name the
/// request's `{EventStreamName}` label carried, and the domain its `{DomainName}` label
/// named — both labels verbatim, so a caller can tell WHICH domain the stream was looked
/// for in (the form [`super::upload_job_not_found`] takes for the other resource addressed
/// under a domain by a second label). Shared by every operation addressing one stream, so
/// the same absent name reads the same whichever of them was called.
pub fn event_stream_not_found(event_stream_name: &str, domain_name: &str) -> String {
    format!("The EventStream with name {event_stream_name} not found for domain {domain_name}")
}

/// The `ResourceNotFoundException` message the EVENT-STREAM operations word an absent
/// domain with: the operations bound under `/domains/{DomainName}/event-streams` report
/// that absence in their own fixed sentence, which names the resource kind, says the name
/// is a name, and ends "not found" — whose only variable part is the `DomainName` the
/// request's URI label carried. Shared by every operation of this surface, so the same
/// absent name reads the same whichever of them was called, and distinct from the sentence
/// the domain operations themselves ([`super::domain_not_found`]) and the upload-job
/// surface ([`super::upload_job_domain_not_found`]) word the same absence with.
pub fn event_stream_domain_not_found(domain_name: &str) -> String {
    format!("Domain with name {domain_name} not found")
}

/// The `BadRequestException` message for an event stream name the domain already holds:
/// the service's fixed sentence for an already-existing stream, with the name the
/// request's `{EventStreamName}` label carried as its only variable part.
pub fn event_stream_exists(event_stream_name: &str) -> String {
    format!("Event stream with name {event_stream_name} already exists")
}
