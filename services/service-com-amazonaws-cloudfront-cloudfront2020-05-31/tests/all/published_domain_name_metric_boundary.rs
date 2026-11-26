// SPDX-License-Identifier: CC0-1.0

//! The `AWS/CloudFront` series filled by traffic dialled AT THE VALUE A DISTRIBUTION PUBLISHED,
//! whichever operation minted that distribution.
//!
//! RULE: **the address an operation publishes for a resource's data plane is an address this
//! emulator ANSWERS that resource's own data plane on — `Distribution$DomainName` is "the
//! distribution's CloudFront domain name. For example: `d111111abcdef8.cloudfront.net`", the
//! value a client dials — so a viewer that holds nothing but that value is served by THAT
//! distribution's edge, and every answer served there is counted into THAT distribution's own
//! `AWS/CloudFront` series (dimensioned by its `DistributionId` beside `Region=Global`): the
//! default series always, and the additional ones (the cache hit rate, the origin latency, the
//! per-status-code error rates) exactly while that distribution's monitoring subscription
//! carries `RealtimeMetricsSubscriptionStatus` `Enabled`.**
//!
//! What this suite adds to its siblings is the JOIN, over the one addressing a client handed a
//! domain name actually has: `published_edge_domain_on_the_endpoint` and
//! `dialled_published_domain_name` dial the published value and read the CONTENT it serves,
//! `edge_additional_metrics` drives the subscription boundary through the router by stating the
//! name as a `Host`, and `dialled_edge_address_metrics` reads the series for traffic dialled at
//! the address this emulator opened, read out of the store. Here the socket is opened to the
//! PUBLISHED VALUE itself — nothing is read out of this emulator's records to reach it — and the
//! series that traffic filled are then read back through CloudWatch. So a published value that is
//! an address but another distribution's, or one no series follows from, fails a case here.
//!
//! The rows span what the rule generalises over: the operation that MINTED the distribution
//! (`CreateDistribution`, `CreateDistributionWithTags`, and `CopyDistribution`, which mints one
//! from another distribution's stored configuration), crossed with what its monitoring
//! subscription says (`Enabled`, the other value the modelled type takes, and no subscription at
//! all), and HOW MANY answers were dialled at each published value — a different number per row,
//! so a series carrying another row's traffic cannot pass. The copy's PRIMARY is a distribution
//! nothing dials, so its series stand as the control.
//!
//! Every expected value is DERIVED from what the dialled answers carried — their status and the
//! `X-Cache` naming who produced them — so no row states a rate of its own.
//!
//! The harness is reused rather than copied: the router carrying this service beside CloudWatch
//! and the series read are [`crate::edge_request_metrics`]'s, the distribution configuration and
//! the subscription call are [`crate::edge_additional_metrics`]'s, the outcome classification is
//! [`crate::edge_additional_metric_reconciliation`]'s, the dialled answer is read through
//! [`crate::dialled_edge_address_metrics`]'s reader, the socket client is
//! [`crate::published_edge_address`]'s, the tag element is [`crate::distribution_tags`]'s and the
//! deployment poll loop and the dialable-address predicate are
//! [`crate::distribution_lifecycle`]'s.

use std::collections::BTreeSet;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;
use smithy_server_runtime::MultiServiceRouter;

use crate::dialled_edge_address_metrics::{assert_no_datapoint, count_of, outcome_of};
use crate::distribution_lifecycle::{
    awaited_deployment, call, create_distribution, is_published_edge_address, Answer, API_VERSION,
};
use crate::distribution_tags::tags_xml;
use crate::edge_additional_metric_reconciliation::Outcome;
use crate::edge_additional_metrics::{config, subscribe};
use crate::edge_request_metrics::{emulator, held, now_secs};
use crate::published_edge_address::dial_as;

/// The `RealtimeMetricsSubscriptionStatus` value that enables the additional series, and the
/// other value that modelled enumerated type takes. Both are held to the type's own value list
/// below rather than trusted as spellings of this suite's.
const ENABLED: &str = "Enabled";
const DISABLED: &str = "Disabled";

/// The whole percentage a rate carries when every one of its observations is of its class, which
/// is what a share of the dialled answers is computed against.
const WHOLE_PERCENT: f64 = 100.0;

/// How close a folded percentage has to be to the share it is a percentage of: the fold is exact
/// arithmetic over whole observations, so the slack is only against the last bit of a division.
const TOLERANCE: f64 = 1e-9;

/// A viewer path the origin these distributions point at — this service's own control-plane
/// endpoint, which the edge fetches in process — HOLDS, answered 200.
const PRESENT_KEY: &str = "/2020-05-31/distribution";

/// A viewer path that origin does NOT hold, answered with the origin's own 404.
const ABSENT_KEY: &str = "/2020-05-31/distribution/E0NOSUCHDISTRIB";

/// The operation that minted a row's distribution. Each of them publishes an address for the
/// distribution it mints, so the rule holds for each.
#[derive(Debug, Clone, Copy)]
enum Mint {
    /// `POST /2020-05-31/distribution`.
    Create,
    /// `POST /2020-05-31/distribution?WithTags`.
    CreateWithTags,
    /// `POST /2020-05-31/distribution/<primary>/copy`, which mints a distribution from another
    /// one's STORED configuration rather than from a submitted document.
    Copy,
}

/// One viewer request a row dials at the published value, and the status the edge is expected to
/// answer it with.
struct Driven {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    status: u16,
}

/// One row: how its distribution is minted, what its monitoring subscription says, and the
/// traffic dialled at the value it published.
struct Row {
    case: &'static str,
    mint: Mint,
    /// The status the row subscribes with, and `None` for a distribution never subscribed.
    subscription: Option<&'static str>,
    /// Whether the additional series are published for the traffic this row drives.
    additional: bool,
    /// The requests this row dials, in order. A row that repeats a read has its second answer
    /// served by that distribution's own edge cache.
    traffic: Vec<Driven>,
}

/// A read the origin answers.
fn present_read() -> Driven {
    Driven {
        case: "a read an origin answers",
        method: "GET",
        path: PRESENT_KEY,
        status: 200,
    }
}

/// The same read again, which the edge cache answers.
fn repeated_read() -> Driven {
    Driven {
        case: "the same read again, which the edge cache answers",
        method: "GET",
        path: PRESENT_KEY,
        status: 200,
    }
}

/// A read of an object the origin does not hold.
fn absent_read() -> Driven {
    Driven {
        case: "a read of an object the origin does not hold",
        method: "GET",
        path: ABSENT_KEY,
        status: 404,
    }
}

/// A method the cache behaviour does not allow, which CloudFront refuses itself without reaching
/// an origin.
fn refused_method() -> Driven {
    Driven {
        case: "a method the cache behaviour does not allow",
        method: "DELETE",
        path: PRESENT_KEY,
        status: 403,
    }
}

/// The rows: every minting operation, crossed with what the subscription says, each dialling a
/// number of requests of its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a created distribution whose subscription enables the additional metrics",
            mint: Mint::Create,
            subscription: Some(ENABLED),
            additional: true,
            traffic: vec![
                present_read(),
                repeated_read(),
                absent_read(),
                refused_method(),
            ],
        },
        Row {
            case: "a distribution created with tags whose subscription disables them",
            mint: Mint::CreateWithTags,
            subscription: Some(DISABLED),
            additional: false,
            traffic: vec![present_read(), repeated_read(), absent_read()],
        },
        Row {
            case: "a copied distribution carrying no subscription at all",
            mint: Mint::Copy,
            subscription: None,
            additional: false,
            traffic: vec![present_read(), repeated_read()],
        },
    ]
}

/// One row's distribution, as the client that minted it knows it.
#[derive(Debug)]
struct Subject {
    id: String,
    /// The value this distribution PUBLISHED as its `DomainName` — the only thing a viewer below
    /// is given to reach it.
    domain: String,
    /// The distribution a COPY was minted from, which nothing dials: the control whose series
    /// stay empty while its copy's fill.
    primary: Option<String>,
}

/// The `Id` and the `DomainName` one minting answer reports.
fn reported(answer: &Answer, case: &str) -> (String, String) {
    assert_eq!(
        answer.status, 201,
        "case {case:?}: expected HTTP 201 from the minting call, got {} with {}",
        answer.status, answer.raw
    );
    let root = answer.root();
    let id = root.find_text("Id").unwrap_or_default().to_string();
    let domain = root.find_text("DomainName").unwrap_or_default().to_string();
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "case {case:?}: expected an Id and a DomainName from the minting call, got {}",
        answer.raw
    );
    (id, domain)
}

/// The `CopyDistributionRequest` a copy row sends: the reference the copy is created under, and
/// the state it is created in — "a Boolean flag to specify the state of the staging distribution
/// when it's created", which has to be enabled for the copy to serve a viewer at all.
fn copy_xml(reference: &str) -> String {
    format!(
        "<CopyDistributionRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Enabled>true</Enabled>\
         </CopyDistributionRequest>"
    )
}

/// Mint one row's distribution through the operation that row names, and report what it
/// published.
async fn minted(router: &MultiServiceRouter, row: &Row, reference: &str) -> Subject {
    let case = row.case;
    let document = config(reference);
    match row.mint {
        Mint::Create => {
            let (id, domain) = reported(&create_distribution(router, &document).await, case);
            Subject {
                id,
                domain,
                primary: None,
            }
        }
        Mint::CreateWithTags => {
            let body = format!(
                "<DistributionConfigWithTags \
                 xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">{document}{}\
                 </DistributionConfigWithTags>",
                tags_xml(Some(&[(
                    "owner".to_string(),
                    Some("published-domain-name-metric-boundary".to_string()),
                )]))
            );
            let answer = call(
                router,
                "POST",
                &format!("/{API_VERSION}/distribution?WithTags"),
                Some(&body),
                None,
            )
            .await;
            let (id, domain) = reported(&answer, case);
            Subject {
                id,
                domain,
                primary: None,
            }
        }
        Mint::Copy => {
            // A copy runs under a PRIMARY's stored configuration, so the row creates one first
            // under a reference of its own. The primary is a distribution of its own, with an
            // address of its own, and nothing dials it.
            let primary = create_distribution(router, &config(&format!("{reference}-primary"))).await;
            let (primary_id, primary_domain) = reported(&primary, case);
            let answer = call(
                router,
                "POST",
                &format!("/{API_VERSION}/distribution/{primary_id}/copy"),
                Some(&copy_xml(reference)),
                None,
            )
            .await;
            let (id, domain) = reported(&answer, case);
            assert_ne!(
                domain, primary_domain,
                "case {case:?}: expected the copy to publish an address of its own, got the \
                 primary's {primary_domain:?}"
            );
            Subject {
                id,
                domain,
                primary: Some(primary_id),
            }
        }
    }
}

/// Dial every request of a row at the value its distribution published — a socket opened to that
/// value, stating it as the host it addressed, which is what a client handed that value does —
/// and answer what the edge served each one.
async fn dialled(row: &Row, domain: &str) -> Vec<Outcome> {
    let case = row.case;
    let mut served = Vec::new();
    for request in &row.traffic {
        let raw = dial_as(domain, request.method, request.path, domain).await;
        let answer = outcome_of(case, &raw);
        assert_eq!(
            answer.status, request.status,
            "case {case:?} ({}): expected the published value {domain:?} to answer {}, got {raw:?}",
            request.case, request.status
        );
        served.push(answer);
    }
    served
}

/// A viewer that holds nothing but the value a distribution published is served by that
/// distribution's edge, and the answers it was served are counted into that distribution's own
/// `AWS/CloudFront` series — the additional ones exactly while its subscription enables them.
#[tokio::test]
async fn traffic_dialled_at_the_published_domain_name_fills_that_distributions_own_series() {
    let router = emulator();
    let table = rows();

    // The statuses the rows name are values of the modelled enumerated type rather than
    // spellings of this suite's own.
    for row in &table {
        if let Some(status) = row.subscription {
            assert!(
                REALTIME_METRICS_SUBSCRIPTION_STATUS_VALUES.contains(&status),
                "case {:?}: {status:?} is no RealtimeMetricsSubscriptionStatus value",
                row.case
            );
        }
    }

    // Every row's distribution is minted first and waited for afterwards, so the propagation the
    // clock owes them is waited out once for all of them.
    let mut subjects: Vec<Subject> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        subjects.push(minted(&router, row, &format!("published-metric-boundary-{index}")).await);
    }

    // What each row published is an address a client here can dial — a host carrying the port it
    // answers on and nothing else — and no two distributions published the same one.
    let mut published: BTreeSet<String> = BTreeSet::new();
    for (row, subject) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        assert!(
            is_published_edge_address(&subject.domain),
            "case {case:?}: expected an address this emulator answers on (host:port), got {:?}",
            subject.domain
        );
        assert!(
            published.insert(subject.domain.clone()),
            "case {case:?}: the address {:?} was already published by another distribution",
            subject.domain
        );
    }

    for (row, subject) in table.iter().zip(subjects.iter()) {
        awaited_deployment(&router, row.case, &subject.id).await;
        if let Some(status) = row.subscription {
            subscribe(&router, row.case, &subject.id, Some(status)).await;
        }
    }

    for (row, subject) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        // The instant THIS row's traffic is served at, which is what its own series are read
        // against: a row's datapoints are read in the window around its own traffic rather than
        // around the first row's, so a slow host cannot move them out of it.
        let at = now_secs();
        let served = dialled(row, &subject.domain).await;
        let requests = count_of(&served, |_| true);
        let cacheable = count_of(&served, Outcome::cacheable);
        let from_cache = count_of(&served, Outcome::served_by_cache);
        let from_origin = count_of(&served, Outcome::served_by_origin);
        // The row drove the traffic it meant to: an answer the cache held and one an origin
        // produced, which is what the rates below are shares of.
        assert!(
            from_cache > 0.0 && from_origin > 0.0,
            "case {case:?}: the dialled traffic must span the answers a cache and an origin \
             produce, got {served:?}"
        );

        // The DEFAULT series are filled by the traffic the published value served, whatever the
        // subscription says: they are what tells a client the value served at all. The count is
        // this row's own — each row dials a number of requests of its own — so a series carrying
        // another distribution's traffic fails here.
        let counted = held(&router, case, &subject.id, edge_metrics::REQUESTS_METRIC, at).await;
        assert_eq!(
            counted.sum, requests,
            "case {case:?}: every answer served at the published value is counted once against \
             the distribution that published it, got {counted:?} for {served:?}"
        );
        let four_xx = held(
            &router,
            case,
            &subject.id,
            edge_metrics::FOUR_XX_ERROR_RATE_METRIC,
            at,
        )
        .await;
        let expected_four_xx =
            WHOLE_PERCENT * count_of(&served, |answer| (400..500).contains(&answer.status))
                / requests;
        assert!(
            (four_xx.average() - expected_four_xx).abs() < TOLERANCE,
            "case {case:?}: expected the 4xx rate to be the share of the dialled answers in that \
             class ({expected_four_xx}), got {four_xx:?}"
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

        // One series per status code CloudFront reports a rate of its own for, read for the codes
        // the dialled answers carried and for the codes they did not.
        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let rate = held(&router, case, &subject.id, metric, at).await;
            if row.additional {
                let expected =
                    WHOLE_PERCENT * count_of(&served, |answer| answer.status == code) / requests;
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

        // The control a copy row carries: the distribution the copy was minted from published an
        // address of its own, nothing dialled it, and nothing is counted against it.
        if let Some(primary) = &subject.primary {
            let control = format!("{case} (the primary nothing dialled)");
            for metric in edge_metrics::DISTRIBUTION_METRICS
                .iter()
                .chain(edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS.iter())
            {
                let holding = held(&router, &control, primary, metric, at).await;
                assert_no_datapoint(&control, metric, &holding);
            }
        }
    }
}
