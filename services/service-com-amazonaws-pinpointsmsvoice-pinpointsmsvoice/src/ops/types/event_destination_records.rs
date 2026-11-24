// SPDX-License-Identifier: CC0-1.0

//! The mapping between the three nested destination shapes of an event destination and
//! their stored records in [`crate::state`].
//!
//! RULE: a stored destination record is the member-for-member image of the wire shape it
//! was built from, and the wire shape reported back is the member-for-member image of the
//! stored record — so a destination written by `CreateConfigurationSetEventDestination`
//! and one rewritten by `UpdateConfigurationSetEventDestination` read back the same way
//! through `GetConfigurationSetEventDestinations`. The mapping of each shape
//! (`CloudWatchLogsDestination`, `KinesisFirehoseDestination`, `SnsDestination`:
//! `smithy_query(query="shape",shape="CloudWatchLogsDestination,KinesisFirehoseDestination,SnsDestination",model_path="services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/smithy-model/com-amazonaws-pinpointsmsvoice-pinpointsmsvoice.json")`)
//! is therefore written ONCE, here, in each direction: a member added to one of those
//! shapes, or a renamed state field, is carried by every handler that converts it or by
//! none — it cannot be carried by one and dropped by another.
//!
//! These are `From` impls rather than functions, so each handler keeps its own shape and
//! only the per-member mapping is shared: the unconditional build in the create handler,
//! the per-member merge in the update handler and the per-record projection in the reader
//! each hand the one value they already hold to `.into()`. Nothing here decides WHETHER a
//! member is written, which stays the handler's own decision, and an absent member stays
//! absent in both directions.
//!
//! Not a modelled shape, so this module carries no `_types` suffix and is not
//! glob-re-exported; trait impls need no import to be in scope.

use super::{CloudWatchLogsDestination, KinesisFirehoseDestination, SnsDestination};
use crate::state::{CloudWatchLogsDest, KinesisFirehoseDest, SnsDest};

impl From<CloudWatchLogsDestination> for CloudWatchLogsDest {
    fn from(destination: CloudWatchLogsDestination) -> Self {
        Self {
            iam_role_arn: destination.iam_role_arn,
            log_group_arn: destination.log_group_arn,
        }
    }
}

impl From<&CloudWatchLogsDest> for CloudWatchLogsDestination {
    fn from(record: &CloudWatchLogsDest) -> Self {
        Self {
            iam_role_arn: record.iam_role_arn.clone(),
            log_group_arn: record.log_group_arn.clone(),
        }
    }
}

impl From<KinesisFirehoseDestination> for KinesisFirehoseDest {
    fn from(destination: KinesisFirehoseDestination) -> Self {
        Self {
            delivery_stream_arn: destination.delivery_stream_arn,
            iam_role_arn: destination.iam_role_arn,
        }
    }
}

impl From<&KinesisFirehoseDest> for KinesisFirehoseDestination {
    fn from(record: &KinesisFirehoseDest) -> Self {
        Self {
            delivery_stream_arn: record.delivery_stream_arn.clone(),
            iam_role_arn: record.iam_role_arn.clone(),
        }
    }
}

impl From<SnsDestination> for SnsDest {
    fn from(destination: SnsDestination) -> Self {
        Self {
            topic_arn: destination.topic_arn,
        }
    }
}

impl From<&SnsDest> for SnsDestination {
    fn from(record: &SnsDest) -> Self {
        Self {
            topic_arn: record.topic_arn.clone(),
        }
    }
}
