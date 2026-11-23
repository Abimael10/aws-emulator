// SPDX-License-Identifier: CC0-1.0

//! The stored CHANNEL record: what `CreateChannel` writes, what every channel read projects,
//! and the delivery cursor that makes a channel's delivery to its destination exactly-once.
//!
//! Beside this one file live the two halves of a channel that are rules of their own: where it
//! delivers ([`super::channel_delivery`]) and whether its service execution role may use the
//! key it is encrypted with ([`super::channel_key_access`]).

use super::*;

/// One stored Kinesis channel: the authoritative record every channel operation reads.
/// `CreateChannel` inserts it and `DescribeChannel` projects it onto its output shape.
///
/// The destinations and the logging configuration are stored as the DESCRIPTION shapes,
/// with every documented default already resolved, so what a create accepted is exactly
/// what every later read reports.
#[derive(Debug, Clone)]
pub struct Channel {
    /// The `ChannelName` the create carried, which is half of this record's key.
    pub name: String,
    /// The region the creating request was signed for.
    pub region: String,
    /// The `ChannelARN`, as [`channel_arn`] builds it.
    pub arn: String,
    /// The `ChannelId`, as [`channel_id`] derives it.
    pub id: String,
    /// When `CreateChannel` minted the channel.
    pub created_at: SystemTime,
    /// When the channel's `CREATING` -> `ACTIVE` transition falls due: "Upon receiving the
    /// request, Amazon Kinesis Data Streams returns immediately with the channel in the
    /// CREATING state. After provisioning is complete, Amazon Kinesis Data Streams sets the
    /// state to ACTIVE" (`CreateChannel`).
    pub active_at: SystemTime,
    /// The `ServiceExecutionRoleARN` the create carried.
    pub service_execution_role_arn: String,
    /// The channel's source streams, each carrying the stored stream's own creation instant.
    pub streams: Vec<ChannelStreamDescription>,
    /// The general purpose bucket destination, when the create named one.
    pub s3_destination: Option<S3DestinationDescription>,
    /// The streaming table destination, when the create named one.
    pub s3_tables_destination: Option<S3TablesDestinationDescription>,
    /// The server-side encryption configuration, when the create named one.
    pub encryption_configuration: Option<ChannelEncryptionConfiguration>,
    /// The CloudWatch Logs configuration, with the documented default names resolved.
    pub logging_configuration: ChannelLoggingConfiguration,
    /// The tags the create carried.
    pub tags: HashMap<String, String>,
    /// When the channel's `UPDATING` -> `ACTIVE` transition falls due, once a configuration
    /// change has been accepted for it: "Upon receiving the request, Amazon Kinesis Data
    /// Streams sets the channel to the UPDATING state and returns immediately. After the
    /// change is applied, Amazon Kinesis Data Streams sets the channel back to the ACTIVE
    /// state" (`UpdateChannel`). `None` on a channel no change has been accepted for.
    pub updating_until: Option<SystemTime>,
    /// The resource-based policy attached to the channel, `None` while none is. A
    /// `ResourceARN` the resource-policy operations address a resource by names a resource of
    /// either kind this service holds
    /// (`^arn:aws.*:kinesis:.*:\d{12}:.*(stream|channel)/\S+$`), so a channel holds one the
    /// same way a stream does.
    pub policy: Option<String>,
    /// How far this channel's delivery has got in each shard of each source stream: the last
    /// sequence number DELIVERED to the destination, keyed by
    /// [`super::channel_delivery::cursor_key`]. A record at or before the cursor has reached
    /// the destination, so no later delivery reads it again — which is what makes the
    /// delivery of one payload exactly-once.
    pub delivered: ::std::collections::BTreeMap<String, String>,
    /// The instant before which no further delivery of this channel is attempted, set when a
    /// destination did not take one ([`super::channel_delivery::DELIVERY_RETRY_SPAN`]).
    /// `None` on a channel whose last delivery was taken, and on one that has made none.
    pub delivery_retry_at: Option<SystemTime>,
}

impl Channel {
    /// The channel's status at `now`: `CREATING` until the transition the create armed falls
    /// due, `UPDATING` until the one an accepted configuration change armed falls due,
    /// `ACTIVE` from then on. Derived from the stored instants, so two reads at the same
    /// instant agree and no read has to write.
    pub fn status(&self, now: SystemTime) -> &'static str {
        if now < self.active_at {
            CHANNEL_CREATING
        } else if self.updating_until.is_some_and(|due| now < due) {
            CHANNEL_UPDATING
        } else {
            CHANNEL_ACTIVE
        }
    }

    /// Where the channel delivers, as `ChannelDestinationType` spells it: "S3 - Delivery to
    /// a general purpose Amazon S3 bucket", "S3_TABLES - Delivery to streaming tables on
    /// Apache Iceberg" (`ChannelSummary$ChannelDestinationType`). Derived from the stored
    /// destination, exactly one of which a channel carries — which is also the one thing an
    /// update cannot change: "You cannot change the destination ... of an existing channel"
    /// (`UpdateChannel`).
    pub fn destination_type(&self) -> &'static str {
        if self.s3_tables_destination.is_some() {
            CHANNEL_DESTINATION_S3_TABLES
        } else {
            CHANNEL_DESTINATION_S3
        }
    }

    /// The stored record projected onto `ChannelSummary`, which is what `ListChannels`
    /// reports: "A summary of a channel". The members it shares with
    /// [`Self::to_description`] come from the same stored fields, so a listing and a read of
    /// one channel never disagree, and its source streams are reported as the identifier
    /// pair (`ChannelStreamIdentifier`) a summary carries rather than the fuller
    /// description.
    pub fn to_summary(&self, now: SystemTime) -> ChannelSummary {
        ChannelSummary {
            channel_name: self.name.clone(),
            channel_arn: self.arn.clone(),
            channel_id: self.id.clone(),
            channel_status: self.status(now).to_string(),
            // See [`Self::to_description`]: a channel this emulator holds provisions, so
            // none of them is `FAILED` and none carries a reason.
            channel_status_reason: None,
            channel_creation_timestamp: timestamp(self.created_at),
            channel_destination_type: self.destination_type().to_string(),
            streams: self
                .streams
                .iter()
                .map(|stream| ChannelStreamIdentifier {
                    stream_arn: stream.stream_arn.clone(),
                    stream_creation_timestamp: stream.stream_creation_timestamp,
                })
                .collect(),
        }
    }

    /// The channel's tags as the modelled `Tag` list, ordered by key — the SAME projection
    /// of the SAME kind of stored tag set a stream's tags are reported through
    /// ([`Stream::tags_ordered`]), because one rule orders the tags of any Kinesis resource.
    pub fn tags_ordered(&self) -> Vec<Tag> {
        tags_ordered(&self.tags)
    }

    /// Whether one of the channel's source streams is the stream `stream_arn` names, created
    /// at `created_at` when the caller pinned an instant.
    ///
    /// An ARN a delete released and a later create took again names another stream, so a
    /// caller that pins the creation instant is answered about THAT incarnation only; one
    /// that pins none is answered about whichever stream the ARN holds. Instants are compared
    /// at the millisecond this service keeps one at ([`requested_millis`]), because an
    /// instant the wire carried as an epoch-seconds number arrives a few nanoseconds either
    /// side of it.
    pub fn sources_stream(
        &self,
        stream_arn: &str,
        created_at: Option<::aws_smithy_types::DateTime>,
    ) -> bool {
        self.streams.iter().any(|stream| {
            stream.stream_arn == stream_arn
                && created_at.is_none_or(|pinned| {
                    requested_millis(pinned) == requested_millis(stream.stream_creation_timestamp)
                })
        })
    }

    /// The stored record projected onto `ChannelDescription`, which is what `CreateChannel`
    /// and `DescribeChannel` both answer with: "The configuration and current status of the
    /// channel, including its ARN, destination configuration, and lifecycle state".
    pub fn to_description(&self, now: SystemTime) -> ChannelDescription {
        ChannelDescription {
            channel_name: self.name.clone(),
            channel_arn: self.arn.clone(),
            channel_id: self.id.clone(),
            channel_status: self.status(now).to_string(),
            // "A message describing the reason for a FAILED status"
            // (`ChannelDescription$ChannelStatusReason`) — a channel this emulator holds
            // provisions, so none of them is `FAILED` and none carries a reason.
            channel_status_reason: None,
            channel_creation_timestamp: timestamp(self.created_at),
            service_execution_role_arn: self.service_execution_role_arn.clone(),
            stream_configuration_list: self.streams.clone(),
            s3_destination_configuration: self.s3_destination.clone(),
            s3_tables_destination_configuration: self.s3_tables_destination.clone(),
            encryption_configuration: self.encryption_configuration.clone(),
            logging_configuration: self.logging_configuration.clone(),
        }
    }
}
