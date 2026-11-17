// SPDX-License-Identifier: CC0-1.0

//! Whether the destination a flow log would publish to EXISTS, asked of the service that
//! OWNS that destination.
//!
//! `CreateFlowLogs` points the log delivery service at a resource of ANOTHER service —
//! "specify the ARN of an S3 bucket", "specify the ARN of a Kinesis Data Firehose delivery
//! stream" (`CreateFlowLogsRequest$LogDestination`) — and no record can ever be delivered to
//! a resource that is not there. So a create resolves the destination where its owner holds
//! it BEFORE any flow log is stored, and reports the monitored resources it could not turn
//! logging on for in `Unsuccessful` ("Information about the flow logs that could not be
//! created successfully", `CreateFlowLogsResult$Unsuccessful`) instead of leaving behind a
//! flow log that asserts a delivery nothing performs.
//!
//! The ONE destination type this question is not asked of is CloudWatch Logs: that
//! destination is "a new or existing CloudWatch Logs log group"
//! (`CreateFlowLogsRequest$LogGroupName`), so a group that is not there yet is one the
//! delivery brings into existence rather than a destination the create has to find.
//!
//! Each owning service is reached the way the framework documents a handler reaching another
//! emulated service ([`smithy_server_runtime::OperationContext::callback`]), through the
//! tree's ONE reading of each question — `service_common::s3_client::bucket_is_absent` and
//! `service_common::firehose_client::delivery_stream_is_absent` — so nothing here opens a
//! socket, leaves the process or names another service crate, and no two services asked
//! whether a destination exists can answer differently. Only that service's ANSWER
//! establishes anything: a read reporting no such resource is what refuses the create, while
//! every other outcome — an emulator assembled without that service to ask, or any other
//! failure to ask it — establishes nothing and so refuses nothing.
//!
//! The one caller-supplied value that reaches a client's authority is the Region, which
//! [`super::request_region`] has already narrowed to the Region grammar this service admits,
//! so no request can re-point one of these clients at a host of its own choosing.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::*;
use crate::ops::types::{UnsuccessfulItem, UnsuccessfulItemError};

/// The `error.code` a per-resource flow-log failure carries.
///
/// `CreateFlowLogs` is a partial-success API: a monitored resource it could not turn logging
/// on for is reported as an `UnsuccessfulItem` rather than as a failure of the whole call, and
/// the code that item carries is the HTTP status the same fault would have been refused with
/// ("Information about the error that occurred", `UnsuccessfulItemError$Code`), which is what
/// a recorded answer of this class shows.
pub const FLOW_LOG_UNSUCCESSFUL_ERROR_CODE: &str = "400";

/// The NAME of the destination `log_destination` names, when the service that OWNS that
/// destination reports holding no such thing — and `None` when the create may proceed.
///
/// The name rather than a yes/no, because the name is what the owning service holds the
/// destination under and so what the failure below reports: an ARN also carries a partition, a
/// Region and an account, none of which is what is missing.
///
/// `None` — so the create proceeds — for the destination type whose group the delivery
/// creates, for a value that names no destination of its type at all (the create has already
/// refused that outright, [`flow_log_destination_names_its_type`]), and for every answer that
/// is not the owning service reporting the resource absent.
///
/// Call it with NO guard of this service's store held: it awaits.
pub async fn absent_flow_log_destination(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    log_destination_type: &str,
    log_destination: &str,
) -> Option<String> {
    match log_destination_type {
        LOG_DESTINATION_TYPE_S3 => {
            let bucket = flow_log_s3_destination(log_destination)?.bucket;
            ::service_common::s3_client::bucket_is_absent(ctx, region, &bucket)
                .await
                .then_some(bucket)
        }
        LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE => {
            let stream = firehose_delivery_stream_in_arn(log_destination)?;
            ::service_common::firehose_client::delivery_stream_is_absent(ctx, region, stream)
                .await
                .then(|| stream.to_string())
        }
        // "The name of a NEW or existing CloudWatch Logs log group where Amazon EC2 publishes
        // your flow logs" (`CreateFlowLogsRequest$LogGroupName`): a group that is not there is
        // created by the delivery, so there is nothing for the create to resolve.
        _ => None,
    }
}

/// The failure a monitored resource is reported with when the destination the create named is
/// not there: the code every per-resource flow-log failure carries
/// ([`FLOW_LOG_UNSUCCESSFUL_ERROR_CODE`]), the sentence a recorded answer of this class pins —
/// the member's own name, the destination that is missing, and the fixed ` does not exist` —
/// and the monitored resource the item is about.
///
/// The member is what the sentence names because the destination is what the caller has to
/// fix, and `destination_name` is the destination as its OWN service names it
/// ([`absent_flow_log_destination`]). The echo is bounded the way every refusal of this crate
/// bounds a value the caller chose the length of ([`echoed_resource_id`]).
pub fn flow_log_destination_absent_item(
    resource_id: &str,
    destination_name: &str,
) -> UnsuccessfulItem {
    UnsuccessfulItem {
        error: Some(UnsuccessfulItemError {
            code: Some(FLOW_LOG_UNSUCCESSFUL_ERROR_CODE.to_string()),
            message: Some(format!(
                "LogDestination: {destination} does not exist",
                destination = echoed_resource_id(destination_name),
            )),
        }),
        resource_id: Some(resource_id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The per-resource failure names the member, the missing destination and the monitored
    /// resource — whatever the destination's own form.
    #[test]
    fn the_unsuccessful_item_names_the_destination_that_is_missing() {
        // (case, the monitored resource, the destination name)
        let cases: [(&str, &str, &str); 4] = [
            (
                "a delivery stream",
                "vpc-0123456789abcdef0",
                "absent-stream",
            ),
            (
                "a bucket",
                "subnet-0123456789abcdef0",
                "absent-flow-logs-bucket",
            ),
            (
                "a bucket whose name carries dots",
                "eni-0123456789abcdef0",
                "absent.flow.logs",
            ),
            (
                "a delivery stream whose name carries underscores",
                "tgw-0123456789abcdef0",
                "absent_stream_2026",
            ),
        ];
        for (case, resource_id, log_destination) in cases {
            let item = flow_log_destination_absent_item(resource_id, log_destination);
            let error = item.error.expect("the item reports an error");
            assert_eq!(
                error.code.as_deref(),
                Some(FLOW_LOG_UNSUCCESSFUL_ERROR_CODE),
                "case {case:?}: expected the per-resource failure code, got {:?}",
                error.code
            );
            let expected = format!("LogDestination: {log_destination} does not exist");
            assert_eq!(
                error.message.as_deref(),
                Some(expected.as_str()),
                "case {case:?}: expected the message {expected:?}, got {:?}",
                error.message
            );
            assert_eq!(
                item.resource_id.as_deref(),
                Some(resource_id),
                "case {case:?}: expected the monitored resource as resourceId, got {:?}",
                item.resource_id
            );
        }
    }
}
