// SPDX-License-Identifier: CC0-1.0

//! Whether a service whose segment `PutTraceSegments` has just accepted is
//! READABLE from the service map, end to end over the wire.
//!
//! The service map is X-Ray's primary aggregate read: it has no selector but the
//! window, an `Edge` names its downstream node by a `ReferenceId` that may land
//! on any page, and a client reads it to discover what it is talking to — so the
//! map is only useful whole, and a client reads it whole by following
//! `NextToken` a bounded number of times. The rule under test is therefore about
//! the WALK and not about one page: a service the ingest accepted is a node of
//! every window containing its segment, and reading that window's map the way a
//! client does reaches it however late its name sorts and however many other
//! services share the window.
//!
//! The contrast the same rule implies is tested beside it: the trace pipeline and
//! the TELEMETRY pipeline are separate, so a host that only uploaded
//! `PutTelemetryRecords` counters is a node of no map, whatever else the window
//! holds.
//!
//! The rows differ along what the rule generalises over: how many services share
//! the window (one, a page's worth, a crowded account) and where the probe's own
//! name sorts among them (alone, last, first, last again), WHICH window is asked
//! about relative to the instant a segment was recorded (the shape a client polls
//! with just after submitting, one that ends before the newest segment, a tight
//! one around it, one holding no recorded instant at all), and HOW a host
//! reported — a segment, telemetry under each of the three reporter members, or
//! both.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_xray_awsxray::ops::get_service_graph::SERVICE_GRAPH_PAGE_SIZE;

use crate::xray_wire::{array, root_document, trace_id, Xray};

/// Now, in seconds of epoch time: the probe records traffic just before it, so
/// the window asked about names an interval of recorded traffic.
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// How many times a client follows `NextToken` before it stops.
///
/// A client walks the map with a paginator, which follows a BOUNDED number of
/// pages; past that it reports what it has. So a window whose map takes more
/// pages than this is a map no client ever sees the whole of, which is
/// indistinguishable from a service that never reported.
const PAGES_A_CLIENT_WALKS: usize = 6;

/// How many services share the window in the crowded rows.
///
/// An account with a hundred-odd traced services is ordinary, and the map of one
/// is what a single walk has to carry.
const CROWDED_WINDOW_SERVICES: usize = 150;

/// `GetServiceGraph` over `[start, end)`, walked the way a client walks it:
/// every page of `Services[*].Name` and `Services[*].Names[*]`, following
/// `NextToken` while one is issued.
///
/// Answers the names read and how many pages it took. Fails the case if the
/// walk a client makes does not reach the end of the map.
async fn walked_names(case: &str, xray: &Xray, start: f64, end: f64) -> (BTreeSet<String>, usize) {
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut body = json!({ "StartTime": start, "EndTime": end });
        if let Some(token) = token.as_deref() {
            body["NextToken"] = json!(token);
        }
        let answer = xray.call("/ServiceGraph", &body).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: every page of the map must be answered, got {answer:?}"
        );
        for service in array(&answer.body, "Services") {
            if let Some(name) = service.get("Name").and_then(Value::as_str) {
                names.insert(name.to_string());
            }
            for alias in array(service, "Names") {
                if let Some(alias) = alias.as_str() {
                    names.insert(alias.to_string());
                }
            }
        }
        pages += 1;
        token = answer.get("NextToken").as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            pages < PAGES_A_CLIENT_WALKS,
            "case {case:?}: a client follows at most {PAGES_A_CLIENT_WALKS} pages, and the map \
             of this window is still unread after {pages} of them (it has carried {} names so \
             far)",
            names.len()
        );
    }
    (names, pages)
}

/// Upload one telemetry batch from the host `hostname` — the counters the X-Ray
/// daemon reports, which name a host and no trace.
async fn put_telemetry(xray: &Xray, hostname: &str, at: f64) {
    put_telemetry_from(
        xray,
        &[
            ("EC2InstanceId", "i-0123456789abcdef0"),
            ("Hostname", hostname),
            ("ResourceARN", "*"),
        ],
        at,
    )
    .await;
}

/// The same upload from the reporting source `source` names, one entry per
/// reporter member the request carries — which is how a source is identified,
/// and what a case varies to show that NO spelling of one becomes a node.
async fn put_telemetry_from(xray: &Xray, source: &[(&str, &str)], at: f64) {
    let mut body = json!({
        "TelemetryRecords": [{
            "Timestamp": at,
            "SegmentsReceivedCount": 5,
            "SegmentsSentCount": 5,
            "SegmentsSpilloverCount": 0,
            "SegmentsRejectedCount": 0,
        }],
    });
    for (member, value) in source {
        body[*member] = json!(value);
    }
    let answer = xray.call("/TelemetryRecords", &body).await;
    assert_eq!(
        answer.status, 200,
        "the telemetry upload from {source:?} must be accepted, got {answer:?}"
    );
}

#[tokio::test]
async fn an_accepted_segments_service_is_read_from_the_map_within_a_client_page_walk() {
    // (case, how many OTHER services share the window, the probe's own name)
    let rows: Vec<(&str, usize, &str)> = vec![
        ("the only service in the window", 0, "probe-traced-solo"),
        (
            "a page's worth of services share the window",
            SERVICE_GRAPH_PAGE_SIZE,
            "probe-traced-one-page",
        ),
        (
            "a crowded window, the probe sorting FIRST by name",
            CROWDED_WINDOW_SERVICES,
            "aaa-probe-traced-first",
        ),
        (
            "a crowded window, the probe sorting LAST by name",
            CROWDED_WINDOW_SERVICES,
            "zzz-probe-traced-last",
        ),
    ];

    for (case, crowd, probe) in rows {
        let xray = Xray::new();
        let base = now() - 1.5;
        let trace = trace_id(910);

        // The services that share the window: named so they sort between the
        // two probe names, which is what puts a probe at either end of the map.
        let mut documents: Vec<String> = Vec::with_capacity(crowd + 1);
        for index in 0..crowd {
            documents.push(
                root_document(
                    &format!("{:016x}", index + 1),
                    &trace,
                    &format!("shares-window-{index:04}"),
                    base,
                    Some(base + 0.4),
                )
                .to_string(),
            );
        }
        // The probe's own root segment, with the recorded request on it.
        let mut probe_document = root_document(
            &format!("{:016x}", crowd + 1),
            &trace,
            probe,
            base,
            Some(base + 0.9),
        );
        probe_document["http"] = json!({
            "request": {"method": "GET", "url": "https://example.invalid/traced"},
            "response": {"status": 200},
        });
        documents.push(probe_document.to_string());

        let put = xray.put_segments(&documents).await;
        assert_eq!(
            put.status, 200,
            "case {case:?}: PutTraceSegments must answer 200, got {put:?}"
        );
        assert!(
            array(&put.body, "UnprocessedTraceSegments").is_empty(),
            "case {case:?}: every document must be accepted, got {put:?}"
        );

        // A host that ONLY reported telemetry: the two pipelines are separate,
        // so this name must be a node of no map.
        let telemetry_only = format!("probe-telonly-{crowd}");
        put_telemetry(&xray, &telemetry_only, base).await;

        let (names, pages) = walked_names(case, &xray, base - 600.0, base + 300.0).await;
        assert!(
            names.contains(probe),
            "case {case:?}: the service of an accepted segment must be readable from the map of \
             a window containing it; {pages} page(s) carried {} names and not {probe:?}",
            names.len()
        );
        assert!(
            !names.contains(&telemetry_only),
            "case {case:?}: a host that only uploaded telemetry reports no traced service, so \
             {telemetry_only:?} must be on no map; {pages} page(s) carried it"
        );
    }
}

#[tokio::test]
async fn the_window_decides_which_services_the_map_carries_and_telemetry_none_of_them() {
    // The same rule read along the WINDOW instead of along the crowd: a stored
    // segment's service is a node of exactly the windows its recorded instant
    // falls in, and no spelling of a telemetry-only reporting source is a node
    // of any of them.
    let xray = Xray::new();
    let now = now();

    // Services that reported a segment, at instants spread over the last ten
    // minutes, and one that reported a segment AND uploaded telemetry under the
    // same name — so absence is a fact about the telemetry pipeline rather than
    // a name this read filters out.
    let recorded: Vec<(&str, f64)> = vec![
        ("probe-window-now", now - 1.5),
        ("probe-window-minutes-back", now - 120.0),
        ("probe-window-near-the-edge", now - 540.0),
        ("probe-window-and-reporting", now - 30.0),
    ];
    for (at, (name, start)) in recorded.iter().enumerate() {
        let trace = trace_id(920 + u32::try_from(at).unwrap_or_default());
        let mut document = root_document(
            &format!("e00100000000000{at}"),
            &trace,
            name,
            *start,
            Some(start + 0.5),
        );
        document["http"] = json!({
            "request": {"method": "GET", "url": "https://example.invalid/traced"},
            "response": {"status": 200},
        });
        let put = xray.put_segments(&[document.to_string()]).await;
        assert_eq!(
            put.status, 200,
            "case {name:?}: PutTraceSegments must answer 200, got {put:?}"
        );
        assert!(
            array(&put.body, "UnprocessedTraceSegments").is_empty(),
            "case {name:?}: the segment must be accepted, got {put:?}"
        );
    }

    // One upload per reporter member a source is identified by, plus one from
    // the host that also reported a segment above.
    let sources: Vec<Vec<(&str, &str)>> = vec![
        vec![("Hostname", "probe-telonly-hostname")],
        vec![("EC2InstanceId", "i-0123456789abcdef0")],
        vec![
            ("EC2InstanceId", "i-00000000000000001"),
            ("Hostname", "probe-telonly-every-member"),
            ("ResourceARN", "*"),
        ],
        vec![("Hostname", "probe-window-and-reporting")],
    ];
    for source in &sources {
        put_telemetry_from(&xray, source, now).await;
    }
    // Every value that only ever identified a reporting source: a node of no map.
    let telemetry_only = [
        "probe-telonly-hostname",
        "i-0123456789abcdef0",
        "i-00000000000000001",
        "probe-telonly-every-member",
        "*",
    ];

    // (case, window start, window end, the services the window holds)
    let windows: Vec<(&str, f64, f64, Vec<&str>)> = vec![
        (
            "ten minutes back and five minutes ahead, as a client polls",
            now - 601.5,
            now + 300.0,
            recorded.iter().map(|(name, _)| *name).collect(),
        ),
        (
            "a window ending before the newest segment",
            now - 600.0,
            now - 60.0,
            vec!["probe-window-minutes-back", "probe-window-near-the-edge"],
        ),
        (
            "a tight window around the newest segment",
            now - 2.0,
            now + 1.0,
            vec!["probe-window-now"],
        ),
        (
            "a window holding no recorded instant",
            now - 3_600.0,
            now - 1_800.0,
            vec![],
        ),
    ];

    for (case, start, end, expected) in windows {
        let (names, pages) = walked_names(case, &xray, start, end).await;
        for name in &expected {
            assert!(
                names.contains(*name),
                "case {case:?}: the service of a segment recorded in this window must be a node \
                 of it; {pages} page(s) carried {} names and not {name:?}",
                names.len()
            );
        }
        for (name, _) in &recorded {
            if !expected.contains(name) {
                assert!(
                    !names.contains(*name),
                    "case {case:?}: a service whose segment lies outside the window is no node \
                     of it, expected {name:?} absent, got {names:?}"
                );
            }
        }
        for host in telemetry_only {
            assert!(
                !names.contains(host),
                "case {case:?}: a telemetry-only reporting source must never be a node, expected \
                 {host:?} absent, got {names:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_telemetry_only_host_stays_off_the_map_while_the_traced_service_stays_on_it() {
    // The contrast re-checked over repeated reads and repeated uploads: neither
    // reading the map nor uploading more counters can make the telemetry host a
    // node or take the traced service off.
    let xray = Xray::new();
    let base = now() - 1.5;
    let trace = trace_id(911);
    let traced = "probe-traced-contrast";
    let telemetry_only = "probe-telonly-contrast";

    let put = xray
        .put_segments(&[
            root_document("00000000000000aa", &trace, traced, base, Some(base + 0.5)).to_string(),
        ])
        .await;
    assert_eq!(put.status, 200, "the probe segment must land, got {put:?}");

    for round in 1..=3 {
        put_telemetry(&xray, telemetry_only, base).await;
        let case = format!("round {round}");
        let (names, _) = walked_names(&case, &xray, base - 600.0, base + 300.0).await;
        assert!(
            names.contains(traced),
            "case {case:?}: the traced service must stay on the map, got {names:?}"
        );
        assert!(
            !names.contains(telemetry_only),
            "case {case:?}: the telemetry-only host must never become a node, got {names:?}"
        );
    }
}
