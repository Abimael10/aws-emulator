// SPDX-License-Identifier: CC0-1.0

//! `ListProfileTimes`, end to end over HTTP through the router this service mounts in
//! production.
//!
//! The rule every case here asserts: `ListProfileTimes` reports the start instants of
//! the aggregated profiles a stored group holds for the requested aggregation period —
//! each posted agent profile belongs to the one period-length window, aligned to UTC,
//! that contains its receipt time, and every such window whose aggregation has fallen
//! due and which holds at least one profile is reported once, filtered to the requested
//! time range, ordered by `orderBy` (descending by default) and paged by
//! `maxResults`/`nextToken`. A window the service is still collecting into has no
//! aggregated profile yet and is reported by nothing — the window of the period the
//! REQUEST names, so an hourly or daily window that has not closed reports nothing
//! however long the finer windows inside it have been aggregated.
//!
//! Rows vary what the rule generalises over: the aggregation period (all three the
//! model declares), how many profiles share a window, where a receipt sits inside its
//! window, the requested range, the order, the page size and the profile count —
//! including the group that has no profiles at all, which is the recorded corpus case
//! `list-profile-times-empty-before-any-data`.
//!
//! The emulator, the HTTP plumbing and the response reducer are the shared
//! [`crate::profiling_group_lifecycle`] harness, not a fork of it.

use aws_smithy_types::DateTime;
use aws_smithy_types::date_time::Format;

use crate::profiling_group_lifecycle::{Answer, Harness};

/// 2020-06-01T00:00:00Z, the reference instant these cases place receipts around. It
/// is a whole number of days past the epoch, so a window aligned to UTC starts here
/// for every one of the three aggregation periods.
const DAY_START: i64 = 1_590_969_600;

/// One day in seconds, for readable receipt and range arithmetic.
const DAY: i64 = 24 * 60 * 60;

/// One hour in seconds.
const HOUR: i64 = 60 * 60;

/// An instant as the ISO 8601 form the modelled `date-time` query members use.
fn iso(at: i64) -> String {
    DateTime::from_secs(at)
        .fmt(Format::DateTime)
        .unwrap_or_else(|error| panic!("unformattable instant {at}: {error}"))
}

/// The reported profile times, in the order the response lists them, as epoch seconds.
///
/// restJson1 carries a `Timestamp` member either as epoch seconds or in the ISO 8601
/// form, depending on the member's `timestampFormat`; these cases are about WHICH
/// instant is reported, so both renderings are read back the same way. `pub(crate)` so
/// every suite in this target reads a reported profile time one way.
pub(crate) fn profile_times(answer: &Answer) -> Vec<i64> {
    let json = answer.json();
    let times = json
        .get("profileTimes")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("no profileTimes array in {json:?}"))
        .clone();
    times
        .iter()
        .map(|entry| {
            let start = entry
                .get("start")
                .unwrap_or_else(|| panic!("no start in profile time {entry:?}"));
            match start {
                serde_json::Value::String(text) => DateTime::from_str(text, Format::DateTime)
                    .unwrap_or_else(|error| panic!("unparseable start {text:?}: {error}"))
                    .as_secs_f64() as i64,
                serde_json::Value::Number(seconds) => seconds
                    .as_f64()
                    .unwrap_or_else(|| panic!("unparseable start {seconds:?}")) as i64,
                other => panic!("start is neither a timestamp string nor seconds: {other:?}"),
            }
        })
        .collect()
}

fn next_token(answer: &Answer) -> Option<String> {
    answer
        .json()
        .get("nextToken")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

async fn create_group(harness: &Harness, name: &str) -> Answer {
    harness
        .send(
            "POST",
            &format!("/profilingGroups?clientToken=tok-{name}"),
            "application/json",
            serde_json::to_vec(&serde_json::json!({ "profilingGroupName": name }))
                .expect("a serializable body"),
        )
        .await
}

async fn list_profile_times(harness: &Harness, name: &str, query: &str) -> Answer {
    harness
        .send(
            "GET",
            &format!("/profilingGroups/{name}/profileTimes{query}"),
            "application/json",
            Vec::new(),
        )
        .await
}

/// Place the group's stored profiles at `receipts` (epoch seconds), oldest first.
///
/// Posting over HTTP stamps each profile with the clock and with the period its own
/// document states, so a case that needs profiles spread over hours and days — which is
/// what an aggregation period is about — places both afterwards: the receipt and the
/// period the data covers, which is the window the service collects the profile into.
/// This writes the same fields
/// `PostAgentProfile` wrote, through the harness's one shared engine, and nothing else.
/// `pub(crate)` so every suite that needs a profile at a chosen instant places it one
/// way.
pub(crate) fn place_receipts(harness: &Harness, name: &str, receipts: &[i64]) {
    let exact: Vec<(i64, u32)> = receipts.iter().map(|at| (*at, 0)).collect();
    place_receipts_at(harness, name, &exact);
}

/// [`place_receipts`] at nanosecond resolution: each receipt is (epoch seconds,
/// sub-second nanoseconds), oldest first.
///
/// A stored instant carries nanoseconds, so a case about how the service COMPARES two
/// stored instants places them a nanosecond apart rather than a second apart. `place_receipts`
/// is this helper with every sub-second part zero, so both go through one writer.
pub(crate) fn place_receipts_at(harness: &Harness, name: &str, receipts: &[(i64, u32)]) {
    let state = harness
        .engine
        .state::<service_com_amazonaws_codeguruprofiler_codeguruprofiler::ComAmazonawsCodeguruprofilerCodeguruprofilerState>();
    let mut guard = state.lock();
    let group = guard
        .groups
        .get_mut(name)
        .unwrap_or_else(|| panic!("no stored group named {name}"));
    assert_eq!(
        group.profiles.len(),
        receipts.len(),
        "group {name} holds {} profiles, not the {} receipts to place",
        group.profiles.len(),
        receipts.len()
    );
    for (profile, (secs, nanos)) in group.profiles.iter_mut().zip(receipts) {
        let at = DateTime::from_secs_and_nanos(*secs, *nanos);
        profile.received_at = at;
        profile.period_start = at;
    }
    group.latest_agent_profile_reported_at = receipts
        .iter()
        .max()
        .map(|(secs, nanos)| DateTime::from_secs_and_nanos(*secs, *nanos));
}

/// Store `bytes` as a profile of the stored group `name`, posted under `content_type` and
/// received at `at` (epoch seconds), WITHOUT submitting them.
///
/// `PostAgentProfile` parses the data it is handed and refuses data that is not an agent
/// profile, so bytes that are deliberately NO profile document — or a document that is no
/// profile, such as a bare list — cannot be put in the store over HTTP. A case whose rule
/// is a READER's (what `GetProfile` serves for stored bytes it cannot read, what a frame
/// metric tallies for a profile that records nothing) places them here instead, through
/// the harness's one shared engine, writing exactly the fields `PostAgentProfile` writes.
/// `pub(crate)` so every suite that needs such a profile places it one way.
pub(crate) fn place_profile(
    harness: &Harness,
    name: &str,
    content_type: &str,
    bytes: Vec<u8>,
    at: i64,
) {
    let state = harness
        .engine
        .state::<service_com_amazonaws_codeguruprofiler_codeguruprofiler::ComAmazonawsCodeguruprofilerCodeguruprofilerState>();
    let mut guard = state.lock();
    let group = guard
        .groups
        .get_mut(name)
        .unwrap_or_else(|| panic!("no stored group named {name}"));
    let received_at = DateTime::from_secs(at);
    group.profiles.push_back(
        service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::AgentProfile {
            bytes,
            content_type: content_type.to_string(),
            received_at,
            period_start: received_at,
        },
    );
    group.latest_agent_profile_reported_at = Some(received_at);
}

/// A group holding one posted profile per receipt time, each with its own payload;
/// answers the payloads, in posting order.
async fn group_with_receipts(harness: &Harness, name: &str, receipts: &[i64]) -> Vec<Vec<u8>> {
    assert_eq!(
        create_group(harness, name).await.status,
        201,
        "creating group {name} must succeed"
    );
    let mut payloads = Vec::new();
    for index in 0..receipts.len() {
        let payload = format!("{{\"start\":{},\"profile\":\"{name}-{index}\"}}", 1_000 + index)
            .into_bytes();
        assert_eq!(
            harness
                .post_agent_profile(name, "application/json", None, payload.clone())
                .await
                .status,
            204,
            "posting profile {index} to {name} must succeed"
        );
        payloads.push(payload);
    }
    place_receipts(harness, name, receipts);
    payloads
}

/// One `ListProfileTimes` request against a group holding `receipts`, and the profile
/// times it must answer with.
#[derive(Debug)]
struct Case {
    label: &'static str,
    /// Receipt times of the group's posted profiles, epoch seconds.
    receipts: Vec<i64>,
    /// A modelled `AggregationPeriod` value (`AGGREGATION_PERIOD_VALUES`: P1D, PT1H,
    /// PT5M).
    period: &'static str,
    range: (i64, i64),
    /// A modelled `OrderBy` value (`ORDER_BY_VALUES`), or none for the default.
    order_by: Option<&'static str>,
    max_results: Option<i32>,
    /// The profile times the response must carry, in order, as epoch seconds.
    expected: Vec<i64>,
    /// Whether the response must carry a `nextToken`.
    expect_next: bool,
}

impl Case {
    fn query(&self) -> String {
        let (start, end) = self.range;
        let mut query = format!(
            "?startTime={}&endTime={}&period={}",
            iso(start),
            iso(end),
            self.period
        );
        if let Some(order_by) = self.order_by {
            query.push_str(&format!("&orderBy={order_by}"));
        }
        if let Some(max_results) = self.max_results {
            query.push_str(&format!("&maxResults={max_results}"));
        }
        query
    }
}

/// The table. Rows differ in aggregation period, how receipts share a window, where a
/// receipt sits inside its window, the requested range, the order and the page size.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "five-minute windows: profiles in one window report one start",
            receipts: vec![
                DAY_START,
                DAY_START + 60,
                DAY_START + 299,
                DAY_START + 300,
                DAY_START + 1800,
            ],
            period: "PT5M",
            range: (DAY_START, DAY_START + HOUR),
            order_by: None,
            max_results: None,
            expected: vec![DAY_START + 1800, DAY_START + 300, DAY_START],
            expect_next: false,
        },
        Case {
            label: "the same profiles over an hourly period share one window",
            receipts: vec![
                DAY_START,
                DAY_START + 60,
                DAY_START + 299,
                DAY_START + 300,
                DAY_START + 1800,
            ],
            period: "PT1H",
            range: (DAY_START, DAY_START + HOUR),
            order_by: None,
            max_results: None,
            expected: vec![DAY_START],
            expect_next: false,
        },
        Case {
            label: "the same profiles over a daily period share one window",
            receipts: vec![
                DAY_START,
                DAY_START + 60,
                DAY_START + 299,
                DAY_START + 300,
                DAY_START + 1800,
            ],
            period: "P1D",
            range: (DAY_START, DAY_START + DAY),
            order_by: None,
            max_results: None,
            expected: vec![DAY_START],
            expect_next: false,
        },
        Case {
            label: "TimestampAscending reverses the default order",
            receipts: vec![DAY_START, DAY_START + 300, DAY_START + 1800],
            period: "PT5M",
            range: (DAY_START, DAY_START + HOUR),
            order_by: Some("TimestampAscending"),
            max_results: None,
            expected: vec![DAY_START, DAY_START + 300, DAY_START + 1800],
            expect_next: false,
        },
        Case {
            label: "TimestampDescending named explicitly is the default order",
            receipts: vec![DAY_START, DAY_START + 300, DAY_START + 1800],
            period: "PT5M",
            range: (DAY_START, DAY_START + HOUR),
            order_by: Some("TimestampDescending"),
            max_results: None,
            expected: vec![DAY_START + 1800, DAY_START + 300, DAY_START],
            expect_next: false,
        },
        Case {
            label: "the requested range keeps only the windows it covers",
            receipts: vec![DAY_START, DAY_START + HOUR + 100, DAY_START + 2 * HOUR + 100],
            period: "PT1H",
            range: (DAY_START + HOUR, DAY_START + 2 * HOUR),
            order_by: None,
            max_results: None,
            expected: vec![DAY_START + HOUR],
            expect_next: false,
        },
        Case {
            label: "a window starting exactly at startTime is reported",
            receipts: vec![DAY_START, DAY_START + HOUR + 100],
            period: "PT1H",
            range: (DAY_START + HOUR, DAY_START + 2 * HOUR),
            order_by: Some("TimestampAscending"),
            max_results: None,
            expected: vec![DAY_START + HOUR],
            expect_next: false,
        },
        Case {
            label: "daily windows over three days",
            receipts: vec![
                DAY_START + 10,
                DAY_START + DAY + 100,
                DAY_START + 2 * DAY + 200,
            ],
            period: "P1D",
            range: (DAY_START, DAY_START + 3 * DAY),
            order_by: None,
            max_results: None,
            expected: vec![DAY_START + 2 * DAY, DAY_START + DAY, DAY_START],
            expect_next: false,
        },
        Case {
            label: "a receipt late in its window reports the window start, not the receipt",
            receipts: vec![DAY_START + DAY - 1],
            period: "PT1H",
            range: (DAY_START, DAY_START + DAY),
            order_by: None,
            max_results: None,
            expected: vec![DAY_START + 23 * HOUR],
            expect_next: false,
        },
        Case {
            label: "maxResults bounds the page and hands back a token",
            receipts: vec![DAY_START, DAY_START + 300, DAY_START + 600],
            period: "PT5M",
            range: (DAY_START, DAY_START + HOUR),
            order_by: Some("TimestampAscending"),
            max_results: Some(1),
            expected: vec![DAY_START],
            expect_next: true,
        },
        Case {
            // `maxResults`'s published upper bound (the model: range 1..1000).
            label: "maxResults at its upper bound returns the whole list",
            receipts: vec![DAY_START, DAY_START + 300],
            period: "PT5M",
            range: (DAY_START, DAY_START + HOUR),
            order_by: None,
            max_results: Some(1000),
            expected: vec![DAY_START + 300, DAY_START],
            expect_next: false,
        },
        Case {
            label: "a group with no posted profiles has no profile times",
            receipts: Vec::new(),
            period: "PT5M",
            range: (DAY_START, DAY_START + HOUR),
            order_by: None,
            max_results: None,
            expected: Vec::new(),
            expect_next: false,
        },
        Case {
            label: "a range holding none of the group's windows is empty",
            receipts: vec![DAY_START],
            period: "PT5M",
            range: (DAY_START + DAY, DAY_START + 2 * DAY),
            order_by: None,
            max_results: None,
            expected: Vec::new(),
            expect_next: false,
        },
        Case {
            // The narrowest range the interval rule allows: `startTime` strictly before
            // `endTime`. A range whose bounds are EQUAL is not an interval and is
            // refused (see `requests_outside_the_operation_s_rules_are_refused`).
            label: "a one-second range reports a window starting inside it",
            receipts: vec![DAY_START],
            period: "PT5M",
            range: (DAY_START, DAY_START + 1),
            order_by: None,
            max_results: None,
            expected: vec![DAY_START],
            expect_next: false,
        },
        Case {
            label: "a pre-epoch receipt floors to the start of its window",
            receipts: vec![-1],
            period: "PT1H",
            range: (-2 * HOUR, 0),
            order_by: None,
            max_results: None,
            expected: vec![-HOUR],
            expect_next: false,
        },
    ]
}

#[tokio::test]
async fn profile_times_are_the_aggregation_windows_of_the_stored_profiles() {
    let harness = Harness::new();

    for (index, case) in cases().iter().enumerate() {
        let name = format!("times-case-{index}");
        group_with_receipts(&harness, &name, &case.receipts).await;

        let answer = list_profile_times(&harness, &name, &case.query()).await;
        assert_eq!(
            answer.status,
            200,
            "case {:?}: expected HTTP 200, got {} {}",
            case.label,
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            profile_times(&answer),
            case.expected,
            "case {:?} ({}): expected profile times {:?}, got {:?}",
            case.label,
            case.query(),
            case.expected,
            profile_times(&answer)
        );
        assert_eq!(
            next_token(&answer).is_some(),
            case.expect_next,
            "case {:?}: expected nextToken present = {}, got {:?}",
            case.label,
            case.expect_next,
            next_token(&answer)
        );

        // Listing is a read: a second identical call answers identically.
        let again = list_profile_times(&harness, &name, &case.query()).await;
        assert_eq!(
            profile_times(&again),
            profile_times(&answer),
            "case {:?}: a repeated list answered {:?} after {:?}",
            case.label,
            profile_times(&again),
            profile_times(&answer)
        );
    }
}

#[tokio::test]
async fn paging_visits_every_profile_time_exactly_once() {
    let harness = Harness::new();
    // Five distinct five-minute windows, plus a second profile inside one of them so
    // the page walk is over windows and not over posted profiles.
    let receipts = vec![
        DAY_START,
        DAY_START + 60,
        DAY_START + 300,
        DAY_START + 600,
        DAY_START + 900,
        DAY_START + 1200,
    ];
    let name = "times-paging";
    group_with_receipts(&harness, name, &receipts).await;
    let all: Vec<i64> = vec![
        DAY_START,
        DAY_START + 300,
        DAY_START + 600,
        DAY_START + 900,
        DAY_START + 1200,
    ];

    for page_size in [1usize, 2, 3, 5, 7] {
        let mut seen: Vec<i64> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut query = format!(
                "?startTime={}&endTime={}&period=PT5M&orderBy=TimestampAscending&maxResults={page_size}",
                iso(DAY_START),
                iso(DAY_START + HOUR)
            );
            if let Some(token) = token.as_deref() {
                query.push_str(&format!("&nextToken={token}"));
            }
            let answer = list_profile_times(&harness, name, &query).await;
            assert_eq!(
                answer.status,
                200,
                "page size {page_size}: expected HTTP 200, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            let page = profile_times(&answer);
            assert!(
                page.len() <= page_size,
                "page size {page_size}: a page carried {} profile times",
                page.len()
            );
            seen.extend(page);
            pages += 1;
            assert!(
                pages <= all.len() + 2,
                "page size {page_size}: paging did not terminate after {pages} pages"
            );
            match next_token(&answer) {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        assert_eq!(
            seen, all,
            "page size {page_size}: paging must visit every profile time once, in order; \
             got {seen:?}"
        );
    }
}

#[tokio::test]
async fn a_reported_profile_time_names_a_window_get_profile_can_serve() {
    let harness = Harness::new();
    // Receipts kept away from the window boundaries, so each window holds exactly the
    // profiles this test expects it to.
    let receipts = vec![
        DAY_START + 30,
        DAY_START + 120,
        DAY_START + 330,
        DAY_START + 930,
    ];
    let name = "times-roundtrip";
    group_with_receipts(&harness, name, &receipts).await;

    let listed = list_profile_times(
        &harness,
        name,
        &format!(
            "?startTime={}&endTime={}&period=PT5M&orderBy=TimestampAscending",
            iso(DAY_START),
            iso(DAY_START + HOUR)
        ),
    )
    .await;
    assert_eq!(listed.status, 200, "listing must succeed");
    let times = profile_times(&listed);
    assert_eq!(
        times,
        vec![DAY_START, DAY_START + 300, DAY_START + 900],
        "the three five-minute windows the receipts fall in, got {times:?}"
    );

    for start in times {
        // Every profile whose data falls in this window is in the window's ONE
        // aggregated profile, so what `GetProfile` serves for it holds all of them: these
        // payloads state one name each, so the combination states the name the most
        // recently posted one states — beside the window the aggregation covers and the
        // number of agents that reported into it.
        let in_window: Vec<usize> = receipts
            .iter()
            .enumerate()
            .filter(|(_, at)| **at >= start && **at < start + 300)
            .map(|(index, _)| index)
            .collect();
        let last = *in_window
            .last()
            .unwrap_or_else(|| panic!("window {start} was reported with no profile in it"));
        let expected = serde_json::json!({ "profile": format!("{name}-{last}") });
        let answer = harness
            .get_profile(name, &format!("?startTime={}&period=PT5M", iso(start)))
            .await;
        assert_eq!(
            answer.status,
            200,
            "window {start}: GetProfile must serve a reported profile time, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let served: serde_json::Value = serde_json::from_slice(&answer.body).unwrap_or_else(|error| {
            panic!(
                "window {start}: expected a JSON profile, got {error}: {}",
                String::from_utf8_lossy(&answer.body)
            )
        });
        let data = crate::profiling_group_lifecycle::profile_data(&answer.body);
        assert_eq!(
            data,
            crate::profiling_group_lifecycle::profile_data(
                &serde_json::to_vec(&expected).expect("a serializable document")
            ),
            "window {start}: expected the profiles posted in that window combined, got {served}"
        );
        let case = format!("window {start}");
        let (window_start, window_end, agents) =
            crate::profiling_group_lifecycle::aggregation_of(&case, &answer.body);
        assert_eq!(
            (window_start.as_str(), window_end.as_str()),
            (
                crate::profiling_group_lifecycle::iso_millis(start).as_str(),
                crate::profiling_group_lifecycle::iso_millis(start + 300).as_str()
            ),
            "window {start}: the aggregated profile must state the window it covers, got \
             {served}"
        );
        assert_eq!(
            agents,
            i64::try_from(in_window.len()).unwrap_or(0),
            "window {start}: expected the window's {} submissions to be reported, got {served}",
            in_window.len()
        );
    }
}

#[tokio::test]
async fn every_modelled_aggregation_period_is_served() {
    // The source of the catalog: the generated `AGGREGATION_PERIOD_VALUES`, so a model
    // that gains or renames an `AggregationPeriod` value cannot leave this operation
    // silently refusing one the model declares.
    use service_com_amazonaws_codeguruprofiler_codeguruprofiler::ops::types::AGGREGATION_PERIOD_VALUES;

    let harness = Harness::new();
    let name = "times-every-period";
    group_with_receipts(&harness, name, &[DAY_START + 42]).await;

    for period in AGGREGATION_PERIOD_VALUES {
        let answer = list_profile_times(
            &harness,
            name,
            &format!(
                "?startTime={}&endTime={}&period={period}",
                iso(DAY_START),
                iso(DAY_START + DAY)
            ),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "period {period}: a modelled AggregationPeriod must be served, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        // Whatever the period, the one posted profile falls in exactly one window of
        // it, and that window starts at or before the receipt.
        let times = profile_times(&answer);
        assert_eq!(
            times.len(),
            1,
            "period {period}: one posted profile is one aggregated profile, got {times:?}"
        );
        assert!(
            times[0] <= DAY_START + 42,
            "period {period}: the window must start at or before the receipt, got {times:?}"
        );
    }
}

#[tokio::test]
async fn an_unknown_group_is_not_found() {
    let harness = Harness::new();
    group_with_receipts(&harness, "times-known", &[DAY_START]).await;

    let answer = list_profile_times(
        &harness,
        "times-missing",
        &format!(
            "?startTime={}&endTime={}&period=PT5M",
            iso(DAY_START),
            iso(DAY_START + HOUR)
        ),
    )
    .await;
    assert_eq!(
        answer.status,
        404,
        "an unknown group must be a 404, got {} {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "an unknown group must report ResourceNotFoundException, got {}",
        String::from_utf8_lossy(&answer.body)
    );
}

#[tokio::test]
async fn requests_outside_the_operation_s_rules_are_refused() {
    let harness = Harness::new();
    let name = "times-refusals";
    group_with_receipts(&harness, name, &[DAY_START, DAY_START + 300]).await;

    let range = format!(
        "startTime={}&endTime={}",
        iso(DAY_START),
        iso(DAY_START + HOUR)
    );
    // Rows: each way a request can be outside what the operation accepts. The
    // `maxResults` rows are the model's published range bounds (1..1000) and one past
    // each; `period` and `orderBy` are the values the model's enums declare.
    let refused: Vec<(&str, String)> = vec![
        (
            "endTime before startTime",
            format!(
                "?startTime={}&endTime={}&period=PT5M",
                iso(DAY_START + HOUR),
                iso(DAY_START)
            ),
        ),
        (
            // Equal bounds are not an interval either, so they are refused rather than
            // answered with an empty page; `interval_validation.rs` asserts that rule
            // across every operation of this service that takes a range.
            "endTime equal to startTime",
            format!(
                "?startTime={}&endTime={}&period=PT5M",
                iso(DAY_START),
                iso(DAY_START)
            ),
        ),
        (
            "a period outside AggregationPeriod",
            format!("?{range}&period=PT10M"),
        ),
        (
            "an orderBy outside OrderBy",
            format!("?{range}&period=PT5M&orderBy=TimestampSideways"),
        ),
        (
            "a nextToken this service never issued",
            format!("?{range}&period=PT5M&nextToken=not-a-cursor"),
        ),
        (
            "a nextToken past the end of the list",
            format!("?{range}&period=PT5M&nextToken=99"),
        ),
        (
            // The group holds two five-minute windows, so a cursor of 2 points exactly
            // at the end — a position this service never issues a token for.
            "a nextToken exactly at the end of the list",
            format!("?{range}&period=PT5M&nextToken=2"),
        ),
        (
            "maxResults below its lower bound",
            format!("?{range}&period=PT5M&maxResults=0"),
        ),
        (
            "maxResults above its upper bound",
            format!("?{range}&period=PT5M&maxResults=1001"),
        ),
    ];

    for (label, query) in refused {
        let answer = list_profile_times(&harness, name, &query).await;
        assert_eq!(
            answer.status,
            400,
            "case {label:?} ({query}): expected HTTP 400, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case {label:?} ({query}): expected ValidationException, got {}",
            String::from_utf8_lossy(&answer.body)
        );
    }

    // A refusal, like a successful list, changed nothing: the group still reports both
    // of its five-minute windows.
    let answer = list_profile_times(&harness, name, &format!("?{range}&period=PT5M")).await;
    assert_eq!(
        profile_times(&answer),
        vec![DAY_START + 300, DAY_START],
        "refused requests must not change what a later list sees, got {}",
        String::from_utf8_lossy(&answer.body)
    );

    // `maxResults` at each published bound is accepted.
    for max_results in [1, 1000] {
        let answer = list_profile_times(
            &harness,
            name,
            &format!("?{range}&period=PT5M&maxResults={max_results}"),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "maxResults={max_results} is inside the model's range and must be accepted, \
             got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
    }
}

/// The length in whole seconds of a modelled `AggregationPeriod` value, read from the
/// service's own table, so a case cannot name a period the service does not aggregate at.
fn period_seconds(period: &str) -> i64 {
    service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::aggregation_period_seconds(
        period,
    )
    .unwrap_or_else(|| panic!("{period} is not a modelled AggregationPeriod"))
}

/// The start of the `period`-long aggregation window, aligned to the UTC epoch, that
/// contains `at` — the instant a reported profile time names.
fn window_start(at: i64, period: i64) -> i64 {
    at.div_euclid(period) * period
}

/// The start of the finest aggregation window that has NOT OPENED yet.
///
/// A profile whose receipt sits at or after this instant belongs to a window the service
/// is not collecting into yet, so the aggregated profile that window owes cannot be due
/// for at least one whole period however long a case takes: the pending cases below
/// cannot race a window boundary.
fn pending_window_start() -> i64 {
    let finest = service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::finest_aggregation_period_seconds();
    let now = DateTime::from(std::time::SystemTime::now()).secs();
    window_start(now, finest) + finest
}

/// How far back from [`pending_window_start`] a case's receipts are placed for its
/// AGGREGATED phase: a day, plus one whole window of the period under test.
///
/// The window a receipt falls in has to have CLOSED for its aggregation to be due, and the
/// pending window start is the next FINEST window boundary — which is up to one finest
/// window AFTER the clock, and can be the first instant of the next hour or of the next day.
/// A day back from it is therefore not necessarily inside an earlier window of a COARSER
/// period: read in the last finest window of a day, a receipt a day before that boundary
/// still sits in the day the clock is in, whose window has not closed, and the aggregation
/// it owes is correctly not due. One more whole window of the period under test puts the
/// receipt in a window that is over whatever phase of the clock the case runs at, which is
/// what the aggregated phase is about — and the pending phase, which reads the clock the same
/// way, is unaffected.
fn aged_by(period: &str) -> i64 {
    DAY + period_seconds(period)
}

/// One pending-aggregation row: where the group's posted profiles are received, relative
/// to the window whose aggregation is not due yet, and how the request is parameterised.
#[derive(Debug)]
struct PendingCase {
    label: &'static str,
    /// One offset in seconds per posted profile, from [`pending_window_start`] for the
    /// pending phase and from [`aged_by`] before that for the aggregated phase.
    /// Non-negative, so every pending receipt sits in a window that has not closed.
    offsets: Vec<i64>,
    /// A modelled `AggregationPeriod` value (`AGGREGATION_PERIOD_VALUES`).
    period: &'static str,
    /// A modelled `OrderBy` value (`ORDER_BY_VALUES`), or none for the default.
    order_by: Option<&'static str>,
    max_results: Option<i32>,
}

impl PendingCase {
    /// The receipts this case places, and the request that must cover the windows they
    /// fall in: the range starts three periods before the earliest receipt and ends one
    /// period after the latest, so every window start the receipts floor to lies strictly
    /// inside it. An empty page is therefore about the aggregation, not about the range.
    fn request(&self, base: i64) -> (Vec<i64>, (i64, i64), String) {
        let receipts: Vec<i64> = self.offsets.iter().map(|at| base + at).collect();
        let period = period_seconds(self.period);
        let earliest = receipts.iter().min().copied().unwrap_or(base);
        let latest = receipts.iter().max().copied().unwrap_or(base);
        let range = (earliest - 3 * period, latest + period);
        let mut query = format!(
            "?startTime={}&endTime={}&period={}",
            iso(range.0),
            iso(range.1),
            self.period
        );
        if let Some(order_by) = self.order_by {
            query.push_str(&format!("&orderBy={order_by}"));
        }
        if let Some(max_results) = self.max_results {
            query.push_str(&format!("&maxResults={max_results}"));
        }
        (receipts, range, query)
    }

    /// The profile times this case must report once the aggregations are due: one entry
    /// per distinct window the receipts fall in, inside the range, in the requested order
    /// and bounded by the page size.
    fn expected(&self, receipts: &[i64], range: (i64, i64)) -> (Vec<i64>, bool) {
        let period = period_seconds(self.period);
        let mut starts: Vec<i64> = receipts
            .iter()
            .map(|at| window_start(*at, period))
            .collect();
        starts.sort_unstable();
        starts.dedup();
        starts.retain(|start| *start >= range.0 && *start < range.1);
        // The model's doc for `orderBy`: "Defaults to TIMESTAMP_DESCENDING."
        if self.order_by != Some("TimestampAscending") {
            starts.reverse();
        }
        let limit = self.max_results.map_or(
            service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::DEFAULT_LIST_PAGE_SIZE,
            |max| usize::try_from(max).unwrap_or(1).max(1),
        );
        let page: Vec<i64> = starts.iter().take(limit).copied().collect();
        (page, starts.len() > limit)
    }
}

/// Rows vary what the rule generalises over: the aggregation period (all three the model
/// declares), how many profiles the group holds, how many windows they fall in, where a
/// receipt sits inside its window, the order and the page size.
fn pending_cases() -> Vec<PendingCase> {
    vec![
        PendingCase {
            label: "one profile, five-minute period",
            offsets: vec![0],
            period: "PT5M",
            order_by: None,
            max_results: None,
        },
        PendingCase {
            label: "one profile, hourly period",
            offsets: vec![0],
            period: "PT1H",
            order_by: None,
            max_results: None,
        },
        PendingCase {
            label: "one profile, daily period",
            offsets: vec![0],
            period: "P1D",
            order_by: None,
            max_results: None,
        },
        PendingCase {
            label: "three profiles inside one five-minute window",
            offsets: vec![0, 1, 299],
            period: "PT5M",
            order_by: Some("TimestampAscending"),
            max_results: None,
        },
        PendingCase {
            label: "profiles spread over three five-minute windows",
            offsets: vec![0, 300, 600],
            period: "PT5M",
            order_by: Some("TimestampAscending"),
            max_results: None,
        },
        PendingCase {
            label: "profiles spread over two hourly windows",
            offsets: vec![0, HOUR],
            period: "PT1H",
            order_by: Some("TimestampDescending"),
            max_results: None,
        },
        PendingCase {
            label: "a page size of one still reports an empty page with no token",
            offsets: vec![0, 300],
            period: "PT5M",
            order_by: None,
            // `maxResults`'s published lower bound (the model: range 1..1000).
            max_results: Some(1),
        },
        PendingCase {
            label: "a page size at its upper bound reports an empty page",
            offsets: vec![0],
            period: "PT1H",
            order_by: Some("TimestampDescending"),
            max_results: Some(1000),
        },
    ]
}

/// `ListProfileTimes` lists AGGREGATED profiles, and a posted agent profile is not one
/// at the instant it is received: the service collects posted profiles into aggregated
/// profiles over windows aligned to UTC, so a window still open is covered by no
/// aggregated profile and contributes no profile time — however many profiles it holds,
/// whatever period, order or page size the request names. Once the aggregation that
/// window owes has fallen due, the same request reports its start.
#[tokio::test]
async fn a_window_is_reported_only_once_the_aggregation_it_owes_is_due() {
    let harness = Harness::new();

    for (index, case) in pending_cases().iter().enumerate() {
        let name = format!("times-pending-{index}");
        let label = case.label;
        // One base instant per case, so the two phases place the same profiles the same
        // span apart even if the clock crosses a window boundary mid-case.
        let base = pending_window_start();
        let (pending_receipts, pending_range, pending_query) = case.request(base);

        group_with_receipts(&harness, &name, &pending_receipts).await;

        // The posts were RECEIVED — the group records that — and are not aggregated yet.
        let described = harness.describe(&name).await;
        assert!(
            described
                .json()
                .pointer("/profilingStatus/latestAgentProfileReportedAt")
                .is_some(),
            "case {label:?}: the posts must be recorded as received, got {}",
            String::from_utf8_lossy(&described.body)
        );

        // Polling until an aggregate appears is what a client written against the real
        // service does: every attempt reports an empty page, and no page token, because
        // there is no aggregated profile to page through.
        for attempt in 0..3 {
            let answer = list_profile_times(&harness, &name, &pending_query).await;
            assert_eq!(
                answer.status,
                200,
                "case {label:?} attempt {attempt} ({pending_query}): expected HTTP 200, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            let reported = profile_times(&answer);
            assert!(
                reported.is_empty(),
                "case {label:?} attempt {attempt} ({pending_query}): a window whose \
                 aggregation is not due must report no profile time, got {reported:?}"
            );
            assert_eq!(
                next_token(&answer),
                None,
                "case {label:?} attempt {attempt}: an empty page must carry no nextToken, \
                 got {:?}",
                next_token(&answer)
            );
        }

        // The same profiles, received [`aged_by`] earlier: every window they fall in has
        // closed and been aggregated, so the same request now reports one start per window.
        let (due_receipts, due_range, due_query) = case.request(base - aged_by(case.period));
        place_receipts(&harness, &name, &due_receipts);
        let (expected, expect_next) = case.expected(&due_receipts, due_range);
        assert!(
            !expected.is_empty(),
            "case {label:?}: the aged receipts must fall inside the requested range"
        );
        let answer = list_profile_times(&harness, &name, &due_query).await;
        assert_eq!(
            answer.status,
            200,
            "case {label:?} ({due_query}): expected HTTP 200, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            profile_times(&answer),
            expected,
            "case {label:?} ({due_query}): expected profile times {expected:?}, got {:?}",
            profile_times(&answer)
        );
        assert_eq!(
            next_token(&answer).is_some(),
            expect_next,
            "case {label:?}: expected nextToken present = {expect_next}, got {:?}",
            next_token(&answer)
        );
        // The pending phase's own range covered the windows it asked about, so the empty
        // pages above were about the aggregation and not about the range: the windows the
        // pending receipts floor to are the ones this case's range contains.
        for receipt in &pending_receipts {
            let start = window_start(*receipt, period_seconds(case.period));
            assert!(
                start >= pending_range.0 && start < pending_range.1,
                "case {label:?}: the pending range {pending_range:?} must contain window \
                 start {start}"
            );
        }
    }
}

/// How far from an aggregation-window boundary the open-window cases below read the clock,
/// in seconds.
///
/// Those cases place receipts relative to the clock and then state which windows have
/// CLOSED; the service answers a few milliseconds later and must decide the same about
/// each of them. Reading the clock this far from every window boundary is what makes the
/// two agree — no boundary a row depends on falls between the instant the row was computed
/// at and the instant the service answers at — and it is a whole order of magnitude more
/// room than the requests themselves need.
const BOUNDARY_MARGIN_SECONDS: i64 = 10;

/// The clock's current second, read at least [`BOUNDARY_MARGIN_SECONDS`] away from every
/// aggregation window boundary.
///
/// The three periods the model's `AggregationPeriod` declares are nested — PT5M divides
/// PT1H, which divides P1D — so an instant that far inside a FINEST window is that far
/// inside the hourly and the daily window containing it too, and the one condition covers
/// all three.
///
/// This POLLS the clock for that condition rather than waiting a fixed span: it answers
/// immediately unless the call lands within the margin of a five-minute boundary, and then
/// as soon as the clock is clear of it.
///
/// `pub(crate)` so every suite that states which windows have CLOSED relative to the
/// clock reads the clock the same way — [`crate::pending_aggregation`] states it about the
/// aggregated profile `GetProfile` serves for such a window.
pub(crate) async fn now_clear_of_window_boundaries() -> i64 {
    let finest = service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::finest_aggregation_period_seconds();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let now = DateTime::from(std::time::SystemTime::now()).secs();
        let inside = now.rem_euclid(finest);
        if inside >= BOUNDARY_MARGIN_SECONDS && inside < finest - BOUNDARY_MARGIN_SECONDS {
            return now;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the clock never came clear of an aggregation window boundary"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Whether the aggregated profile that the `period`-long window starting at `start` owes
/// has fallen due as of `now` (both epoch seconds): the window has CLOSED and the
/// aggregation pass over it is through
/// (`state::AGGREGATION_PASS_MILLIS`, stated in milliseconds).
///
/// This is the rule the open-window rows are stated by, written out once: the window is the
/// one of the period being asked about, so what a finer window inside it has already been
/// aggregated into says nothing about it.
fn aggregation_is_due(start: i64, period: i64, now: i64) -> bool {
    let pass = service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::AGGREGATION_PASS_MILLIS;
    start
        .saturating_add(period)
        .saturating_mul(1_000)
        .saturating_add(pass)
        <= now.saturating_mul(1_000)
}

/// The profile times a group holding `receipts` must report for a `period`-long
/// aggregation period as of `now`, ascending and distinct: one per window of that period
/// whose own aggregation has fallen due.
fn due_window_starts(receipts: &[i64], period: i64, now: i64) -> Vec<i64> {
    let mut starts: Vec<i64> = receipts
        .iter()
        .map(|at| window_start(*at, period))
        .filter(|start| aggregation_is_due(*start, period, now))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// A window of the REQUESTED aggregation period that the service is still collecting into
/// is covered by no aggregated profile, so it contributes no profile time — however long
/// the finer windows inside it have been closed, and whatever order or page size the
/// request names. Only the windows of that period which have closed, plus the aggregation
/// pass they owe, are reported.
///
/// The rows vary what the rule generalises over: every aggregation period the service
/// aggregates at (read from its own table), receipts in the window being collected into,
/// in finer windows that have closed INSIDE that window, and in hourly and daily windows
/// that have closed, each of the two orders the model declares plus the default, and a page
/// size at the model's lower bound.
#[tokio::test]
async fn an_open_window_contributes_no_profile_time_whatever_the_period() {
    let harness = Harness::new();
    let now = now_clear_of_window_boundaries().await;
    let finest = service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::finest_aggregation_period_seconds();
    // The finest window the service is collecting into as of `now`.
    let collecting = window_start(now, finest);
    // Receipts, oldest first: one a day back and one an hour back (so the daily and the
    // hourly window holding them have closed), two in finest windows that have closed
    // inside the current hour and day, and one in the window being collected into.
    let receipts: Vec<i64> = vec![
        collecting - DAY - 150,
        collecting - HOUR - 150,
        collecting - 450,
        collecting - 150,
        collecting + 1,
    ];
    let name = "times-open-window";
    group_with_receipts(&harness, name, &receipts).await;

    // A range holding every window start the receipts floor to, at every period: from the
    // start of the day containing the earliest receipt to a day past now.
    let range = (window_start(receipts[0], DAY), now + DAY);

    for (period_name, period) in
        service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::AGGREGATION_PERIODS
    {
        let ascending = due_window_starts(&receipts, *period, now);
        // The window of this period that the service is still collecting into, and the
        // receipts it holds whose FINEST window has already been aggregated — the state a
        // period-independent gate reports as an available aggregated profile.
        let open = window_start(now, *period);
        assert!(
            receipts.iter().any(|at| window_start(*at, *period) == open),
            "case {period_name}: the window being collected into must hold a receipt for \
             this row to be about it"
        );
        let aggregated_inside_open: Vec<i64> = receipts
            .iter()
            .copied()
            .filter(|at| {
                window_start(*at, *period) == open
                    && aggregation_is_due(window_start(*at, finest), finest, now)
            })
            .collect();

        for order_by in [None, Some("TimestampAscending"), Some("TimestampDescending")] {
            // The model's doc for `orderBy`: "Defaults to TIMESTAMP_DESCENDING."
            let mut expected = ascending.clone();
            if order_by != Some("TimestampAscending") {
                expected.reverse();
            }
            let mut query = format!(
                "?startTime={}&endTime={}&period={period_name}",
                iso(range.0),
                iso(range.1)
            );
            if let Some(order_by) = order_by {
                query.push_str(&format!("&orderBy={order_by}"));
            }
            let answer = list_profile_times(&harness, name, &query).await;
            assert_eq!(
                answer.status,
                200,
                "case {period_name}/{order_by:?} ({query}): expected HTTP 200, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            let reported = profile_times(&answer);
            assert_eq!(
                reported, expected,
                "case {period_name}/{order_by:?} ({query}): only a window whose own \
                 aggregation is due is reported — the window [{open}] is still collecting \
                 and holds {} already-aggregated receipts {aggregated_inside_open:?} — \
                 expected {expected:?}, got {reported:?}",
                aggregated_inside_open.len()
            );
            assert!(
                !reported.contains(&open),
                "case {period_name}/{order_by:?}: the window being collected into [{open}] \
                 must be reported by nothing, got {reported:?}"
            );
            assert_eq!(
                next_token(&answer),
                None,
                "case {period_name}/{order_by:?}: one page holds every reported window, so \
                 it carries no nextToken, got {:?}",
                next_token(&answer)
            );
        }

        // The same rule at the page size the model's `maxResults` range starts at: the
        // first window of the default order, and a token exactly when another follows.
        let query = format!(
            "?startTime={}&endTime={}&period={period_name}&maxResults=1",
            iso(range.0),
            iso(range.1)
        );
        let answer = list_profile_times(&harness, name, &query).await;
        let first_page: Vec<i64> = ascending.iter().rev().take(1).copied().collect();
        assert_eq!(
            profile_times(&answer),
            first_page,
            "case {period_name}/maxResults=1 ({query}): expected {first_page:?}, got {:?}",
            profile_times(&answer)
        );
        assert_eq!(
            next_token(&answer).is_some(),
            ascending.len() > 1,
            "case {period_name}/maxResults=1: expected nextToken present = {}, got {:?}",
            ascending.len() > 1,
            next_token(&answer)
        );
    }

    // The other reader of the same aggregated profiles agrees: the group's status names the
    // aggregated profile that starts latest — the narrowest period's where two start
    // together — over exactly the windows whose aggregation is due.
    let expected_latest = service_com_amazonaws_codeguruprofiler_codeguruprofiler::state::AGGREGATION_PERIODS
        .iter()
        .filter_map(|(period_name, period)| {
            due_window_starts(&receipts, *period, now)
                .last()
                .map(|start| (*start, *period, *period_name))
        })
        .max_by_key(|(start, period, _)| (*start, std::cmp::Reverse(*period)))
        .map(|(start, _, period_name)| (period_name.to_string(), start));
    let described = harness.describe(name).await;
    assert_eq!(
        crate::latest_aggregated_profile::latest_aggregated_profile(
            "the open window of every period",
            &described.json()
        ),
        expected_latest,
        "the status must name the latest aggregated profile the due windows derive, got {}",
        String::from_utf8_lossy(&described.body)
    );
}
