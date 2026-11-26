// SPDX-License-Identifier: CC0-1.0

//! Whether the logging destination a web ACL is POINTED AT is one WAF can resolve, asked
//! before a `LoggingConfiguration` naming it is stored.
//!
//! # The rule
//!
//! A `LoggingConfiguration` "defines an association between logging destinations and a web
//! ACL resource, for logging from WAF", and `PutLoggingConfiguration` "enables the
//! specified LoggingConfiguration, to start logging from a web ACL" — so the destination
//! is one WAF has to deliver into from the moment the put is accepted, and WAF resolves it
//! BEFORE it stores anything: a configuration kept against a destination nothing holds
//! would report logging that is on while no record is ever written.
//!
//! WAF resolves it in ONE Region: the Region the web ACL itself lives in, which for a
//! CLOUDFRONT-scoped web ACL is the single Region that scope is served at and for a
//! REGIONAL one is that web ACL's own Region. A destination ARN naming any other Region
//! therefore addresses a destination WAF never looks for, so it does not exist as far as
//! this web ACL is concerned — nothing needs to be asked to establish that. A destination
//! whose own ARN form carries no Region (an Amazon S3 bucket, addressed in a global
//! namespace) names no Region to disagree, and is resolved in the web ACL's.
//!
//! # How the question is asked
//!
//! Each destination kind is resolved with the service that OWNS it, by that service's own
//! read, over the in-process callback into this same emulator — the one existence reading
//! this tree holds per kind ([`::service_common::cloudwatch_logs::log_group_is_absent`],
//! [`::service_common::firehose_client::delivery_stream_is_absent`],
//! [`::service_common::s3_client::bucket_is_absent`]), for the same destination kinds the
//! delivery path writes into ([`super::Destination`]). No socket is opened and nothing
//! leaves the process.
//!
//! # What an unanswered question means
//!
//! A destination is reported ABSENT only where something established that it is not
//! there: the owning service answering that it holds none, or the destination being
//! addressed in another Region. Every other answer leaves the configuration alone — the
//! destination being held, a destination of no kind WAF delivers to, and a hop nothing
//! answered at all, which is what an emulator that does not mount the owning service
//! answers, there being no one to ask. So no configuration is refused for a destination
//! nobody was asked about, and nothing is invented either way.

use super::delivered_streams::{DELIVERY_STREAM_RESOURCE_TYPE, FIREHOSE_VENDOR};
use super::S3_VENDOR;

/// What a configured destination ADDRESSES: the kind of destination, and the name the
/// service that owns it holds it under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressed<'destination> {
    /// A CloudWatch Logs log group, named by either published form of its ARN.
    LogGroup(&'destination str),
    /// An Amazon Data Firehose delivery stream.
    DeliveryStream(&'destination str),
    /// An Amazon S3 bucket, whose ARN's resource field IS the bucket's own name.
    Bucket(&'destination str),
}

/// What `configured` addresses, or `None` when it names none of the destination kinds
/// `PutLoggingConfiguration` publishes ("you can use an Amazon CloudWatch Logs log group,
/// an Amazon Simple Storage Service (Amazon S3) bucket, or an Amazon Kinesis Data
/// Firehose").
///
/// The ONE reading of a configured destination this crate holds: the resolution below asks
/// the owning service about what this answers, and the delivery that writes a record
/// addresses the destination it answers ([`super::Destination`]), so the destinations WAF
/// resolves and the destinations it delivers to cannot come apart. A log group is read by
/// the one reading of such an ARN this tree shares
/// ([`::service_common::cloudwatch_logs::log_group_of_arn`], which admits the group on its
/// own and the group with everything under it); a delivery stream by the
/// `deliverystream/<name>` resource of a `firehose` ARN; a bucket by an ARN whose resource
/// field is the bucket's own name and nothing else.
pub fn addressed(configured: &str) -> Option<Addressed<'_>> {
    if let Some(log_group) = ::service_common::cloudwatch_logs::log_group_of_arn(configured) {
        return Some(Addressed::LogGroup(log_group));
    }
    if ::service_common::arn::vendor(configured) == Some(FIREHOSE_VENDOR) {
        return ::service_common::arn::resource_id_of_type(configured, DELIVERY_STREAM_RESOURCE_TYPE)
            .map(Addressed::DeliveryStream);
    }
    let bucket = ::service_common::arn::parse_of(configured, S3_VENDOR)?.resource;
    (!bucket.contains(['/', ':'])).then_some(Addressed::Bucket(bucket))
}

/// How many destinations ONE configuration is resolved against at most, which bounds the
/// resolutions a single request can ask for.
///
/// The figure is the ceiling the model puts on the list itself —
/// `LogDestinationConfigs` is `@length(1..100)`
/// (`smithy_query(query="shape",shape="LogDestinationConfigs")`) — so a configuration WAF
/// would accept is never clamped by it; it is stated here, at the resolution, so the work
/// one request causes is bounded where that work is done rather than only at the request
/// boundary that refuses a longer list ([`resolvable`], which also resolves a destination
/// repeated in a list once).
pub const MAX_RESOLVED_DESTINATIONS: usize = 100;

/// The destinations of `configured` to resolve, in the order they are named: each DISTINCT
/// value once, at most [`MAX_RESOLVED_DESTINATIONS`] of them.
///
/// The clamp and the de-duplication both sit ahead of the resolutions, so neither the
/// length of the list nor a value repeated in it can multiply the hops one request makes.
pub fn resolvable(configured: &[String]) -> Vec<&str> {
    let mut resolvable: Vec<&str> = Vec::new();
    for destination in configured.iter().take(MAX_RESOLVED_DESTINATIONS) {
        if !resolvable.contains(&destination.as_str()) {
            resolvable.push(destination);
        }
    }
    resolvable
}

/// Whether `destination` was established NOT to exist for a web ACL whose destinations WAF
/// resolves in `region` — the one outcome a `PutLoggingConfiguration` refuses on (see this
/// module's own docs for what every other answer means).
///
/// Call it with NO state guard held: it awaits.
pub async fn is_absent(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    destination: &str,
    region: &str,
) -> bool {
    let Some(arn) = ::service_common::arn::parse(destination) else {
        // No ARN at all addresses no destination of any service; such a value is the
        // parameter refusal the put makes about it before anything is resolved.
        return false;
    };
    // The Region WAF resolves this web ACL's destinations in is the only one it looks in,
    // so a destination addressed in another Region is one it does not find.
    if !arn.region.is_empty() && arn.region != region {
        ::tracing::debug!(
            destination_region = ?arn.region,
            web_acl_region = ?region,
            "a logging destination is addressed in another Region than the one WAF \
             resolves this web ACL's destinations in"
        );
        return true;
    }
    // Each kind is resolved with the service that owns it, through the one existence
    // reading this tree holds for that kind; a destination of no kind WAF delivers to has
    // nobody to ask, so nothing is known about it.
    let absent = match addressed(destination) {
        Some(Addressed::LogGroup(log_group)) => {
            ::service_common::cloudwatch_logs::log_group_is_absent(ctx, region, log_group).await
        }
        Some(Addressed::DeliveryStream(stream)) => {
            ::service_common::firehose_client::delivery_stream_is_absent(ctx, region, stream).await
        }
        Some(Addressed::Bucket(bucket)) => {
            ::service_common::s3_client::bucket_is_absent(ctx, region, bucket).await
        }
        None => false,
    };
    ::tracing::debug!(
        vendor = ?arn.vendor,
        absent,
        "the service that owns the logging destination was asked about it"
    );
    absent
}
