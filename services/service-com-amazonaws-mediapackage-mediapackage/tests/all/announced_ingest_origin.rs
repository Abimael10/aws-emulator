// SPDX-License-Identifier: CC0-1.0

//! WHERE a channel's HLS ingest endpoints are ANNOUNCED — the address every
//! `hlsIngest.ingestEndpoints[].url` names — and that the address announced is one the
//! ingest plane actually answers the Digest exchange on.
//!
//! RULE under test: an address this service hands a caller to DIAL later names a host
//! this emulator answers on — so a channel's ingest URLs are rooted at the endpoint the
//! creating request reached, and only a caller that resolves AWS's own hostnames, talking
//! to an emulator served on no port at all, is told the hostname AWS itself publishes.
//! `IngestEndpoint$Url` is "The ingest URL to which the source stream should be sent"
//! (`smithy_query(query="shape",shape="com.amazonaws.mediapackage#IngestEndpoint",model_path="services/service-com-amazonaws-mediapackage-mediapackage/smithy-model/com-amazonaws-mediapackage-mediapackage.json")`),
//! and an encoder pushing to it is driven entirely by the `WWW-Authenticate` challenge the
//! listener behind it answers an unauthenticated `PUT` with: announced at a host this
//! process does not answer on, the URL yields no challenge, so no Digest response can be
//! computed and nothing can ever be ingested.
//!
//! The rows differ along what the rule generalises over: the shape of the emulator's own
//! address a client reaches it at (an IPv4 literal with and without a port, a loopback
//! name, a bracketed IPv6 literal), where the request carries that address (the `Host`
//! header or an absolute-form URI), whether the emulator is served on a port at all, and
//! which endpoint of the redundant ingest pair is being read or pushed to. The channel
//! identity is varied too — one channel per row — because the announced address is a
//! function of the endpoint reached rather than of the channel.
//!
//! Every operation that projects a Channel is asserted, since each puts the member on the
//! wire on its own: `CreateChannel`, `DescribeChannel`, `ListChannels` (per element),
//! `UpdateChannel`, `ConfigureLogs`, `RotateChannelCredentials` and
//! `RotateIngestEndpointCredentials`.

use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use service_com_amazonaws_mediapackage_mediapackage::state;

use super::data_plane::{
    announced_origin as origin, challenge_parameter, control, control_addressed,
    digest_authorization, emulator, path_of, plane, Ingest, Reached, OWN_ENDPOINTS, REGION,
};

/// The ingest endpoints a projection reports, as `(url, username, password)` triples.
fn ingests_of(body: &Value) -> Vec<Ingest> {
    body.pointer("/hlsIngest/ingestEndpoints")
        .and_then(Value::as_array)
        .map(|endpoints| {
            endpoints
                .iter()
                .map(|endpoint| {
                    let text = |member: &str| {
                        endpoint
                            .get(member)
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    Ingest {
                        url: text("url"),
                        username: text("username"),
                        password: text("password"),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ingest URLs a projection reports.
fn urls_of(body: &Value) -> Vec<String> {
    ingests_of(body)
        .into_iter()
        .map(|ingest| ingest.url)
        .collect()
}

/// Create a channel through one addressing, asserting it succeeded, and report the
/// projection the create answered with.
async fn create_channel_at(router: &MultiServiceRouter, addressing: &Reached, id: &str) -> Value {
    let (status, created) = control_addressed(
        router,
        "POST",
        "/channels",
        Some(&json!({ "id": id })),
        addressing.authority,
        addressing.host_header,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: CreateChannel expected 200, got {status} {created}",
        addressing.case
    );
    created
}

/// Every projection of the channel `id` beyond the create that carries ingest endpoints,
/// as `(the operation, its ingest URLs)`: the four reads and writes and the two credential
/// rotations that each put the member on the wire.
async fn projected_urls(
    router: &MultiServiceRouter,
    id: &str,
    rotated_endpoint_id: &str,
) -> Vec<(&'static str, Vec<String>)> {
    let path = format!("/channels/{id}");

    let (status, described) = control(router, "GET", &path, None).await;
    assert_eq!(status, 200, "DescribeChannel {id}: body {described}");

    let (status, listed) = control(router, "GET", "/channels", None).await;
    assert_eq!(status, 200, "ListChannels: body {listed}");
    let element = listed
        .get("channels")
        .and_then(Value::as_array)
        .and_then(|channels| {
            channels
                .iter()
                .find(|channel| channel.get("id").and_then(Value::as_str) == Some(id))
                .cloned()
        })
        .unwrap_or_else(|| panic!("ListChannels must report {id}, got {listed}"));

    let (status, updated) = control(
        router,
        "PUT",
        &path,
        Some(&json!({ "description": "re-read" })),
    )
    .await;
    assert_eq!(status, 200, "UpdateChannel {id}: body {updated}");

    let (status, configured) = control(
        router,
        "PUT",
        &format!("{path}/configure_logs"),
        Some(&json!({ "egressAccessLogs": { "logGroupName": "/aws/MediaPackage/egress" } })),
    )
    .await;
    assert_eq!(status, 200, "ConfigureLogs {id}: body {configured}");

    let (status, rotated) = control(router, "PUT", &format!("{path}/credentials"), None).await;
    assert_eq!(
        status, 200,
        "RotateChannelCredentials {id}: body {rotated}"
    );

    let (status, rotated_one) = control(
        router,
        "PUT",
        &format!("{path}/ingest_endpoints/{rotated_endpoint_id}/credentials"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "RotateIngestEndpointCredentials {id}: body {rotated_one}"
    );

    vec![
        ("DescribeChannel", urls_of(&described)),
        ("ListChannels", urls_of(&element)),
        ("UpdateChannel", urls_of(&updated)),
        ("ConfigureLogs", urls_of(&configured)),
        ("RotateChannelCredentials", urls_of(&rotated)),
        ("RotateIngestEndpointCredentials", urls_of(&rotated_one)),
    ]
}

/// The id of the ingest endpoint at `position` of a projection's redundant pair.
fn endpoint_id(body: &Value, position: usize) -> String {
    body.pointer("/hlsIngest/ingestEndpoints")
        .and_then(Value::as_array)
        .and_then(|endpoints| endpoints.get(position))
        .and_then(|endpoint| endpoint.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("an ingest endpoint must report its id, got {body}"))
        .to_string()
}

/// Assert that `url` is an ingest URL announced under `expected_origin`, keeping the ingest
/// path this service publishes below it: `/in/v2/<channel label>/<endpoint id>/channel`,
/// each label 32 lowercase-hex characters.
fn assert_ingest_url(case: &str, url: &str, expected_origin: &str) {
    assert!(
        url.starts_with(&format!("{expected_origin}/")),
        "case {case:?}: an ingest URL must be rooted on {expected_origin:?}, got {url:?}"
    );
    let path = path_of(url);
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    assert_eq!(
        segments.len(),
        5,
        "case {case:?}: expected /in/v2/<channel>/<endpoint>/channel, got {path:?}"
    );
    assert_eq!(
        (segments[0], segments[1], segments[4]),
        ("in", "v2", "channel"),
        "case {case:?}: expected /in/v2/<channel>/<endpoint>/channel, got {path:?}"
    );
    for label in [segments[2], segments[3]] {
        assert!(
            label.len() == 32
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "case {case:?}: {label:?} must be a 32-character lowercase-hex label, in {path:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE — the announced address is the endpoint the creating request reached
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_projection_announces_the_endpoint_the_channel_was_created_through() {
    for (index, addressing) in OWN_ENDPOINTS.iter().enumerate() {
        let case = addressing.case;
        let router = emulator();
        let id = format!("announced-{index}");
        let expected = origin(addressing.announced);

        let created = create_channel_at(&router, addressing, &id).await;
        let published = urls_of(&created);
        assert_eq!(
            published.len(),
            state::INGEST_ENDPOINT_COUNT,
            "case {case:?}: CreateChannel must publish the redundant ingest pair, got {created}"
        );
        for url in &published {
            assert_ingest_url(case, url, &expected);
            // The point of the rule: the announced address is not some host off this
            // process.
            assert!(
                !url.contains("amazonaws.com"),
                "case {case:?}: an ingest URL must not name an AWS host, got {url:?}"
            );
        }
        assert_ne!(
            published[0], published[1],
            "case {case:?}: each endpoint of the pair has its own URL, got {published:?}"
        );

        let rotated = endpoint_id(&created, 0);
        for (operation, urls) in projected_urls(&router, &id, &rotated).await {
            assert_eq!(
                urls, published,
                "case {case:?}: {operation} must re-report the published ingest URLs, \
                 expected {published:?}, got {urls:?}"
            );
        }
    }
}

/// RULE — a caller that addressed one of AWS's own hostnames is announced the port this
/// emulator is served on, which is the one address this process answers on; only an
/// emulator served on no port at all announces the hostname AWS itself publishes, because
/// it has no own address to announce in its place.
#[tokio::test]
async fn a_caller_on_an_aws_hostname_is_announced_the_port_the_emulator_is_served_on() {
    // (case, the base URL `OperationContext::endpoint` reports, the expected origin)
    let served = "127.0.0.1:39355";
    let cases: Vec<(&str, Option<String>, Option<String>)> = vec![
        (
            "served on a data port",
            Some(origin(served)),
            Some(origin(served)),
        ),
        (
            "served on another port",
            Some(origin("[::1]:443")),
            Some(origin("[::1]:443")),
        ),
        // No own address to announce: the hostname AWS itself publishes stands.
        ("served on no port at all", None, None),
    ];

    for (index, (case, base, expected)) in cases.into_iter().enumerate() {
        let mut router = emulator();
        if let Some(base) = &base {
            router.set_url_base(
                EndpointUrl::parse(base).unwrap_or_else(|| panic!("case {case:?}: {base} parses")),
            );
        }
        let id = format!("aws-host-{index}");
        let (status, created) =
            control(&router, "POST", "/channels", Some(&json!({ "id": id }))).await;
        assert_eq!(status, 200, "case {case:?}: create body {created}");

        let published = urls_of(&created);
        for url in &published {
            match &expected {
                // An own address to announce: the URL is rooted on exactly it.
                Some(expected) => assert_ingest_url(case, url, expected),
                // None: the URL is rooted on the host AWS itself publishes, asserted as
                // the STRUCTURE that form has — the TLS scheme, then
                // `<account label>.mediapackage.<region>.amazonaws.com`.
                None => {
                    let path = path_of(url);
                    let announced = url
                        .strip_prefix(state::MEDIA_ORIGIN_PREFIX)
                        .and_then(|rest| rest.strip_suffix(path))
                        .unwrap_or_default();
                    let (label, domain) = announced.split_once('.').unwrap_or_default();
                    assert_eq!(
                        domain,
                        format!("mediapackage.{REGION}.amazonaws.com"),
                        "case {case:?}: expected AWS's own regional host, got {url:?}"
                    );
                    assert_eq!(
                        label.len(),
                        32,
                        "case {case:?}: expected a 32-character account label, got {label:?}"
                    );
                }
            }
        }

        let rotated = endpoint_id(&created, 0);
        for (operation, urls) in projected_urls(&router, &id, &rotated).await {
            assert_eq!(
                urls, published,
                "case {case:?}: {operation} must re-report the published ingest URLs, \
                 expected {published:?}, got {urls:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE — the announced address answers the Digest exchange
// ---------------------------------------------------------------------------

/// RULE — a `PUT` to the ingest URL AS PUBLISHED is answered by that channel's own ingest
/// listener: no credentials is `401` carrying a `WWW-Authenticate: Digest` challenge in the
/// WebDAV realm, a Digest response computed from that challenge with the endpoint's own
/// credentials is accepted (a named object is stored; a write addressed at the collection
/// itself names nothing to store and is the plane's method refusal), and one computed with
/// any other password is `401` again.
///
/// The rows span both endpoints of the redundant pair and the paths below the published
/// URL, because the challenge is a property of the published address rather than of one
/// endpoint or one object name.
#[tokio::test]
async fn the_published_ingest_url_challenges_an_unauthenticated_put_over_digest() {
    for (index, addressing) in OWN_ENDPOINTS.iter().enumerate() {
        let case = addressing.case;
        let router = emulator();
        let created = create_channel_at(&router, addressing, &format!("digest-{index}")).await;
        let ingests = ingests_of(&created);

        // (the object the push addresses below the published collection, and the status an
        // authenticated PUT of it is answered with: a named object is STORED, while a write
        // addressed at the collection itself names nothing to store, which RFC 4918 answers
        // for a PUT to a collection URI with the method refusal the plane states.)
        for (name, accepted) in [("", 405u16), ("index.m3u8", 200), ("segment-1.ts", 200)] {
            for (position, ingest) in ingests.iter().enumerate() {
                let case = format!("{case} at endpoint {position} under {name:?}");
                let url = if name.is_empty() {
                    ingest.url.clone()
                } else {
                    format!("{}/{name}", ingest.url)
                };
                let body = b"#EXTM3U\n#EXT-X-VERSION:3\n";

                let probe = plane(&router, "PUT", &url, &[], body).await;
                assert_eq!(
                    probe.status,
                    401,
                    "case {case:?}: an unauthenticated PUT must be challenged, got {} {}",
                    probe.status,
                    probe.text()
                );
                let challenge = probe.header("www-authenticate").unwrap_or_default();
                // The fixed text of the challenge: the scheme token and the protection
                // space, the algorithm and the quality of protection it advertises.
                assert!(
                    challenge.starts_with("Digest "),
                    "case {case:?}: expected a Digest challenge, got {challenge:?}"
                );
                for (parameter, value) in
                    [("realm", "WebDAV"), ("algorithm", "MD5"), ("qop", "auth")]
                {
                    let answered = challenge_parameter(challenge, parameter);
                    assert_eq!(
                        answered, value,
                        "case {case:?}: challenge {parameter} expected {value:?}, got \
                         {answered:?} in {challenge:?}"
                    );
                }
                assert!(
                    !challenge_parameter(challenge, "nonce").is_empty(),
                    "case {case:?}: the challenge must carry a nonce, got {challenge:?}"
                );

                // The credentials the control plane generated for THIS endpoint, answering
                // the challenge it was just given, are accepted.
                let authorized = plane(
                    &router,
                    "PUT",
                    &url,
                    &[(
                        "authorization",
                        digest_authorization(
                            challenge,
                            "PUT",
                            path_of(&url),
                            &ingest.username,
                            &ingest.password,
                        ),
                    )],
                    body,
                )
                .await;
                assert_eq!(
                    authorized.status,
                    accepted,
                    "case {case:?}: the endpoint's own credentials must authenticate and be \
                     answered {accepted}, got {} {}",
                    authorized.status,
                    authorized.text()
                );

                // Any other password does not verify, so the push is challenged again.
                let refused = plane(
                    &router,
                    "PUT",
                    &url,
                    &[(
                        "authorization",
                        digest_authorization(
                            challenge,
                            "PUT",
                            path_of(&url),
                            &ingest.username,
                            &format!("{}-wrong", ingest.password),
                        ),
                    )],
                    body,
                )
                .await;
                assert_eq!(
                    refused.status,
                    401,
                    "case {case:?}: a wrong password must be refused, got {} {}",
                    refused.status,
                    refused.text()
                );
            }
        }
    }
}
