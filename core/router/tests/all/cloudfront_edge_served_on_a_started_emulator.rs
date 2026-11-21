// SPDX-License-Identifier: CC0-1.0

//! The CloudFront edge, and the `AWS/CloudFront` series it fills, driven against the emulator
//! a customer STARTS: the built binary, on its own port, with request authentication on, every
//! control-plane call signed over TLS and the viewer request made by DIALLING the address the
//! distribution published.
//!
//! RULE: **the value a distribution publishes as `Distribution$DomainName` is an address the
//! client that read it can dial; once the distribution reports `Deployed`, a viewer request
//! dialled there — over either scheme that one address serves — is answered out of the
//! distribution's own origin (the object's bytes for a key the origin holds, the origin's own
//! refusal for one it does not), and every answer served there is counted into THAT
//! distribution's `AWS/CloudFront` series, dimensioned `DistributionId` beside `Region=Global`,
//! with the DEFAULT series published for every distribution and the ADDITIONAL ones (the cache
//! hit rate, the origin latency and the per-status-code error rates) published and listed
//! exactly while that distribution's monitoring subscription carries
//! `RealtimeMetricsSubscriptionStatus` `Enabled`.**
//!
//! The sibling suites pin that rule on a router a test assembles in its own process
//! ([`crate::cloudfront_dialled_edge_scheme_publishes_its_series`], whose vocabulary for a
//! served answer this suite reads rather than restating, and
//! `all/cloudfront_subscribed_minute_of_additional_metrics`). The emulator a client RUNS is a
//! different configuration of the same code, and it is the only one a customer has: the
//! credentials are verified, so the viewer request — which carries none — has to be admitted on
//! its own merits; the edge's own origin fetch and every metric publication run as nested calls
//! on the binary's own runtime; and the address the distribution published has to be dialable
//! from outside the process that opened it, over the certificate that start published. A
//! distribution whose published address answers nothing THERE is indistinguishable, to the
//! client holding the name, from a distribution that serves nothing at all — whatever an
//! in-process suite can reach.
//!
//! The origin is a private Amazon S3 bucket whose policy grants the CloudFront service
//! principal `s3:GetObject` and nothing else, scoped by `AWS:SourceArn` to the distributions
//! fronting it: the edge therefore has to SIGN its origin fetch for the object store to serve
//! it, and a read of a key the bucket does not hold is refused by the origin rather than
//! answered not-found ("If you don't have the `s3:ListBucket` permission, Amazon S3 returns an
//! HTTP status code 403 Access Denied error", `GetObject`).
//!
//! The rows differ along what the rule generalises over: whether the distribution's opt-in is
//! in place while its edge serves the traffic. Every expected value is DERIVED from what the
//! dialled answers carried — their status and the `X-Cache` naming who produced each — so no
//! row states a count or a rate of its own.
//!
//! The started binary itself is the harness [`Started`] the sibling started-emulator suites
//! share, which is `pub(crate)` for exactly this reason: nothing here starts the binary, signs
//! a call or trusts a certificate a second time.

use serde_json::{json, Value};

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::service::edge_metrics;

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::cloudfront_dialled_edge_address_over_a_private_s3_origin::is_dialable;
use crate::cloudfront_dialled_edge_scheme_publishes_its_series::{
    dimensions, fold, source_arn_condition, window, Held, Served, Tally, CACHING_OPTIMIZED,
    ENABLED, ORIGIN_REFUSAL_STATUS, PERIOD, TOLERANCE,
};
use crate::cloudfront_edge_metrics_drive_the_alarm_chain::now_secs;
use crate::cloudfront_oac_s3_origin_edge::{
    policy, API_VERSION, CLOUDFRONT_PRINCIPAL, REGION,
};
use crate::query_wire::element;

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`), and the
/// content type its `awsJson1_0` wire carries.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const CLOUDWATCH_CONTENT_TYPE: &str = "application/x-amz-json-1.0";

/// The private bucket both distributions front, the key it HOLDS and the bytes it holds there.
const ORIGIN_BUCKET: &str = "started-edge-origin";
const PRESENT_KEY: &str = "present.txt";
const PRESENT_BODY: &str = "the object the started emulator's edge serves";

/// A key that bucket does NOT hold, which the grant above makes the origin refuse.
const ABSENT_KEY: &str = "absent-never-written.txt";

/// How long a condition is waited for, and how often it is looked at again. The wait is on the
/// CONDITION; the span is generous, so a loaded host spends none of it.
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
const LOOK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// One CloudFront control-plane call on the started emulator, which must answer `expected`.
fn cloudfront(
    started: &Started,
    case: &str,
    method: &str,
    path: &str,
    document: Option<&str>,
    expected: u16,
) -> String {
    let headers: Vec<(&str, &str)> = document
        .map(|_| vec![("content-type", "application/xml")])
        .unwrap_or_default();
    let (status, body) = started.call(
        "cloudfront",
        method,
        path,
        &headers,
        document.unwrap_or_default().as_bytes().to_vec(),
    );
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected} from CloudFront {method} {path}, got {status} \
         with {body:?}; the log:\n{}",
        started.log()
    );
    body
}

/// One Amazon S3 call, which must answer a 2xx.
fn s3(
    started: &Started,
    case: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) {
    let (status, answered) = started.call("s3", method, path, headers, body);
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected Amazon S3 {method} {path} to succeed, got {status} with \
         {answered:?}; the log:\n{}",
        started.log()
    );
}

/// One CloudWatch call, which must answer 200.
fn cloudwatch(started: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = started.rpc(
        CLOUDWATCH_SIGNING_NAME,
        &format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        CLOUDWATCH_CONTENT_TYPE,
        body,
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with \
         {text:?}; the log:\n{}",
        started.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One viewer request made by DIALLING `authority` — the address the distribution published —
/// over `scheme`, carrying no credential of any kind, which is what a client handed a domain
/// name sends. It goes over the client that trusts the certificate this start published.
fn dial(started: &Started, case: &str, scheme: &str, authority: &str, key: &str) -> Served {
    let url = format!("{scheme}://{authority}/{key}");
    let answered = started
        .client()
        .get(&url)
        .send()
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the published address {url} answered nothing: {error}; the \
                 log:\n{}",
                started.log()
            )
        });
    let status = answered.status().as_u16();
    let x_cache = answered
        .headers()
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Served {
        status,
        x_cache,
        body: answered.text().unwrap_or_default(),
    }
}

/// Look at `GetDistribution` until CloudFront reports `id` `Deployed`, which is the state a
/// viewer request is served in. The wait is on the CONDITION, never on a span of the clock.
fn awaited_deployment(started: &Started, case: &str, id: &str) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = cloudfront(
            started,
            case,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            None,
            200,
        );
        let status = element(&read, "Status");
        if status == "Deployed" {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the distribution {id} never reported Deployed inside \
             {SETTLE_DEADLINE:?}, last {status:?}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// `CreateMonitoringSubscription` with the flag that buys the additional series, and the
/// `GetMonitoringSubscription` read that confirms a client sees it.
fn subscribe(started: &Started, case: &str, id: &str) {
    let document = format!(
        "<MonitoringSubscription xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <RealtimeMetricsSubscriptionConfig>\
         <RealtimeMetricsSubscriptionStatus>{ENABLED}</RealtimeMetricsSubscriptionStatus>\
         </RealtimeMetricsSubscriptionConfig>\
         </MonitoringSubscription>"
    );
    cloudfront(
        started,
        case,
        "POST",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        Some(&document),
        200,
    );
    let read = cloudfront(
        started,
        case,
        "GET",
        &format!("/{API_VERSION}/distributions/{id}/monitoring-subscription"),
        None,
        200,
    );
    assert_eq!(
        element(&read, "RealtimeMetricsSubscriptionStatus"),
        ENABLED,
        "case {case:?}: the opt-in a client bought must read back {ENABLED}, got {read:?}"
    );
}

/// Every datapoint `GetMetricStatistics` answers for one of a distribution's series.
fn datapoints(
    started: &Started,
    case: &str,
    distribution_id: &str,
    metric: &str,
    at: f64,
) -> Vec<Value> {
    let (start, end) = window(at);
    let answered = cloudwatch(
        started,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": edge_metrics::NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions(distribution_id),
            "StartTime": start,
            "EndTime": end,
            "Period": PERIOD,
            "Statistics": ["SampleCount", "Sum", "Average"],
        }),
    );
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// What one series holds in that window.
fn held(started: &Started, case: &str, distribution_id: &str, metric: &str, at: f64) -> Held {
    let answered = datapoints(started, case, distribution_id, metric, at);
    fold(case, metric, &answered)
}

/// What `ListMetrics` reports for one of a distribution's series.
fn listed(started: &Started, case: &str, distribution_id: &str, metric: &str) -> Vec<Value> {
    let answered = cloudwatch(
        started,
        case,
        "ListMetrics",
        &json!({
            "Namespace": edge_metrics::NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions(distribution_id),
        }),
    );
    answered["Metrics"].as_array().cloned().unwrap_or_default()
}

/// One viewer request a row dials: the scheme it dials the one published address over, the key
/// it asks for, and the status the edge is expected to answer it with.
struct Driven {
    case: &'static str,
    scheme: &'static str,
    key: &'static str,
    status: u16,
}

/// The traffic every row drives at the address it was handed: a read the origin answers, the
/// same read again (which the edge cache answers), the same read over the OTHER half of that
/// one address, and a read of a key the origin refuses.
fn driven() -> [Driven; 4] {
    [
        Driven {
            case: "a read the origin answers, over the encrypted half",
            scheme: "https",
            key: PRESENT_KEY,
            status: 200,
        },
        Driven {
            case: "the same read again, which the edge cache answers",
            scheme: "https",
            key: PRESENT_KEY,
            status: 200,
        },
        Driven {
            case: "the same read over the plaintext half of the one address",
            scheme: "http",
            key: PRESENT_KEY,
            status: 200,
        },
        Driven {
            case: "a read of a key the origin refuses",
            scheme: "https",
            key: ABSENT_KEY,
            status: ORIGIN_REFUSAL_STATUS,
        },
    ]
}

/// One row: whether this distribution's monitoring subscription is in place while its edge
/// serves the traffic.
struct Row {
    case: &'static str,
    subscribed: bool,
}

/// The rows: the opt-in on and off, over distributions that are otherwise identical.
fn rows() -> [Row; 2] {
    [
        Row {
            case: "the distribution whose monitoring subscription is in place",
            subscribed: true,
        },
        Row {
            case: "the distribution carrying no monitoring subscription",
            subscribed: false,
        },
    ]
}

/// The `DistributionConfig` each row is created from: the private bucket as its only origin,
/// under the origin access control `control` and the managed caching policy.
fn distribution_config(caller_reference: &str, control: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>origin-bucket</Id><DomainName>{ORIGIN_BUCKET}.s3.{REGION}.amazonaws.com</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         <OriginAccessControlId>{control}</OriginAccessControlId>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_OPTIMIZED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>the edge of a started emulator</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

#[test]
fn the_started_emulators_published_edge_serves_and_fills_its_series() {
    let started = Started::start();

    // The origin: one object, and a policy granting the CloudFront service principal the one
    // action its signed origin request needs.
    let origin = "the private origin";
    s3(&started, origin, "PUT", &format!("/{ORIGIN_BUCKET}"), &[], Vec::new());
    s3(
        &started,
        origin,
        "PUT",
        &format!("/{ORIGIN_BUCKET}/{PRESENT_KEY}"),
        &[],
        PRESENT_BODY.as_bytes().to_vec(),
    );
    let control = cloudfront(
        &started,
        origin,
        "POST",
        &format!("/{API_VERSION}/origin-access-control"),
        Some(&format!(
            "<OriginAccessControlConfig \
             xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>started-edge</Name><SigningProtocol>sigv4</SigningProtocol>\
             <SigningBehavior>always</SigningBehavior>\
             <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
             </OriginAccessControlConfig>"
        )),
        201,
    );
    let control = element(&control, "Id");

    // Both distributions are created first, so one document grants the whole set.
    let table = rows();
    let mut subjects: Vec<(String, String, String)> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        let created = cloudfront(
            &started,
            row.case,
            "POST",
            &format!("/{API_VERSION}/distribution"),
            Some(&distribution_config(
                &format!("started-edge-{index}"),
                &control,
            )),
            201,
        );
        subjects.push((
            element(&created, "Id"),
            element(&created, "DomainName"),
            element(&created, "ARN"),
        ));
    }
    let arns: Vec<String> = subjects.iter().map(|(_, _, arn)| arn.clone()).collect();
    s3(
        &started,
        origin,
        "PUT",
        &format!("/{ORIGIN_BUCKET}?policy"),
        &[("content-type", "application/json")],
        policy(
            CLOUDFRONT_PRINCIPAL,
            "s3:GetObject",
            &format!("arn:aws:s3:::{ORIGIN_BUCKET}/*"),
            Some(&source_arn_condition(&arns)),
        )
        .into_bytes(),
    );

    for (row, (id, domain, _arn)) in table.iter().zip(subjects.iter()) {
        let case = row.case;
        awaited_deployment(&started, case, id);
        // The published name is an address the client that read it can dial, which is what the
        // rows below dial.
        assert!(
            is_dialable(domain),
            "case {case:?}: expected a published address the client can dial, got {domain:?}"
        );
        if row.subscribed {
            subscribe(&started, case, id);
        }

        let at = now_secs();
        let mut tally = Tally::default();
        for request in driven() {
            let served = dial(&started, case, request.scheme, domain, request.key);
            assert_eq!(
                served.status, request.status,
                "case {case:?} ({}): expected the address {domain} the distribution published to \
                 answer {}, got {served:?}; the log:\n{}",
                request.case,
                request.status,
                started.log()
            );
            if served.status == 200 {
                assert_eq!(
                    served.body, PRESENT_BODY,
                    "case {case:?} ({}): expected the origin's own bytes, got {served:?}",
                    request.case
                );
            } else {
                assert!(
                    !served.body.contains(PRESENT_BODY),
                    "case {case:?} ({}): a refusal carries none of the object's bytes, got \
                     {served:?}",
                    request.case
                );
            }
            assert!(
                served.names_its_producer(),
                "case {case:?} ({}): every answer reports who produced it, got {served:?}",
                request.case
            );
            tally.record(&served);
        }
        assert!(
            tally.hits > 0.0 && tally.from_origin > 0.0,
            "case {case:?}: the traffic must reach both the cache and the origin for the rates \
             below to mean anything, got {tally:?}"
        );

        // The DEFAULT series: published for the traffic the published address served, whatever
        // the opt-in reads.
        let counted = held(&started, case, id, edge_metrics::REQUESTS_METRIC, at);
        assert_eq!(
            counted.sum, tally.requests,
            "case {case:?}: expected the counter to count every answer the published address \
             served ({}), got {counted:?}; the log:\n{}",
            tally.requests,
            started.log()
        );
        let downloaded = held(&started, case, id, edge_metrics::BYTES_DOWNLOADED_METRIC, at);
        assert_eq!(
            downloaded.sum, tally.bytes,
            "case {case:?}: expected the bytes of every body the viewer was served ({}), got \
             {downloaded:?}",
            tally.bytes
        );
        for (metric, expected) in [
            (
                edge_metrics::FOUR_XX_ERROR_RATE_METRIC,
                tally.four_xx_percent(),
            ),
            (
                edge_metrics::FIVE_XX_ERROR_RATE_METRIC,
                tally.five_xx_percent(),
            ),
            (
                edge_metrics::TOTAL_ERROR_RATE_METRIC,
                tally.total_error_percent(),
            ),
        ] {
            let rate = held(&started, case, id, metric, at);
            assert_eq!(
                rate.samples, tally.requests,
                "case {case:?} ({metric}): every answer served observes this rate ({}), got \
                 {rate:?}",
                tally.requests
            );
            assert!(
                (rate.average() - expected).abs() < TOLERANCE,
                "case {case:?} ({metric}): expected the share of the answers of that class \
                 ({expected}), got {rate:?}"
            );
        }

        // The ADDITIONAL series: published, and listed, exactly while the opt-in is on.
        if !row.subscribed {
            for metric in edge_metrics::ADDITIONAL_DISTRIBUTION_METRICS {
                let published = datapoints(&started, case, id, metric, at);
                assert!(
                    published.is_empty(),
                    "case {case:?} ({metric}): a distribution carrying no subscription publishes \
                     no datapoint of this series, got {published:?}"
                );
                let series = listed(&started, case, id, metric);
                assert!(
                    series.is_empty(),
                    "case {case:?} ({metric}): a series nothing published is listed nowhere, got \
                     {series:?}"
                );
            }
            let requests_listed = listed(&started, case, id, edge_metrics::REQUESTS_METRIC);
            assert!(
                !requests_listed.is_empty(),
                "case {case:?}: the default series the served traffic filled is listed all the \
                 same, got {requests_listed:?}"
            );
            continue;
        }

        let hit_rate = held(&started, case, id, edge_metrics::CACHE_HIT_RATE_METRIC, at);
        assert_eq!(
            hit_rate.samples, tally.cacheable,
            "case {case:?}: the hit rate is a rate of the cacheable answers served ({}), got \
             {hit_rate:?}",
            tally.cacheable
        );
        let expected_hit_rate = 100.0 * tally.hits / tally.cacheable;
        assert!(
            (hit_rate.average() - expected_hit_rate).abs() < TOLERANCE,
            "case {case:?}: expected a hit rate of {expected_hit_rate} ({} of {} cacheable \
             answers came from the cache), got {hit_rate:?}",
            tally.hits,
            tally.cacheable
        );

        let latency = held(&started, case, id, edge_metrics::ORIGIN_LATENCY_METRIC, at);
        assert_eq!(
            latency.samples, tally.from_origin,
            "case {case:?}: the origin latency is published for the answers an origin produced \
             ({}), got {latency:?}",
            tally.from_origin
        );
        assert_eq!(
            latency.units,
            vec![edge_metrics::MILLISECONDS_UNIT.to_string()],
            "case {case:?}: an origin latency is a span of time, got {latency:?}"
        );

        for (code, metric) in edge_metrics::STATUS_ERROR_RATE_METRICS {
            let rate = held(&started, case, id, metric, at);
            assert_eq!(
                rate.samples, tally.requests,
                "case {case:?} ({metric}): every answer served observes a per-status rate ({}), \
                 got {rate:?}",
                tally.requests
            );
            let expected = tally.status_percent(code);
            assert!(
                (rate.average() - expected).abs() < TOLERANCE,
                "case {case:?} ({metric}): expected a rate of {expected} over the answers served, \
                 got {rate:?}"
            );
        }
    }
}
