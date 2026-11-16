// SPDX-License-Identifier: CC0-1.0

//! What a LIVE probe publishes, driven end to end with CloudWatch Network Monitor
//! and Amazon CloudWatch on one engine.
//!
//! RULE under test: A PROBE THAT HAS REACHED `ACTIVE` PUBLISHES ITS MEASUREMENTS
//! INTO THE `AWS/NetworkMonitor` NAMESPACE, DIMENSIONED BY THE MONITOR AND THE PROBE
//! THE MEASUREMENTS ARE ABOUT, ONE WINDOW PER `aggregationPeriod` — and a probe that
//! never goes live publishes nothing. The model states the contract itself:
//! `CreateMonitor` / `CreateProbe` document that "Each probe then aggregates and
//! sends metrics to Amazon CloudWatch", and `CreateMonitorInput$aggregationPeriod`
//! is "The time, in seconds, that metrics are aggregated and sent to Amazon
//! CloudWatch". So a client that creates a monitor and then polls CloudWatch, or
//! alarms on the namespace, gets an answer from the clock alone instead of reading
//! an empty namespace for ever.
//!
//! What one window READS is the measurement the path really produced: nothing in
//! this emulator answers a probe's packets, so every packet is lost and `PacketLoss`
//! is the whole of what was sent (100 percent).
//!
//! Every case goes through the front door: `CreateMonitor` / `GetMonitor` /
//! `GetProbe` / `UpdateProbe` over this service's REST wire, and `ListMetrics` /
//! `GetMetricStatistics` over CloudWatch's own JSON wire. Nothing reaches into
//! either store.
//!
//! The rows span what the rule generalises over: both `aggregationPeriod` values the
//! model documents and a monitor that names none, both protocols a probe can use,
//! IPv4 and IPv6 destinations, packet sizes across the documented range, more than
//! one probe of one monitor (so a series belongs to ONE probe), and a probe switched
//! off before it ever goes live.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;
use smithy_server_runtime::MultiServiceRouter;

use super::monitor_and_probe::{
    call, create_monitor, get_monitor, probe_json, probes_of, source_arn_of, string_at, REGION,
};

/// One probe body whose `sourceArn` names the subnet at `index`, for the cases that
/// give each probe of ONE monitor a subnet of its own: a monitor holds only so many
/// probes in one source subnet, and that limit is not what these rows are about.
fn probe_in_subnet(index: usize, destination: &str) -> String {
    format!(
        r#"{{"sourceArn":"{}","destination":"{destination}","protocol":"ICMP","packetSize":56}}"#,
        source_arn_of(index)
    )
}

/// The namespace CloudWatch Network Monitor publishes a probe's measurements in, as a
/// client names it.
const NAMESPACE: &str = "AWS/NetworkMonitor";

/// The metric this suite reads: the share of a probe's packets that never came back.
const PACKET_LOSS: &str = "PacketLoss";

/// The unit a share of packets carries (`StandardUnit` holds `Percent`).
const PERCENT: &str = "Percent";

/// What a path NOTHING answers on loses: every packet the probe sent.
const TOTAL_LOSS: f64 = 100.0;

/// The `X-Amz-Target` prefix CloudWatch's JSON wire carries.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// CloudWatch's SigV4 signing name, which is what routes a call to it.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// How long a case looks for a window, far longer than the span a probe's own
/// activation and its first aggregation period take — so a loaded machine fails
/// nothing, while a series that never appears still ends the test instead of
/// hanging.
const PUBLISH_DEADLINE: Duration = Duration::from_secs(240);

/// How often it looks again, as a client's own waiter does.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The period a read lays over the series, in seconds: the minute a regular-resolution
/// metric is stored at (`MetricDatum$StorageResolution`).
const PERIOD_SECONDS: i64 = 60;

/// One emulator: CloudWatch Network Monitor and Amazon CloudWatch mounted on ONE
/// shared engine, the way the router mounts every service — because the probe is this
/// service's resource and the series it publishes lives in CloudWatch's store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what this suite is about; the router still reads the SigV4
    // credential scope, which is where each call's service and Region come from.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_networkmonitor_networkmonitor::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(cloudwatch::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// One CloudWatch call that has to be served, and the parsed body it answered.
async fn cloudwatch_ok(
    case: &str,
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/\
                 {CLOUDWATCH_SIGNING_NAME}/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let text = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with body \
         {text}"
    );
    serde_json::from_slice(&payload).unwrap_or(Value::Null)
}

/// Every `PacketLoss` series of the namespace CloudWatch lists, across EVERY page, as
/// its dimension set — read exactly as a client reads it.
async fn listed_series(case: &str, router: &MultiServiceRouter) -> Vec<BTreeMap<String, String>> {
    let mut series: Vec<BTreeMap<String, String>> = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut request = json!({"Namespace": NAMESPACE, "MetricName": PACKET_LOSS});
        if let Some(token) = &next_token {
            request["NextToken"] = json!(token);
        }
        let answered = cloudwatch_ok(case, router, "ListMetrics", &request).await;
        for metric in answered["Metrics"].as_array().cloned().unwrap_or_default() {
            let dimensions: BTreeMap<String, String> = metric["Dimensions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|dimension| {
                    (
                        dimension["Name"].as_str().unwrap_or_default().to_string(),
                        dimension["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            series.push(dimensions);
        }
        next_token = answered["NextToken"].as_str().map(ToString::to_string);
        if next_token.is_none() {
            return series;
        }
    }
}

/// The series `probe_id` publishes, when CloudWatch lists one for it: the dimension
/// set whose values carry that probe.
async fn series_of_probe(
    case: &str,
    router: &MultiServiceRouter,
    probe_id: &str,
) -> Option<BTreeMap<String, String>> {
    listed_series(case, router)
        .await
        .into_iter()
        .find(|dimensions| dimensions.values().any(|value| value == probe_id))
}

/// Now, as the epoch seconds a CloudWatch read windows by.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The `(Average, SampleCount, Unit)` of every datapoint the series `dimensions`
/// names has, read exactly as a client reads one: `GetMetricStatistics` over a window
/// around now, at the minute a regular-resolution metric is stored at.
async fn datapoints(
    case: &str,
    router: &MultiServiceRouter,
    dimensions: &BTreeMap<String, String>,
) -> Vec<(f64, f64, String)> {
    let now = now_secs();
    let dimension_list: Vec<Value> = dimensions
        .iter()
        .map(|(name, value)| json!({"Name": name, "Value": value}))
        .collect();
    let answered = cloudwatch_ok(
        case,
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": PACKET_LOSS,
            "Dimensions": dimension_list,
            // An hour back and a minute on, so every window the clock can have
            // published by now is inside the read whatever minute it started in.
            "StartTime": now - 3_600.0,
            "EndTime": now + 60.0,
            "Period": PERIOD_SECONDS,
            "Statistics": ["Average", "SampleCount"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            (
                point["Average"].as_f64().unwrap_or_default(),
                point["SampleCount"].as_f64().unwrap_or_default(),
                point["Unit"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Look for `probe_id`'s series until CloudWatch lists it AND it carries at least
/// `windows` observations, up to a deadline far longer than the spans involved.
/// Nothing but the clock publishes one.
async fn published(
    case: &str,
    router: &MultiServiceRouter,
    probe_id: &str,
    windows: f64,
) -> (BTreeMap<String, String>, Vec<(f64, f64, String)>) {
    let deadline = Instant::now() + PUBLISH_DEADLINE;
    loop {
        if let Some(dimensions) = series_of_probe(case, router, probe_id).await {
            let points = datapoints(case, router, &dimensions).await;
            let observed: f64 = points.iter().map(|(_, sample_count, _)| sample_count).sum();
            if observed >= windows {
                return (dimensions, points);
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {probe_id} to have published at least {windows} window(s) of \
             {PACKET_LOSS} with nothing but the clock running, the series still holds fewer"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `GET /monitors/{monitorName}/probes/{probeId}`.
async fn get_probe(router: &MultiServiceRouter, name: &str, probe_id: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/monitors/{name}/probes/{probe_id}"),
        None,
    )
    .await
}

/// The `probeId` of the single probe `GetMonitor` lists for `name`, waited for the
/// way the client does: a monitor lists the probe it was created with, and the probe
/// reaches `ACTIVE` once the clock has made its activation due.
async fn live_probe(case: &str, router: &MultiServiceRouter, name: &str) -> String {
    let deadline = Instant::now() + PUBLISH_DEADLINE;
    loop {
        let (status, got) = get_monitor(router, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetMonitor expected 200, got {status} {got:?}"
        );
        let probe_id = probes_of(&got)
            .first()
            .map(|probe| string_at(probe, "probeId"))
            .unwrap_or_default();
        if !probe_id.is_empty() {
            let (status, probe) = get_probe(router, name, &probe_id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetProbe expected 200, got {status} {probe:?}"
            );
            if string_at(&probe, "state") == "ACTIVE" {
                return probe_id;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the monitor's probe to reach ACTIVE, it has not"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row: the monitor and the probe a case creates.
struct ProbeCase {
    case: &'static str,
    /// The `aggregationPeriod` the create names, or `None` to leave it out — "60 is
    /// the default if no period is chosen"
    /// (`CreateMonitorInput$aggregationPeriod`).
    aggregation_period: Option<i64>,
    /// The probe's `destination`, "the destination IP address … either IPV4 or IPV6"
    /// (`ProbeInput$destination`).
    destination: &'static str,
    /// Its `protocol`, "either TCP or ICMP" (`CreateMonitor`).
    protocol: &'static str,
    /// The members that protocol needs beside them: `destinationPort` is "required
    /// only if the protocol is TCP", and `packetSize` "must be a number between 56
    /// and 8500" (`CreateMonitor`).
    extra: &'static str,
}

/// The rows: both documented aggregation periods and a monitor that names none, both
/// protocols, IPv4 and IPv6 destinations, and packet sizes at both ends of the
/// documented range. The recorded case — 30 s windows, ICMP, `packetSize` 56 — is the
/// first row.
const PROBE_CASES: &[ProbeCase] = &[
    ProbeCase {
        case: "the recorded case: 30 s windows, ICMP, packetSize 56",
        aggregation_period: Some(30),
        destination: "10.0.4.7",
        protocol: "ICMP",
        extra: r#","packetSize":56"#,
    },
    ProbeCase {
        case: "60 s windows, the largest packet",
        aggregation_period: Some(60),
        destination: "10.0.4.8",
        protocol: "ICMP",
        extra: r#","packetSize":8500"#,
    },
    ProbeCase {
        case: "no aggregationPeriod named, so the documented default",
        aggregation_period: None,
        destination: "10.0.4.9",
        protocol: "ICMP",
        extra: "",
    },
    ProbeCase {
        case: "a TCP probe, which carries a destination port",
        aggregation_period: Some(30),
        destination: "192.0.2.10",
        protocol: "TCP",
        extra: r#","destinationPort":443,"packetSize":56"#,
    },
    ProbeCase {
        case: "an IPv6 destination",
        aggregation_period: Some(60),
        destination: "2001:db8::1",
        protocol: "ICMP",
        extra: r#","packetSize":56"#,
    },
];

/// The `CreateMonitor` body one row sends: its own monitor, holding its one probe.
fn create_body(row: &ProbeCase, name: &str) -> String {
    let probe = probe_json(row.destination, row.protocol, row.extra);
    match row.aggregation_period {
        Some(period) => {
            format!(r#"{{"monitorName":"{name}","aggregationPeriod":{period},"probes":[{probe}]}}"#)
        }
        None => format!(r#"{{"monitorName":"{name}","probes":[{probe}]}}"#),
    }
}

/// RULE: a probe that has reached `ACTIVE` publishes its measurements, under the
/// monitor and probe they are about, and what they read is the loss a destination
/// that does not answer produces.
#[tokio::test]
async fn every_live_probe_publishes_the_packet_loss_it_measures() {
    let router = emulator();
    for (index, row) in PROBE_CASES.iter().enumerate() {
        let case = row.case;
        let name = format!("published-{index}");
        let (status, created) = create_monitor(&router, &create_body(row, &name)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateMonitor expected 200, got {status} {created:?}"
        );
        let probe_id = live_probe(case, &router, &name).await;
        let (dimensions, points) = published(case, &router, &probe_id, 1.0).await;

        // The series is REPORTED as this probe's, of this monitor: both identities
        // the measurements are about, taken from what the API answered rather than
        // assumed.
        let values: BTreeSet<&String> = dimensions.values().collect();
        let expected: BTreeSet<&String> = [&name, &probe_id].into_iter().collect();
        assert_eq!(
            values, expected,
            "case {case:?}: expected the series to be dimensioned by the monitor and the probe, \
             got {dimensions:?}"
        );
        assert!(
            !points.is_empty(),
            "case {case:?}: expected datapoints for {probe_id}, got none"
        );
        for (average, sample_count, unit) in &points {
            assert_eq!(
                *average, TOTAL_LOSS,
                "case {case:?}: a destination that never answers loses every packet, so the \
                 average must be {TOTAL_LOSS}, got {average} in {points:?}"
            );
            assert!(
                *sample_count > 0.0,
                "case {case:?}: a published window is an observation, got a sample count of \
                 {sample_count} in {points:?}"
            );
            assert_eq!(
                unit, PERCENT,
                "case {case:?}: a share of packets is published in {PERCENT}, got {unit:?} in \
                 {points:?}"
            );
        }
    }
}

/// RULE, the half that keeps a series one probe's own: each probe of a monitor
/// publishes its OWN series, so a reader can tell which path a datapoint describes.
#[tokio::test]
async fn each_probe_of_a_monitor_publishes_its_own_series() {
    let case = "two probes of one monitor";
    let router = emulator();
    let name = "two-probe-monitor";
    let probes: Vec<String> = (0..2)
        .map(|index| probe_in_subnet(100 + index, "10.2.0.1"))
        .collect();
    let body = format!(
        r#"{{"monitorName":"{name}","aggregationPeriod":30,"probes":[{}]}}"#,
        probes.join(",")
    );
    let (status, created) = create_monitor(&router, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateMonitor expected 200, got {status} {created:?}"
    );
    let (_, got) = get_monitor(&router, name).await;
    let ids: Vec<String> = probes_of(&got)
        .iter()
        .map(|probe| string_at(probe, "probeId"))
        .collect();
    assert_eq!(
        ids.len(),
        2,
        "case {case:?}: the monitor must hold both probes, got {got:?}"
    );

    let mut series: Vec<BTreeMap<String, String>> = Vec::new();
    for probe_id in &ids {
        let (dimensions, points) = published(case, &router, probe_id, 1.0).await;
        for (average, _, _) in &points {
            assert_eq!(
                *average, TOTAL_LOSS,
                "case {case:?}: expected {probe_id} to report {TOTAL_LOSS}, got {average}"
            );
        }
        // No other probe's id rides along in this series.
        for other in &ids {
            assert!(
                other == probe_id || !dimensions.values().any(|value| value == other),
                "case {case:?}: the series of {probe_id} must not name {other}, got \
                 {dimensions:?}"
            );
        }
        series.push(dimensions);
    }
    assert_ne!(
        series.first(),
        series.get(1),
        "case {case:?}: each probe publishes a series of its own, got {series:?}"
    );
}

/// RULE, the cadence half: the windows keep coming, one per `aggregationPeriod`, so a
/// client that watches the series goes on getting observations rather than a single
/// datapoint at activation.
///
/// Read at the minute a regular-resolution metric is stored at, so the two windows of
/// a 30-second monitor are observations of the same datapoint.
#[tokio::test]
async fn a_live_probe_keeps_publishing_one_window_per_aggregation_period() {
    let case = "30 s windows, read until a second one lands";
    let router = emulator();
    let name = "cadence-monitor";
    let body = format!(
        r#"{{"monitorName":"{name}","aggregationPeriod":30,"probes":[{}]}}"#,
        probe_json("10.3.0.1", "ICMP", r#","packetSize":56"#)
    );
    let (status, created) = create_monitor(&router, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateMonitor expected 200, got {status} {created:?}"
    );
    let probe_id = live_probe(case, &router, name).await;
    let (_, points) = published(case, &router, &probe_id, 2.0).await;
    let observed: f64 = points.iter().map(|(_, sample_count, _)| sample_count).sum();
    assert!(
        observed >= 2.0,
        "case {case:?}: expected the series to gain a window per aggregation period, got \
         {observed} observation(s) in {points:?}"
    );
    for (average, _, _) in &points {
        assert_eq!(
            *average, TOTAL_LOSS,
            "case {case:?}: every window of an unanswered destination reads {TOTAL_LOSS}, got \
             {average} in {points:?}"
        );
    }
}

/// RULE, the half that WITHHOLDS a series: a probe a caller switched off before it
/// ever went live sends no packets, so it measures nothing and publishes nothing — an
/// empty series is a fact about the probe rather than about the publisher.
///
/// Race-free without a timing assumption: a control monitor created in the same
/// instant is left alone, and the absence is only asserted once THAT probe has
/// published, which is what shows the publisher ran at all.
#[tokio::test]
async fn a_probe_switched_off_before_it_goes_live_publishes_nothing() {
    let case = "a probe deactivated while it was still provisioning";
    let router = emulator();
    let control = "publishing-control";
    let silent = "switched-off-monitor";
    for name in [control, silent] {
        let body = format!(
            r#"{{"monitorName":"{name}","aggregationPeriod":30,"probes":[{}]}}"#,
            probe_json("10.4.0.1", "ICMP", r#","packetSize":56"#)
        );
        let (status, created) = create_monitor(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateMonitor for {name:?} expected 200, got {status} {created:?}"
        );
    }
    // Straight away, while the probe is still `PENDING`: it never reaches `ACTIVE`,
    // so no window of it is ever owed.
    let (_, got) = get_monitor(&router, silent).await;
    let silent_probe = probes_of(&got)
        .first()
        .map(|probe| string_at(probe, "probeId"))
        .unwrap_or_default();
    assert!(
        !silent_probe.is_empty(),
        "case {case:?}: the monitor must list its probe, got {got:?}"
    );
    let (status, updated) = call(
        &router,
        "PATCH",
        &format!("/monitors/{silent}/probes/{silent_probe}"),
        Some(r#"{"state":"INACTIVE"}"#),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateProbe INACTIVE expected 200, got {status} {updated:?}"
    );

    let control_probe = live_probe(case, &router, control).await;
    let (_, control_points) = published(case, &router, &control_probe, 1.0).await;
    assert!(
        !control_points.is_empty(),
        "case {case:?}: the control probe must publish, got {control_points:?}"
    );
    let listed = series_of_probe(case, &router, &silent_probe).await;
    assert_eq!(
        listed, None,
        "case {case:?}: expected no series for a probe that never went live, got {listed:?}"
    );
    let (status, probe) = get_probe(&router, silent, &silent_probe).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetProbe expected 200, got {status} {probe:?}"
    );
    assert_eq!(
        string_at(&probe, "state"),
        "INACTIVE",
        "case {case:?}: the switched-off probe must still report INACTIVE, got {probe:?}"
    );
}
