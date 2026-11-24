// SPDX-License-Identifier: CC0-1.0

//! Whether reading the service map of ONE window twice answers the same map, and
//! whether the FIRST read after an ingest already carries what that ingest
//! accepted — end to end over the wire.
//!
//! `GetServiceGraph` is derived from the stored segment documents at read time
//! and is modelled `@paginated` on `NextToken` over `Services`
//! (`smithy_query(query="operation",operation="GetServiceGraph")`), so a client
//! discovers a topology by walking one window's pages and then watching that
//! window: it polls the same bounds again and again and compares what it got.
//! Two rules follow, and they are what these cases pin:
//!
//! * a service whose segment `PutTraceSegments` accepted is a node of every
//!   window holding that segment's recorded instant, from the FIRST read after
//!   the ingest answered — no settling pass, no second read, no delay; and
//! * reading a window does not change it: the same bounds answer the same map on
//!   every later read, node for node and member for member, whatever else the
//!   account does in between, so a watcher never sees a node appear or drop out
//!   of a window whose stored traffic did not change.
//!
//! Beside them the separation of the two ingest pipelines: `PutTelemetryRecords`
//! ("used by the Amazon Web Services X-Ray daemon to upload telemetry") stores a
//! reporting source's counters and nothing it stores is traced traffic, so no
//! spelling of a reporting source — `EC2InstanceId`, `Hostname`, `ResourceARN` —
//! is ever a node `Name` or one of a node's `Names` aliases, however often that
//! daemon reports and however often the map is read.
//!
//! The rows differ along what the rules generalise over: WHICH window is asked
//! about relative to the recorded instants (the shape a client polls with, a
//! tight one, one starting exactly on a segment's instant, the widest this read
//! serves, one holding no recorded instant at all), HOW MANY times it is read,
//! WHAT the account does between two reads (nothing, more telemetry from each
//! reporter member), and the FORM of the accepted segment (complete, in
//! progress, carrying an `http` block, carrying a downstream call) with its
//! instant at each edge of the window.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::xray_wire::{array, root_document, trace_id, Xray};

/// Now, in whole seconds of epoch time: every fixture records traffic just
/// before it, so the windows asked about name intervals of recorded traffic.
///
/// A WHOLE second, because a case places a recorded instant exactly on a
/// window's bound: a window's bounds travel as the modelled `Timestamp` and a
/// segment's `start_time` as the document's own JSON number, so only an instant
/// both forms carry exactly is the same instant on both sides of that
/// comparison — a fractional one is each form's nearest representable neighbour,
/// which is what every other case here keeps well clear of the bounds for.
fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
        .floor()
}

/// More pages than the map of any window these cases build is cut into, so a
/// walk that does not terminate fails its case instead of hanging.
const MAX_PAGES: usize = 20;

/// The widest window this read serves, in seconds: the bound the read publishes
/// ([`service_com_amazonaws_xray_awsxray::state::MAX_SERVICE_GRAPH_WINDOW_HOURS`]),
/// read from the service's own statement of it so no case drifts from it.
#[allow(clippy::cast_precision_loss)]
const WIDEST_WINDOW_SECONDS: f64 =
    (service_com_amazonaws_xray_awsxray::state::MAX_SERVICE_GRAPH_WINDOW_HOURS * 60 * 60) as f64;

/// Every `Service` the window `[start, end)` reports, in the order its pages
/// serve them — what a client that walks the whole map reads.
///
/// Fails the case when a page is refused or the walk does not end, because
/// neither answer is a map.
async fn walked_services(case: &str, xray: &Xray, start: f64, end: f64) -> Vec<Value> {
    let mut services: Vec<Value> = Vec::new();
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
        services.extend(array(&answer.body, "Services").iter().cloned());
        pages += 1;
        token = answer.get("NextToken").as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            pages < MAX_PAGES,
            "case {case:?}: the walk of this window must end; it is still unread after {pages} \
             pages carrying {} services",
            services.len()
        );
    }
    services
}

/// Every name a walk of a window reports: each node's `Name` and each entry of
/// its `Names` alias list, which is what a client looks its own service up by.
fn reported_names(services: &[Value]) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for service in services {
        if let Some(name) = service.get("Name").and_then(Value::as_str) {
            names.insert(name.to_string());
        }
        for alias in array(service, "Names") {
            if let Some(alias) = alias.as_str() {
                names.insert(alias.to_string());
            }
        }
    }
    names
}

/// Submit one segment document and require that the ingest accepted it, which is
/// what every case here reads the map after.
async fn accept_segment(case: &str, xray: &Xray, document: &Value) {
    let put = xray.put_segments(&[document.to_string()]).await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: PutTraceSegments must answer 200, got {put:?}"
    );
    assert!(
        array(&put.body, "UnprocessedTraceSegments").is_empty(),
        "case {case:?}: the document must be accepted, got {put:?}"
    );
}

/// Upload one telemetry batch from the reporting source `source` names — one
/// entry per reporter member the request carries, which is how a daemon
/// identifies itself and what a case varies to show no spelling of one becomes a
/// node.
async fn put_telemetry(case: &str, xray: &Xray, source: &[(&str, &str)], at: f64) {
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
        "case {case:?}: the telemetry upload from {source:?} must be accepted, got {answer:?}"
    );
}

/// The reporting sources every case uploads telemetry from, one per spelling of a
/// source's identity, and the values that only ever identified one.
fn telemetry_sources() -> Vec<Vec<(&'static str, &'static str)>> {
    vec![
        vec![("Hostname", "probe-telonly-host")],
        vec![("EC2InstanceId", "i-0123456789abcdef0")],
        vec![
            ("EC2InstanceId", "i-00000000000000009"),
            ("Hostname", "probe-telonly-every-member"),
            ("ResourceARN", "*"),
        ],
    ]
}

/// The values the uploads above identified a reporting source by: none of them
/// is traced traffic, so none is ever a node of any map.
const TELEMETRY_ONLY_VALUES: [&str; 5] = [
    "probe-telonly-host",
    "i-0123456789abcdef0",
    "i-00000000000000009",
    "probe-telonly-every-member",
    "*",
];

/// One case of the repeated-read table: the window, how many times it is read,
/// and what the account does between two of those reads.
struct RepeatedRead {
    case: &'static str,
    start: f64,
    end: f64,
    reads: usize,
    /// The reporting source a daemon uploads from between two reads, for a case
    /// whose rule is that an upload cannot change the map; `None` for a case
    /// that reads one window with nothing happening in between.
    between: Option<Vec<(&'static str, &'static str)>>,
}

#[tokio::test]
async fn the_map_of_one_window_is_the_same_on_every_read_of_it() {
    let xray = Xray::new();
    let base = now() - 1.5;

    // Three services that reported a segment, at instants spread over the ten
    // minutes a client polls, and telemetry from three reporting sources.
    let recorded: Vec<(&str, f64)> = vec![
        ("probe-repeat-now", base),
        ("probe-repeat-minutes-back", base - 120.0),
        ("probe-repeat-near-the-edge", base - 540.0),
    ];
    for (at, (name, start)) in recorded.iter().enumerate() {
        let mut document = root_document(
            &format!("a0010000000000{at:02}"),
            &trace_id(930 + u32::try_from(at).unwrap_or_default()),
            name,
            *start,
            Some(start + 0.5),
        );
        document["http"] = json!({
            "request": {"method": "GET", "url": "https://example.invalid/traced"},
            "response": {"status": 200},
        });
        accept_segment("the repeated-read fixture", &xray, &document).await;
    }
    for source in &telemetry_sources() {
        put_telemetry("the repeated-read fixture", &xray, source, base).await;
    }

    // One row per shape of window and per thing the account does between two
    // reads of it.
    let rows: Vec<RepeatedRead> = vec![
        RepeatedRead {
            case: "the window a client polls with, read a dozen times",
            start: base - 600.0,
            end: base + 300.0,
            reads: 12,
            between: None,
        },
        RepeatedRead {
            case: "the same window while a daemon keeps reporting between reads",
            start: base - 600.0,
            end: base + 300.0,
            reads: 8,
            between: Some(vec![("Hostname", "probe-telonly-host")]),
        },
        RepeatedRead {
            case: "a tight window around the newest segment",
            start: base - 0.25,
            end: base + 0.25,
            reads: 5,
            between: None,
        },
        RepeatedRead {
            case: "a window starting exactly on a segment's recorded instant",
            start: base,
            end: base + 60.0,
            reads: 5,
            between: None,
        },
        RepeatedRead {
            case: "the widest window this read serves",
            start: base + 300.0 - WIDEST_WINDOW_SECONDS,
            end: base + 300.0,
            reads: 5,
            between: None,
        },
        RepeatedRead {
            case: "a window holding no recorded instant",
            start: base - 3_600.0,
            end: base - 1_800.0,
            reads: 4,
            between: None,
        },
    ];

    for RepeatedRead {
        case,
        start,
        end,
        reads,
        between,
    } in rows
    {
        let first = walked_services(case, &xray, start, end).await;
        // The services whose recorded instant this window holds: a node of it on
        // every read, as the half-open selection every windowed derivation of
        // this service makes decides it.
        let expected: Vec<&str> = recorded
            .iter()
            .filter(|(_, at)| *at >= start && *at < end)
            .map(|(name, _)| *name)
            .collect();
        for read in 1..=reads {
            if let Some(source) = between.as_deref() {
                put_telemetry(case, &xray, source, base).await;
            }
            let again = walked_services(case, &xray, start, end).await;
            assert_eq!(
                again,
                first,
                "case {case:?}: read {read} of the same window must answer the map its first read \
                 did, got {} services instead of {}",
                again.len(),
                first.len()
            );
            let names = reported_names(&again);
            for name in &expected {
                assert!(
                    names.contains(*name),
                    "case {case:?}: read {read} must still carry the service of a segment this \
                     window holds, expected {name:?} among {names:?}"
                );
            }
            for (name, _) in &recorded {
                if !expected.contains(name) {
                    assert!(
                        !names.contains(*name),
                        "case {case:?}: read {read} must not carry a service whose segment lies \
                         outside this window, expected {name:?} absent, got {names:?}"
                    );
                }
            }
            for host in TELEMETRY_ONLY_VALUES {
                assert!(
                    !names.contains(host),
                    "case {case:?}: read {read} must never carry a telemetry-only reporting \
                     source, expected {host:?} absent, got {names:?}"
                );
            }
        }
    }
}

/// One case of the first-read table: the form of the accepted segment, where
/// its instant sits inside the window asked about, and what that first read owes
/// besides the service's own node.
struct FirstRead {
    case: &'static str,
    /// The name the segment reports its service by, which is the node name the
    /// first read owes.
    name: &'static str,
    /// How far into the window the segment's instant sits.
    offset: f64,
    /// Whether the segment completed; one that did not is still in progress and
    /// carries no `end_time`.
    complete: bool,
    /// A block the document carries beside the four members every segment has.
    extra: Option<Value>,
    /// Nodes the same first read owes because the document named them — the
    /// downstream service a call out of the process reached.
    downstream: &'static [&'static str],
}

#[tokio::test]
async fn the_first_read_after_an_accepted_segment_carries_its_service() {
    // One row per form of accepted segment, and per edge of the window its
    // instant can sit on.
    let rows: Vec<FirstRead> = vec![
        FirstRead {
            case: "a completed root segment",
            name: "probe-first-complete",
            offset: 60.0,
            complete: true,
            extra: None,
            downstream: &[],
        },
        FirstRead {
            case: "a root segment still in progress",
            name: "probe-first-in-progress",
            offset: 60.0,
            complete: false,
            extra: None,
            downstream: &[],
        },
        FirstRead {
            case: "a root segment carrying the recorded request",
            name: "probe-first-http",
            offset: 60.0,
            complete: true,
            extra: Some(json!({
                "http": {
                    "request": {"method": "GET", "url": "https://example.invalid/traced"},
                    "response": {"status": 200},
                },
            })),
            downstream: &[],
        },
        FirstRead {
            case: "a root segment that called a downstream service",
            name: "probe-first-caller",
            offset: 60.0,
            complete: true,
            extra: Some(json!({
                "subsegments": [{
                    "id": "b000000000000001",
                    "name": "probe-first-downstream",
                    "namespace": "remote",
                    "start_time": 0.0,
                    "end_time": 0.0,
                }],
            })),
            // The call left the process, so the service on the far side of it is
            // a node of the same first read.
            downstream: &["probe-first-downstream"],
        },
        FirstRead {
            case: "a segment recorded exactly on the window's StartTime",
            name: "probe-first-at-start",
            offset: 0.0,
            complete: true,
            extra: None,
            downstream: &[],
        },
        FirstRead {
            case: "a segment recorded just under the window's EndTime",
            name: "probe-first-at-end",
            offset: 599.0,
            complete: true,
            extra: None,
            downstream: &[],
        },
    ];

    for FirstRead {
        case,
        name,
        offset,
        complete,
        extra,
        downstream,
    } in rows
    {
        let xray = Xray::new();
        // The window a client asks about, and the instant inside it the segment
        // is recorded at.
        let start = now() - 300.0;
        let end = start + 600.0;
        let at = start + offset;
        let mut document = root_document(
            "c000000000000001",
            &trace_id(940),
            name,
            at,
            complete.then_some(at + 0.5),
        );
        if let Some(extra) = extra {
            for (member, value) in extra.as_object().expect("the extra block is an object") {
                document[member.clone()] = value.clone();
            }
        }
        // A daemon that only uploads counters reports no traced service, so its
        // identity must be on no map — including this very first read.
        for source in &telemetry_sources() {
            put_telemetry(case, &xray, source, at).await;
        }
        accept_segment(case, &xray, &document).await;

        let services = walked_services(case, &xray, start, end).await;
        let names = reported_names(&services);
        assert!(
            names.contains(name),
            "case {case:?}: the FIRST read of a window holding the accepted segment's instant \
             must carry its service, expected {name:?} among {names:?}"
        );
        for called in downstream {
            assert!(
                names.contains(*called),
                "case {case:?}: the FIRST read must carry the downstream service the accepted \
                 document named, expected {called:?} among {names:?}"
            );
        }
        for host in TELEMETRY_ONLY_VALUES {
            assert!(
                !names.contains(host),
                "case {case:?}: a telemetry-only reporting source must be on no map, expected \
                 {host:?} absent, got {names:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_segment_recorded_on_a_windows_end_time_is_outside_it() {
    // The other side of the selection the derivation makes: a window is the
    // half-open interval `[StartTime, EndTime)` — the one selection every
    // windowed read of this service makes — so a segment recorded exactly on a
    // window's `EndTime` belongs to the next window, not to this one, and it is
    // the window that starts there that carries it.
    let xray = Xray::new();
    let at = now() - 1.5;
    let name = "probe-edge-on-the-end";
    accept_segment(
        "a segment on the window's EndTime",
        &xray,
        &root_document("d000000000000001", &trace_id(950), name, at, Some(at + 0.25)),
    )
    .await;

    // (case, window start, window end, whether the window holds the instant)
    let rows: [(&str, f64, f64, bool); 3] = [
        ("the window that ends on the instant", at - 600.0, at, false),
        ("the window that starts on the instant", at, at + 600.0, true),
        ("the window that spans the instant", at - 600.0, at + 600.0, true),
    ];
    for (case, start, end, holds) in rows {
        let names = reported_names(&walked_services(case, &xray, start, end).await);
        assert_eq!(
            names.contains(name),
            holds,
            "case {case:?}: expected the service {name:?} {}, got {names:?}",
            if holds { "present" } else { "absent" }
        );
    }
}
