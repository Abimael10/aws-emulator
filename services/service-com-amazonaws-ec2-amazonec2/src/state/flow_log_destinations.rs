// SPDX-License-Identifier: CC0-1.0

//! WHERE a flow log publishes: which service's ARN each `LogDestinationType` names, the
//! reading of a destination against the type the request asked for, and the refusal for a
//! destination that is not one of that type.
//!
//! "The meaning of this parameter depends on the destination type. If the destination type
//! is `cloud-watch-logs`, specify the ARN of a CloudWatch Logs log group … If the
//! destination type is `s3`, specify the ARN of an S3 bucket … If the destination type is
//! `kinesis-data-firehose`, specify the ARN of a Kinesis Data Firehose delivery stream"
//! (`CreateFlowLogsRequest$LogDestination`): the member takes ONE service's ARNs, and which
//! service that is comes from `LogDestinationType`. So a destination naming any other
//! service names nothing the destination type could deliver to, and the create is refused
//! rather than a flow log stored whose records nothing could receive.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::*;

/// The service segment of `arn` — the third of an ARN's colon-separated parts — or `None`
/// when the value is not an ARN at all.
///
/// An ARN is `arn:<partition>:<service>:<region>:<account>:<resource>`, and the resource is
/// the only part that may itself carry a `:`, so the service is read by position from the
/// front and nothing after it is parsed. This answers WHICH service a value names, which is
/// all the destination rule below asks of it.
pub fn arn_service_segment(arn: &str) -> Option<&str> {
    let mut segments = arn.splitn(4, ':');
    (segments.next()? == "arn").then_some(())?;
    // The partition, which the destination rule does not read.
    segments.next()?;
    let service = segments.next()?;
    (!service.is_empty()).then_some(service)
}

/// The service whose ARNs the destination type `log_destination_type` names — as the ARN's
/// own service segment spells it — or `None` for a value that is no documented destination
/// type.
///
/// One entry per member of `LogDestinationType`
/// (`smithy_query(query="shape",shape="LogDestinationType")`: `cloud-watch-logs` | `s3` |
/// `kinesis-data-firehose`), each taking the ARN that member's documentation gives
/// (`CreateFlowLogsRequest$LogDestination`: `arn:aws:logs:region:account_id:log-group:my_group`,
/// `arn:aws:s3:::my_bucket/my_subfolder/`,
/// `arn:aws:firehose:region:account_id:deliverystream:my_stream`).
pub fn flow_log_destination_arn_service(log_destination_type: &str) -> Option<&'static str> {
    match log_destination_type {
        LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS => Some("logs"),
        LOG_DESTINATION_TYPE_S3 => Some("s3"),
        LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE => Some("firehose"),
        _ => None,
    }
}

/// Whether `log_destination` is an ARN of the destination the type `log_destination_type`
/// publishes to.
///
/// The ARN has to name that type's own service ([`flow_log_destination_arn_service`]) AND the
/// RESOURCE of it that the type publishes to: a log group is read out of a CloudWatch Logs
/// ARN ([`log_group_name_in_arn`], because the group's name is what the flow log reports), an
/// Amazon S3 ARN has to name a location Amazon S3 could hold ([`flow_log_s3_destination`],
/// the bucket and the optional subfolder the records are delivered under), and a Firehose ARN
/// has to name a delivery stream ([`FIREHOSE_DELIVERY_STREAM_RESOURCE`], the resource
/// `CreateFlowLogsRequest$LogDestination` gives for that type). A destination the delivery
/// could not be made to is not a destination. A value under a type this service does not
/// document is left to the member's own enum validation, which refuses the TYPE.
pub fn flow_log_destination_names_its_type(
    log_destination_type: &str,
    log_destination: &str,
) -> bool {
    let Some(service) = flow_log_destination_arn_service(log_destination_type) else {
        return true;
    };
    if arn_service_segment(log_destination) != Some(service) {
        return false;
    }
    match log_destination_type {
        LOG_DESTINATION_TYPE_S3 => flow_log_s3_destination(log_destination).is_some(),
        LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS => log_group_name_in_arn(log_destination).is_some(),
        LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE => {
            firehose_delivery_stream_in_arn(log_destination).is_some()
        }
        _ => true,
    }
}

/// The resource a Firehose destination's ARN names: "specify the ARN of a Kinesis Data
/// Firehose delivery stream. For example:
/// `arn:aws:firehose:region:account_id:deliverystream:my_stream`"
/// (`CreateFlowLogsRequest$LogDestination`).
const FIREHOSE_DELIVERY_STREAM_RESOURCE: &str = "deliverystream";

/// The NAME of the delivery stream the Firehose ARN `arn` names, or `None` when it names no
/// delivery stream at all — its resource part is [`FIREHOSE_DELIVERY_STREAM_RESOURCE`]
/// followed by one of the two separators an AWS ARN writes a resource's name after (`:` as
/// that member's example spells it, `/` as the service itself spells its stream ARNs), and
/// then a name.
///
/// The NAME rather than a yes/no, because that is what Firehose itself holds a stream under:
/// the same reading answers whether the destination is one of this type
/// ([`flow_log_destination_names_its_type`]) and what to ask Firehose about
/// ([`super::absent_flow_log_destination`]), the way [`log_group_name_in_arn`] serves both
/// for the other destination type named by an ARN.
pub fn firehose_delivery_stream_in_arn(arn: &str) -> Option<&str> {
    // Six parts: the `arn` literal, the partition, the service, the Region, the account and
    // the resource — the only part that may itself carry a separator.
    let resource = arn.splitn(6, ':').nth(5)?;
    let name = resource
        .strip_prefix(FIREHOSE_DELIVERY_STREAM_RESOURCE)
        .and_then(|rest| rest.strip_prefix(':').or_else(|| rest.strip_prefix('/')))?;
    (!name.is_empty()).then_some(name)
}

/// Refuse a `LogDestination` that is not an ARN of the destination its `LogDestinationType`
/// publishes to: HTTP 400 [`INVALID_PARAMETER`], with the sentence a recorded refusal of this
/// class pins — the fixed `Invalid `, the destination TYPE the request asked for, the fixed
/// ` destination: `, and the destination the request sent.
///
/// The type is what the sentence names because the type is what decides which ARNs the
/// member takes: the same ARN is a destination under one type and no destination at all
/// under another. The echo is bounded the way every refusal of this crate bounds a value the
/// caller chose the length of ([`echoed_resource_id`]).
pub fn invalid_flow_log_destination<E>(
    log_destination_type: &str,
    log_destination: &str,
) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_PARAMETER,
        400,
        format!(
            "Invalid {log_destination_type} destination: {destination}",
            destination = echoed_resource_id(log_destination),
        ),
    )
}

/// Refuse a member the destination type in force does not take at all: HTTP 400
/// [`INVALID_PARAMETER`], with the sentence a recorded refusal of this class pins — the
/// member's own name, the fixed ` is not supported when LogDestinationType is `, and the
/// destination type the request asked for.
///
/// Each destination type takes its own members: a log group is named only for CloudWatch Logs
/// ("This parameter is valid only if the destination type is `cloud-watch-logs`" —
/// `CreateFlowLogsRequest$LogGroupName`), and the destination options describe the objects an
/// Amazon S3 delivery writes (`DestinationOptionsRequest`: a file format, Hive-compatible
/// prefixes and per-hour partitioning, all of them properties of the delivered OBJECTS). So a
/// request carrying a member its destination type has no use for is refused rather than served
/// a flow log that would ignore it. `member` is the member as the REQUEST names it, which is
/// how the sentence names it; the type is the one in force, so a request that named none is
/// told the default it got.
pub fn flow_log_member_unsupported_for_destination<E>(
    member: &str,
    log_destination_type: &str,
) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_PARAMETER,
        400,
        format!("{member} is not supported when LogDestinationType is {log_destination_type}"),
    )
}

/// Whether the request's two delivery-role members name CROSS-ACCOUNT delivery without
/// giving it both halves, for the destination type `log_destination_type` in force.
///
/// "The ARN of the IAM role that allows Amazon EC2 to publish flow logs across accounts"
/// (`CreateFlowLogsRequest$DeliverCrossAccountRole`) and "the ARN of the IAM role that allows
/// Amazon EC2 to publish flow logs to the log destination. This parameter is required if the
/// destination type is `cloud-watch-logs`, or if the destination type is
/// `kinesis-data-firehose` and the delivery stream and the resources to monitor are in
/// different accounts" (`$DeliverLogsPermissionArn`): the roles a cross-account delivery needs
/// are a PAIR — one says which role may publish to the destination, the other which role the
/// monitored account is reached through — so a request that names one of them alone asks for a
/// delivery it has given only half the standing for.
///
/// The one destination type whose delivery takes `DeliverLogsPermissionArn` on its own is
/// `cloud-watch-logs`, where that member is mandatory whether or not the delivery crosses
/// accounts ([`super::flow_log_delivery_role_required`]); under every other type the member
/// stands for cross-account delivery alone, so naming it there without the cross-account role
/// is the same half-given request as naming the cross-account role without it.
pub fn flow_log_cross_account_role_is_incomplete(
    log_destination_type: &str,
    deliver_logs_permission_arn: Option<&str>,
    deliver_cross_account_role: Option<&str>,
) -> bool {
    let names_destination_role = deliver_logs_permission_arn.is_some();
    let names_cross_account_role = deliver_cross_account_role.is_some();
    // The destination type that takes the destination role on its own: a request naming only
    // that member under it asks for the same-account delivery that member is required for,
    // rather than half a cross-account one.
    let role_stands_alone = log_destination_type == LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS;
    match (names_destination_role, names_cross_account_role) {
        (true, false) => !role_stands_alone,
        (false, true) => true,
        _ => false,
    }
}

/// Refuse a `CreateFlowLogs` that named one of the two cross-account delivery roles without
/// the other ([`flow_log_cross_account_role_is_incomplete`]): HTTP 400 [`INVALID_PARAMETER`],
/// with the sentence a recorded refusal of this class pins — wholly fixed text that names both
/// modelled members, with no part taken from the request.
pub fn flow_log_cross_account_role_pair_required<E>() -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_PARAMETER,
        400,
        "When providing a role for cross-account delivery, both the DeliverLogsPermissionArn \
         and DeliverCrossAccountRole fields must be set.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each destination type takes its own service's ARNs and nothing else.
    #[test]
    fn a_destination_is_read_against_the_type_the_request_asked_for() {
        const GROUP_ARN: &str = "arn:aws:logs:us-west-2:000000000000:log-group:flow-logs";
        const BUCKET_ARN: &str = "arn:aws:s3:::flow-logs-bucket/vpc/";
        const STREAM_ARN: &str = "arn:aws:firehose:us-west-2:000000000000:deliverystream:flow";
        // (case, destination type, destination, whether it names that type's destination)
        let cases: [(&str, &str, &str, bool); 14] = [
            ("a log group for cloud-watch-logs", LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS, GROUP_ARN, true),
            ("a bucket for cloud-watch-logs", LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS, BUCKET_ARN, false),
            ("a stream for cloud-watch-logs", LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS, STREAM_ARN, false),
            ("a bucket for s3", LOG_DESTINATION_TYPE_S3, BUCKET_ARN, true),
            ("a bucket with no subfolder for s3", LOG_DESTINATION_TYPE_S3, "arn:aws:s3:::flow-logs-bucket", true),
            ("a log group for s3", LOG_DESTINATION_TYPE_S3, GROUP_ARN, false),
            ("a bucket name Amazon S3 could not hold, for s3", LOG_DESTINATION_TYPE_S3, "arn:aws:s3:::NotABucket", false),
            ("a stream for kinesis-data-firehose", LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE, STREAM_ARN, true),
            (
                "a stream whose ARN separates its name with a slash",
                LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE,
                "arn:aws:firehose:us-west-2:000000000000:deliverystream/flow",
                true,
            ),
            ("a bucket for kinesis-data-firehose", LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE, BUCKET_ARN, false),
            (
                "a Firehose ARN naming another resource",
                LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE,
                "arn:aws:firehose:us-west-2:000000000000:destination:flow",
                false,
            ),
            (
                "a delivery stream with no name",
                LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE,
                "arn:aws:firehose:us-west-2:000000000000:deliverystream:",
                false,
            ),
            ("no ARN at all", LOG_DESTINATION_TYPE_S3, "flow-logs-bucket", false),
            ("a group name for cloud-watch-logs", LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS, "flow-logs", false),
        ];
        for (case, log_destination_type, log_destination, expected) in cases {
            let actual =
                flow_log_destination_names_its_type(log_destination_type, log_destination);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected}, got {actual} for {log_destination:?}"
            );
        }
    }

    /// The service segment is read by position, and only out of a value that is an ARN.
    #[test]
    fn the_service_segment_is_the_third_part_of_an_arn() {
        // (case, value, the service segment it carries)
        let cases: [(&str, &str, Option<&str>); 5] = [
            ("a log group ARN", "arn:aws:logs:us-west-2:0:log-group:g", Some("logs")),
            ("an Amazon S3 ARN", "arn:aws:s3:::bucket", Some("s3")),
            ("another partition", "arn:aws-us-gov:s3:::bucket", Some("s3")),
            ("no service segment", "arn:aws::::", None),
            ("not an ARN", "logs:us-west-2", None),
        ];
        for (case, value, expected) in cases {
            let actual = arn_service_segment(value);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
