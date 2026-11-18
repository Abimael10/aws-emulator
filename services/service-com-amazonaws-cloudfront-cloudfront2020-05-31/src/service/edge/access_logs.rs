// SPDX-License-Identifier: CC0-1.0

//! STANDARD LOGGING v2: the ACCESS-LOG RECORD one served viewer request delivers to each
//! destination a vended-log delivery points the distribution at.
//!
//! # The rule this module implements
//!
//! Standard logging v2 is configured in CloudWatch Logs rather than in CloudFront: a client
//! creates a delivery SOURCE for the distribution — `PutDeliverySource` with the
//! distribution's ARN and the one log type CloudFront sends, [`ACCESS_LOGS_LOG_TYPE`] ("For
//! CloudFront, the valid value is ACCESS_LOGS",
//! `smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`) — a delivery
//! DESTINATION naming the resource the logs are sent to, and a DELIVERY pairing the two. From
//! then on CloudFront is the SENDER of that pipeline, so every viewer request the edge serves
//! for that distribution produces ONE access-log record in EVERY destination its deliveries
//! name, read back the way a consumer of that kind of destination reads it.
//!
//! Which deliveries carry a distribution's records, where each writes them and under which
//! record layout are CloudWatch Logs' own records, so they are asked of that service
//! (`crate::state::vended_log_dispatch` of the CloudWatch Logs emulator, over the store both
//! services share) instead of being tracked a second time here. That reading covers EVERY kind
//! of destination a delivery can name — a log group, an S3 bucket or a Firehose delivery
//! stream — so a distribution's records fan out to all of them, and a distribution no delivery
//! names produces nothing, which is exactly what a client that wired no pipeline sees.
//!
//! # What one record says
//!
//! The record carries the delivery's own record fields, in the delivery's own order and in its
//! destination's output format — all three the delivery's to state. Every VALUE is read off the
//! request the edge served, the answer it served and the distribution that served it, by the
//! one reading of a CloudFront log field this service has
//! ([`super::realtime_logs::LoggedRequest`]): the two planes report one request the same way,
//! so a standard log record and a real-time log record of the same request cannot disagree.
//!
//! Two fields the standard access log names are not real-time fields and are read here: `date`
//! ("the date on which the event occurred", in UTC) and `time` ("the time when the CloudFront
//! server finished responding to the request", in UTC) — the two halves of the instant the
//! edge finished responding, which is the instant the real-time record's `timestamp` carries.
//!
//! Delivery is BEST EFFORT, as it is for every vended-logs destination: the viewer has already
//! been answered, so a destination the account has since deleted or a write the owning service
//! refuses loses the record and is reported in tracing rather than failing the request.

use std::time::SystemTime;

use smithy_server_runtime::OperationContext;

use super::realtime_logs::LoggedRequest;

/// The one `logType` a CloudFront delivery source sends: "For CloudFront, the valid value is
/// ACCESS_LOGS" (`PutDeliverySource$logType`'s own catalogue).
const ACCESS_LOGS_LOG_TYPE: &str = "ACCESS_LOGS";

/// The standard access-log field naming the DATE the request was served on: "the date on which
/// the event occurred in Coordinated Universal Time (UTC), for example, 2019-06-26" — the day
/// of the instant the edge finished responding.
const DATE_FIELD: &str = "date";

/// The standard access-log field naming the TIME the request was served at: "the time when the
/// CloudFront server finished responding to the request (in UTC), for example, 01:42:39".
const TIME_FIELD: &str = "time";

/// How many destinations ONE served viewer request delivers its access-log record to.
///
/// THE EMULATOR'S OWN CEILING, not a quota cited from AWS: each delivery of the distribution's
/// pipeline costs one in-process write while the edge holds the answered request, and the
/// deliveries an account may create are bounded only by the CloudWatch Logs store that holds
/// them — so without this, one viewer request could make as many writes as there are deliveries.
/// It sits far above any realistic pipeline (a distribution delivering its access logs to one or
/// a few destinations), and a request that exhausts it stops delivering and says so in the log
/// rather than writing more.
const MAX_ACCESS_LOG_DELIVERIES_PER_REQUEST: usize = 32;

/// Deliver the access-log record of one served viewer request to every destination a delivery
/// carries this distribution's access logs into.
///
/// Called from the ONE seam that serves the edge with the answer in hand: a record describes a
/// request that was ANSWERED, so its status, its size and the span it took are part of it and
/// cannot be known earlier. Called with this service's store RELEASED — it awaits the write
/// into the service that owns each destination.
///
/// Nothing here can fail the request: the viewer has already been answered.
pub(super) async fn deliver<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    distribution_id: &str,
    logged: &LoggedRequest<'_>,
) {
    let resource_arn = crate::state::distribution_arn(distribution_id);
    let deliveries = ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::vended_log_deliveries(
        engine,
        &resource_arn,
        ACCESS_LOGS_LOG_TYPE,
    );
    for delivery in deliveries.iter().take(MAX_ACCESS_LOG_DELIVERIES_PER_REQUEST) {
        let record = delivery.record(|field| field_value(logged, field));
        let written = ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::deliver_record(
            ctx,
            delivery,
            logged.at,
            &record,
        )
        .await;
        if !written {
            ::tracing::debug!(
                delivery = %delivery.delivery_id,
                "the destination this delivery names did not take the access-log record"
            );
        }
    }
    if deliveries.len() > MAX_ACCESS_LOG_DELIVERIES_PER_REQUEST {
        ::tracing::warn!(
            deliveries = MAX_ACCESS_LOG_DELIVERIES_PER_REQUEST,
            "this request's access-log deliveries are spent, so the remaining deliveries of this \
             distribution carry no record for it"
        );
    }
}

/// What this served request states for ONE record field.
///
/// Everything but the two calendar halves of the serving instant is the real-time plane's own
/// reading of the field ([`LoggedRequest::value`]), including the placeholder a field this
/// request has no value for carries: one request is reported identically whichever log plane
/// reports it.
fn field_value(logged: &LoggedRequest<'_>, field: &str) -> String {
    match field {
        DATE_FIELD => calendar_halves(logged.at).0,
        TIME_FIELD => calendar_halves(logged.at).1,
        other => logged.value(other),
    }
}

/// The date and the time-of-day of `at`, in UTC, as the two fields that report them carry them:
/// `2019-06-26` and `01:42:39`.
///
/// Read from ONE rendering of the instant — the whole-second UTC `date-time` form this
/// workspace's one timestamp renderer produces — so the two halves can never describe different
/// instants.
fn calendar_halves(at: SystemTime) -> (String, String) {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default();
    let stamp = ::aws_smithy_types::DateTime::from_secs(seconds)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default();
    let (date, rest) = stamp.split_once('T').unwrap_or_default();
    (
        date.to_string(),
        rest.trim_end_matches('Z').to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_calendar_halves_are_the_two_halves_of_one_utc_instant() {
        for (case, seconds, date, time) in [
            ("the epoch itself", 0_u64, "1970-01-01", "00:00:00"),
            (
                "a time of day with no leading zeros",
                1_561_515_759,
                "2019-06-26",
                "02:22:39",
            ),
            ("the last second of a year", 1_767_225_599, "2025-12-31", "23:59:59"),
            ("a leap day", 1_709_164_800, "2024-02-29", "00:00:00"),
        ] {
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
            let (got_date, got_time) = calendar_halves(at);
            assert_eq!(
                (got_date.as_str(), got_time.as_str()),
                (date, time),
                "case {case}: expected {date:?} and {time:?} for {seconds} seconds, got \
                 {got_date:?} and {got_time:?}"
            );
        }
    }

    #[test]
    fn a_sub_second_instant_reports_the_whole_second_it_falls_in() {
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_561_515_759_750);
        let (date, time) = calendar_halves(at);
        assert_eq!(
            (date.as_str(), time.as_str()),
            ("2019-06-26", "02:22:39"),
            "case a fraction of a second past the instant: expected the whole second, got \
             {date:?} and {time:?}"
        );
    }
}
