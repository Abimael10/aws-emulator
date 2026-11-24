// SPDX-License-Identifier: CC0-1.0

//! `InsertAdBreak` end to end: the ad marker a broadcasting channel's playlist takes.
//!
//! Every case goes through the real front door — `MultiServiceRouter::dispatch` with a
//! restJson1 request, exactly as a signed `aws ivs insert-ad-break` call arrives — so
//! what it asserts is the emulator's HTTP behaviour, not a handler called directly.
//!
//! The rule under test, stated once and then exercised over a SET of inputs:
//! `InsertAdBreak` marks an ad break of the requested duration in the playlist of the
//! stream a channel is currently ingesting, "using the ad configuration associated with
//! the channel". It answers a freshly minted `adBreakId` and records the break against
//! that live broadcast — with the duration asked for and the channel's own
//! `adConfigurationArn` — and it refuses in a fixed ORDER, the request's own members
//! before the channel's transient live state: `ValidationException` (400) for a request
//! outside the modelled constraints (`ChannelArn`'s pattern and length,
//! `AdDurationSeconds`' `@range 1..300`), `ResourceNotFoundException` (404) for a channel
//! ARN the store does not hold at all, `ValidationException` (400) naming the
//! `channelArn` member for a channel with no valid ad configuration associated, and only
//! then `ChannelNotBroadcasting` (404) for a channel that is not ingesting.
//! The marker belongs to the broadcast, so it does not outlive it.
//!
//! No IVS operation reads an ad break back — the model has no such read
//! (`smithy_query(query="list_operations")` lists none) — so what a call PERSISTED is
//! asserted by reading this service's store through the shared `Ivs` harness, which is
//! how the existing suites assert state no operation reports back.
//!
//! Nothing in the IVS control plane starts a stream: an ingest session does, and the
//! RTMPS ingest listener that accepts one is filed separately. So these cases bring a
//! channel live through `state::start_stream`, exactly as `live_stream_reads.rs` does.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_ivs_amazoninteractivevideoservice::state;

// The one harness for this crate's suites, declared in `channel_lifecycle.rs` and shared
// by every suite here rather than copied.
use super::channel_lifecycle::{text, Ivs, Reply, CHANNEL_ARN_PREFIX};

/// The modelled bounds of `AdDurationSeconds` (`@range 1..300`, from
/// `smithy_query(query="shape",shape="AdDurationSeconds")`).
const DURATION_MIN: i64 = 1;
const DURATION_MAX: i64 = 300;

/// The modelled width of an `AdBreakId` (`@length 12..12` over `^[a-zA-Z0-9]+$`).
const AD_BREAK_ID_LEN: usize = 12;

/// Call `InsertAdBreak` for `channel_arn` and `duration`.
async fn insert_ad_break(ivs: &Ivs, channel_arn: &str, duration: i64) -> Reply {
    ivs.call(
        "InsertAdBreak",
        &json!({ "channelArn": channel_arn, "durationSeconds": duration }),
    )
    .await
}

/// Bring a channel live the way an ingest session does, returning the id of the stream
/// the store holds for it afterwards.
fn start_ingest(ivs: &Ivs, channel_arn: &str) -> String {
    ivs.with_store_mut(|store| {
        state::start_stream(store, channel_arn)
            .stream_id
            .expect("a minted stream carries its id")
    })
}

/// What the store holds for a channel's ad break: the id, the duration, the ad
/// configuration it was inserted with and the stream whose playlist took it.
fn stored_ad_break(
    ivs: &Ivs,
    channel_arn: &str,
) -> Option<(String, i32, Option<String>, Option<String>)> {
    ivs.with_store(|store| {
        store.ad_breaks.get(channel_arn).map(|record| {
            (
                record.ad_break_id.clone(),
                record.duration_seconds,
                record.ad_configuration_arn.clone(),
                record.stream_id.clone(),
            )
        })
    })
}

/// Whether `id` satisfies the modelled `AdBreakId`: exactly 12 characters over
/// `^[a-zA-Z0-9]+$`.
fn ad_break_id_is_well_formed(id: &str) -> bool {
    id.len() == AD_BREAK_ID_LEN && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Whether any string the error body carries names `arn`.
///
/// Which wire key the modelled `exceptionMessage` member renders under is a separately
/// filed defect of this service's error serializer, so this looks at the whole error
/// object rather than pinning one key.
fn error_mentions(body: &Value, needle: &str) -> bool {
    body.as_object().is_some_and(|members| {
        members
            .values()
            .any(|value| value.as_str().is_some_and(|text| text.contains(needle)))
    })
}

/// How this service states an insert into a channel with no valid ad configuration
/// associated: the service's member-constraint sentence, naming the `channelArn` the
/// request sent and that member's own value.
fn ad_configuration_required(channel_arn: &str) -> String {
    format!(
        "Request member: channelArn failed to satisfy the constraints: valid ad \
         configuration is required to be associated with the channel requested for the \
         ad break with ARN: {channel_arn}"
    )
}

/// An ad configuration this emulator really holds, so a row that attaches one attaches a
/// resource rather than a literal.
async fn ad_configuration(ivs: &Ivs, name: &str) -> String {
    text(
        &ivs.ok(
            "CreateAdConfiguration",
            json!({ "name": name,
                    "mediaTailorPlaybackConfigurations": [
                        { "playbackConfigurationArn":
                          "arn:aws:mediatailor:us-east-1:000000000000:playbackConfiguration/ads" }
                    ] }),
        )
        .await["adConfiguration"],
        "arn",
    )
}

/// The rule over a table spanning the duration range, the channel surface and which ad
/// configuration the channel carries: a broadcasting channel with an ad configuration
/// associated takes the break, the response carries a well-formed fresh `adBreakId`, and
/// the store holds that break against the live stream and that configuration.
#[tokio::test]
async fn a_broadcasting_channel_takes_the_ad_break_it_is_sent() {
    let ivs = Ivs::new();
    let ads = ad_configuration(&ivs, "monetised").await;
    let other_ads = ad_configuration(&ivs, "monetised-two").await;

    // Rows differ along what the rule generalises over: the duration (both published
    // bounds and values between them), the shape of the channel (its type, latency mode,
    // container format, tags) and which of the two ad configurations it carries.
    let cases: Vec<(&str, Value, i64)> = vec![
        (
            "at the 1-second minimum",
            json!({ "name": "shortest", "adConfigurationArn": ads }),
            DURATION_MIN,
        ),
        (
            "at the 300-second maximum, the other ad configuration",
            json!({ "name": "longest", "adConfigurationArn": other_ads }),
            DURATION_MAX,
        ),
        (
            "a 15-second break on a basic channel, normal latency",
            json!({ "name": "basic_break", "type": "BASIC", "latencyMode": "NORMAL",
                    "adConfigurationArn": ads }),
            15,
        ),
        (
            "a 30-second break on an advanced-hd channel with tags",
            json!({ "name": "advanced-hd-break", "type": "ADVANCED_HD",
                    "tags": { "env": "test", "team": "video" },
                    "adConfigurationArn": other_ads }),
            30,
        ),
        (
            "a 299-second break on an advanced-sd fragmented-mp4 channel",
            json!({ "name": "advanced-sd-break", "type": "ADVANCED_SD",
                    "containerFormat": "FRAGMENTED_MP4",
                    "adConfigurationArn": other_ads }),
            DURATION_MAX - 1,
        ),
        (
            "a 60-second break on an authorized channel",
            json!({ "name": "private_break", "authorized": true,
                    "adConfigurationArn": ads }),
            60,
        ),
    ];

    // Every ad break is a distinct marker, so no two calls may ever report one id.
    let mut minted: BTreeSet<String> = BTreeSet::new();
    for (case, request, duration) in cases {
        let created = ivs.create(request).await;
        let channel = created
            .get("channel")
            .unwrap_or_else(|| panic!("case {case:?}: CreateChannel returns a channel"))
            .clone();
        let arn = text(&channel, "arn");
        let attached = text(&channel, "adConfigurationArn");
        let stream_id = start_ingest(&ivs, &arn);

        let reply = insert_ad_break(&ivs, &arn, duration).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a broadcasting channel must answer HTTP 200, got {} {}",
            reply.status, reply.body
        );
        let id = text(&reply.body, "adBreakId");
        assert!(
            ad_break_id_is_well_formed(&id),
            "case {case:?}: adBreakId must be 12 characters over ^[a-zA-Z0-9]+$, got {id:?}"
        );
        assert!(
            minted.insert(id.clone()),
            "case {case:?}: adBreakId {id:?} was already minted for another ad break"
        );

        let stored = stored_ad_break(&ivs, &arn).unwrap_or_else(|| {
            panic!("case {case:?}: the store must hold the ad break inserted into {arn}")
        });
        assert!(
            !attached.is_empty(),
            "case {case:?}: the channel this row inserts into carries an ad configuration"
        );
        let expected_configuration = Some(attached.clone());
        assert_eq!(
            stored,
            (
                id.clone(),
                i32::try_from(duration).expect("a modelled duration fits an i32"),
                expected_configuration,
                Some(stream_id.clone()),
            ),
            "case {case:?}: the stored ad break must be the one reported, for the duration \
             asked and against the channel's own ad configuration and live stream"
        );
    }
}

/// A second insert on the same live channel is its own break: it mints a different id and
/// is what the playlist then carries.
#[tokio::test]
async fn each_insert_on_a_live_channel_mints_its_own_ad_break() {
    let ivs = Ivs::new();
    let ads = ad_configuration(&ivs, "repeated-ads").await;
    let arn = ivs
        .create_arn(json!({ "name": "repeated", "adConfigurationArn": ads }))
        .await;
    let stream_id = start_ingest(&ivs, &arn);

    // Rows: successive inserts of different durations into one live playlist.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for duration in [DURATION_MIN, 45, DURATION_MAX] {
        let reply = insert_ad_break(&ivs, &arn, duration).await;
        assert_eq!(
            reply.status, 200,
            "case duration {duration}: expected HTTP 200, got {} {}",
            reply.status, reply.body
        );
        let id = text(&reply.body, "adBreakId");
        assert!(
            seen.insert(id.clone()),
            "case duration {duration}: adBreakId {id:?} repeats an earlier break"
        );
        let stored = stored_ad_break(&ivs, &arn);
        assert_eq!(
            stored,
            Some((
                id.clone(),
                i32::try_from(duration).expect("a modelled duration fits an i32"),
                Some(ads.clone()),
                Some(stream_id.clone()),
            )),
            "case duration {duration}: the store must hold the break just inserted, got {stored:?}"
        );
    }
}

/// A channel that has an ad configuration associated, exists and holds no live stream
/// cannot take an ad marker: there is no playlist to mark, which is what
/// `ChannelNotBroadcasting` carries. Nothing is persisted for it either.
#[tokio::test]
async fn a_channel_with_no_live_stream_is_not_broadcasting() {
    let ivs = Ivs::new();
    let ads = ad_configuration(&ivs, "offline-ads").await;
    // A channel that IS broadcasting, so no case below can be served out of another
    // channel's stream by accident.
    let bystander = ivs
        .create_arn(json!({ "name": "bystander", "adConfigurationArn": ads }))
        .await;
    start_ingest(&ivs, &bystander);

    // Rows differ along how the channel came to have no live stream — never ingested, the
    // ingest left the LIVE state, the broadcast was stopped through `StopStream` — and
    // along the duration asked for. Every one of them carries an ad configuration, so the
    // broadcast-state refusal is the one the insert reaches.
    let never = ivs
        .create_arn(json!({ "name": "never_live", "adConfigurationArn": ads }))
        .await;
    let offline = ivs
        .create_arn(json!({ "name": "went_offline", "adConfigurationArn": ads }))
        .await;
    start_ingest(&ivs, &offline);
    ivs.with_store_mut(|store| {
        let stream = store
            .streams
            .get_mut(&offline)
            .expect("the channel is broadcasting");
        stream.state = Some("OFFLINE".to_string());
    });
    let stopped = ivs
        .create_arn(json!({ "name": "stopped", "type": "BASIC", "adConfigurationArn": ads }))
        .await;
    start_ingest(&ivs, &stopped);
    let stop = ivs
        .call("StopStream", &json!({ "channelArn": stopped }))
        .await;
    assert_eq!(
        stop.status, 200,
        "StopStream on a live channel must succeed, got {} {}",
        stop.status, stop.body
    );

    let cases: Vec<(&str, String, i64)> = vec![
        ("never ingested", never, DURATION_MIN),
        ("the ingest left the LIVE state", offline, 30),
        ("the broadcast was stopped", stopped, DURATION_MAX),
    ];

    for (case, arn, duration) in cases {
        let reply = insert_ad_break(&ivs, &arn, duration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (404, "ChannelNotBroadcasting"),
            "case {case:?}: expected 404 ChannelNotBroadcasting, got {} {} {}",
            reply.status,
            reply.error_code,
            reply.body
        );
        assert!(
            error_mentions(&reply.body, &arn),
            "case {case:?}: the refusal must name the channel asked about, got {}",
            reply.body
        );
        assert!(
            reply.body.get("adBreakId").is_none(),
            "case {case:?}: a refusal carries no adBreakId, got {}",
            reply.body
        );
        assert_eq!(
            stored_ad_break(&ivs, &arn),
            None,
            "case {case:?}: a refused insert must persist no ad break for {arn}"
        );
    }

    // And the channel that is live still takes a break after every refusal above.
    let reply = insert_ad_break(&ivs, &bystander, 10).await;
    assert_eq!(
        reply.status, 200,
        "case bystander: the broadcasting channel still takes an ad break, got {} {}",
        reply.status, reply.body
    );
}

/// A channel ARN the store does not hold is `ResourceNotFoundException`, not
/// `ChannelNotBroadcasting`: the channel is resolved before its stream is, so a
/// well-formed ARN that was never minted, and one whose channel was deleted, answer the
/// same way.
#[tokio::test]
async fn a_channel_arn_the_store_does_not_hold_is_not_found() {
    let ivs = Ivs::new();
    let deleted = ivs.create_arn(json!({ "name": "deleted" })).await;
    let delete = ivs.call("DeleteChannel", &json!({ "arn": deleted })).await;
    assert_eq!(
        delete.status, 204,
        "DeleteChannel must answer the modelled 204, got {} {}",
        delete.status, delete.body
    );
    let deleted_live = ivs.create_arn(json!({ "name": "deleted-live" })).await;
    start_ingest(&ivs, &deleted_live);
    let delete_live = ivs
        .call("DeleteChannel", &json!({ "arn": deleted_live }))
        .await;
    assert_eq!(
        delete_live.status, 204,
        "DeleteChannel on a live channel must answer the modelled 204, got {} {}",
        delete_live.status, delete_live.body
    );

    // Rows differ along how the ARN came to name no channel and along its form within the
    // modelled pattern — region, account, id length and id alphabet, including an ARN
    // exactly at the modelled 128-character bound.
    let at_the_bound = format!(
        "{CHANNEL_ARN_PREFIX}{}",
        "a".repeat(128 - CHANNEL_ARN_PREFIX.len())
    );
    let cases: Vec<(&str, String, i64)> = vec![
        (
            "never minted, all-zero id",
            format!("{CHANNEL_ARN_PREFIX}000000000000"),
            DURATION_MIN,
        ),
        (
            "never minted, another region",
            "arn:aws:ivs:eu-west-1:000000000000:channel/abcdef123456".to_string(),
            60,
        ),
        (
            "never minted, another account",
            "arn:aws:ivs:us-east-1:123456789012:channel/abcdef123456".to_string(),
            90,
        ),
        (
            "never minted, single-character id",
            format!("{CHANNEL_ARN_PREFIX}a"),
            120,
        ),
        (
            "never minted, at the 128-character bound",
            at_the_bound,
            DURATION_MAX,
        ),
        ("deleted after creation", deleted, 30),
        ("deleted while broadcasting", deleted_live, 30),
    ];

    for (case, arn, duration) in cases {
        let reply = insert_ad_break(&ivs, &arn, duration).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected 404 ResourceNotFoundException for {arn}, got {} {} {}",
            reply.status,
            reply.error_code,
            reply.body
        );
        assert!(
            error_mentions(&reply.body, &arn),
            "case {case:?}: the refusal must name the ARN asked for, got {}",
            reply.body
        );
        assert_eq!(
            stored_ad_break(&ivs, &arn),
            None,
            "case {case:?}: a refused insert must persist no ad break for {arn}"
        );
    }
}

/// The channel's ad configuration is a precondition on the `channelArn` the request sent,
/// so a channel with none validly associated is refused with 400 `ValidationException`
/// naming that member — BEFORE the channel's broadcast state is consulted, which is why
/// the refusal is the same for an idle channel and for one that is ingesting. Attaching a
/// configuration is what gets past it: the same insert then reaches the broadcast-state
/// rule.
#[tokio::test]
async fn a_channel_with_no_ad_configuration_is_refused_before_its_broadcast_state() {
    let ivs = Ivs::new();
    let ads = ad_configuration(&ivs, "attachable").await;

    // A channel whose associated ad configuration this store does not hold has none
    // either: the association is an ARN, and what counts is the resource it resolves to.
    // The two ARNs below are well formed under the modelled `AdConfigurationArn` pattern
    // `^arn:aws:ivs:[a-z0-9-]+:[0-9]+:ad-configuration/[a-zA-Z0-9-]+$`
    // (`smithy_query(query="shape",shape="AdConfigurationArn")`) and were never minted.
    let never_minted = "arn:aws:ivs:us-east-1:000000000000:ad-configuration/abcdef123456";
    let other_region = "arn:aws:ivs:eu-west-1:000000000000:ad-configuration/abcdef123456";

    // A channel that WAS associated and has had the association removed again, which is
    // the same state as one that never had it.
    let detached = ivs
        .create_arn(json!({ "name": "detached", "adConfigurationArn": ads }))
        .await;
    start_ingest(&ivs, &detached);
    let update = ivs
        .call(
            "UpdateChannel",
            &json!({ "arn": detached, "adConfigurationArn": "" }),
        )
        .await;
    assert_eq!(
        update.status, 200,
        "UpdateChannel must detach the ad configuration, got {} {}",
        update.status, update.body
    );

    // Rows differ along HOW the channel comes to have no valid ad configuration (the
    // member absent, sent empty, naming a configuration that was never minted, in another
    // Region, or detached again), along whether the channel is INGESTING — which is the
    // state the refusal must not depend on — and along the duration asked for.
    let absent_idle = ivs.create_arn(json!({ "name": "absent_idle" })).await;
    let absent_live = ivs.create_arn(json!({ "name": "absent_live" })).await;
    start_ingest(&ivs, &absent_live);
    let empty_live = ivs
        .create_arn(json!({ "name": "empty_live", "adConfigurationArn": "" }))
        .await;
    start_ingest(&ivs, &empty_live);
    let dangling_live = ivs
        .create_arn(json!({ "name": "dangling_live", "adConfigurationArn": never_minted }))
        .await;
    start_ingest(&ivs, &dangling_live);
    let dangling_idle = ivs
        .create_arn(json!({ "name": "dangling_idle", "type": "BASIC",
                            "adConfigurationArn": other_region }))
        .await;

    let cases: Vec<(&str, String, i64)> = vec![
        ("the member absent, channel idle", absent_idle.clone(), 30),
        (
            "the member absent, channel ingesting",
            absent_live.clone(),
            DURATION_MIN,
        ),
        (
            "the member sent empty, channel ingesting",
            empty_live,
            DURATION_MAX,
        ),
        (
            "an ad configuration that was never minted, channel ingesting",
            dangling_live,
            60,
        ),
        (
            "an ad configuration of another Region, channel idle",
            dangling_idle,
            90,
        ),
        (
            "the association removed again, channel ingesting",
            detached,
            DURATION_MAX,
        ),
    ];

    for (case, arn, duration) in cases {
        let reply = insert_ad_break(&ivs, &arn, duration).await;
        assert_eq!(
            (
                reply.status,
                reply.error_code.as_str(),
                text(&reply.body, "message").as_str()
            ),
            (
                400,
                "ValidationException",
                ad_configuration_required(&arn).as_str()
            ),
            "case {case:?}: expected 400 ValidationException {:?}, got {} {} {}",
            ad_configuration_required(&arn),
            reply.status,
            reply.error_code,
            reply.body
        );
        assert!(
            reply.body.get("adBreakId").is_none(),
            "case {case:?}: a refusal carries no adBreakId, got {}",
            reply.body
        );
        assert_eq!(
            stored_ad_break(&ivs, &arn),
            None,
            "case {case:?}: a refused insert must persist no ad break for {arn}"
        );
    }

    // Attaching a configuration is what gets past the precondition: the ingesting channel
    // then takes the break, and the idle one reaches the broadcast-state rule instead.
    for (case, arn, status, code) in [
        ("ingesting", absent_live, 200, ""),
        ("idle", absent_idle, 404, "ChannelNotBroadcasting"),
    ] {
        let update = ivs
            .call(
                "UpdateChannel",
                &json!({ "arn": arn, "adConfigurationArn": ads }),
            )
            .await;
        assert_eq!(
            update.status, 200,
            "case attached, {case}: UpdateChannel must attach the ad configuration, got {} {}",
            update.status, update.body
        );
        let reply = insert_ad_break(&ivs, &arn, 30).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (status, code),
            "case attached, {case}: expected {status} {code:?}, got {} {} {}",
            reply.status,
            reply.error_code,
            reply.body
        );
    }
}

/// A request outside the modelled constraints is a bad REQUEST, so it is refused with 400
/// `ValidationException` rather than acted on: each bound of `AdDurationSeconds` one step
/// past it, a required member left out, and a `channelArn` outside its shape.
#[tokio::test]
async fn a_request_outside_the_modelled_constraints_is_refused() {
    let ivs = Ivs::new();
    let ads = ad_configuration(&ivs, "strict-ads").await;
    let live = ivs
        .create_arn(json!({ "name": "live_strict", "adConfigurationArn": ads }))
        .await;
    start_ingest(&ivs, &live);

    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "durationSeconds one below the 1-second minimum",
            json!({ "channelArn": live, "durationSeconds": DURATION_MIN - 1 }),
            "durationSeconds",
        ),
        (
            "durationSeconds well below the minimum",
            json!({ "channelArn": live, "durationSeconds": -30 }),
            "durationSeconds",
        ),
        (
            "durationSeconds one past the 300-second maximum",
            json!({ "channelArn": live, "durationSeconds": DURATION_MAX + 1 }),
            "durationSeconds",
        ),
        (
            "durationSeconds left out, though it is required",
            json!({ "channelArn": live }),
            "durationSeconds",
        ),
        (
            "channelArn left out, though it is required",
            json!({ "durationSeconds": 30 }),
            "channelArn",
        ),
        (
            "channelArn empty, below its 1-character minimum",
            json!({ "channelArn": "", "durationSeconds": 30 }),
            "channelArn",
        ),
        (
            "channelArn one past its 128-character maximum",
            json!({ "channelArn": format!("{CHANNEL_ARN_PREFIX}{}",
                                          "a".repeat(129 - CHANNEL_ARN_PREFIX.len())),
                    "durationSeconds": 30 }),
            "channelArn",
        ),
        (
            "channelArn naming a stream key, not a channel",
            json!({ "channelArn": "arn:aws:ivs:us-east-1:000000000000:stream-key/abcdef123456",
                    "durationSeconds": 30 }),
            "channelArn",
        ),
        (
            "channelArn is not an ARN at all",
            json!({ "channelArn": "live_strict", "durationSeconds": 30 }),
            "channelArn",
        ),
    ];

    for (case, request, member) in cases {
        let reply = ivs.call("InsertAdBreak", &request).await;
        assert_eq!(
            (reply.status, reply.error_code.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException for {request}, got {} {} {}",
            reply.status,
            reply.error_code,
            reply.body
        );
        assert!(
            error_mentions(&reply.body, member),
            "case {case:?}: the refusal must name {member}, got {}",
            reply.body
        );
    }

    // None of the refusals above marked anything in the live channel's playlist.
    assert_eq!(
        stored_ad_break(&ivs, &live),
        None,
        "case refused requests: no refused request may persist an ad break for {live}"
    );
}

/// The marker belongs to the broadcast it was inserted into: ending that broadcast takes
/// it with the stream, and a channel that goes live again starts with no ad break and
/// mints a new id for the next one.
#[tokio::test]
async fn an_ad_break_does_not_outlive_the_broadcast_it_was_inserted_into() {
    let ivs = Ivs::new();
    let ads = ad_configuration(&ivs, "lifetime-ads").await;

    // Rows: the two ways a broadcast ends — the ingest is stopped, and the channel is
    // deleted while live.
    let stopped = ivs
        .create_arn(json!({ "name": "stop_clears", "adConfigurationArn": ads }))
        .await;
    start_ingest(&ivs, &stopped);
    let first = text(&insert_ad_break(&ivs, &stopped, 30).await.body, "adBreakId");
    assert!(
        stored_ad_break(&ivs, &stopped).is_some(),
        "case stopped: the live playlist holds the break just inserted"
    );
    let stop = ivs
        .call("StopStream", &json!({ "channelArn": stopped }))
        .await;
    assert_eq!(
        stop.status, 200,
        "case stopped: StopStream must succeed, got {} {}",
        stop.status, stop.body
    );
    assert_eq!(
        stored_ad_break(&ivs, &stopped),
        None,
        "case stopped: the marker goes with the stream it was inserted into"
    );

    // Live again: a new broadcast starts with an unmarked playlist and its own break.
    start_ingest(&ivs, &stopped);
    assert_eq!(
        stored_ad_break(&ivs, &stopped),
        None,
        "case re-ingested: a fresh broadcast carries no earlier break"
    );
    let second = text(&insert_ad_break(&ivs, &stopped, 30).await.body, "adBreakId");
    assert_ne!(
        first, second,
        "case re-ingested: the next break is its own marker, got {second:?} twice"
    );

    let deleted = ivs
        .create_arn(json!({ "name": "delete_clears", "adConfigurationArn": ads }))
        .await;
    start_ingest(&ivs, &deleted);
    assert_eq!(
        insert_ad_break(&ivs, &deleted, 30).await.status,
        200,
        "case deleted: the live channel takes a break before it is deleted"
    );
    let delete = ivs.call("DeleteChannel", &json!({ "arn": deleted })).await;
    assert_eq!(
        delete.status, 204,
        "case deleted: DeleteChannel must answer the modelled 204, got {} {}",
        delete.status, delete.body
    );
    assert_eq!(
        stored_ad_break(&ivs, &deleted),
        None,
        "case deleted: a deleted channel leaves no ad break behind"
    );
}
