// SPDX-License-Identifier: CC0-1.0

//! A channel's schedule, end to end over HTTP.
//!
//! The harness is the one `channel_lifecycle.rs` owns, so every assertion here
//! goes through the router: a request is built, dispatched to the mounted
//! service, and the JSON response read back.
//!
//! The rule under test is the one the read implements: a read of a channel's
//! schedule reports one entry per program persisted on the channel the request
//! names — in ascending scheduled-start order, with the media, duration and ad
//! breaks that program carries — restricted to the audience and the schedule
//! window the request asks for, served in pages bounded by `MaxResults` with an
//! opaque continuation token, and refused as NOT FOUND when the account holds no
//! channel under that name. It reports the store and changes nothing in it.
//!
//! The programs are written straight into the one store the mounted service
//! reads, at the same seam a program write uses: `CreateProgram` is still a
//! scaffold (`src/ops/create_program.rs`), so seeding the store is how this
//! suite puts a schedule in front of the read.
//!
//! The tables span the class the rule generalises over: programs playing a live
//! source and a VOD source, with and without a stated duration, with no ad break
//! and with several, with no audience media and with media for several
//! audiences, scheduled in the past and the future, two sharing one start
//! instant, inserted in an order that is not the schedule's; page sizes from one
//! to the published maximum; windows from zero minutes to past the whole
//! schedule; and audiences the programs define and one they do not.

use serde_json::Value;

use service_com_amazonaws_mediatailor_mediatailor::ops::types::{
    AdBreak, AlternateMedia, AudienceMedia, SlateSource,
};
use service_com_amazonaws_mediatailor_mediatailor::state::{service_state, ProgramRecord};

use crate::channel_lifecycle::{
    encode, Emulator, Reply, CHANNEL_NOT_FOUND_MESSAGE, NOT_FOUND_CODE, NOT_FOUND_STATUS,
};

/// `GetChannelSchedule`'s success status: `GET /channel/{ChannelName}/schedule`
/// with `@http` code 200
/// (`smithy_query(query="operation",operation="GetChannelSchedule")`).
const SCHEDULE_STATUS: u16 = 200;

/// The refusal this service answers a request value its model cannot constrain
/// with: `BadRequestException`, "A request contains unexpected data", HTTP 400 —
/// the one client error the MediaTailor model declares.
const BAD_REQUEST_STATUS: u16 = 400;
const BAD_REQUEST_CODE: &str = "BadRequestException";

/// `MaxResults`' modelled range on this operation is 1..100
/// (`smithy_query(query="operation",operation="GetChannelSchedule")`), so 100 is
/// the largest page the schedule serves and the page it serves when the request
/// names no size.
const MAX_PAGE_SIZE: usize = 100;

/// The channel every schedule in this suite is read from, and a request that
/// creates it: one HLS output is all a channel needs, and nothing here depends
/// on the channel's own members.
const CHANNEL: &str = "schedule-channel";

/// The source location the seeded programs play their default media from, and
/// the one an audience's alternate media plays from.
const SOURCE_LOCATION: &str = "primary-location";
const ALTERNATE_LOCATION: &str = "alternate-location";

/// A fixed instant the deterministic rows are scheduled around, in epoch
/// milliseconds, so their order and their reported instants depend on no clock.
const BASE_MILLIS: i64 = 1_700_000_000_000;

/// One minute and one hour in milliseconds, so a row states its offset in the
/// units a schedule is read in.
const MINUTE_MILLIS: i64 = 60_000;

fn create_channel_request() -> Value {
    serde_json::json!({
        "PlaybackMode": "LINEAR",
        "Tier": "STANDARD",
        "FillerSlate": {
            "SourceLocationName": SOURCE_LOCATION,
            "VodSourceName": "filler",
        },
        "Outputs": [{ "ManifestName": "index", "SourceGroup": "default" }],
    })
}

/// The ARN a program is stored under. MediaTailor ARNs are
/// `arn:aws:mediatailor:<region>:<account>:<type>/<name>`, and a program is
/// addressed through the channel it is scheduled on, so its resource part names
/// both. The region and account are the ones every ARN of this emulator carries
/// (`engine::emulator_arn` in `us-east-1`, the region this suite signs for).
fn program_arn(channel: &str, program: &str) -> String {
    format!("arn:aws:mediatailor:us-east-1:000000000000:program/{channel}/{program}")
}

/// One program a row seeds, stated the way the row reads.
struct Seed {
    name: &'static str,
    /// Where the program sits in the schedule, as an offset in milliseconds
    /// from the row set's base instant.
    offset_millis: i64,
    duration_millis: Option<i64>,
    live_source_name: Option<&'static str>,
    vod_source_name: Option<&'static str>,
    /// The offset of each ad break programmed into it, from its own start.
    ad_break_offsets: &'static [i64],
    /// The audiences it defines media for, each with the VOD source that
    /// audience's alternate media plays, and that media's own duration.
    audience_media: &'static [(&'static str, &'static str, i64)],
}

impl Seed {
    fn record(&self, channel: &str, base_millis: i64) -> ProgramRecord {
        ProgramRecord {
            name: self.name.to_string(),
            channel_name: channel.to_string(),
            arn: program_arn(channel, self.name),
            source_location_name: SOURCE_LOCATION.to_string(),
            live_source_name: self.live_source_name.map(str::to_string),
            vod_source_name: self.vod_source_name.map(str::to_string),
            scheduled_start_time_millis: base_millis.saturating_add(self.offset_millis),
            duration_millis: self.duration_millis,
            ad_breaks: self
                .ad_break_offsets
                .iter()
                .map(|offset| AdBreak {
                    message_type: Some("SPLICE_INSERT".to_string()),
                    offset_millis: *offset,
                    slate: Some(SlateSource {
                        source_location_name: Some(SOURCE_LOCATION.to_string()),
                        vod_source_name: Some("break-slate".to_string()),
                    }),
                    splice_insert_message: None,
                    time_signal_message: None,
                    ad_break_metadata: None,
                })
                .collect(),
            audience_media: self
                .audience_media
                .iter()
                .map(|(audience, vod_source, duration_millis)| AudienceMedia {
                    audience: Some((*audience).to_string()),
                    alternate_media: Some(vec![AlternateMedia {
                        source_location_name: Some(ALTERNATE_LOCATION.to_string()),
                        live_source_name: None,
                        vod_source_name: Some((*vod_source).to_string()),
                        clip_range: None,
                        scheduled_start_time_millis: None,
                        ad_breaks: None,
                        duration_millis: Some(*duration_millis),
                    }]),
                })
                .collect(),
        }
    }
}

/// Writes a program into the one store the mounted service reads — the seam a
/// program write uses — under the channel it is scheduled on.
fn seed_program(emulator: &Emulator, record: ProgramRecord) {
    let state = service_state(&**emulator.engine()).expect("the service's own store");
    let mut store = state.lock();
    store
        .programs
        .entry(record.channel_name.clone())
        .or_default()
        .insert(record.name.clone(), record);
}

/// `GetChannelSchedule`: `ChannelName` is the `@httpLabel` and every other
/// member travels in the query string.
async fn schedule(emulator: &Emulator, channel: &str, query: &[(&str, &str)]) -> Reply {
    let mut uri = format!("/channel/{}/schedule", encode(channel));
    if !query.is_empty() {
        let pairs: Vec<String> = query
            .iter()
            .map(|(name, value)| format!("{name}={}", encode(value)))
            .collect();
        uri.push('?');
        uri.push_str(&pairs.join("&"));
    }
    emulator.request("GET", &uri, None).await
}

/// Creates [`CHANNEL`] on a fresh emulator, so every schedule below is read
/// from a channel the account holds.
async fn emulator_with_channel() -> Emulator {
    let emulator = Emulator::new();
    let created = emulator.create(CHANNEL, &create_channel_request()).await;
    assert_eq!(
        created.status, 200,
        "the channel the schedule is read from must be created, got {} {:?}",
        created.status, created.body
    );
    emulator
}

/// The program names a reply reports, in the order it reports them.
fn reported_names(reply: &Reply) -> Vec<String> {
    reply
        .get("/Items")
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.get("ProgramName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The programs the projection table spans: a live program with no stated
/// duration and no ad break, a VOD program with a duration and two ad breaks,
/// two programs sharing one start instant so the order has a tie to break, and a
/// program defining media for two audiences. They are seeded in an order that is
/// not the schedule's.
const PROJECTION_SEEDS: &[Seed] = &[
    Seed {
        name: "d-last",
        offset_millis: 30 * MINUTE_MILLIS,
        duration_millis: Some(10 * MINUTE_MILLIS),
        live_source_name: None,
        vod_source_name: Some("vod-last"),
        ad_break_offsets: &[0, 5 * MINUTE_MILLIS],
        audience_media: &[],
    },
    Seed {
        name: "b-tie",
        offset_millis: 10 * MINUTE_MILLIS,
        duration_millis: Some(90_000),
        live_source_name: None,
        vod_source_name: Some("vod-tie-b"),
        ad_break_offsets: &[],
        audience_media: &[("en", "vod-en", 60_000), ("fr", "vod-fr", 120_000)],
    },
    Seed {
        name: "a-tie",
        offset_millis: 10 * MINUTE_MILLIS,
        duration_millis: None,
        live_source_name: Some("live-tie-a"),
        vod_source_name: None,
        ad_break_offsets: &[1_000],
        audience_media: &[],
    },
    Seed {
        name: "c-first",
        offset_millis: -5 * MINUTE_MILLIS,
        duration_millis: Some(0),
        live_source_name: Some("live-first"),
        vod_source_name: None,
        ad_break_offsets: &[],
        audience_media: &[],
    },
];

/// The order [`PROJECTION_SEEDS`] is scheduled in: ascending start instant, and
/// by program name between the two that share one instant.
const PROJECTION_ORDER: &[&str] = &["c-first", "a-tie", "b-tie", "d-last"];

#[tokio::test]
async fn the_schedule_reports_every_program_in_scheduled_order_with_its_own_media() {
    let emulator = emulator_with_channel().await;
    for seed in PROJECTION_SEEDS {
        seed_program(&emulator, seed.record(CHANNEL, BASE_MILLIS));
    }

    let reply = schedule(&emulator, CHANNEL, &[]).await;
    assert_eq!(
        reply.status, SCHEDULE_STATUS,
        "expected the schedule to be served, got {} {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reported_names(&reply),
        PROJECTION_ORDER
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
        "expected the schedule in scheduled-start order, got {:?}",
        reply.get("/Items")
    );
    assert_eq!(
        reply.get("/NextToken"),
        &Value::Null,
        "expected no continuation token on a schedule served whole, got {:?}",
        reply.get("/NextToken")
    );

    for (position, name) in PROJECTION_ORDER.iter().enumerate() {
        let seed = PROJECTION_SEEDS
            .iter()
            .find(|seed| seed.name == *name)
            .expect("every reported program is a seeded row");
        let case = format!("{name} at position {position}");
        let item = reply.get(&format!("/Items/{position}"));
        let start_millis = BASE_MILLIS + seed.offset_millis;

        assert_eq!(
            item.get("Arn").and_then(Value::as_str),
            Some(program_arn(CHANNEL, name).as_str()),
            "case {case:?}: expected the program's own ARN, got {:?}",
            item.get("Arn")
        );
        assert_eq!(
            item.get("ChannelName").and_then(Value::as_str),
            Some(CHANNEL),
            "case {case:?}: expected the channel the schedule belongs to, got {:?}",
            item.get("ChannelName")
        );
        assert_eq!(
            item.get("SourceLocationName").and_then(Value::as_str),
            Some(SOURCE_LOCATION),
            "case {case:?}: expected the program's source location, got {:?}",
            item.get("SourceLocationName")
        );
        assert_eq!(
            item.get("LiveSourceName").and_then(Value::as_str),
            seed.live_source_name,
            "case {case:?}: expected the program's live source, got {:?}",
            item.get("LiveSourceName")
        );
        assert_eq!(
            item.get("VodSourceName").and_then(Value::as_str),
            seed.vod_source_name,
            "case {case:?}: expected the program's VOD source, got {:?}",
            item.get("VodSourceName")
        );
        assert_eq!(
            item.get("ScheduleEntryType").and_then(Value::as_str),
            Some("PROGRAM"),
            "case {case:?}: expected a program playing its own media to be typed PROGRAM, \
             got {:?}",
            item.get("ScheduleEntryType")
        );
        assert_eq!(
            item.get("ApproximateStartTime").and_then(Value::as_f64),
            Some(start_millis as f64 / 1_000.0),
            "case {case:?}: expected the instant the program is scheduled for, got {:?}",
            item.get("ApproximateStartTime")
        );
        assert_eq!(
            item.get("ApproximateDurationSeconds").and_then(Value::as_i64),
            seed.duration_millis.map(|millis| millis / 1_000),
            "case {case:?}: expected the program's duration in whole seconds, got {:?}",
            item.get("ApproximateDurationSeconds")
        );
        assert_eq!(
            item.get("Audiences"),
            Some(&Value::Array(
                seed.audience_media
                    .iter()
                    .map(|(audience, _, _)| Value::String((*audience).to_string()))
                    .collect()
            )),
            "case {case:?}: expected the audiences the program defines media for, got {:?}",
            item.get("Audiences")
        );

        // Every ad break programmed into the program is reported at the instant
        // it plays: the program's start plus the break's own offset.
        let breaks = item
            .get("ScheduleAdBreaks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            breaks.len(),
            seed.ad_break_offsets.len(),
            "case {case:?}: expected {} ad break(s), got {breaks:?}",
            seed.ad_break_offsets.len()
        );
        for (index, offset) in seed.ad_break_offsets.iter().enumerate() {
            assert_eq!(
                breaks[index].get("ApproximateStartTime").and_then(Value::as_f64),
                Some((start_millis + offset) as f64 / 1_000.0),
                "case {case:?}: expected ad break {index} at its offset from the program's \
                 start, got {:?}",
                breaks[index]
            );
            assert_eq!(
                breaks[index].get("SourceLocationName").and_then(Value::as_str),
                Some(SOURCE_LOCATION),
                "case {case:?}: expected ad break {index} to report its slate's source \
                 location, got {:?}",
                breaks[index]
            );
        }
    }
}

#[tokio::test]
async fn a_channel_with_no_program_has_an_empty_schedule() {
    let emulator = emulator_with_channel().await;

    let reply = schedule(&emulator, CHANNEL, &[]).await;
    assert_eq!(
        reply.status, SCHEDULE_STATUS,
        "expected a channel with no program to be served a schedule, got {} {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.get("/Items"),
        &Value::Array(Vec::new()),
        "expected an empty schedule rather than an absent one, got {:?}",
        reply.get("/Items")
    );
    assert_eq!(
        reply.get("/NextToken"),
        &Value::Null,
        "expected no continuation token on an empty schedule, got {:?}",
        reply.get("/NextToken")
    );
}

#[tokio::test]
async fn a_name_the_account_holds_no_channel_under_has_no_schedule() {
    let emulator = emulator_with_channel().await;
    for seed in PROJECTION_SEEDS {
        seed_program(&emulator, seed.record(CHANNEL, BASE_MILLIS));
    }

    // Names of several forms that address no channel, and the channel's own
    // name once it has been deleted.
    let absent = [
        "absent",
        "SCHEDULE-CHANNEL",
        "schedule-channel-2",
        "a.b_c",
        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    ];
    for name in absent {
        let reply = schedule(&emulator, name, &[]).await;
        assert_eq!(
            reply.status, NOT_FOUND_STATUS,
            "case {name:?}: expected the schedule of an absent channel to be refused as not \
             found, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code, NOT_FOUND_CODE,
            "case {name:?}: expected {NOT_FOUND_CODE}, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.message(),
            CHANNEL_NOT_FOUND_MESSAGE,
            "case {name:?}: expected {CHANNEL_NOT_FOUND_MESSAGE:?}, got {:?}",
            reply.message()
        );
    }

    // The channel is deleted with programs on it: from then on its name
    // addresses no channel, so it has no schedule either.
    assert_eq!(
        emulator.delete(CHANNEL).await.status,
        200,
        "the channel under test must be deleted"
    );
    for round in 0..2 {
        let reply = schedule(&emulator, CHANNEL, &[]).await;
        assert_eq!(
            reply.status, NOT_FOUND_STATUS,
            "round {round}: expected the schedule of a deleted channel to be refused as not \
             found, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code, NOT_FOUND_CODE,
            "round {round}: expected {NOT_FOUND_CODE}, got {:?}",
            reply.error_code
        );
        assert_eq!(
            reply.message(),
            CHANNEL_NOT_FOUND_MESSAGE,
            "round {round}: expected {CHANNEL_NOT_FOUND_MESSAGE:?}, got {:?}",
            reply.message()
        );
    }

    // And the schedule went with the channel: a new channel under the same name
    // carries none of the deleted one's programs.
    let recreated = emulator.create(CHANNEL, &create_channel_request()).await;
    assert_eq!(
        recreated.status, 200,
        "expected the deleted name to be free, got {} {:?}",
        recreated.status, recreated.body
    );
    assert_eq!(
        schedule(&emulator, CHANNEL, &[]).await.get("/Items"),
        &Value::Array(Vec::new()),
        "expected the new channel to carry no program of the deleted one"
    );
}

#[tokio::test]
async fn a_page_is_bounded_by_max_results_and_the_token_walks_the_whole_schedule() {
    let emulator = emulator_with_channel().await;
    for seed in PROJECTION_SEEDS {
        seed_program(&emulator, seed.record(CHANNEL, BASE_MILLIS));
    }
    let whole: Vec<String> = PROJECTION_ORDER
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    // Page sizes from one to the published maximum, and the request that names
    // none: every one of them walks the same schedule in the same order.
    let sizes: &[Option<usize>] = &[
        Some(1),
        Some(2),
        Some(3),
        Some(PROJECTION_ORDER.len()),
        Some(MAX_PAGE_SIZE),
        None,
    ];
    for size in sizes {
        let case = format!("page size {size:?}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut query: Vec<(&str, String)> = Vec::new();
            if let Some(size) = size {
                query.push(("maxResults", size.to_string()));
            }
            if let Some(token) = token.as_deref() {
                query.push(("nextToken", token.to_string()));
            }
            let borrowed: Vec<(&str, &str)> = query
                .iter()
                .map(|(name, value)| (*name, value.as_str()))
                .collect();
            let reply = schedule(&emulator, CHANNEL, &borrowed).await;
            assert_eq!(
                reply.status, SCHEDULE_STATUS,
                "case {case:?}: expected page {pages} to be served, got {} {:?}",
                reply.status, reply.body
            );

            let names = reported_names(&reply);
            assert!(
                names.len() <= size.unwrap_or(MAX_PAGE_SIZE),
                "case {case:?}: expected page {pages} to hold at most {:?} entries, got {names:?}",
                size.unwrap_or(MAX_PAGE_SIZE)
            );
            walked.extend(names);
            pages += 1;

            token = reply
                .get("/NextToken")
                .as_str()
                .map(std::string::ToString::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                pages <= PROJECTION_ORDER.len() + 1,
                "case {case:?}: expected the walk to end, got {pages} pages and {walked:?}"
            );
        }

        assert_eq!(
            walked, whole,
            "case {case:?}: expected the pages to walk the whole schedule once, in order, \
             got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_continuation_token_this_service_never_issued_is_refused() {
    let emulator = emulator_with_channel().await;
    for seed in PROJECTION_SEEDS {
        seed_program(&emulator, seed.record(CHANNEL, BASE_MILLIS));
    }

    // Tokens of several forms, none of them one this service issued: text that
    // is not hex at all, hex that decodes to no order key ("abc"), and hex
    // whose start instant is not a number ("x c-first").
    for token in ["not-a-token", "616263", "7820632d6669727374"] {
        let reply = schedule(&emulator, CHANNEL, &[("nextToken", token)]).await;
        assert_eq!(
            reply.status, BAD_REQUEST_STATUS,
            "case {token:?}: expected a foreign continuation token to be refused, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code, BAD_REQUEST_CODE,
            "case {token:?}: expected {BAD_REQUEST_CODE}, got {:?}",
            reply.error_code
        );
    }
}

#[tokio::test]
async fn the_window_bounds_the_far_end_of_the_schedule() {
    let emulator = emulator_with_channel().await;

    // These rows are scheduled relative to the instant the read runs, because
    // the window the request asks for is measured from it.
    let now_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock at or after the epoch")
        .as_millis() as i64;
    let rows: &[Seed] = &[
        Seed {
            name: "already-playing",
            offset_millis: -10 * MINUTE_MILLIS,
            duration_millis: Some(20 * MINUTE_MILLIS),
            live_source_name: Some("live-now"),
            vod_source_name: None,
            ad_break_offsets: &[],
            audience_media: &[],
        },
        Seed {
            name: "soon",
            offset_millis: 5 * MINUTE_MILLIS,
            duration_millis: Some(5 * MINUTE_MILLIS),
            live_source_name: None,
            vod_source_name: Some("vod-soon"),
            ad_break_offsets: &[],
            audience_media: &[],
        },
        Seed {
            name: "later",
            offset_millis: 45 * MINUTE_MILLIS,
            duration_millis: Some(5 * MINUTE_MILLIS),
            live_source_name: None,
            vod_source_name: Some("vod-later"),
            ad_break_offsets: &[],
            audience_media: &[],
        },
    ];
    for row in rows {
        seed_program(&emulator, row.record(CHANNEL, now_millis));
    }

    // (the window the request names, the programs it reaches). A window bounds
    // only the far end, so a program already playing is in every one of them.
    let cases: &[(Option<&str>, &[&str])] = &[
        (Some("0"), &["already-playing"]),
        (Some("10"), &["already-playing", "soon"]),
        (Some("60"), &["already-playing", "soon", "later"]),
        (Some("1440"), &["already-playing", "soon", "later"]),
        (None, &["already-playing", "soon", "later"]),
    ];
    for (window, expected) in cases {
        let query: Vec<(&str, &str)> = window
            .map(|minutes| vec![("durationMinutes", minutes)])
            .unwrap_or_default();
        let reply = schedule(&emulator, CHANNEL, &query).await;
        assert_eq!(
            reply.status, SCHEDULE_STATUS,
            "case {window:?}: expected the schedule to be served, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reported_names(&reply),
            expected
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
            "case {window:?}: expected the programs inside the window, got {:?}",
            reply.get("/Items")
        );
    }

    // A window that is not a number of minutes is unexpected data.
    for window in ["ten", "-1", "1.5", "10m", "9999999999999999999999"] {
        let reply = schedule(&emulator, CHANNEL, &[("durationMinutes", window)]).await;
        assert_eq!(
            reply.status, BAD_REQUEST_STATUS,
            "case {window:?}: expected a window that is not a number of minutes to be \
             refused, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code, BAD_REQUEST_CODE,
            "case {window:?}: expected {BAD_REQUEST_CODE}, got {:?}",
            reply.error_code
        );
    }
}

#[tokio::test]
async fn an_audience_read_reports_the_media_that_audience_sees() {
    let emulator = emulator_with_channel().await;
    for seed in PROJECTION_SEEDS {
        seed_program(&emulator, seed.record(CHANNEL, BASE_MILLIS));
    }

    // `b-tie` is the one row that defines alternate media, for `en` and `fr`;
    // every other row plays the same media for every audience. A read for an
    // audience a row defines media for reports that media and is typed
    // ALTERNATE_MEDIA; every other row is reported from its default media.
    let cases: &[(&str, &str, i64)] = &[("en", "vod-en", 60_000), ("fr", "vod-fr", 120_000)];
    for (audience, alternate_source, alternate_duration_millis) in cases {
        let reply = schedule(&emulator, CHANNEL, &[("audience", audience)]).await;
        assert_eq!(
            reply.status, SCHEDULE_STATUS,
            "case {audience:?}: expected the schedule to be served, got {} {:?}",
            reply.status, reply.body
        );
        // The audience changes WHAT plays, never which slots the schedule has.
        assert_eq!(
            reported_names(&reply),
            PROJECTION_ORDER
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
            "case {audience:?}: expected every scheduled program, got {:?}",
            reply.get("/Items")
        );

        for (position, name) in PROJECTION_ORDER.iter().enumerate() {
            let item = reply.get(&format!("/Items/{position}"));
            let case = format!("{audience} / {name}");
            assert_eq!(
                item.get("Audiences"),
                Some(&Value::Array(vec![Value::String((*audience).to_string())])),
                "case {case:?}: expected the entry to report the audience it was read for, \
                 got {:?}",
                item.get("Audiences")
            );
            if *name == "b-tie" {
                assert_eq!(
                    item.get("ScheduleEntryType").and_then(Value::as_str),
                    Some("ALTERNATE_MEDIA"),
                    "case {case:?}: expected the alternate media to be typed \
                     ALTERNATE_MEDIA, got {:?}",
                    item.get("ScheduleEntryType")
                );
                assert_eq!(
                    item.get("SourceLocationName").and_then(Value::as_str),
                    Some(ALTERNATE_LOCATION),
                    "case {case:?}: expected the alternate media's source location, got {:?}",
                    item.get("SourceLocationName")
                );
                assert_eq!(
                    item.get("VodSourceName").and_then(Value::as_str),
                    Some(*alternate_source),
                    "case {case:?}: expected the alternate media's VOD source, got {:?}",
                    item.get("VodSourceName")
                );
                assert_eq!(
                    item.get("ApproximateDurationSeconds").and_then(Value::as_i64),
                    Some(alternate_duration_millis / 1_000),
                    "case {case:?}: expected the alternate media's duration, got {:?}",
                    item.get("ApproximateDurationSeconds")
                );
            } else {
                assert_eq!(
                    item.get("ScheduleEntryType").and_then(Value::as_str),
                    Some("PROGRAM"),
                    "case {case:?}: expected a program that defines no media for this \
                     audience to be typed PROGRAM, got {:?}",
                    item.get("ScheduleEntryType")
                );
                assert_eq!(
                    item.get("SourceLocationName").and_then(Value::as_str),
                    Some(SOURCE_LOCATION),
                    "case {case:?}: expected the program's own source location, got {:?}",
                    item.get("SourceLocationName")
                );
            }
        }
    }

    // An audience no program defines media for sees every program's default
    // media.
    let reply = schedule(&emulator, CHANNEL, &[("audience", "de")]).await;
    for position in 0..PROJECTION_ORDER.len() {
        let item = reply.get(&format!("/Items/{position}"));
        assert_eq!(
            item.get("ScheduleEntryType").and_then(Value::as_str),
            Some("PROGRAM"),
            "case {position}: expected an undefined audience to see the default media, got {:?}",
            item.get("ScheduleEntryType")
        );
        assert_eq!(
            item.get("SourceLocationName").and_then(Value::as_str),
            Some(SOURCE_LOCATION),
            "case {position}: expected the program's own source location, got {:?}",
            item.get("SourceLocationName")
        );
    }
}

#[tokio::test]
async fn reading_the_schedule_changes_nothing_a_later_read_sees() {
    let emulator = emulator_with_channel().await;
    for seed in PROJECTION_SEEDS {
        seed_program(&emulator, seed.record(CHANNEL, BASE_MILLIS));
    }

    let before = schedule(&emulator, CHANNEL, &[]).await.body;
    let described = emulator.describe(CHANNEL).await.body;

    // Every form of read, several times over.
    for round in 0..2 {
        for query in [
            vec![],
            vec![("maxResults", "1")],
            vec![("audience", "en")],
            vec![("durationMinutes", "1440")],
        ] {
            let reply = schedule(&emulator, CHANNEL, &query).await;
            assert_eq!(
                reply.status, SCHEDULE_STATUS,
                "round {round}, case {query:?}: expected the read to succeed, got {} {:?}",
                reply.status, reply.body
            );
        }
        // Refused reads too: neither stores anything.
        assert_eq!(
            schedule(&emulator, CHANNEL, &[("nextToken", "not-a-token")])
                .await
                .status,
            BAD_REQUEST_STATUS,
            "round {round}: expected a foreign token to be refused"
        );
        assert_eq!(
            schedule(&emulator, "absent", &[]).await.status,
            NOT_FOUND_STATUS,
            "round {round}: expected an absent channel to be refused"
        );
    }

    assert_eq!(
        schedule(&emulator, CHANNEL, &[]).await.body,
        before,
        "expected the schedule to read back exactly as it did before it was read"
    );
    assert_eq!(
        emulator.describe(CHANNEL).await.body,
        described,
        "expected reading the schedule to leave the channel itself untouched"
    );
}
