// SPDX-License-Identifier: CC0-1.0

//! WHERE a channel's outputs are announced for PLAYBACK — the `PlaybackUrl` /
//! `DualStackPlaybackUrl` pair every output of every channel this service reports carries.
//!
//! RULE under test: those two members are addresses a player DIALS afterwards
//! (`PlaybackUrl` is "The URL that your player uses for playback" and
//! `DualStackPlaybackUrl` "The dual-stack (IPv4 and IPv6) URL that your player uses for
//! playback",
//! `smithy_query(query="shape",shape="com.amazonaws.mediatailor#ResponseOutputItem",model_path="services/service-com-amazonaws-mediatailor-mediatailor/smithy-model/com-amazonaws-mediatailor-mediatailor.json")`),
//! so each names a host that serves the channel's manifests: the endpoint the creating
//! request reached when this emulator has an own address, and otherwise the host AWS
//! itself publishes channel-assembly playback on — the channel's generated label, the
//! account's channel-assembly subdomain label, then the fixed label sequence
//! `channel-assembly.mediatailor.<region>` and the partition DNS suffix (`amazonaws.com`
//! single-stack, `api.aws` dual-stack). Neither is ever the service's own control-plane
//! API host, which answers the control-plane routes only. The path carries the whole
//! identity of the output (`/v1/{channel,dash}/<ChannelName>/<ManifestName>.{m3u8,mpd}`),
//! and because the pair is minted once and persisted, every read of the channel reports it
//! unchanged.
//!
//! The rows differ along what the rule generalises over: the shape of the emulator's own
//! address and where the request carries it, whether the emulator is served on a port at
//! all, the Region the caller signed for, the playlist form of the output (which decides
//! the manifest it ends in), how many outputs one channel has, and the form of the channel
//! and manifest names.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

/// The fixed labels of the host AWS publishes a channel's playback on, in order, between
/// the generated labels and the partition DNS suffix. Source: the channel-assembly
/// playback host of a recorded `DescribeChannel` response,
/// `<32-hex>.<subdomain>.channel-assembly.mediatailor.<region>.amazonaws.com`.
const CHANNEL_ASSEMBLY_LABELS: [&str; 2] = ["channel-assembly", "mediatailor"];

/// The partition DNS suffix of a single-stack AWS endpoint, and of its dual-stack
/// counterpart, each written as the labels it is made of.
const SINGLE_STACK_SUFFIX: [&str; 2] = ["amazonaws", "com"];
const DUAL_STACK_SUFFIX: [&str; 2] = ["api", "aws"];

/// How long the two generated labels of that host are: the per-channel label and the
/// account's channel-assembly subdomain label.
const CHANNEL_LABEL_CHARS: usize = 32;
const SUBDOMAIN_LABEL_CHARS: usize = 6;

/// One address a client reaches this emulator at: the authority its request URI names and
/// the `Host` header it carries (`None` sends none at all, as an absolute-form request
/// does), plus the authority the announced URLs must then name.
struct Addressing {
    case: &'static str,
    authority: &'static str,
    host_header: Option<&'static str>,
    announced: &'static str,
}

/// The addresses a client on the machine this emulator runs on reaches it at. Each is an
/// address or a loopback name rather than a service hostname, so each is the emulator's
/// own endpoint.
const OWN_ENDPOINTS: &[Addressing] = &[
    Addressing {
        case: "an IPv4 literal with the data port",
        authority: "127.0.0.1:9297",
        host_header: Some("127.0.0.1:9297"),
        announced: "127.0.0.1:9297",
    },
    Addressing {
        case: "an IPv4 literal on the default port",
        authority: "127.0.0.1",
        host_header: Some("127.0.0.1"),
        announced: "127.0.0.1",
    },
    Addressing {
        case: "a loopback name",
        authority: "localhost:9297",
        host_header: Some("localhost:9297"),
        announced: "localhost:9297",
    },
    Addressing {
        case: "a bracketed IPv6 literal",
        authority: "[::1]:9297",
        host_header: Some("[::1]:9297"),
        announced: "[::1]:9297",
    },
    Addressing {
        case: "no Host header, the endpoint on an absolute-form URI",
        authority: "127.0.0.1:39355",
        host_header: None,
        announced: "127.0.0.1:39355",
    },
];

/// A router carrying exactly this service over one shared engine — the assembly
/// `core/router` performs, minus the other services. Served on no port at all until a test
/// gives it a base, which is the case that announces AWS's own hostname.
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_mediatailor_mediatailor::EmulatedService::new(Arc::new(
            engine::InMemoryEngine::new(),
        )),
    );
    router
}

/// Send one request to an ADDRESS the caller chooses, signed for `region` — the SigV4
/// credential scope is what routes the request and what the handler reads the Region from.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    authority: &str,
    host_header: Option<&str>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{authority}{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{region}\
                 /mediatailor/aws4_request, SignedHeaders=host, Signature=unverified"
            ),
        );
    if let Some(host) = host_header {
        builder = builder.header("host", host);
    }
    let request = builder
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// This service's own regional API hostname, which is what an SDK client that resolves
/// AWS's names addresses — a host that names a service, and so never the emulator's own
/// endpoint.
fn api_host(region: &str) -> String {
    format!("mediatailor.{region}.amazonaws.com")
}

/// `CreateChannel` through one addressing, asserting it succeeded, and the projection it
/// answered with.
async fn create_channel(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    request: &Value,
    authority: &str,
    host_header: Option<&str>,
    case: &str,
) -> Value {
    let (status, body) = call(
        router,
        region,
        "POST",
        &format!("/channel/{name}"),
        Some(request.clone()),
        authority,
        host_header,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateChannel expected 200, got {status} {body}"
    );
    body
}

/// `DescribeChannel` through the service's own API hostname, which is how a read reaches
/// the stored channel in these tests.
async fn describe_channel(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    case: &str,
) -> Value {
    let host = api_host(region);
    let (status, body) = call(
        router,
        region,
        "GET",
        &format!("/channel/{name}"),
        None,
        &host,
        Some(&host),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeChannel expected 200, got {status} {body}"
    );
    body
}

/// The `(PlaybackUrl, DualStackPlaybackUrl)` pair of output `index`.
fn urls_of(body: &Value, index: usize, case: &str) -> (String, String) {
    let output = body
        .pointer(&format!("/Outputs/{index}"))
        .unwrap_or_else(|| panic!("case {case:?}: output {index} must be reported, got {body}"));
    let member = |name: &str| {
        output
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: output {index} must report {name}, got {output}")
            })
            .to_string()
    };
    (member("PlaybackUrl"), member("DualStackPlaybackUrl"))
}

/// The playback path one requested output is served at: the playlist form the output asked
/// for decides the manifest it ends in — a DASH-only output an `.mpd` under `dash`, every
/// other output an `.m3u8` under `channel`.
fn expected_path(channel: &str, output: &Value) -> String {
    let manifest = output["ManifestName"].as_str().expect("a manifest name");
    let dash_only =
        output.get("DashPlaylistSettings").is_some() && output.get("HlsPlaylistSettings").is_none();
    if dash_only {
        format!("/v1/dash/{channel}/{manifest}.mpd")
    } else {
        format!("/v1/channel/{channel}/{manifest}.m3u8")
    }
}

/// The authority and the path of an `https` URL, which is what every assertion here is
/// about.
fn split_url(case: &str, member: &str, url: &str) -> (String, String) {
    let rest = url.strip_prefix("https://").unwrap_or_else(|| {
        panic!("case {case:?}: {member} must be fetched over TLS, got {url:?}")
    });
    let (authority, path) = rest
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {member} must carry a path, got {url:?}"));
    (authority.to_string(), format!("/{path}"))
}

/// The two generated labels of a channel-assembly playback host, asserted structurally:
/// the per-channel label, the account's subdomain label, the fixed label sequence, the
/// Region and the partition suffix. The concrete label VALUES are generated per channel
/// and per deployment, so they are never asserted literally — only their form.
fn assert_channel_assembly_host(
    case: &str,
    member: &str,
    url: &str,
    region: &str,
    suffix: [&str; 2],
    path: &str,
) -> (String, String) {
    let (authority, actual_path) = split_url(case, member, url);
    assert_eq!(
        actual_path, path,
        "case {case:?}: {member} must address the output at {path:?}, got {actual_path:?}"
    );
    let labels: Vec<&str> = authority.split('.').collect();
    let mut tail: Vec<&str> = CHANNEL_ASSEMBLY_LABELS.to_vec();
    tail.push(region);
    tail.extend_from_slice(&suffix);
    assert_eq!(
        labels.len(),
        2 + tail.len(),
        "case {case:?}: {member} must sit on two generated labels followed by {tail:?}, \
         got {authority:?}"
    );
    assert_eq!(
        labels[2..].to_vec(),
        tail,
        "case {case:?}: {member} must carry the channel-assembly label sequence {tail:?}, \
         got {authority:?}"
    );
    let channel_label = labels[0];
    assert!(
        channel_label.len() == CHANNEL_LABEL_CHARS
            && channel_label
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "case {case:?}: {member} must name the channel by a {CHANNEL_LABEL_CHARS}-character \
         hexadecimal label, got {channel_label:?}"
    );
    let subdomain = labels[1];
    assert!(
        subdomain.len() == SUBDOMAIN_LABEL_CHARS
            && subdomain
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "case {case:?}: {member} must carry a {SUBDOMAIN_LABEL_CHARS}-character lowercase \
         subdomain label, got {subdomain:?}"
    );
    (channel_label.to_string(), subdomain.to_string())
}

/// The channels these tests span: each row differs in its playback mode, how many outputs
/// it has and in which playlist form, and in the form of its channel and manifest names.
fn table() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "one HLS output",
            "hls-channel",
            json!({
                "PlaybackMode": "LOOP",
                "Outputs": [{
                    "ManifestName": "default",
                    "SourceGroup": "default",
                    "HlsPlaylistSettings": { "ManifestWindowSeconds": 30 },
                }],
            }),
        ),
        (
            "one DASH-only output, whose manifest is an .mpd",
            "dash.only_1",
            json!({
                "PlaybackMode": "LOOP",
                "Outputs": [{
                    "ManifestName": "manifest",
                    "SourceGroup": "group-b",
                    "DashPlaylistSettings": { "ManifestWindowSeconds": 60 },
                }],
            }),
        ),
        (
            "three outputs: HLS, DASH and one carrying both playlist forms",
            "m",
            json!({
                "PlaybackMode": "LINEAR",
                "FillerSlate": {
                    "SourceLocationName": "slate-location",
                    "VodSourceName": "slate-source",
                },
                "Outputs": [
                    {
                        "ManifestName": "hls",
                        "SourceGroup": "group-a",
                        "HlsPlaylistSettings": { "ManifestWindowSeconds": 30 },
                    },
                    {
                        "ManifestName": "dash",
                        "SourceGroup": "group-a",
                        "DashPlaylistSettings": { "ManifestWindowSeconds": 30 },
                    },
                    {
                        "ManifestName": "both.forms-1",
                        "SourceGroup": "group-c",
                        "HlsPlaylistSettings": { "ManifestWindowSeconds": 30 },
                        "DashPlaylistSettings": { "ManifestWindowSeconds": 30 },
                    },
                ],
            }),
        ),
    ]
}

/// RULE — a channel created through the emulator's OWN endpoint announces every output on
/// that same address, whatever shape it took and wherever the request carried it, and
/// never on a host AWS publishes endpoints at; the read reports the stored pair unchanged.
#[tokio::test]
async fn every_output_is_announced_at_the_endpoint_the_create_reached() {
    let region = "us-west-2";
    for addressing in OWN_ENDPOINTS {
        for (row, name, request) in table() {
            let case = format!("{} / {row}", addressing.case);
            let router = router();
            let created = create_channel(
                &router,
                region,
                name,
                &request,
                addressing.authority,
                addressing.host_header,
                &case,
            )
            .await;
            let described = describe_channel(&router, region, name, &case).await;

            let outputs = request["Outputs"].as_array().expect("the row lists outputs");
            for (index, output) in outputs.iter().enumerate() {
                let path = expected_path(name, output);
                let expected = format!("https://{}{path}", addressing.announced);
                let (playback, dual_stack) = urls_of(&created, index, &case);
                for (member, actual) in [
                    ("PlaybackUrl", &playback),
                    ("DualStackPlaybackUrl", &dual_stack),
                ] {
                    assert_eq!(
                        actual, &expected,
                        "case {case:?}: output {index} {member} must be announced at the \
                         endpoint the create reached, expected {expected:?}, got {actual:?}"
                    );
                    // The point of the rule: the announced host is not one AWS publishes
                    // endpoints at, because this process does not answer on those names.
                    for suffix in [".amazonaws.com", ".api.aws", ".on.aws"] {
                        let (authority, _) = split_url(&case, member, actual);
                        assert!(
                            !authority.ends_with(suffix),
                            "case {case:?}: output {index} {member} must not name an AWS \
                             endpoint host, got {authority:?}"
                        );
                    }
                }
                let (read_playback, read_dual_stack) = urls_of(&described, index, &case);
                assert_eq!(
                    (read_playback.as_str(), read_dual_stack.as_str()),
                    (playback.as_str(), dual_stack.as_str()),
                    "case {case:?}: DescribeChannel must report output {index}'s stored \
                     playback URLs unchanged, got {read_playback:?} and {read_dual_stack:?}"
                );
            }
        }
    }
}

/// RULE — a caller that addressed one of AWS's own hostnames is announced the port this
/// emulator is served on, which is the one address this process answers on.
#[tokio::test]
async fn a_caller_on_an_aws_hostname_is_announced_the_served_port() {
    let region = "eu-central-1";
    // (case, the base URL `OperationContext::endpoint` reports, the announced authority)
    let cases = [
        ("served on the data port", "https://127.0.0.1:9297", "127.0.0.1:9297"),
        ("served on another port", "https://[::1]:443", "[::1]:443"),
    ];
    for (case, base, announced) in cases {
        let mut router = router();
        router.set_url_base(
            EndpointUrl::parse(base).unwrap_or_else(|| panic!("case {case:?}: {base} parses")),
        );
        let host = api_host(region);
        let name = "served-port";
        let request = json!({
            "PlaybackMode": "LOOP",
            "Outputs": [{ "ManifestName": "default", "SourceGroup": "default" }],
        });
        let created =
            create_channel(&router, region, name, &request, &host, Some(&host), case).await;
        let path = expected_path(name, &request["Outputs"][0]);
        let expected = format!("https://{announced}{path}");
        let (playback, dual_stack) = urls_of(&created, 0, case);
        for (member, actual) in [
            ("PlaybackUrl", &playback),
            ("DualStackPlaybackUrl", &dual_stack),
        ] {
            assert_eq!(
                actual, &expected,
                "case {case:?}: {member} must be announced on the port this emulator is \
                 served on, expected {expected:?}, got {actual:?}"
            );
        }
    }
}

/// RULE — an emulator served on no port at all has no own address to announce, so a caller
/// that resolves AWS's own hostnames is told the host AWS itself publishes channel-assembly
/// playback on: the generated labels, the fixed `channel-assembly.mediatailor.<region>`
/// sequence and the partition suffix, single-stack and dual-stack. Every output of one
/// channel shares that host and differs only in its path, and the read reports the stored
/// pair unchanged.
#[tokio::test]
async fn an_unserved_emulator_announces_the_channel_assembly_playback_host() {
    // The Region is a variable part of the host, so each row signs for another one,
    // including one outside the Region the recording was taken in.
    let regions = ["us-west-2", "us-east-1", "ap-southeast-2"];
    for region in regions {
        let router = router();
        let mut channel_labels: BTreeSet<String> = BTreeSet::new();
        let mut subdomains: BTreeSet<String> = BTreeSet::new();
        let mut urls: BTreeSet<String> = BTreeSet::new();

        for (row, name, request) in table() {
            let case = format!("{region} / {row}");
            let host = api_host(region);
            let created =
                create_channel(&router, region, name, &request, &host, Some(&host), &case).await;
            let described = describe_channel(&router, region, name, &case).await;

            let outputs = request["Outputs"].as_array().expect("the row lists outputs");
            let mut hosts_of_this_channel: BTreeSet<String> = BTreeSet::new();
            for (index, output) in outputs.iter().enumerate() {
                let path = expected_path(name, output);
                let (playback, dual_stack) = urls_of(&created, index, &case);

                let (channel_label, subdomain) = assert_channel_assembly_host(
                    &case,
                    "PlaybackUrl",
                    &playback,
                    region,
                    SINGLE_STACK_SUFFIX,
                    &path,
                );
                let (dual_label, dual_subdomain) = assert_channel_assembly_host(
                    &case,
                    "DualStackPlaybackUrl",
                    &dual_stack,
                    region,
                    DUAL_STACK_SUFFIX,
                    &path,
                );
                // One channel, one pair of hosts: the dual-stack URL is the same host
                // under the dual-stack suffix.
                assert_eq!(
                    (dual_label.as_str(), dual_subdomain.as_str()),
                    (channel_label.as_str(), subdomain.as_str()),
                    "case {case:?}: output {index}'s dual-stack host must carry the same \
                     generated labels as the single-stack one, got {dual_label:?} and \
                     {dual_subdomain:?}"
                );
                channel_labels.insert(channel_label.clone());
                subdomains.insert(subdomain);
                hosts_of_this_channel.insert(split_url(&case, "PlaybackUrl", &playback).0);
                assert!(
                    urls.insert(playback.clone()),
                    "case {case:?}: one playback URL addresses one output, but {playback:?} \
                     was announced twice"
                );

                let (read_playback, read_dual_stack) = urls_of(&described, index, &case);
                assert_eq!(
                    (read_playback.as_str(), read_dual_stack.as_str()),
                    (playback.as_str(), dual_stack.as_str()),
                    "case {case:?}: DescribeChannel must report output {index}'s stored \
                     playback URLs unchanged, got {read_playback:?} and {read_dual_stack:?}"
                );
            }
            assert_eq!(
                hosts_of_this_channel.len(),
                1,
                "case {case:?}: every output of one channel is served on that channel's one \
                 host, got {hosts_of_this_channel:?}"
            );
        }

        // The per-channel label names one channel, so the three channels of this Region
        // have three of them; the subdomain label is the account's, so they share one.
        assert_eq!(
            channel_labels.len(),
            table().len(),
            "region {region:?}: each channel must have its own generated label, got \
             {channel_labels:?}"
        );
        assert_eq!(
            subdomains.len(),
            1,
            "region {region:?}: the channel-assembly subdomain label is the account's, so \
             every channel of one Region shares it, got {subdomains:?}"
        );
    }
}
