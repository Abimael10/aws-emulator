// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the two ADDRESSES every channel this service reports announces,
//! driven over the wire through `MultiServiceRouter::dispatch` on the harness
//! `channel_lifecycle` owns.
//!
//! The rules under test, each stated once and then exercised over a SET of inputs:
//!
//! 1. A member this API fills with an address a client DIALS is announced as an address
//!    this emulator ANSWERS that protocol on. So a channel's `ingestEndpoint`, and the
//!    `srt.endpoint` that equals it, is the `host:port` of the socket this deployment
//!    serves RTMP and RTMPS contributions on — a host of this machine's own and a port it
//!    really listens on, carrying no scheme and no path — which is what lets a client point
//!    an encoder at `rtmps://<ingestEndpoint>/app/<streamKey>` and have the publish arrive.
//! 2. A member that carries a URL the client FETCHES keeps AWS's `https` scheme and the
//!    playlist path `/api/video/v1/<region>.<account>.channel.<id>.m3u8`, with the Region,
//!    the account and the channel id read off the channel's own ARN, and is rooted on the
//!    `host[:port]` the caller reached this emulator at — a loopback address written as the
//!    name that resolves to that interface, so the host is a hostname and the URL is still
//!    dialed where the caller reached it.
//! 3. The ZONE and the address both come from the CALLER's own addressing: the loopback
//!    zone for a caller that reached an address, the reached name for a caller that reached
//!    one of this emulator's own names — never a zone AWS serves its own names in.
//! 4. Every projection of one channel agrees: the create, the single read, the batch read,
//!    the update and the live stream read off that channel all announce the same addresses
//!    for the same caller, and a read announces its own caller's addresses without changing
//!    what a later caller is told.
//! 5. The contribute server is the ACCOUNT's, not a channel's: every channel of the account
//!    reports the SAME `ingestEndpoint`, whatever Region its credential scope names and
//!    whatever the channel's own id is, and a read or an update does not move it.

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

/// The host of an authority, without its `:port` (an IPv6 literal keeps its brackets,
/// where its own colons live).
fn host_only(authority: &str) -> String {
    match authority.find(']') {
        Some(end) => authority[..=end].to_string(),
        None => authority
            .split(':')
            .next()
            .unwrap_or(authority)
            .to_string(),
    }
}

/// Rule 1's form, on the announced contribute ADDRESS: a `host:port` carrying no scheme,
/// no path and no userinfo, whose host is one of this machine's own spellings and whose
/// port is one this process really listens on — so an encoder can dial it.
pub(crate) fn assert_dialable_address(case: &str, member: &str, authority: &str) {
    assert!(
        !authority.is_empty(),
        "case {case:?}: {member} must be announced, got {authority:?}"
    );
    for forbidden in ["://", "/", "@", " "] {
        assert!(
            !authority.contains(forbidden),
            "case {case:?}: {member} must be a bare address, carrying no {forbidden:?} (no \
             scheme, no path), got {authority:?}"
        );
    }
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: {member} must carry a port, got {authority:?}"));
    let port: u16 = port.parse().unwrap_or_else(|error| {
        panic!("case {case:?}: {member} must carry a port, got {authority:?} ({error})")
    });
    assert!(
        port > 0,
        "case {case:?}: {member} must carry the port this emulator listens on, got \
         {authority:?}"
    );
    assert!(
        names_this_machine(host),
        "case {case:?}: {member} must name a host this emulator answers on, got {authority:?}"
    );
}

/// Whether `host` is one of this machine's own spellings: a loopback literal, the loopback
/// name RFC 6761 reserves for that interface, or a name under it.
pub(crate) fn names_this_machine(host: &str) -> bool {
    let named = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(address) = named.parse::<std::net::IpAddr>() {
        return address.is_loopback();
    }
    named == "localhost" || named.ends_with(".localhost")
}

/// The port an announced `host:port` names, which is the port this emulator listens on for
/// that plane.
pub(crate) fn announced_port(case: &str, authority: &str) -> u16 {
    authority
        .rsplit_once(':')
        .and_then(|(_host, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: {authority:?} must carry a port"))
}

/// Every label of an announced host is a DNS label, and no announced host is a foreign
/// domain's.
fn assert_dns_labels(case: &str, member: &str, host: &str) {
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
}

/// Rules 1 and 5 on a channel's INGEST address: the account's contribute socket, with the
/// `srt.endpoint` beside it the same value — one address serves both protocols. Answers the
/// value, so a caller can compare it across projections.
pub(crate) fn assert_announced_ingest(case: &str, member: &str, channel: &Value) -> String {
    let ingest = text(channel, "ingestEndpoint");
    assert_dialable_address(case, &format!("{member} ingestEndpoint"), &ingest);
    assert_eq!(
        text(&channel["srt"], "endpoint"),
        ingest,
        "case {case:?}: {member} srt.endpoint must be the channel's ingest endpoint, got {}",
        channel["srt"]
    );
    ingest
}

/// Rules 2 and 3 on one announced playback URL: AWS's scheme and playlist path, the
/// channel's own identity in the path, and the host the caller reached this emulator at —
/// written as a name, never as an address literal.
fn assert_announced_playback(case: &str, member: &str, url: &str, authority: &str, arn: &str) {
    let (region, account, id) = channel_of_arn(arn);
    assert_eq!(
        url,
        format!(
            "https://{authority}{PLAYBACK_PATH_PREFIX}/{region}.{account}.channel.{id}\
             {PLAYBACK_PATH_SUFFIX}"
        ),
        "case {case:?}: {member} must be the playlist URL on the address this caller \
         reached the emulator at ({authority:?}), got {url:?}"
    );
    let (host, path) = split_url(url);
    assert_eq!(
        host, authority,
        "case {case:?}: {member} must name the address this caller reached the emulator \
         at, got host {host:?} in {url:?}"
    );
    let named = host_only(&host);
    assert!(
        named.parse::<std::net::IpAddr>().is_err() && !named.starts_with('['),
        "case {case:?}: {member} must name the host, not an address literal, got {host:?} \
         in {url:?}"
    );
    assert_dns_labels(case, &format!("{member} host"), &named);
    assert!(
        path.starts_with(PLAYBACK_PATH_PREFIX) && path.ends_with(PLAYBACK_PATH_SUFFIX),
        "case {case:?}: {member} must carry the playlist path AWS lays out, got {path:?}"
    );
}

/// One addressing a client reaches this emulator through: the base the emulator is served
/// at, the `Host` the client sent, the zone every name announced to it sits in, and the
/// `host[:port]` every URL announced to it is rooted on.
struct Addressing {
    case: &'static str,
    /// The base `OperationContext::endpoint` reports, or `None` for an emulator served on
    /// no port at all (the harness default).
    served_at: Option<&'static str>,
    /// The host the client addressed, or `None` for a client that names only this
    /// service's published endpoint on the request URI.
    host: Option<&'static str>,
    /// The `host[:port]` every URL announced to that client must be rooted on — whose host
    /// is the zone that client's own addressing puts this emulator in.
    authority: &'static str,
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
            // `smithy_server_runtime::UNSERVED_URL_BASE`: the one base an emulator that
            // is served on no port reports, and still this emulator's own.
            authority: "emulator.invalid",
        },
        Addressing {
            case: "a client on the loopback address",
            served_at: None,
            host: Some("127.0.0.1:9297"),
            // An ADDRESS names no zone, and the zone a resolver answers with the
            // interface it names is the loopback zone RFC 6761 reserves — which is also
            // how that address is spelled inside a URL.
            authority: "localhost:9297",
        },
        Addressing {
            case: "a client naming the loopback by name, on another port",
            served_at: None,
            host: Some("localhost:40305"),
            authority: "localhost:40305",
        },
        Addressing {
            case: "a client on an IPv6 literal",
            served_at: None,
            host: Some("[::1]:9297"),
            authority: "localhost:9297",
        },
        Addressing {
            case: "a client that resolves AWS's own hosts, on an emulator served on a port",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("ivs.us-east-1.amazonaws.com"),
            authority: "localhost:40305",
        },
        Addressing {
            case: "an SDK client on an emulator served at a name",
            served_at: Some("https://emulator.test:8443"),
            host: None,
            authority: "emulator.test:8443",
        },
        Addressing {
            case: "a client on the loopback address of an emulator served elsewhere",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("localhost:9297"),
            authority: "localhost:9297",
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
/// the one contribute address this deployment answers RTMP on and the playlist URL on the
/// address that caller reached this emulator at.
#[tokio::test]
async fn every_channel_projection_announces_both_addresses_for_its_own_caller() {
    for addressing in addressings() {
        let Addressing {
            case,
            served_at,
            host,
            authority,
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
        assert_announced_playback(
            case,
            "CreateChannel $.channel.playbackUrl",
            &text(channel, "playbackUrl"),
            authority,
            &arn,
        );
        let ingest = assert_announced_ingest(case, "CreateChannel $.channel", channel);

        // Rule 4: every other projection of the same channel agrees, for the same caller.
        let read = call("GetChannel", json!({ "arn": arn })).await;
        assert_announced_playback(
            case,
            "GetChannel $.channel.playbackUrl",
            &text(&read["channel"], "playbackUrl"),
            authority,
            &arn,
        );
        assert_announced_ingest(case, "GetChannel $.channel", &read["channel"]);
        let updated = call("UpdateChannel", json!({ "arn": arn, "name": "renamed" })).await;
        assert_announced_playback(
            case,
            "UpdateChannel $.channel.playbackUrl",
            &text(&updated["channel"], "playbackUrl"),
            authority,
            &arn,
        );
        assert_announced_ingest(case, "UpdateChannel $.channel", &updated["channel"]);
        let batch = call("BatchGetChannel", json!({ "arns": [arn] })).await;
        let batched = &batch["channels"][0];
        assert_announced_playback(
            case,
            "BatchGetChannel $.channels[0].playbackUrl",
            &text(batched, "playbackUrl"),
            authority,
            &arn,
        );
        assert_announced_ingest(case, "BatchGetChannel $.channels[0]", batched);

        // The live stream is played back from its channel's own URL, so the same rule
        // holds for the stream a broadcasting channel reports.
        ivs.with_store_mut(|store| state::start_stream(store, &arn));
        let stream = call("GetStream", json!({ "channelArn": arn })).await;
        assert_announced_playback(
            case,
            "GetStream $.stream.playbackUrl",
            &text(&stream["stream"], "playbackUrl"),
            authority,
            &arn,
        );

        // Rule 5: the contribute server is the account's, so nothing of this channel is in
        // it.
        let (_, _, channel_id) = channel_of_arn(&arn);
        assert!(
            !ingest.contains(&channel_id) && !ingest.contains(&arn),
            "case {case:?}: the ingest endpoint must carry no part of the channel's own \
             identity, got {ingest:?} for channel {channel_id:?}"
        );
    }
}

/// Rule 1's point: the announced contribute address is DIALED, so a client that takes the
/// value a channel reports and opens a connection to it reaches this emulator.
///
/// Rows differ along how the caller read the value: an SDK client naming this service's
/// endpoint, a client on the loopback address, and a client that resolves AWS's own hosts —
/// none of which changes the socket the contribution arrives on. What a publisher then
/// SPEAKS on that socket is `live_stream_reads.rs`'s subject.
#[tokio::test]
async fn the_announced_contribute_address_accepts_a_connection() {
    let ivs = Ivs::new();
    let callers = [
        ("an SDK client naming this service's endpoint", None),
        ("a client on the loopback address", Some("127.0.0.1:9297")),
        (
            "a client that resolves AWS's own hosts",
            Some("ivs.us-east-1.amazonaws.com"),
        ),
    ];
    let mut announced: BTreeSet<String> = BTreeSet::new();
    for (case, host) in callers {
        let created = ivs
            .call_as("CreateChannel", &json!({ "name": "dialed" }), host, AUTHORIZATION)
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateChannel must answer HTTP 200, got {} {}",
            created.status, created.body
        );
        let ingest = assert_announced_ingest(case, "CreateChannel $.channel", &created.body["channel"]);
        // The announced host is one of this machine's own spellings (asserted above), so
        // the publisher's connection is this emulator's own loopback port.
        let port = announced_port(case, &ingest);
        let dialled = ::tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await;
        assert!(
            dialled.is_ok(),
            "case {case:?}: the announced ingest endpoint {ingest:?} must accept a \
             connection, got {:?}",
            dialled.err()
        );
        announced.insert(ingest);
    }
    assert_eq!(
        announced.len(),
        1,
        "every caller is told the one contribute address this deployment serves, got \
         {announced:?}"
    );
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
    let addresses = [
        ("127.0.0.1:9297", "localhost:9297"),
        ("localhost:40305", "localhost:40305"),
        ("[::1]:8443", "localhost:8443"),
    ];
    for round in 1..=2 {
        for (host, authority) in addresses {
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
                authority,
                &arn,
            );
            assert_announced_ingest(&case, "GetChannel $.channel", &read.body["channel"]);
        }
    }
}

/// Rule 5: the ingest server is the ACCOUNT's, not a channel's — one contribute address for
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
            let ingest = assert_announced_ingest(&case, "CreateChannel $.channel", channel);

            // The ingest server is not the channel: nothing of the channel's own identity
            // is in the address it contributes to.
            let (_, _, channel_id) = channel_of_arn(&arn);
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
        "every channel of this account ingests on ONE contribute address, whatever Region \
         its caller named, got {announced:?}"
    );
}
