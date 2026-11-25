// SPDX-License-Identifier: CC0-1.0

//! The DNS query logging configuration a hosted zone publishes its queries under,
//! and the CloudWatch Logs log group that publishing goes to.
//!
//! `CreateQueryLoggingConfig`'s own documentation states both halves of what such a
//! configuration IS: "Creates a configuration for DNS query logging. After you create
//! a query logging configuration, Amazon Route 53 begins to publish log data to an
//! Amazon CloudWatch Logs log group", and the configuration is "for a specified public
//! hosted zone". So one configuration ties ONE hosted zone to ONE log group, which is
//! exactly the three members `QueryLoggingConfig` carries (`Id`, `HostedZoneId`,
//! `CloudWatchLogsLogGroupArn`) — the configuration is stored as that projection
//! itself ([`super::ComAmazonawsRoute53Awsdnsv20130401State::query_logging_configs`]),
//! because every member of it is persisted verbatim and nothing else about it is
//! derived.
//!
//! The LOG GROUP is not this service's record. It belongs to CloudWatch Logs — the
//! group `CreateLogGroup` creates and `DescribeLogGroups` reports — so the ARN a
//! request names is resolved against that emulator's OWN store through the shared
//! engine, the same way an alarm-reading health check resolves its alarm
//! ([`super::monitored_alarm`]). Nothing is copied into this store: an ARN that names
//! no log group of that store names none for this service either, which is what
//! `NoSuchCloudWatchLogsLogGroup` ("There is no CloudWatch Logs log group with the
//! specified ARN") reports. The read takes that store's lock with this service's own
//! released, so no handler holds two services' locks at once.

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs_state;

use crate::ops::types::create_query_logging_config_types::{
    NoSuchCloudWatchLogsLogGroup, QueryLoggingConfigAlreadyExists,
};
use crate::ops::types::shared_types::NoSuchQueryLoggingConfig;

/// Whether the account holds the CloudWatch Logs log group that `arn` names.
///
/// TWO things make the answer yes, and both are read off the OWNING service: the
/// value has to be a log-group ARN at all (`logs_state::is_log_group_arn`, which
/// admits either form of one and rejects a log STREAM's ARN), and the group it names
/// has to be one that store holds. A CloudWatch Logs emulator that is not mounted
/// beside this service holds no log group, so no ARN resolves — this service never
/// invents the existence of another service's resource.
pub fn holds_log_group<Eng: Send + Sync + 'static>(engine: &Eng, arn: &str) -> bool {
    if !logs_state::is_log_group_arn(arn) {
        return false;
    }
    let Some(state) = logs_state::service_state(engine) else {
        return false;
    };
    let name = logs_state::log_group_name_from_identifier(arn);
    let store = state.lock();
    store.log_groups.contains_key(name)
}

/// The modelled `NoSuchCloudWatchLogsLogGroup` (HTTP 404) for an ARN that names no
/// log group of the account's.
///
/// The ONE place this service words that refusal. Route 53 reports it as the fixed
/// statement about the log group the request named — the caller already holds the ARN
/// it sent, and what it does not know is that nothing answers for it.
pub fn no_such_cloud_watch_logs_log_group() -> NoSuchCloudWatchLogsLogGroup {
    NoSuchCloudWatchLogsLogGroup {
        message: Some("The specified CloudWatch Logs log group doesn't exist.".to_string()),
    }
}

/// The modelled `QueryLoggingConfigAlreadyExists` (HTTP 409) for a zone that already
/// has a query logging configuration.
///
/// The ONE place this service words that refusal, in the terms the error's own
/// documentation states the rule in: "You can create only one query logging
/// configuration for a hosted zone, and a query logging configuration already exists
/// for this hosted zone."
pub fn query_logging_config_already_exists() -> QueryLoggingConfigAlreadyExists {
    QueryLoggingConfigAlreadyExists {
        message: Some(
            "A query logging configuration already exists for this hosted zone".to_string(),
        ),
    }
}

/// The modelled `NoSuchQueryLoggingConfig` (HTTP 404) for an id that names no query
/// logging configuration in this store.
///
/// The ONE place this service words that refusal, so every configuration-addressed
/// operation — the read and the delete — answers a missing configuration identically.
/// Entirely FIXED text, unlike the traffic-policy refusal beside it
/// ([`super::no_such_traffic_policy`], which this service reports by its code alone): it
/// states the fact about the
/// configuration and carries nothing the request named, so a caller learns WHICH id it
/// asked about from its own request.
pub fn no_such_query_logging_config() -> NoSuchQueryLoggingConfig {
    NoSuchQueryLoggingConfig {
        message: Some(NO_SUCH_QUERY_LOGGING_CONFIG_MESSAGE.to_string()),
    }
}

/// The sentence Route 53 refuses a query logging configuration it does not hold with.
pub const NO_SUCH_QUERY_LOGGING_CONFIG_MESSAGE: &str =
    "The query logging configuration does not exist.";

/// The sentence a query logging configuration over a PRIVATE hosted zone is refused
/// with (`InvalidInput`, HTTP 400).
///
/// `CreateQueryLoggingConfigRequest$HostedZoneId` states the rule the refusal reports:
/// "You can log queries only for public hosted zones." A private hosted zone's queries
/// are resolved inside a VPC, so there is nothing for Route 53's edge locations to log.
pub const PUBLIC_HOSTED_ZONE_ONLY_MESSAGE: &str =
    "You can log queries only for public hosted zones.";
