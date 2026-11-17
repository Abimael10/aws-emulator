// SPDX-License-Identifier: CC0-1.0

//! The ONE construction of the in-process Amazon SQS client an emulated service DELIVERS a
//! message through, so no two services configure that hop differently.
//!
//! A service that sends a message to a queue on a caller's behalf — the AWS IoT rules engine's
//! `sqs` topic-rule action today — reaches Amazon SQS the way any emulated service reaches
//! another: the AWS SDK driven over the emulator's own in-process transport, through the
//! operation callback ([`smithy_server_runtime::OperationContext::callback`]). The transport is
//! what makes the hop in-process: no socket is opened, no name is resolved, and nothing outside
//! this process is reachable from it. The endpoint URL below is addressing for that transport,
//! not a host this process connects to, and it is built from the Region alone — no part of a
//! caller's request is a destination.
//!
//! Built exactly as [`super::kinesis_client`] builds the Kinesis one, and for the same reason.
//! The copies that predate this module — the `callback_client!` macro in
//! `service-com-amazonaws-eventbridge-awsevents`'s `ops::put_events` and its siblings in the
//! AppConfig, EventBridge Scheduler, AWS Config, Amazon API Gateway, Amazon SNS and
//! EventBridge v2 crates — are to be MIGRATED onto it, each in the change that owns its
//! service. Nothing new should build the client by hand.

/// The Amazon SQS client a service delivers a message through, addressed to `region`.
///
/// The transport is in-process, so there is nothing to retry or time out — each of those would
/// need a sleep implementation this client is not given — and the credentials the callback
/// hands over are used directly rather than cached. Those credentials are the emulator's own:
/// the caller's `Authorization` header is never forwarded.
#[must_use]
pub fn sqs_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_sqs::Client {
    let config = ::aws_sdk_sqs::Config::builder()
        .behavior_version(::aws_sdk_sqs::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_sqs::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://sqs.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_sqs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_sqs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_sqs::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_sqs::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_sqs::Client::from_conf(config)
}

/// The ARN of the queue the URL `queue_url` addresses, in `region` — the resource a statement
/// about that queue names (`arn:aws:sqs:<region>:<account>:<queueName>`).
///
/// A queue is ADDRESSED by its URL, whose last path segment is the queue's name and whose
/// preceding one is the account that owns it, so both are read off the URL the caller
/// configured rather than from a record: a decision about a queue must not depend on whether
/// that queue exists. `None` for a URL carrying no queue name at all.
///
/// This is deliberately NOT the reading Amazon SQS's own crate makes of a queue URL
/// (`service-com-amazonaws-sqs-amazonsqs`'s `state::queue_name_from_url`, which RESOLVES a URL
/// to a queue of this account and answers `None` for any other account): that one is a lookup
/// and this one is an ARN, and the two differ on exactly the case that difference is about — a
/// URL naming another account's queue gets the ARN of that account's queue here, because the
/// statement a role carries about it is written against that ARN whether or not this emulator
/// holds the queue. It cannot call the owning crate either: that crate depends on this one.
#[must_use]
pub fn queue_arn_of_url(region: &str, queue_url: &str) -> Option<String> {
    let path = super::request_path(queue_url);
    let mut segments = path.trim_matches('/').rsplit('/');
    let name = segments.next().filter(|name| !name.is_empty())?;
    // The account segment is the queue owner's, which is the account segment of its ARN; a URL
    // carrying none names a queue of this emulator's own account.
    let account = match segments.next().filter(|account| !account.is_empty()) {
        Some(account) => account,
        None => ::engine::emulator_account_id(),
    };
    Some(format!("arn:aws:sqs:{region}:{account}:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the ARN of a queue is read off the URL that addresses it — the queue's name from
    /// the last path segment and its owner from the one before — so one written by any service
    /// of this emulator names the same queue a statement about it does.
    ///
    /// The rows differ along the form the URL arrives in: the absolute form a client is handed,
    /// the origin form, one carrying a trailing slash, one carrying no account segment, and one
    /// carrying no queue name at all.
    #[test]
    fn a_queues_arn_is_read_off_the_url_that_addresses_it() {
        let account = ::engine::emulator_account_id();
        let rows: &[(&str, &str, Option<String>)] = &[
            (
                "the absolute form a client is handed",
                "https://sqs.us-east-1.amazonaws.com/000000000000/orders",
                Some("arn:aws:sqs:us-east-1:000000000000:orders".to_string()),
            ),
            (
                "one whose owner is another account",
                "https://sqs.us-east-1.amazonaws.com/111111111111/orders.fifo",
                Some("arn:aws:sqs:us-east-1:111111111111:orders.fifo".to_string()),
            ),
            (
                "the origin form",
                "/000000000000/orders",
                Some("arn:aws:sqs:us-east-1:000000000000:orders".to_string()),
            ),
            (
                "one carrying a trailing slash",
                "https://sqs.us-east-1.amazonaws.com/000000000000/orders/",
                Some("arn:aws:sqs:us-east-1:000000000000:orders".to_string()),
            ),
            (
                "one carrying a query string",
                "https://sqs.us-east-1.amazonaws.com/000000000000/orders?x=1",
                Some("arn:aws:sqs:us-east-1:000000000000:orders".to_string()),
            ),
            (
                "one carrying no account segment",
                "https://sqs.us-east-1.amazonaws.com/orders",
                Some(format!("arn:aws:sqs:us-east-1:{account}:orders")),
            ),
            ("one carrying no queue name at all", "", None),
            ("the root path", "/", None),
        ];
        for (case, url, expected) in rows {
            let actual = queue_arn_of_url("us-east-1", url);
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected {url:?} to name {expected:?}, got {actual:?}"
            );
        }
    }
}
