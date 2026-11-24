// SPDX-License-Identifier: CC0-1.0

//! The TEST LOG EVENTS a create writes into the destination of every log type it enables.
//!
//! Configuring a log type's publishing is a configuration of a group the ACCOUNT owns, and
//! the service proves the wiring as it resolves it: it opens a test stream in the group and
//! writes one test event there, so a client that enabled publishing can see straight away
//! whether the destination really receives what the domain will publish. That is why a group
//! granting the service nothing is refused ([`super::log_destinations`]) — the probe is the
//! same write the refusal is about.
//!
//! Two probes are written per destination, because two different writers reach the group:
//!
//! * [`CALLER_PROBE_STREAM`] — the stream written under the identity that asked for the
//!   publishing, which the account's own group always admits, so it lands in every
//!   destination that exists;
//! * [`SERVICE_PROBE_STREAM`] — the stream written under this service's own principal
//!   ([`super::log_destinations::SERVICE_PRINCIPAL`]), which lands ONLY where the group's
//!   resource policy lets that principal open a stream and put events
//!   ([`super::log_destinations::admits_delivery`]).
//!
//! So a granted group holds both streams and an ungranted one only the caller's: the whole
//! point of the probe is that the two are distinguishable, and the create whose destination
//! withholds the grant is refused with the second write never made.
//!
//! Each probe carries exactly one event, whose message is the service's own fixed sentence
//! for it ([`PROBE_MESSAGE`]); it is the service announcing that it created a test log event,
//! so it carries no part of the request.
//!
//! The records are built here and DELIVERED through the one path a domain's published records
//! take ([`super::log_publishing::deliver`]), so a probe is stored, masked, transformed and
//! bounded exactly as every other record that group takes in. Built while no store is held
//! and delivered with this service's store released, by the one lock order every handler
//! keeps.

use ::std::collections::BTreeSet;
use ::std::time::SystemTime;

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;

use super::log_destinations::{admits_delivery, enabled_destinations};
use super::log_publishing::PublishedRecord;
use crate::ops::types::LogPublishingOption;

/// The stream the probe written under the CALLER's identity goes to, as the service names
/// it: the `cust` probe, which every destination the account holds takes.
pub const CALLER_PROBE_STREAM: &str = "cust-test-log-stream";

/// The stream the probe written under this service's OWN principal goes to, as the service
/// names it: the `es` probe, which only a group whose resource policy admits that principal
/// takes.
pub const SERVICE_PROBE_STREAM: &str = "es-test-log-stream";

/// The one event each probe stream carries: the service's own fixed sentence saying it wrote
/// a test log event. It states what happened and nothing of the request, so it reads the same
/// in every destination.
pub const PROBE_MESSAGE: &str = "Amazon OpenSearch Service created a test log event";

/// The probe records the destinations of `options`' enabled entries take, at `at`.
///
/// One [`CALLER_PROBE_STREAM`] record per destination the account holds, and one
/// [`SERVICE_PROBE_STREAM`] record per destination whose resource policy lets this service
/// deliver into it. Empty for a request that enables no log type, for one whose destinations
/// name no group this account holds, and where the engine hands out no CloudWatch Logs store
/// — nothing can be probed in a store that was never readable.
///
/// `region` is the Region the request was signed for, which is the Region the probe streams
/// are stamped with: the destination groups are that Region's own.
pub fn destination_probes<'a, Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    options: impl IntoIterator<Item = (&'a String, &'a LogPublishingOption)>,
    at: SystemTime,
) -> Vec<PublishedRecord> {
    let destinations = enabled_destinations(options);
    if destinations.is_empty() {
        return Vec::new();
    }
    let Some(state) = logs::service_state(engine) else {
        return Vec::new();
    };
    let store = state.lock();
    let mut probes = Vec::new();
    // The destinations one request names are not distinct: several log types may publish to
    // ONE group, and that group is one destination, probed once.
    let mut probed: BTreeSet<(&str, &str)> = BTreeSet::new();
    for (arn, name) in destinations {
        // A group the account does not hold under that name is no destination at all, so
        // there is nothing there to probe; the create's own refusal is what reports it.
        if store
            .log_groups
            .get(name)
            .is_none_or(|group| group.log_group_arn != arn)
        {
            continue;
        }
        let service_admitted = admits_delivery(&store, arn, name);
        for stream in [CALLER_PROBE_STREAM, SERVICE_PROBE_STREAM] {
            if stream == SERVICE_PROBE_STREAM && !service_admitted {
                continue;
            }
            // The probe is the PROOF that this destination takes that writer's events, and
            // it is written when the probe stream is opened: a group already holding the
            // test event in that stream has been probed, so nothing is written there again
            // — the proof a reader finds is one test event per probe stream, not one per
            // call that configured the destination.
            if already_probed(&store, name, stream) || !probed.insert((name, stream)) {
                continue;
            }
            probes.push(PublishedRecord {
                log_group_name: name.to_string(),
                log_group_arn: arn.to_string(),
                log_stream_name: stream.to_string(),
                region: region.to_string(),
                message: PROBE_MESSAGE.to_string(),
                at,
            });
        }
    }
    probes
}

/// Whether the group stored under `log_group_name` already holds this service's test event in
/// its `log_stream_name` stream — the one thing that says the destination has been probed for
/// that writer already.
fn already_probed(
    store: &logs::ComAmazonawsCloudwatchlogsLogs20140328State,
    log_group_name: &str,
    log_stream_name: &str,
) -> bool {
    store
        .log_groups
        .get(log_group_name)
        .and_then(|group| group.streams.get(log_stream_name))
        .is_some_and(|stream| {
            stream
                .events
                .iter()
                .any(|event| event.message == PROBE_MESSAGE)
        })
}
