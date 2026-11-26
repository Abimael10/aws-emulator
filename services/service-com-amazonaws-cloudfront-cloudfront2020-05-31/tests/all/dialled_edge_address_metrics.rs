// SPDX-License-Identifier: CC0-1.0

//! What a distribution's PUBLISHED address produces: viewer traffic DIALLED at the
//! `DomainName` a distribution reports, read back through CloudWatch.
//!
//! RULE: **the value a distribution publishes as `Distribution$DomainName` is an address a
//! viewer on the machine this emulator runs on can DIAL, and every answer served at that
//! address is counted into THAT distribution's own `AWS/CloudFront` series — dimensioned by
//! its `DistributionId` beside `Region=Global` — with the default series published for every
//! distribution and the additional ones (the cache hit rate, the origin latency and the
//! per-status-code error rates) published exactly while that distribution's monitoring
//! subscription carries `RealtimeMetricsSubscriptionStatus` `Enabled`; a distribution whose
//! published address nothing dialled has no datapoint in any of them, subscribed or not.**
//!
//! The two halves of that rule are pinned separately by the sibling suites —
//! `published_edge_address` opens a socket to the published address and
//! `edge_additional_metrics` drives the subscription gating through the router — and this
//! suite pins them TOGETHER, over the one addressing a client handed a domain name actually
//! uses: a client that enables a monitoring subscription and then measures the traffic it
//! drives has to dial the name it was told AND read the series that traffic filled, so an
//! address it cannot reach leaves every one of those series empty and the subscription
//! indistinguishable from none.
//!
//! The rows span what the rule generalises over: every value the subscription flag's
//! enumerated type takes (`REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES`) and a distribution
//! carrying no subscription at all, each with a published address of its own, beside a
//! SUBSCRIBED control whose address nothing dials. The traffic driven through each address
//! spans the outcomes an edge answer can have: one an origin produced, the same read again
//! which the edge cache holds, an origin's own 404, and a method the cache behaviour does not
//! allow, which CloudFront refuses itself without reaching an origin.
//!
//! Every expected value is DERIVED from what the dialled answers carried — their status and
//! the `X-Cache` naming who produced them — so no row states a rate of its own.
//!
//! Only the dialled client's own reading is new here; everything else is reused rather than
//! copied. The router carrying this service beside CloudWatch and the series read are
//! [`crate::edge_request_metrics`]'s, the create and the subscription call are
//! [`crate::edge_additional_metrics`]'s, the edge-outcome classification is
//! [`crate::edge_additional_metric_reconciliation`]'s, the viewer dial is
//! [`crate::published_edge_address`]'s, and the deployment poll loop and the dialable-address
//! predicate are [`crate::distribution_lifecycle`]'s.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    awaited_deployment, edge_listener_authority, is_published_edge_address,
};
use crate::edge_additional_metric_reconciliation::{Outcome, CACHE_ERROR, CACHE_HIT, CACHE_MISS};
use crate::edge_additional_metrics::{created, subscribe};
use crate::edge_request_metrics::{emulator_with_engine, held, now_secs, Held};
use crate::published_edge_address::dial_as;

/// The `RealtimeMetricsSubscriptionStatus` value that enables the additional series, as the
/// enumerated type spells it.
const ENABLED: &str = "Enabled";

/// The whole percentage a rate carries when every one of its observations is of its class,
/// which is what a share of the dialled answers is computed against.
const WHOLE_PERCENT: f64 = 100.0;

/// How close a folded percentage has to be to the share it is a percentage of: the fold is
/// exact arithmetic over whole observations, so the slack is only against the last bit of a
/// division.
const TOLERANCE: f64 = 1e-9;

/// One distribution of a row, as the client that created it knows it: its id, the NAME it
/// published for a viewer to address, and the address this emulator opened for its edge, which
/// is where a viewer dials that name.
#[derive(Debug, Clone)]
struct Subject {
    id: String,
    domain: String,
    address: String,
}

/// Create one distribution, holding its published `DomainName` to the one rule about it: the
/// DNS name CloudFront assigns a distribution.
async fn subject(
    router: &MultiServiceRouter,
    engine: &::std::sync::Arc<engine::InMemoryEngine>,
    case: &str,
    caller_reference: &str,
) -> Subject {
    let (id, domain) = created(router, caller_reference).await;
    assert!(
        is_published_edge_address(&domain),
        "case {case:?}: expected an address this emulator answers on \
         (host:port), got {domain:?}"
    );
    let address = edge_listener_authority(engine, case, &id);
    Subject {
        id,
        domain,
        address,
    }
}

// ---------------------------------------------------------------------------
// The traffic, and what the dialled answers carried
// ---------------------------------------------------------------------------

/// One viewer request a row dials at the published address, and the status the edge is
/// expected to answer it with.
struct Driven {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    status: u16,
}

/// The traffic every row drives, in order: the second row repeats the first row's read, so the
/// edge cache is what answers it. No row uploads a body, so no request of the table is one the
/// hit rate excludes for its METHOD; what excludes a request here is its answer.
fn driven() -> [Driven; 4] {
    [
        Driven {
            case: "a read an origin answers",
            method: "GET",
            path: "/2020-05-31/distribution",
            status: 200,
        },
        Driven {
            case: "the same read again, which the edge cache answers",
            method: "GET",
            path: "/2020-05-31/distribution",
            status: 200,
        },
        Driven {
            case: "a read of an object the origin does not hold",
            method: "GET",
            path: "/2020-05-31/distribution/E0NOSUCHDISTRIB",
            status: 404,
        },
        Driven {
            case: "a method the cache behaviour does not allow",
            method: "DELETE",
            path: "/2020-05-31/distribution",
            status: 403,
        },
    ]
}

/// Read the status line and the `X-Cache` of the raw HTTP answer the published address wrote
/// back, as the [`Outcome`] every expectation below is reconciled against.
///
/// The edge reports a cache outcome on every answer it serves, so an answer carrying none of
/// the three fails the case rather than being read as one of them.
///
/// `pub(crate)` so a sibling suite whose viewer dials a published address reads the answer it
/// wrote back the same way rather than carrying its own copy.
pub(crate) fn outcome_of(case: &str, raw: &str) -> Outcome {
    let status_line = raw
        .lines()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the published address answered nothing"));
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an HTTP status line, got {status_line:?}")
        });
    let x_cache = raw
        .lines()
        .take_while(|line| !line.trim().is_empty())
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("x-cache")
                .then(|| value.trim().to_string())
        })
        .unwrap_or_else(|| panic!("case {case:?}: the edge reported no X-Cache, got {raw:?}"));
    assert!(
        [CACHE_HIT, CACHE_MISS, CACHE_ERROR].contains(&x_cache.as_str()),
        "case {case:?}: expected the edge to report a cache outcome, got {x_cache:?}"
    );
    Outcome { status, x_cache }
}

/// How many of `served` satisfy `predicate`, as the count a rate is computed from.
///
/// `pub(crate)` so every suite that computes a rate from the answers the edge served counts
/// them the same way.
pub(crate) fn count_of(served: &[Outcome], predicate: impl Fn(&Outcome) -> bool) -> f64 {
    // A row drives a handful of requests, so every count here is exact in `f64`.
    #[allow(clippy::cast_precision_loss)]
    let counted = served.iter().filter(|answer| predicate(answer)).count() as f64;
    counted
}

/// Dial every request of [`driven`] at `address`, stating the published name `domain` as the
/// host it addressed, and answer what the edge served each one.
async fn dialled(case: &str, address: &str, domain: &str) -> Vec<Outcome> {
    let mut served = Vec::new();
    for request in driven() {
        let raw = dial_as(address, request.method, request.path, domain).await;
        let answer = outcome_of(case, &raw);
        assert_eq!(
            answer.status, request.status,
            "case {case:?} ({}): expected the published address to answer {}, got {raw:?}",
            request.case, request.status
        );
        served.push(answer);
    }
    served
}

/// Assert that a series holds nothing: a series a distribution is not subscribed to, or one of
/// a distribution nothing viewed, has no datapoint in any minute.
///
/// `pub(crate)` so every suite that reads a series a distribution does not publish holds it to
/// this one assertion.
pub(crate) fn assert_no_datapoint(case: &str, metric: &str, holding: &Held) {
    assert_eq!(
        holding.samples, 0.0,
        "case {case:?} ({metric}): expected no datapoint at all, got {holding:?}"
    );
    assert_eq!(
        holding.sum, 0.0,
        "case {case:?} ({metric}): expected no datapoint at all, got {holding:?}"
    );
}

/// One row: what the dialled distribution's monitoring subscription says.
struct Row {
    case: &'static str,
    /// The status the row subscribes with, `Some(None)` for a subscription carrying no
    /// configuration at all, and `None` for a distribution that is never subscribed.
    subscription: Option<Option<&'static str>>,
    /// Whether the additional series are published for the traffic this row drives.
    additional: bool,
}

/// The rows: every value the flag's enumerated type takes, a subscription carrying no
/// configuration, and no subscription at all.
fn rows() -> Vec<Row> {
    let mut rows: Vec<Row> = REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES
        .iter()
        .map(|status| Row {
            case: status,
            subscription: Some(Some(status)),
            additional: *status == ENABLED,
        })
        .collect();
    rows.push(Row {
        case: "a subscription carrying no configuration",
        subscription: Some(None),
        additional: false,
    });
    rows.push(Row {
        case: "no subscription at all",
        subscription: None,
        additional: false,
    });
    rows
}

#[tokio::test]
async fn traffic_dialled_at_a_published_address_fills_that_distributions_series() {
    let (router, engine) = emulator_with_engine();
    let table = rows();

    // Every row's distribution, plus the control nothing dials, is created first and then
    // waited for, so the propagation the clock owes them is waited out once for all of them.
    let mut subjects: Vec<Subject> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        subjects.push(
            subject(
                &router,
                &engine,
                row.case,
                &format!("dialled-metrics-{index}"),
            )
            .await,
        );
    }
    let control_case = "a subscribed distribution nothing dials";
    let control = subject(&router, &engine, control_case, "dialled-metrics-control").await;
    for (row, subject) in table.iter().zip(subjects.iter()) {
        awaited_deployment(&router, row.case, &subject.id).await;
    }
    awaited_deployment(&router, control_case, &control.id).await;
    // The control is subscribed and never dialled, so what decides its series is the traffic
    // and not the subscription.
    subscribe(&router, control_case, &control.id, Some(ENABLED)).await;

    // The instant the control's (empty) series are read against, taken before any row runs:
    // nothing is ever published for it, so any window answers the same.
    let control_at = now_secs();
    for (row, subject) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        if let Some(status) = row.subscription {
            subscribe(&router, case, &subject.id, status).await;
        }
        // The instant THIS row's traffic is served at, which is what its own series are read
        // against: a row's datapoints are read in the window around its own traffic rather
        // than around the first row's, so a slow host cannot move them out of it.
        let at = now_secs();

        // The traffic, dialled at the address this emulator opened for the distribution, with
        // the name it published stated as the host — a socket is opened and the requests are
        // written on it, which is what a viewer handed that name does.
        let served = dialled(case, &subject.address, &subject.domain).await;
        let requests = count_of(&served, |_| true);
        let cacheable = count_of(&served, Outcome::cacheable);
        let from_cache = count_of(&served, Outcome::served_by_cache);
        let from_origin = count_of(&served, Outcome::served_by_origin);
        let generated = count_of(&served, |answer| answer.x_cache == CACHE_ERROR);
        // The table is only a span of outcomes if the dialled answers really carried each of
        // them.
        assert!(
            from_cache > 0.0 && from_origin > 0.0 && generated > 0.0,
            "case {case:?}: the dialled traffic must span the outcomes an edge answer can have, \
             got {served:?}"
        );

        // The DEFAULT series are filled by the traffic the published address served, whatever
        // the subscription says: they are what tells a client the address served at all.
        let counted = held(&router, case, &subject.id, edge_metrics::REQUESTS_METRIC, at).await;
        assert_eq!(
            counted.sum, requests,
            "case {case:?}: every answer served at the published address is counted once, got \
             {counted:?}"
        );
        let four_xx = held(
            &router,
            case,
            &subject.id,
            edge_metrics::FOUR_XX_ERROR_RATE_METRIC,
            at,
        )
        .await;
        let expected_four_xx = WHOLE_PERCENT
            * count_of(&served, |answer| (400..500).contains(&answer.status))
            / requests;
        assert!(
            (four_xx.average() - expected_four_xx).abs() < TOLERANCE,
            "case {case:?}: expected the 4xx rate to be the share of the dialled answers in \
             that class ({expected_four_xx}), got {four_xx:?}"
        );

        // The ADDITIONAL series: each one is the measure of exactly this traffic while the
        // subscription enables them, and has no datapoint at all otherwise.
        let hit_rate = held(
            &router,
            case,
            &subject.id,
            edge_metrics::CACHE_HIT_RATE_METRIC,
            at,
        )
        .await;
        let latency = held(
            &router,
            case,
            &subject.id,
            edge_metrics::ORIGIN_LATENCY_METRIC,
            at,
        )
        .await;
        if row.additional {
            assert_eq!(
                hit_rate.samples, cacheable,
                "case {case:?}: the hit rate is a rate of the cacheable answers alone, got \
                 {hit_rate:?} for {served:?}"
            );
            let expected_hit_rate = WHOLE_PERCENT * from_cache / cacheable;
            assert!(
                (hit_rate.average() - expected_hit_rate).abs() < TOLERANCE,
                "case {case:?}: expected a hit rate of {expected_hit_rate} for the {from_cache} \
                 of {cacheable} cacheable answers the cache held, got {hit_rate:?}"
            );
            assert_eq!(
                latency.samples, from_origin,
                "case {case:?}: the origin latency is published for the answers an origin \
                 produced, got {latency:?} for {served:?}"
            );
            assert_eq!(
                latency.units,
                vec![edge_metrics::MILLISECONDS_UNIT.to_string()],
                "case {case:?}: an origin latency is a span of time, got {latency:?}"
            );
        } else {
            assert_no_datapoint(case, edge_metrics::CACHE_HIT_RATE_METRIC, &hit_rate);
            assert_no_datapoint(case, edge_metrics::ORIGIN_LATENCY_METRIC, &latency);
        }

        // One series per status code CloudFront reports a rate of its own for, read for the
        // codes the dialled answers carried and for the codes they did not.
        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let rate = held(&router, case, &subject.id, metric, at).await;
            if row.additional {
                let expected = WHOLE_PERCENT
                    * count_of(&served, |answer| answer.status == code)
                    / requests;
                assert_eq!(
                    rate.samples, requests,
                    "case {case:?} ({metric}): every answer observes a per-status rate, got \
                     {rate:?}"
                );
                assert!(
                    (rate.average() - expected).abs() < TOLERANCE,
                    "case {case:?} ({metric}): expected a rate of {expected} for the answers \
                     carrying {code}, got {rate:?} for {served:?}"
                );
            } else {
                assert_no_datapoint(case, metric, &rate);
            }
        }
    }

    // The control: its address was published and nothing dialled it, so no traffic was served
    // for it and none of its series — default or additional — holds anything, even though its
    // subscription enables the additional ones.
    for metric in edge_metrics::DISTRIBUTION_METRICS
        .iter()
        .chain(edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS.iter())
    {
        let holding = held(&router, control_case, &control.id, metric, control_at).await;
        assert_no_datapoint(control_case, metric, &holding);
    }
}
