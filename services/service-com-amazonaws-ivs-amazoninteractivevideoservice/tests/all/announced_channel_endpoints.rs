// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the two ADDRESSES every channel this service reports announces,
//! driven over the wire through `MultiServiceRouter::dispatch` on the harness
//! `channel_lifecycle` owns.
//!
//! The rules under test, each stated once and then exercised over a SET of inputs:
//!
//! 1. A member this API fills with an address is announced in the FORM that member carries.
//!    A channel's `ingestEndpoint`, and the `srt.endpoint` that equals it, carries the HOST
//!    a client composes an authority from, appending the port its own protocol dials
//!    (`rtmps://<ingestEndpoint>:443/app/<streamKey>`, `srt://<srt.endpoint>:<port>`), so it
//!    is a DNS hostname — a minted label under the labels this service publishes its
//!    contribute plane in, in the zone this emulator may announce a name of its own in —
//!    carrying no scheme, no path and NO PORT, and never an address literal.
//! 2. A member that carries a URL the client FETCHES exactly as it stands keeps the `https`
//!    scheme and the playlist path `/api/video/v1/<region>.<account>.channel.<id>.m3u8`,
//!    with the Region, the account and the channel id read off the channel's own ARN, and its
//!    HOST is a hostname of the SAME class as rule 1's: a minted label under the labels this
//!    service publishes its playback edge in, in the zone this emulator may announce in,
//!    carrying NO PORT — the port such a URL is dialed on is the one its scheme names.
//! 3. The ZONE every announced name sits in — the host of an announced URL included — comes
//!    from the CALLER's own addressing: the loopback zone for a caller that reached an
//!    address, the reached name for a caller that reached one of this emulator's own names
//!    — never a zone AWS serves its own names in.
//! 4. Every projection of one channel agrees: the create, the single read, the batch read,
//!    the update and the live stream read off that channel all announce the same addresses
//!    for the same caller, and a read announces its own caller's addresses without changing
//!    what a later caller is told.
//! 5. The contribute host is the ACCOUNT's, not a channel's: every channel of the account
//!    reports the SAME `ingestEndpoint`, whatever Region its credential scope names and
//!    whatever the channel's own id is, and a read or an update does not move it.
//! 6. The address a client COMPOSES from the announced contribute host — that host plus each
//!    port the plane's own wires are dialed on — is served by this deployment's own
//!    contribute plane: a connection opened to it is answered by a server that speaks RTMP.
//!    A port this emulator opened nothing on is served by nothing, the port an SRT ingest URL
//!    names included, because this emulator answers no SRT.
//! 7. The announced playback NAME is one a client INSIDE this emulator resolves: the register
//!    every such client reads answers this service for it, on the bare name and on the port
//!    its scheme names, and the URL as announced is answered by this service's own playback
//!    plane.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_ivs_amazoninteractivevideoservice::state;

use super::channel_lifecycle::{text, Ivs, AUTHORIZATION};

/// The playlist path AWS serves a channel's master manifest at, ahead of the channel's
/// identity: `Channel$playbackUrl` is the "Channel playback URL" and `Stream$playbackUrl`
/// the "URL of the master playlist, required by the video player to play the HLS stream"
/// (`smithy_query(query="shape",shape="Channel,Stream")`); neither shape constrains the
/// value, so the path AWS's recorded playback URLs carry is the form under test.
const PLAYBACK_PATH_PREFIX: &str = "/api/video/v1";

/// The suffix of that path.
const PLAYBACK_PATH_SUFFIX: &str = ".m3u8";

/// The domain label a channel's contribute host carries between its minted label and this
/// service's own domain: the ingest server is the account's, published once rather than per
/// Region.
const CONTRIBUTE_DOMAIN_LABEL: &str = "global-contribute";

/// The domain label a channel's PLAYBACK host carries between the channel's Region and this
/// service's own domain: the playlists of a Region's channels are served by that Region's
/// edge.
const PLAYBACK_DOMAIN_LABEL: &str = "playback";

/// The label this service's own names sit under, inside the zone the caller's addressing
/// puts this emulator in.
const SERVICE_DOMAIN_LABEL: &str = "ivs";

/// How many characters a minted host label is written with, and the alphabet it is written
/// in: the twelve lowercase-hex characters of an IVS host label.
const HOST_LABEL_LENGTH: usize = 12;

/// The ports a client APPENDS to the announced contribute host, which is the half of the
/// authority the member does not carry: RTMPS on 443 and RTMP on 1935, the two wires this
/// deployment's contribute plane really speaks and the two an encoder configured with a
/// channel's ingest endpoint opens.
const CONTRIBUTE_PORTS: [u16; 2] = [443, 1935];

/// The port an SRT ingest URL names. The `srt.endpoint` a channel announces is the same host
/// as its `ingestEndpoint` — one host for both, as the recording holds — but nothing of this
/// emulator answers SRT, so no plane may be found there: a reachability this deployment does
/// not provide must not be claimed.
const SRT_PORT: u16 = 9000;

/// The port a playback URL is DIALED on, which is the port its `https` scheme names: the
/// half of the authority the member itself does not carry, exactly as a contribute port is.
const PLAYBACK_PORT: u16 = 443;

/// Hosts no address this emulator announces may name: a value on one of these leaves the
/// sandbox, because nothing here answers on an Amazon production host.
const FOREIGN_DOMAINS: [&str; 2] = ["live-video.net", "amazonaws.com"];

/// A SigV4 `Authorization` header in `region`'s credential scope, which is how a request
/// names the Region it is addressed to (and therefore the Region its resources are minted
/// in).
fn authorization_in(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIVS/20260101/{region}/ivs/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=\
         0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// The Region, the account and the channel id a `ChannelArn` names
/// (`arn:aws:ivs:<region>:<account>:channel/<id>`).
fn channel_of_arn(arn: &str) -> (String, String, String) {
    let segments: Vec<&str> = arn.splitn(6, ':').collect();
    assert_eq!(
        segments.len(),
        6,
        "a channel ARN has six colon-separated segments, got {arn:?}"
    );
    let id = segments[5]
        .strip_prefix("channel/")
        .unwrap_or_else(|| panic!("a channel ARN names a channel resource, got {arn:?}"));
    (
        segments[3].to_string(),
        segments[4].to_string(),
        id.to_string(),
    )
}

/// The authority (`host[:port]`) of an absolute URL, and the path under it.
fn split_url(url: &str) -> (String, String) {
    let after_scheme = url
        .split_once("://")
        .unwrap_or_else(|| panic!("a playback URL is absolute, got {url:?}"))
        .1;
    match after_scheme.find('/') {
        Some(end) => (
            after_scheme[..end].to_string(),
            after_scheme[end..].to_string(),
        ),
        None => (after_scheme.to_string(), String::new()),
    }
}

/// Rule 1's and rule 2's shared form, on one announced HOST: a DNS hostname carrying no
/// scheme, no path, no userinfo and no port, whose first label is a minted one and whose
/// remaining labels are `suffix` — the labels this service publishes that plane under,
/// ending in the zone the caller's own addressing puts this emulator in.
fn assert_announced_hostname(case: &str, member: &str, host: &str, suffix: &str) {
    assert!(
        !host.is_empty(),
        "case {case:?}: {member} must be announced, got {host:?}"
    );
    for forbidden in ["://", "/", "@", " ", ":"] {
        assert!(
            !host.contains(forbidden),
            "case {case:?}: {member} must be a bare hostname carrying no {forbidden:?} (no \
             scheme, no path, no port), got {host:?}"
        );
    }
    assert!(
        host.parse::<std::net::IpAddr>().is_err() && !host.starts_with('['),
        "case {case:?}: {member} must be a hostname, not an address literal, got {host:?}"
    );
    for label in host.split('.') {
        assert!(
            !label.is_empty()
                && label
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-'),
            "case {case:?}: every label of {member} must be a DNS label, got {label:?} in \
             {host:?}"
        );
    }
    for domain in FOREIGN_DOMAINS {
        assert!(
            !host.ends_with(domain),
            "case {case:?}: {member} must not send a client to a {domain} host, got {host:?}"
        );
    }
    let label = host
        .strip_suffix(&format!(".{suffix}"))
        .unwrap_or_else(|| panic!("case {case:?}: {member} must be a minted label under {suffix:?}, got {host:?}"));
    assert!(
        label.chars().count() == HOST_LABEL_LENGTH
            && label
                .chars()
                .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character)),
        "case {case:?}: {member} must carry a {HOST_LABEL_LENGTH}-character lowercase-hex \
         label, got {label:?} in {host:?}"
    );
}

/// The labels a contribute host carries after its minted label, for a caller whose
/// addressing puts this emulator in `zone`.
fn contribute_suffix(zone: &str) -> String {
    format!("{CONTRIBUTE_DOMAIN_LABEL}.{SERVICE_DOMAIN_LABEL}.{zone}")
}

/// The labels the host of a playback URL carries after its minted label, for a channel of
/// `region` announced to a caller whose addressing puts this emulator in `zone`.
fn playback_suffix(region: &str, zone: &str) -> String {
    format!("{region}.{PLAYBACK_DOMAIN_LABEL}.{SERVICE_DOMAIN_LABEL}.{zone}")
}

/// Rules 1 and 5 on a channel's INGEST host: the account's contribute server, with the
/// `srt.endpoint` beside it the same value — one host serves both protocols. Answers the
/// value, so a caller can compare it across projections.
pub(crate) fn assert_announced_ingest(
    case: &str,
    member: &str,
    channel: &Value,
    zone: &str,
) -> String {
    let ingest = text(channel, "ingestEndpoint");
    assert_announced_hostname(
        case,
        &format!("{member} ingestEndpoint"),
        &ingest,
        &contribute_suffix(zone),
    );
    assert_eq!(
        text(&channel["srt"], "endpoint"),
        ingest,
        "case {case:?}: {member} srt.endpoint must be the channel's ingest endpoint, got {}",
        channel["srt"]
    );
    ingest
}

/// Rules 2 and 3 on one announced playback URL: the `https` scheme, a HOST of the same class
/// as the ingest host beside it — a minted label over this service's playback labels in the
/// zone this caller's addressing puts the emulator in, with no port — and the playlist path
/// with the channel's own identity in it. Answers the host, so a caller can compare it across
/// projections.
fn assert_announced_playback(case: &str, member: &str, url: &str, zone: &str, arn: &str) -> String {
    let (region, account, id) = channel_of_arn(arn);
    let (host, path) = split_url(url);
    assert!(
        url.starts_with("https://"),
        "case {case:?}: {member} must be fetched over https, got {url:?}"
    );
    assert_announced_hostname(
        case,
        &format!("{member}'s host"),
        &host,
        &playback_suffix(&region, zone),
    );
    assert_eq!(
        path,
        format!("{PLAYBACK_PATH_PREFIX}/{region}.{account}.channel.{id}{PLAYBACK_PATH_SUFFIX}"),
        "case {case:?}: {member} must carry the playlist path AWS lays out for this channel, \
         got {url:?}"
    );
    assert_eq!(
        url,
        format!("https://{host}{path}"),
        "case {case:?}: {member} must be the host and the path and nothing else, got {url:?}"
    );
    host
}

/// One addressing a client reaches this emulator through: the base the emulator is served
/// at, the `Host` the client sent, and the zone every name announced to it sits in — the host
/// of an announced URL included.
struct Addressing {
    case: &'static str,
    /// The base `OperationContext::endpoint` reports, or `None` for an emulator served on
    /// no port at all (the harness default).
    served_at: Option<&'static str>,
    /// The host the client addressed, or `None` for a client that names only this
    /// service's published endpoint on the request URI.
    host: Option<&'static str>,
    /// The zone every name announced to that client must sit in — the zone that client's
    /// own addressing puts this emulator in.
    zone: &'static str,
}

/// The addressings this suite spans: rows differ along what rules 1-3 generalise over — how
/// the client addressed this emulator (an address with a port, a name with a port, an IPv6
/// literal, one of AWS's own hostnames, none at all) and whether this emulator is served
/// on a port.
fn addressings() -> Vec<Addressing> {
    vec![
        Addressing {
            case: "an SDK client naming this service's endpoint, on an unserved emulator",
            served_at: None,
            host: None,
            // `smithy_server_runtime::UNSERVED_URL_BASE`: the one base an emulator that is
            // served on no port reports, a name RFC 6761 reserves so that nothing resolves
            // it — which is the honest zone for a deployment reachable nowhere.
            zone: "emulator.invalid",
        },
        Addressing {
            case: "a client on the loopback address",
            served_at: None,
            host: Some("127.0.0.1:9297"),
            // An ADDRESS names no zone, and the zone a resolver answers with the
            // interface it names is the loopback zone RFC 6761 reserves.
            zone: "localhost",
        },
        Addressing {
            case: "a client naming the loopback by name, on another port",
            served_at: None,
            host: Some("localhost:40305"),
            zone: "localhost",
        },
        Addressing {
            case: "a client on an IPv6 literal",
            served_at: None,
            host: Some("[::1]:9297"),
            zone: "localhost",
        },
        Addressing {
            case: "a client that resolves AWS's own hosts, on an emulator served on a port",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("ivs.us-east-1.amazonaws.com"),
            zone: "localhost",
        },
        Addressing {
            case: "an SDK client on an emulator served at a name",
            served_at: Some("https://emulator.test:8443"),
            host: None,
            zone: "emulator.test",
        },
        Addressing {
            case: "a client on the loopback address of an emulator served elsewhere",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("localhost:9297"),
            zone: "localhost",
        },
    ]
}

/// The creates this suite spans, so the rules are asserted over a set of channels rather
/// than one: rows differ along the channel type, the latency mode, the flags, the name and
/// the tags — none of which the announced addresses depend on.
fn create_requests() -> Vec<(&'static str, Value)> {
    vec![
        ("every member defaulted", json!({})),
        ("name only", json!({ "name": "announced" })),
        (
            "basic, normal latency, insecure ingest",
            json!({ "name": "basic_1", "type": "BASIC", "latencyMode": "NORMAL",
                    "insecureIngest": true }),
        ),
        (
            "advanced-hd, authorized, fragmented mp4, tagged",
            json!({ "name": "advanced-hd", "type": "ADVANCED_HD", "authorized": true,
                    "containerFormat": "FRAGMENTED_MP4", "tags": { "env": "prod" } }),
        ),
        ("an empty name", json!({ "name": "" })),
    ]
}

/// Rules 1-4 over the addressing table: every operation that projects a channel announces
/// the one contribute host this deployment serves and the playlist URL on that channel's
/// own playback host, both in the zone that caller's addressing puts this emulator in.
#[tokio::test]
async fn every_channel_projection_announces_both_addresses_for_its_own_caller() {
    for addressing in addressings() {
        let Addressing {
            case,
            served_at,
            host,
            zone,
        } = addressing;
        let ivs = match served_at {
            Some(base) => Ivs::served_at(base),
            None => Ivs::new(),
        };
        let call = |operation: &'static str, body: Value| {
            let ivs = &ivs;
            async move {
                let reply = ivs.call_as(operation, &body, host, AUTHORIZATION).await;
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: {operation} must answer HTTP 200, got {} {}",
                    reply.status, reply.body
                );
                reply.body
            }
        };

        // Rules 1 and 2 on the response a client caches at creation time.
        let created = call("CreateChannel", json!({ "name": "announced" })).await;
        let channel = &created["channel"];
        let arn = text(channel, "arn");
        let playback = assert_announced_playback(
            case,
            "CreateChannel $.channel.playbackUrl",
            &text(channel, "playbackUrl"),
            zone,
            &arn,
        );
        let ingest = assert_announced_ingest(case, "CreateChannel $.channel", channel, zone);

        // Rule 4: every other projection of the same channel agrees, for the same caller.
        let read = call("GetChannel", json!({ "arn": arn })).await;
        assert_announced_playback(
            case,
            "GetChannel $.channel.playbackUrl",
            &text(&read["channel"], "playbackUrl"),
            zone,
            &arn,
        );
        assert_announced_ingest(case, "GetChannel $.channel", &read["channel"], zone);
        let updated = call("UpdateChannel", json!({ "arn": arn, "name": "renamed" })).await;
        assert_announced_playback(
            case,
            "UpdateChannel $.channel.playbackUrl",
            &text(&updated["channel"], "playbackUrl"),
            zone,
            &arn,
        );
        assert_announced_ingest(case, "UpdateChannel $.channel", &updated["channel"], zone);
        let batch = call("BatchGetChannel", json!({ "arns": [arn] })).await;
        let batched = &batch["channels"][0];
        assert_announced_playback(
            case,
            "BatchGetChannel $.channels[0].playbackUrl",
            &text(batched, "playbackUrl"),
            zone,
            &arn,
        );
        assert_announced_ingest(case, "BatchGetChannel $.channels[0]", batched, zone);

        // The live stream is played back from its channel's own URL, so the same rule
        // holds for the stream a broadcasting channel reports.
        ivs.with_store_mut(|store| state::start_stream(store, &arn));
        let stream = call("GetStream", json!({ "channelArn": arn })).await;
        assert_announced_playback(
            case,
            "GetStream $.stream.playbackUrl",
            &text(&stream["stream"], "playbackUrl"),
            zone,
            &arn,
        );

        // Rules 1 and 2 together: the two hosts are of ONE class — both end in this
        // service's own domain inside the zone this caller's addressing names, and neither
        // carries a port — while each names the plane it serves, which is the plane label it
        // carries ahead of that domain.
        let suffix = format!("{SERVICE_DOMAIN_LABEL}.{zone}");
        for (member, announced) in [("ingestEndpoint", &ingest), ("playbackUrl's host", &playback)]
        {
            assert!(
                announced.ends_with(&suffix) && !announced.contains(':'),
                "case {case:?}: {member} must be a port-less name under {suffix:?}, got \
                 {announced:?}"
            );
        }
        assert!(
            ingest.contains(&format!(".{CONTRIBUTE_DOMAIN_LABEL}."))
                && playback.contains(&format!(".{PLAYBACK_DOMAIN_LABEL}.")),
            "case {case:?}: each host must carry the label of the plane it names, got \
             {ingest:?} and {playback:?}"
        );

        // Rule 5: the contribute server is the account's, so nothing of this channel is in
        // it — which channel a publish reaches is carried by the stream key.
        let (_, _, channel_id) = channel_of_arn(&arn);
        assert!(
            !ingest.contains(&channel_id) && !ingest.contains(&arn),
            "case {case:?}: the ingest endpoint must carry no part of the channel's own \
             identity, got {ingest:?} for channel {channel_id:?}"
        );

        // Rule 5's other half: a second channel of the same account contributes to the SAME
        // host, so one encoder configuration serves the next channel.
        let second = call("CreateChannel", json!({ "name": "announced-2" })).await;
        let second_arn = text(&second["channel"], "arn");
        let second_ingest = assert_announced_ingest(
            case,
            "CreateChannel $.channel (second)",
            &second["channel"],
            zone,
        );
        assert_announced_playback(
            case,
            "CreateChannel $.channel.playbackUrl (second)",
            &text(&second["channel"], "playbackUrl"),
            zone,
            &second_arn,
        );
        assert_eq!(
            second_ingest, ingest,
            "case {case:?}: every channel of the account contributes to one host, got \
             {second_ingest:?} and {ingest:?}"
        );
    }
}

/// Rule 4's other half: a read announces its OWN caller's addresses and changes nothing, so
/// a second caller reached through another addressing is answered its own, and the first
/// caller still gets what it got before.
#[tokio::test]
async fn a_read_through_one_address_does_not_change_what_another_caller_is_told() {
    let ivs = Ivs::new();
    let created = ivs
        .call_as("CreateChannel", &json!({ "name": "shared" }), None, AUTHORIZATION)
        .await;
    let arn = text(&created.body["channel"], "arn");

    // Rows: the addresses two clients of one emulator reach it at, each read twice so the
    // second read of the first address proves the first read of the second changed nothing.
    // The zone a reached NAME puts this emulator in is spanned by the table of
    // `every_channel_projection_announces_both_addresses_for_its_own_caller`, which varies
    // the base the emulator is served at as well as the host the client addressed.
    let addresses = [
        ("127.0.0.1:9297", "localhost"),
        ("localhost:40305", "localhost"),
        ("[::1]:8443", "localhost"),
    ];
    for round in 1..=2 {
        for (host, zone) in addresses {
            let case = format!("round {round} at {host}");
            let read = ivs
                .call_as("GetChannel", &json!({ "arn": arn }), Some(host), AUTHORIZATION)
                .await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: GetChannel must answer HTTP 200, got {} {}",
                read.status, read.body
            );
            assert_announced_playback(
                &case,
                "GetChannel $.channel.playbackUrl",
                &text(&read.body["channel"], "playbackUrl"),
                zone,
                &arn,
            );
            assert_announced_ingest(&case, "GetChannel $.channel", &read.body["channel"], zone);
        }
    }
}

/// Rule 5: the ingest server is the ACCOUNT's, not a channel's — one contribute host for
/// every channel of the account whatever Region its caller's credential scope names,
/// carrying no part of the channel's own identity, and unmoved by a read or an update.
#[tokio::test]
async fn the_ingest_endpoint_is_the_accounts_and_not_a_channels() {
    let ivs = Ivs::new();
    // Rows: the Regions a caller's credential scope can name — ordinary Region codes and
    // the shapes around them (a one-character one, a digit-bearing one) — each with the
    // whole create table driven through it. None of them may change the contribute name,
    // because this one deployment is what every channel of the account contributes to.
    let regions = [
        "us-east-1",
        "us-west-2",
        "ap-southeast-2",
        "a",
        "us-gov-west-1",
        "x1-2-3",
    ];
    let mut announced: BTreeSet<String> = BTreeSet::new();

    for region in regions {
        let authorization = authorization_in(region);
        for (row, request) in create_requests() {
            let case = format!("{region}: {row}");
            let created = ivs
                .call_as("CreateChannel", &request, None, &authorization)
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: CreateChannel must answer HTTP 200, got {} {}",
                created.status, created.body
            );
            let channel = &created.body["channel"];
            let arn = text(channel, "arn");
            let ingest = assert_announced_ingest(
                &case,
                "CreateChannel $.channel",
                channel,
                "emulator.invalid",
            );
            // The channel's own Region is in its ARN, and therefore both in the Region label
            // of its playback host and in the playlist PATH of its playback URL; the create
            // minted the channel in the Region its caller signed for.
            assert_announced_playback(
                &case,
                "CreateChannel $.channel.playbackUrl",
                &text(channel, "playbackUrl"),
                "emulator.invalid",
                &arn,
            );
            let (arn_region, _, channel_id) = channel_of_arn(&arn);
            assert_eq!(
                arn_region, region,
                "case {case:?}: a channel is minted in the Region its caller signed for, got \
                 {arn:?}"
            );

            // The ingest server is not the channel: nothing of the channel's own identity
            // is in the address it contributes to.
            assert!(
                !ingest.contains(&channel_id) && !ingest.contains(&arn),
                "case {case:?}: the ingest endpoint must carry no part of the channel's \
                 own identity, got {ingest:?} for channel {channel_id:?}"
            );

            // Rule 5 on every other projection of the same channel: a read, an update and
            // a batch read all report the contribute name the create reported.
            let read = ivs
                .call_as("GetChannel", &json!({ "arn": arn }), None, &authorization)
                .await;
            assert_eq!(
                text(&read.body["channel"], "ingestEndpoint"),
                ingest,
                "case {case:?}: GetChannel must answer the ingest endpoint the create \
                 reported, got {}",
                read.body["channel"]
            );
            let updated = ivs
                .call_as(
                    "UpdateChannel",
                    &json!({ "arn": arn, "latencyMode": "NORMAL" }),
                    None,
                    &authorization,
                )
                .await;
            assert_eq!(
                text(&updated.body["channel"], "ingestEndpoint"),
                ingest,
                "case {case:?}: an update must not move the account's ingest endpoint, \
                 got {}",
                updated.body["channel"]
            );
            let batch = ivs
                .call_as(
                    "BatchGetChannel",
                    &json!({ "arns": [arn] }),
                    None,
                    &authorization,
                )
                .await;
            assert_eq!(
                text(&batch.body["channels"][0], "ingestEndpoint"),
                ingest,
                "case {case:?}: a batch read must answer the same ingest endpoint as the \
                 single read, got {}",
                batch.body["channels"][0]
            );

            announced.insert(ingest);
        }
    }

    assert_eq!(
        announced.len(),
        1,
        "every channel of this account ingests on ONE contribute host, whatever Region \
         its caller named, got {announced:?}"
    );
}

/// Rule 6: the address a client COMPOSES from the announced contribute host is served by
/// this deployment's contribute plane.
///
/// A client does not dial the member as it stands — it appends the port its protocol uses —
/// so what has to answer is the host on each of those ports. Rows differ along the port a
/// client appends (RTMPS, RTMP, SRT), along how the caller read the value (an SDK client, a
/// client on the loopback address, a client that resolves AWS's own hosts), and on the ports
/// this emulator opened nothing on — an unopened one and the port an SRT ingest URL names —
/// which must be served by nothing.
#[tokio::test]
async fn the_address_a_client_composes_from_the_announced_ingest_host_is_served() {
    let ivs = Ivs::new();
    // Every caller whose addressing puts this emulator in the loopback zone is told the one
    // name that zone's resolver answers with the interface the core binds its listeners on.
    let callers = [
        ("a client on the loopback address", "127.0.0.1:9297"),
        ("a client naming the loopback by name", "localhost:40305"),
        ("a client on an IPv6 literal", "[::1]:9297"),
    ];
    let planes = ivs.dialable_planes();
    let mut announced: BTreeSet<String> = BTreeSet::new();

    for (case, host) in callers {
        let created = ivs
            .call_as("CreateChannel", &json!({ "name": "dialed" }), Some(host), AUTHORIZATION)
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateChannel must answer HTTP 200, got {} {}",
            created.status, created.body
        );
        let ingest = assert_announced_ingest(
            case,
            "CreateChannel $.channel",
            &created.body["channel"],
            "localhost",
        );

        for port in CONTRIBUTE_PORTS {
            let composed = format!("{ingest}:{port}");
            let dialed = planes.dialable(&ingest, port);
            let served = dialed.as_ref().map(|dialed| dialed.served_by.clone());
            assert_eq!(
                served.as_deref(),
                Some("ivs"),
                "case {case:?}: {composed} must be served by this service's contribute \
                 plane, got {served:?}"
            );
            let mut conn = dialed
                .expect("the composed address resolves to a plane")
                .plane
                .dial(None)
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: {composed} must accept a connection: {error}")
                });
            // And what answers there SPEAKS the protocol the member is for: the server half
            // of an RTMP handshake completes on the connection, through the one handshake
            // this crate's suites share with the publisher that dials the plane's socket.
            super::channel_lifecycle::rtmp_handshake(&mut conn)
                .await
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: {composed} must answer RTMP: {error}")
                });
        }

        announced.insert(ingest);
    }
    assert_eq!(
        announced.len(),
        1,
        "every caller is told the one contribute host this deployment serves, got \
         {announced:?}"
    );

    // The plane's own SOCKET is open too, which is where a publisher that holds a port
    // dials it: the core binds it, and the announced name is how the control plane reports
    // the plane a client composes an address for.
    let socket = ivs.contribute_socket();
    let port = ::service_common::data_plane_addressing::addressed_port(Some(&socket))
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or_else(|| panic!("the contribute socket carries a port, got {socket:?}"));
    let dialled = ::tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await;
    assert!(
        dialled.is_ok(),
        "the contribute socket {socket:?} must accept a connection, got {:?}",
        dialled.err()
    );

    // The ports nothing was opened on are served by nothing, so the register answers for the
    // addresses this plane really has and not for every address of this machine — the port an
    // SRT ingest URL names included, because the plane speaks the RTMP wires and this
    // emulator answers no SRT.
    let host = announced
        .iter()
        .next()
        .expect("a created channel announced a contribute host")
        .clone();
    let unopened = match port {
        8443 => 8444,
        _ => 8443,
    };
    for (case, asked) in [
        ("a port nothing was opened on", unopened),
        ("the port an SRT ingest URL names", SRT_PORT),
    ] {
        let nothing = planes.dialable(&host, asked);
        assert!(
            nothing.is_none(),
            "case {case:?}: {host}:{asked} must be served by nothing, got {nothing:?}"
        );
    }
}

/// Rule 7: the playback NAME a channel announces is one a client INSIDE this emulator
/// resolves — the register every such client reads answers this service for it, on the bare
/// name and on the port its scheme names — and the URL as announced is answered by this
/// service's own playback plane rather than by whoever answers an address nobody published.
///
/// Rows differ along the Region the caller's credential scope names, because the playback edge
/// is the channel's own Region's: each Region announces a name of its own, and each has to
/// resolve. The closing rows are the names nothing announced, which must resolve to nothing —
/// a Region only a REFUSED create named, and the Region of a channel that was DELETED, because
/// the records are a projection of the channels this store holds rather than a log of the calls
/// it was sent.
#[tokio::test]
async fn the_announced_playback_name_resolves_to_this_service_and_serves_its_path() {
    let ivs = Ivs::new();
    let published = ivs.published_planes();
    // The Regions a caller's credential scope can name, including the label shapes around an
    // ordinary Region code — the same set the contribute rule is driven over.
    let regions = ["us-east-1", "us-west-2", "ap-southeast-2", "a", "x1-2-3"];
    let mut announced: BTreeSet<String> = BTreeSet::new();
    // Each channel beside the playback host it announced, so the rows below can delete one and
    // ask what its Region's name resolves to afterwards.
    let mut created_channels: Vec<(String, String)> = Vec::new();

    for region in regions {
        let case = format!("a channel in {region}");
        let created = ivs
            .call_as(
                "CreateChannel",
                &json!({ "name": "resolved" }),
                Some("127.0.0.1:9297"),
                &authorization_in(region),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateChannel must answer HTTP 200, got {} {}",
            created.status, created.body
        );
        let channel = &created.body["channel"];
        let arn = text(channel, "arn");
        let url = text(channel, "playbackUrl");
        let host = assert_announced_playback(
            &case,
            "CreateChannel $.channel.playbackUrl",
            &url,
            "localhost",
            &arn,
        );

        // The name itself resolves to THIS service, both as the URL spells it and with the
        // port its scheme names written out, which is the authority a client that normalises
        // the URL dials.
        for (spelling, authority) in [
            ("the announced name", host.clone()),
            (
                "the name on the port its scheme names",
                format!("{host}:{PLAYBACK_PORT}"),
            ),
        ] {
            let answering = published.answering_service(&authority);
            assert_eq!(
                answering.as_deref(),
                Some("ivs"),
                "case {case:?}: {spelling} ({authority}) must resolve to this service, got \
                 {answering:?}"
            );
        }

        // And the URL as announced is this emulator's to answer: the plane claims the path
        // the URL carries and reports that nothing is packaged for a channel no publisher has
        // reached yet, with no body of its own.
        let fetched = ivs.fetch(&url).await;
        assert_eq!(
            fetched.status, 404,
            "case {case:?}: the announced URL {url} must be answered by this service's \
             playback plane, got {} {:?}",
            fetched.status, fetched.body
        );
        assert!(
            fetched.body.is_empty(),
            "case {case:?}: a channel nothing publishes to serves no playlist, got {:?}",
            fetched.body
        );
        announced.insert(host.clone());
        created_channels.push((arn, host));
    }

    assert_eq!(
        announced.len(),
        regions.len(),
        "each Region's channels are played back from that Region's own edge, got {announced:?}"
    );

    // A create that is REFUSED announces no channel, so it publishes no name either: the
    // records follow the channels this store persisted, not the calls it was sent. The
    // authority a successful create in this Region WOULD have published is composed from the
    // label a successful create in another Region announced, because the label is the
    // account's and the Region is a label of its own.
    let refused_region = "eu-west-1";
    let minted_label = announced
        .iter()
        .next()
        .and_then(|host| host.split_once('.'))
        .map(|(label, _)| label.to_string())
        .expect("a created channel announced a playback host");
    let refused = ivs
        .call_as(
            "CreateChannel",
            // `containerFormat` must be FRAGMENTED_MP4 when multitrack input is enabled
            // (`Channel$containerFormat`, `smithy_query(query="shape",shape="Channel")`), so
            // this request is refused before anything is persisted.
            &json!({ "name": "refused", "containerFormat": "TS",
                     "multitrackInputConfiguration": { "enabled": true } }),
            Some("127.0.0.1:9297"),
            &authorization_in(refused_region),
        )
        .await;
    assert_eq!(
        refused.status, 400,
        "case \"a refused create\": CreateChannel must refuse that pair, got {} {}",
        refused.status, refused.body
    );

    // A name of the right FORM that this emulator announced for nothing resolves to nothing:
    // the register answers for the addresses this deployment really published and not for
    // every name under its own zone.
    for (case, authority) in [
        (
            "the name a refused create would have published",
            format!(
                "{minted_label}.{refused_region}.{PLAYBACK_DOMAIN_LABEL}.\
                 {SERVICE_DOMAIN_LABEL}.localhost"
            ),
        ),
        (
            "a playback name of a Region no channel was created in",
            format!("ffffffffffff.eu-central-1.{PLAYBACK_DOMAIN_LABEL}.{SERVICE_DOMAIN_LABEL}.localhost"),
        ),
        (
            "a label nothing was minted for",
            format!("000000000000.us-east-1.{PLAYBACK_DOMAIN_LABEL}.{SERVICE_DOMAIN_LABEL}.localhost"),
        ),
    ] {
        let answering = published.answering_service(&authority);
        assert!(
            answering.is_none(),
            "case {case:?}: {authority} must resolve to nothing, got {answering:?}"
        );
    }

    // And the records follow the channels the store HOLDS, not the ones it once held: the
    // Region of a deleted channel announces nothing for the next caller, while every Region
    // that still holds one keeps resolving.
    let (deleted_arn, deleted_host) = created_channels
        .first()
        .cloned()
        .expect("a created channel announced a playback host");
    let deleted = ivs
        .call_as(
            "DeleteChannel",
            &json!({ "arn": deleted_arn }),
            Some("127.0.0.1:9297"),
            AUTHORIZATION,
        )
        .await;
    assert_eq!(
        deleted.status, 204,
        "case \"a deleted channel\": DeleteChannel must answer HTTP 204, got {} {}",
        deleted.status, deleted.body
    );
    for (case, authority) in [
        ("the deleted channel's Region", deleted_host.clone()),
        (
            "the same name on the port its scheme names",
            format!("{deleted_host}:{PLAYBACK_PORT}"),
        ),
    ] {
        let answering = published.answering_service(&authority);
        assert!(
            answering.is_none(),
            "case {case:?}: {authority} must resolve to nothing once no channel lives there, \
             got {answering:?}"
        );
    }
    for (arn, host) in created_channels.iter().skip(1) {
        let answering = published.answering_service(host);
        assert_eq!(
            answering.as_deref(),
            Some("ivs"),
            "case {arn:?}: a Region that still holds a channel keeps resolving {host}, got \
             {answering:?}"
        );
    }
}
